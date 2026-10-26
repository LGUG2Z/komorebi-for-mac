//! Keeping file access inside the session directory.
//!
//! Session files name other session files (a timeline entry's `file`, an annotation's log
//! reference), and sessions are handed between people and agents. Nothing read from a session may
//! therefore be allowed to point outside it: absolute paths, `..`, and symlinks that lead out are
//! all refused.

use super::envelope;
use crate::session::MANIFEST_FILE;
use color_eyre::eyre;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

/// A session directory known to be one, with its real (symlink-free) location.
#[derive(Debug, Clone)]
pub struct SessionPath {
    root: PathBuf,
}

impl SessionPath {
    pub fn new(dir: &Path) -> eyre::Result<Self> {
        let root = dir.canonicalize().map_err(|error| {
            envelope::not_found(format!("cannot open {}: {error}", dir.display()))
        })?;

        if !root.join(MANIFEST_FILE).is_file() {
            return Err(envelope::not_found(format!(
                "{} is not a recording session (no {MANIFEST_FILE})",
                dir.display()
            )));
        }

        Ok(Self { root })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The path of `relative` inside the session. It must be a plain relative path and, if it
    /// exists, must really be inside the session once symlinks are followed.
    pub fn resolve(&self, relative: &str) -> eyre::Result<PathBuf> {
        let path = Path::new(relative);
        let plain = !relative.is_empty()
            && path
                .components()
                .all(|component| matches!(component, Component::Normal(_)));
        if !plain {
            return Err(envelope::usage(format!(
                "'{relative}' is not a path inside the session"
            )));
        }

        let joined = self.root.join(path);
        if let Ok(real) = joined.canonicalize()
            && !real.starts_with(&self.root)
        {
            return Err(envelope::usage(format!(
                "'{relative}' leads outside the session"
            )));
        }

        Ok(joined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "komorebi-recorder-guard-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn a_directory_without_a_manifest_is_not_a_session() {
        let dir = temp("empty");
        let error = SessionPath::new(&dir).expect_err("not a session");
        assert_eq!(envelope::exit_kind(&error), envelope::ExitKind::NotFound);
        let missing = SessionPath::new(&dir.join("nope")).expect_err("missing");
        assert_eq!(envelope::exit_kind(&missing), envelope::ExitKind::NotFound);
    }

    #[test]
    fn plain_relative_paths_resolve_inside_the_session() {
        let dir = temp("plain");
        std::fs::write(dir.join(MANIFEST_FILE), "{}").expect("manifest");
        let session = SessionPath::new(&dir).expect("session");

        let path = session.resolve("oslog/apps/x.jsonl").expect("resolve");
        assert!(path.starts_with(session.root()));
        assert!(path.ends_with("oslog/apps/x.jsonl"));
    }

    #[test]
    fn escapes_are_refused() {
        let dir = temp("escape");
        std::fs::write(dir.join(MANIFEST_FILE), "{}").expect("manifest");
        let session = SessionPath::new(&dir).expect("session");

        for bad in ["../x", "a/../../x", "/etc/passwd", "", "."] {
            assert!(session.resolve(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn a_symlink_that_leads_out_is_refused() {
        let outside = temp("outside");
        std::fs::write(outside.join("secret.txt"), "x").expect("secret");
        let dir = temp("symlink");
        std::fs::write(dir.join(MANIFEST_FILE), "{}").expect("manifest");
        std::os::unix::fs::symlink(&outside, dir.join("link")).expect("symlink");
        let session = SessionPath::new(&dir).expect("session");

        assert!(session.resolve("link/secret.txt").is_err());
        assert!(session.resolve("link").is_err());
        // a symlink that stays inside is fine
        std::fs::create_dir_all(dir.join("real")).expect("real");
        std::os::unix::fs::symlink(dir.join("real"), dir.join("alias")).expect("alias");
        assert!(session.resolve("alias").is_ok());
    }
}
