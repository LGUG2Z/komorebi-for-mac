//! What the viewer remembers between runs: which sessions were opened and how it was left.

use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::path::PathBuf;

const FILE: &str = "viewer-prefs.json";
const RECENT: usize = 12;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Most recent first.
    pub recent: Vec<String>,
    pub overlay: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            recent: Vec::new(),
            overlay: true,
        }
    }
}

impl Prefs {
    /// Read the preferences kept in `dir`, or the defaults if there are none or they are damaged:
    /// they are only conveniences, never worth refusing to start over.
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join(FILE))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Write them back. A failure is ignored for the same reason.
    pub fn save(&self, dir: &Path) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(dir.join(FILE), text);
        }
    }

    /// Note that a session was opened, moving it to the front.
    pub fn opened(&mut self, session: &Path) {
        let text = session.display().to_string();
        self.recent.retain(|recent| *recent != text);
        self.recent.insert(0, text);
        self.recent.truncate(RECENT);
    }

    /// The recent sessions that still exist.
    #[must_use]
    pub fn existing(&self) -> Vec<PathBuf> {
        self.recent
            .iter()
            .map(PathBuf::from)
            .filter(|path| path.join(crate::session::MANIFEST_FILE).is_file())
            .collect()
    }
}

/// Where preferences live: komorebi's data directory.
#[must_use]
pub fn directory() -> PathBuf {
    komorebi_client::DATA_DIR.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "komorebi-recorder-prefs-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn preferences_round_trip_and_default_when_missing() {
        let dir = temp("round");
        assert_eq!(Prefs::load(&dir), Prefs::default());

        let mut prefs = Prefs {
            overlay: false,
            ..Prefs::default()
        };
        prefs.opened(Path::new("/a"));
        prefs.save(&dir);
        assert_eq!(Prefs::load(&dir), prefs);
    }

    #[test]
    fn damaged_preferences_are_ignored_not_fatal() {
        let dir = temp("damaged");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join(FILE), "not json {").expect("write");
        assert_eq!(Prefs::load(&dir), Prefs::default());

        // an older file with fewer fields still reads
        std::fs::write(dir.join(FILE), r#"{"overlay": false}"#).expect("write");
        assert!(!Prefs::load(&dir).overlay);
    }

    #[test]
    fn the_most_recent_session_is_first_and_each_is_listed_once() {
        let mut prefs = Prefs::default();
        prefs.opened(Path::new("/a"));
        prefs.opened(Path::new("/b"));
        prefs.opened(Path::new("/a"));
        assert_eq!(prefs.recent, ["/a", "/b"]);

        for index in 0..30 {
            prefs.opened(Path::new(&format!("/s{index}")));
        }
        assert_eq!(prefs.recent.len(), RECENT);
        assert_eq!(prefs.recent[0], "/s29");
    }

    #[test]
    fn only_sessions_that_still_exist_are_offered() {
        let (session, _) = crate::testkit::SessionBuilder::new("prefs-existing").build();
        let mut prefs = Prefs::default();
        prefs.opened(&session.dir);
        prefs.opened(Path::new("/gone/forever"));

        assert_eq!(prefs.existing(), [session.dir]);
    }
}
