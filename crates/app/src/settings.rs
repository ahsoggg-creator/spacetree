//! Persistent user settings: `%APPDATA%\SpaceTree\settings.json` (or `~/.config/spacetree`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const MAX_RECENT: usize = 8;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Download and install new releases from GitHub in the background.
    pub auto_update: bool,
    /// Unix seconds of the last update check (GitHub allows 60 anonymous API calls per hour).
    pub last_update_check: u64,
    /// Recently scanned paths, most recent first.
    pub recent: Vec<String>,
    /// Treemap coloring: by file type (false) or by modification age (true).
    pub color_by_age: bool,
    /// Delete to the recycle bin unless the user explicitly asks for permanent deletion.
    pub confirm_delete: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            auto_update: true,
            last_update_check: 0,
            recent: Vec::new(),
            color_by_age: false,
            confirm_delete: true,
        }
    }
}

pub fn dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("SpaceTree"))
    } else {
        std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config").join("spacetree"))
    }
}

fn file() -> Option<PathBuf> {
    dir().map(|d| d.join("settings.json"))
}

impl Settings {
    pub fn load() -> Settings {
        file()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Best effort: settings are a convenience, failing to save them is not worth an error dialog.
    pub fn save(&self) {
        let Some(f) = file() else { return };
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let tmp = f.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &f);
            }
        }
    }

    pub fn push_recent(&mut self, path: &str) {
        self.recent.retain(|p| !p.eq_ignore_ascii_case(path));
        self.recent.insert(0, path.to_owned());
        self.recent.truncate(MAX_RECENT);
    }
}
