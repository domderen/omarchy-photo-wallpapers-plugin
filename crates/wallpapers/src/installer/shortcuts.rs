//! Only an exact, application-owned block is changed in the user's Lua config.
use crate::storage::{atomic_bytes, read_bounded, validate_private_parents};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::path::Path;
const START: &str = "-- BEGIN domderen.photo-wallpapers meeting shortcuts\n";
const END: &str = "-- END domderen.photo-wallpapers meeting shortcuts\n";
fn block(binary: &Path) -> Result<String> {
    let command = binary
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("invalid_shortcut_path"))?;
    ensure!(
        !command.chars().any(char::is_control),
        "invalid_shortcut_path"
    );
    let quoted = format!("'{}'", command.replace('\'', "'\\''"));
    Ok(format!(
        "{START}o.bind(\"SUPER + CTRL + M\", \"Photo Wallpapers Meeting Mode on\", {})\no.bind(\"SUPER + CTRL + SHIFT + M\", \"Photo Wallpapers Meeting Mode off\", {})\n{END}",
        serde_json::to_string(&format!("{quoted} meeting on"))?,
        serde_json::to_string(&format!("{quoted} meeting off"))?
    ))
}
fn strip(text: &str, owned: &str) -> Result<String> {
    let count = text.matches(START).count();
    ensure!(count <= 1, "modified_shortcut_block");
    if count == 0 {
        ensure!(!text.contains(END), "modified_shortcut_block");
        return Ok(text.into());
    }
    ensure!(text.matches(owned).count() == 1, "modified_shortcut_block");
    Ok(text.replacen(owned, "", 1))
}
pub fn install(home: &Path, binary: &Path, bindings: &Value) -> Result<bool> {
    let path = home.join(".config/hypr/bindings.lua");
    validate_private_parents(&path)?;
    let original = if path.exists() {
        String::from_utf8(read_bounded(&path, 1024 * 1024)?)?
    } else {
        String::new()
    };
    let owned = block(binary)?;
    let ours = original.contains(&owned);
    let entries = bindings
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("shortcut_snapshot_unavailable"))?;
    for bind in entries {
        if bind["key"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("m"))
            && matches!(bind["modmask"].as_u64(), Some(68 | 69))
        {
            let description = if bind["modmask"] == 68 {
                "Photo Wallpapers Meeting Mode on"
            } else {
                "Photo Wallpapers Meeting Mode off"
            };
            if !(ours && bind["description"] == description) {
                return Ok(false);
            }
        }
    }
    let mut text = strip(&original, &owned)?;
    // Keep preceding bytes exactly; one owned leading newline is part of the block.
    if !text.is_empty() && !text.ends_with('\n') {
        return Err(anyhow::anyhow!("shortcut_config_needs_newline"));
    }
    text.push_str(&owned);
    if text != original {
        atomic_bytes(&path, text.as_bytes())?;
    }
    Ok(true)
}
pub fn remove(home: &Path, binary: &Path) -> Result<()> {
    let path = home.join(".config/hypr/bindings.lua");
    validate_private_parents(&path)?;
    if !path.exists() {
        return Ok(());
    }
    let original = String::from_utf8(read_bounded(&path, 1024 * 1024)?)?;
    let text = strip(&original, &block(binary)?)?;
    if text != original {
        atomic_bytes(&path, text.as_bytes())?;
    }
    Ok(())
}
