use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct SourceRef {
    pub kind: String,
    pub id: String,
}
impl SourceRef {
    pub fn library() -> Self {
        Self {
            kind: "library".into(),
            id: "all".into(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Enhancements {
    pub exposure: bool,
    pub white_balance: bool,
    pub contrast: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub revision: u64,
    pub sources: Vec<SourceRef>,
    pub interval_seconds: u64,
    pub paused: bool,
    pub pinned: Option<String>,
    pub enhancements: Enhancements,
    pub match_colors: bool,
    pub theme: crate::themes::ThemeSettings,
    pub cache_bytes: u64,
    pub fallback: String,
    pub privacy: crate::privacy::policy::PrivacySettings,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            revision: 0,
            sources: vec![],
            interval_seconds: 3600,
            paused: false,
            pinned: None,
            enhancements: Enhancements::default(),
            match_colors: false,
            theme: crate::themes::ThemeSettings::default(),
            cache_bytes: 10 * 1024 * 1024 * 1024,
            fallback: "fit".into(),
            privacy: crate::privacy::policy::PrivacySettings::default(),
        }
    }
}
impl Settings {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (60..=31_536_000).contains(&self.interval_seconds),
            "invalid_interval"
        );
        anyhow::ensure!(self.cache_bytes >= 1024 * 1024, "invalid_cache_budget");
        anyhow::ensure!(
            self.fallback == "fit" || self.fallback == "skip",
            "invalid_fallback"
        );
        self.theme.validate()?;
        anyhow::ensure!(self.sources.len() <= 1000, "too_many_sources");
        for source in &self.sources {
            anyhow::ensure!(
                !source.id.is_empty()
                    && source.id.len() <= 1024
                    && ["library", "album", "shared_album", "person"]
                        .contains(&source.kind.as_str()),
                "invalid_source"
            );
        }
        Ok(())
    }
}

// Resource URLs may contain credentials; no Debug implementation intentionally.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Resource {
    pub url: String,
    pub bytes: u64,
    pub checksum: String,
    pub content_type: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Asset {
    pub id: String,
    pub cloud_guid: String,
    pub cloud_master_guid: String,
    pub library_id: String,
    pub revision: String,
    pub filename: String,
    pub width: u32,
    pub height: u32,
    pub orientation: u32,
    pub captured_at: Option<String>,
    pub favorite: bool,
    pub hidden: bool,
    pub deleted: bool,
    pub rendition: String,
    pub resources: BTreeMap<String, Resource>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Source {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub count: u64,
    pub available: bool,
    pub selected: bool,
    pub stale: bool,
    pub provider: String,
}
impl Default for Source {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: String::new(),
            name: String::new(),
            count: 0,
            available: true,
            selected: false,
            stale: false,
            provider: "icloud".into(),
        }
    }
}
impl Source {
    pub fn library() -> Self {
        Self {
            id: "all".into(),
            kind: "library".into(),
            name: "All photos".into(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Membership {
    pub asset_id: String,
    pub kind: String,
    pub source_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Command {
    pub id: String,
    pub action: String,
}
