//! Private newline JSON IPC. Bounded frames, same-user peers, no request-body logging.
use crate::{
    engine::EngineHandle,
    storage::{Paths, private_dir},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    sync::Arc,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;
const LIMIT: usize = 65536;
const IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// Increment when request/response semantics change incompatibly.
pub const PROTOCOL_VERSION: u64 = 1;
#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Request {
    pub id: String,
    pub command: String,
    pub operation: String,
    pub input: Value,
    pub expected_revision: Option<u64>,
}
pub fn bind(paths: &Paths) -> Result<UnixListener> {
    private_dir(&paths.runtime)?;
    let socket = paths.socket();
    if socket.exists() {
        ensure!(
            std::fs::symlink_metadata(&socket)?.file_type().is_socket(),
            "socket_path_occupied"
        );
        ensure!(
            std::os::unix::net::UnixStream::connect(&socket).is_err(),
            "daemon_already_running"
        );
        std::fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}
async fn frame(
    reader: &mut BufReader<impl AsyncRead + Unpin>,
    output: &mut Zeroizing<Vec<u8>>,
    deadline: &mut Option<tokio::time::Instant>,
    subscribed: bool,
) -> Result<Option<Zeroizing<Vec<u8>>>> {
    loop {
        // State lives in connection(), so status updates cannot discard a
        // partially consumed request when this future is cancelled by select!.
        if !subscribed && deadline.is_none() {
            *deadline = Some(tokio::time::Instant::now() + IO_TIMEOUT);
        }
        let bytes = if let Some(deadline) = *deadline {
            tokio::time::timeout_at(deadline, reader.fill_buf())
                .await
                .map_err(|_| anyhow::anyhow!("request_timeout"))??
        } else {
            reader.fill_buf().await?
        };
        if bytes.is_empty() {
            return Ok(None);
        }
        if output.is_empty() {
            *deadline = Some(tokio::time::Instant::now() + IO_TIMEOUT);
        }
        let length = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map(|n| n + 1)
            .unwrap_or(bytes.len());
        ensure!(output.len() + length <= LIMIT, "request_too_large");
        let done = bytes[length - 1] == b'\n';
        output.extend_from_slice(&bytes[..length]);
        reader.consume(length);
        if done {
            *deadline = None;
            return Ok(Some(std::mem::take(output)));
        }
    }
}
async fn send(write: &mut (impl AsyncWrite + Unpin), bytes: &[u8]) -> Result<()> {
    tokio::time::timeout(IO_TIMEOUT, write.write_all(bytes)).await??;
    Ok(())
}
async fn connection(stream: UnixStream, engine: EngineHandle) -> Result<()> {
    let uid = std::fs::metadata(&engine.paths.data)?.uid();
    ensure!(stream.peer_cred()?.uid() == uid, "unauthorized_peer");
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut updates = engine.updates.clone();
    let mut subscribed = false;
    let mut partial = Zeroizing::new(Vec::new());
    let mut deadline = None;
    loop {
        tokio::select! {
         input=frame(&mut reader,&mut partial,&mut deadline,subscribed)=>{
          let input=match input{Ok(Some(v))=>v,Ok(None)=>break,Err(error)=>{
            let code=if error.to_string()=="request_timeout"{"request_timeout"}else{"request_too_large"};
            let mut bytes=serde_json::to_vec(&json!({"id":"","result":{"error":code}}))?;bytes.push(b'\n');send(&mut write,&bytes).await?;break;
          }};
          let request=serde_json::from_slice::<Request>(&input);drop(input);
          let response=match request {
           Ok(request)if request.id.len()<=128&&request.id.is_ascii()=>{
            let id=request.id.clone();let result=if request.command=="subscribe"{subscribed=true;updates.borrow_and_update().clone()}else{engine.request(request).await?};json!({"id":id,"result":result})
           },_=>json!({"id":"","result":{"error":"invalid_request"}}),
          };
          let mut bytes=serde_json::to_vec(&response)?;bytes.push(b'\n');send(&mut write,&bytes).await?;
         }
         result=updates.changed(),if subscribed=>{if result.is_err(){break;}let status=updates.borrow_and_update().clone();let mut bytes=serde_json::to_vec(&json!({"event":"status","status":status}))?;bytes.push(b'\n');send(&mut write,&bytes).await?;}
        }
    }
    Ok(())
}
pub async fn serve(
    listener: UnixListener,
    engine: EngineHandle,
    stop: CancellationToken,
) -> Result<()> {
    let slots = Arc::new(tokio::sync::Semaphore::new(32));
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
        _=stop.cancelled()=>break,
        stream=listener.accept()=>{let(stream,_)=stream?;if let Ok(permit)=slots.clone().try_acquire_owned(){let engine=engine.clone();tasks.spawn(async move{let _permit=permit;let _=connection(stream,engine).await;});}},
        _=tasks.join_next(),if !tasks.is_empty()=>{},
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}
pub async fn call(paths: &Paths, request: Request) -> Result<Value> {
    let response = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut stream = UnixStream::connect(paths.socket())
            .await
            .map_err(|_| anyhow::anyhow!("engine_unavailable"))?;
        let mut bytes = Zeroizing::new(serde_json::to_vec(&request)?);
        ensure!(bytes.len() < LIMIT, "request_too_large");
        bytes.push(b'\n');
        stream.write_all(&bytes).await?;
        drop(bytes);
        let mut reader = BufReader::new(stream.take(2 * 1024 * 1024));
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let value: Value = serde_json::from_str(&line)?;
        Ok::<_, anyhow::Error>(value["result"].clone())
    })
    .await??;
    Ok(response)
}
