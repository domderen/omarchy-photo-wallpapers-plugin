use super::policy::{PrivacyChoice, ScreeningState, Signals, decide};
use crate::storage::{Store, atomic_bytes, read_bounded, validate_private_parents};
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const PREVIEW_LIMIT: u64 = 4 * 1024 * 1024;
const RECORD_LIMIT: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetScope {
    pub account: String,
    pub library: String,
    pub asset: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentKey {
    pub scope: AssetScope,
    pub rendition: String,
    /// Pixel/rendition revision, not a catalog's metadata-only revision.
    pub rendition_revision: String,
    pub media_sha256: String,
    pub preview_sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionKey {
    pub modelset_sha256: String,
    pub preprocessing_version: String,
    pub policy_version: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    schema_version: u32,
    pub content: ContentKey,
    pub identity: DecisionKey,
    pub signals: Signals,
    pub state: ScreeningState,
}
impl Decision {
    pub fn from_signals(content: ContentKey, identity: DecisionKey, signals: Signals) -> Self {
        let state = decide(&signals, PrivacyChoice::Auto, false);
        Self {
            schema_version: 1,
            content,
            identity,
            signals,
            state,
        }
    }
    fn valid(&self) -> bool {
        self.schema_version == 1 && self.state == decide(&self.signals, PrivacyChoice::Auto, false)
    }
}
fn text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control)
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn digest(value: &impl Serialize) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}
impl ContentKey {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            [
                &self.scope.account,
                &self.scope.library,
                &self.scope.asset,
                &self.rendition,
                &self.rendition_revision
            ]
            .into_iter()
            .all(|s| text(s))
                && hash(&self.media_sha256)
                && hash(&self.preview_sha256),
            "invalid_privacy_content"
        );
        Ok(())
    }
}
impl DecisionKey {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            hash(&self.modelset_sha256)
                && text(&self.preprocessing_version)
                && text(&self.policy_version),
            "invalid_privacy_identity"
        );
        Ok(())
    }
}
#[derive(Clone)]
pub struct PrivacyStore {
    store: Store,
}
impl PrivacyStore {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
    pub fn bind_render(&self, render_sha256: &str, content: &ContentKey) -> Result<()> {
        content.validate()?;
        ensure!(
            hash(render_sha256) && self.has_evidence(content),
            "invalid_privacy_render_binding"
        );
        self.store.connection()?.execute(
            "INSERT OR REPLACE INTO privacy_render_content(render_sha256,scope,data) VALUES(?1,?2,?3)",
            params![render_sha256,digest(&content.scope)?,serde_json::to_string(content)?],
        )?;
        Ok(())
    }
    pub fn content_for_render(
        &self,
        render_sha256: &str,
        scope: &AssetScope,
    ) -> Result<Option<ContentKey>> {
        if !hash(render_sha256) {
            return Ok(None);
        }
        let raw: Option<String> = self.store.connection()?.query_row(
            "SELECT data FROM privacy_render_content WHERE render_sha256=?1 AND scope=?2 AND length(data)<=16384",
            params![render_sha256,digest(scope)?], |r|r.get(0),
        ).optional()?;
        Ok(raw
            .and_then(|s| serde_json::from_str::<ContentKey>(&s).ok())
            .filter(|c| &c.scope == scope && c.validate().is_ok()))
    }
    pub fn preview_path(&self, sha256: &str) -> Result<PathBuf> {
        ensure!(hash(sha256), "invalid_privacy_preview");
        Ok(self
            .store
            .paths
            .data
            .join("privacy/previews")
            .join(format!("{sha256}.png")))
    }
    /// Caller supplies a bounded full-frame preview from the trusted decoder.
    pub fn save_preview(&self, bytes: &[u8]) -> Result<String> {
        ensure!(
            !bytes.is_empty() && bytes.len() as u64 <= PREVIEW_LIMIT,
            "invalid_privacy_preview"
        );
        let sha256 = format!("{:x}", Sha256::digest(bytes));
        atomic_bytes(&self.preview_path(&sha256)?, bytes)?;
        Ok(sha256)
    }
    pub fn has_evidence(&self, content: &ContentKey) -> bool {
        if content.validate().is_err() {
            return false;
        }
        self.preview_path(&content.preview_sha256)
            .ok()
            .and_then(|p| {
                validate_private_parents(&p).ok()?;
                read_bounded(&p, PREVIEW_LIMIT).ok()
            })
            .is_some_and(|bytes| format!("{:x}", Sha256::digest(bytes)) == content.preview_sha256)
    }
    pub fn save(&self, decision: &Decision) -> Result<()> {
        decision.content.validate()?;
        decision.identity.validate()?;
        ensure!(
            decision.valid() && self.has_evidence(&decision.content),
            "invalid_privacy_decision"
        );
        let data = serde_json::to_string(decision)?;
        ensure!(data.len() <= RECORD_LIMIT, "invalid_privacy_decision");
        self.store.connection()?.execute(
            "INSERT OR REPLACE INTO privacy_decisions(key,data) VALUES(?1,?2)",
            params![digest(&(&decision.content, &decision.identity))?, data],
        )?;
        Ok(())
    }
    pub fn lookup(&self, content: &ContentKey, identity: &DecisionKey) -> Result<Option<Decision>> {
        content.validate()?;
        identity.validate()?;
        if !self.has_evidence(content) {
            return Ok(None);
        }
        let data: Option<String> = self
            .store
            .connection()?
            .query_row(
                "SELECT data FROM privacy_decisions WHERE key=?1 AND length(data)<=?2",
                params![digest(&(content, identity))?, RECORD_LIMIT as i64],
                |r| r.get(0),
            )
            .optional()?;
        Ok(data
            .and_then(|s| serde_json::from_str::<Decision>(&s).ok())
            .filter(|d| d.valid() && &d.content == content && &d.identity == identity))
    }
    pub fn set_choice(&self, content: &ContentKey, choice: PrivacyChoice) -> Result<()> {
        content.validate()?;
        ensure!(
            choice != PrivacyChoice::Allow || self.has_evidence(content),
            "privacy_evidence_unavailable"
        );
        let scope = digest(&content.scope)?;
        let mut db = self.store.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM privacy_choices WHERE scope=?1", [&scope])?;
        if choice != PrivacyChoice::Auto {
            tx.execute(
                "INSERT INTO privacy_choices(scope,content,data) VALUES(?1,?2,?3)",
                params![scope, digest(content)?, serde_json::to_string(&choice)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn scope_choice(&self, scope: &AssetScope) -> Result<PrivacyChoice> {
        let value: Option<String> = self
            .store
            .connection()?
            .query_row(
                "SELECT substr(data,1,65) FROM privacy_choices WHERE scope=?1",
                [digest(scope)?],
                |r| r.get(0),
            )
            .optional()?;
        Ok(match value {
            Some(value) => match serde_json::from_str::<PrivacyChoice>(&value) {
                Ok(PrivacyChoice::PersonalOnly) | Err(_) => PrivacyChoice::PersonalOnly,
                _ => PrivacyChoice::Auto,
            },
            None => PrivacyChoice::Auto,
        })
    }
    pub fn set_scope_choice(&self, scope: &AssetScope, choice: PrivacyChoice) -> Result<()> {
        ensure!(
            choice != PrivacyChoice::Allow,
            "privacy_evidence_unavailable"
        );
        ensure!(
            text(&scope.account) && text(&scope.library) && text(&scope.asset),
            "invalid_privacy_content"
        );
        let key = digest(scope)?;
        let mut db = self.store.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM privacy_choices WHERE scope=?1", [&key])?;
        if choice == PrivacyChoice::PersonalOnly {
            tx.execute(
                "INSERT INTO privacy_choices(scope,content,data) VALUES(?1,'',?2)",
                params![key, serde_json::to_string(&choice)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn content_for_asset(
        &self,
        scope: &AssetScope,
        pixel_revision: &str,
    ) -> Result<Option<ContentKey>> {
        let db = self.store.connection()?;
        let mut query=db.prepare("SELECT json_extract(data,'$.content') FROM privacy_decisions WHERE length(data)<=16384 AND json_valid(data) AND json_extract(data,'$.content.scope.account')=?1 AND json_extract(data,'$.content.scope.library')=?2 AND json_extract(data,'$.content.scope.asset')=?3 AND json_extract(data,'$.content.rendition_revision')=?4 ORDER BY rowid DESC LIMIT 32")?;
        for row in query.query_map(
            params![scope.account, scope.library, scope.asset, pixel_revision],
            |r| r.get::<_, String>(0),
        )? {
            if let Ok(content) = serde_json::from_str::<ContentKey>(&row?)
                && content.validate().is_ok()
                && self.has_evidence(&content)
            {
                return Ok(Some(content));
            }
        }
        Ok(None)
    }
    pub fn counts(
        &self,
        identity: Option<&DecisionKey>,
        pool: &[String],
    ) -> Result<serde_json::Value> {
        Ok(self.summary(identity, pool)?.0)
    }
    pub(crate) fn summary(
        &self,
        identity: Option<&DecisionKey>,
        pool: &[String],
    ) -> Result<(serde_json::Value, std::collections::HashSet<String>)> {
        let mut states = std::collections::HashMap::new();
        let mut eligible = std::collections::HashSet::new();
        let account = self.store.meta("base-account-id")?;
        if let Some(identity) = identity {
            let db = self.store.connection()?;
            let mut query =
                db.prepare("SELECT data FROM privacy_decisions WHERE length(data)<=16384")?;
            let selected: std::collections::HashSet<_> = pool.iter().collect();
            for row in query.query_map([], |r| r.get::<_, String>(0))? {
                let Ok(d) = serde_json::from_str::<Decision>(&row?) else {
                    continue;
                };
                if &d.identity != identity
                    || !d.valid()
                    || account != serde_json::json!(d.content.scope.account)
                    || !selected.contains(&d.content.scope.asset)
                {
                    continue;
                }
                if let Some(asset) = self.store.asset(&d.content.scope.asset)?
                    && asset.library_id == d.content.scope.library
                    && crate::engine::privacy::pixel_revision(&asset)?
                        == d.content.rendition_revision
                    && self.has_evidence(&d.content)
                {
                    let state = match self.choice_for(&d.content)? {
                        PrivacyChoice::Auto => d.state,
                        PrivacyChoice::Allow => ScreeningState::Clear,
                        PrivacyChoice::PersonalOnly => ScreeningState::Sensitive,
                    };
                    if state == ScreeningState::Clear {
                        eligible.insert(asset.id.clone());
                    }
                    states.insert(asset.id, state);
                }
            }
        }
        let mut counts = serde_json::json!({"clear":0,"sensitive":0,"uncertain":0,"error":0,"unscanned":pool.len().saturating_sub(states.len())});
        for state in states.values() {
            let field = match state {
                ScreeningState::Clear => "clear",
                ScreeningState::Sensitive => "sensitive",
                ScreeningState::Uncertain => "uncertain",
                ScreeningState::Error => "error",
                ScreeningState::Unscanned => "unscanned",
            };
            counts[field] = serde_json::json!(counts[field].as_u64().unwrap_or(0) + 1);
        }
        Ok((counts, eligible))
    }
    pub fn choice_for(&self, content: &ContentKey) -> Result<PrivacyChoice> {
        content.validate()?;
        let value: Option<(String, String)> = self
            .store
            .connection()?
            .query_row(
                "SELECT substr(content,1,65),substr(data,1,65) FROM privacy_choices WHERE scope=?1",
                [digest(&content.scope)?],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(match value {
            Some((_, data))
                if serde_json::from_str::<PrivacyChoice>(&data).ok()
                    == Some(PrivacyChoice::PersonalOnly) =>
            {
                PrivacyChoice::PersonalOnly
            }
            Some((key, data))
                if key == digest(content)?
                    && self.has_evidence(content)
                    && serde_json::from_str::<PrivacyChoice>(&data).ok()
                        == Some(PrivacyChoice::Allow) =>
            {
                PrivacyChoice::Allow
            }
            Some((_, data)) if serde_json::from_str::<PrivacyChoice>(&data).is_err() => {
                PrivacyChoice::PersonalOnly
            }
            _ => PrivacyChoice::Auto,
        })
    }
}
