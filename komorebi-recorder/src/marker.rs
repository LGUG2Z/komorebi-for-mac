//! Marking a bug region while recording: the logic behind the recorder window's Mark button.
//!
//! The first click opens a region at that moment, the second closes it, and each is written to
//! `annotations.jsonl` the instant it happens, so a region that is open when the recorder dies is
//! still there, open-ended. Marking uses the same master clock as the recording, so the region
//! lines up with the video exactly. There is deliberately no global hotkey: a bug is marked from
//! the recorder window.

use crate::annotations;
use crate::annotations::AUTHOR_RECORDER_MARK;
use crate::annotations::Kind;
use crate::annotations::New;
use crate::annotations::Patch;
use crate::clock;
use color_eyre::eyre;
use std::path::PathBuf;

#[derive(Debug)]
pub struct Marker {
    dir: PathBuf,
    /// The id of the region that has been started and not yet ended.
    open: Option<String>,
    marked: u32,
}

impl Marker {
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            open: None,
            marked: 0,
        }
    }

    /// A region has been started and not yet ended.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// How many regions have been marked, including one still open.
    #[must_use]
    pub fn marked(&self) -> u32 {
        self.marked
    }

    /// The master-clock time the open region started at.
    pub fn open_since_mach_ns(&self) -> Option<u64> {
        let id = self.open.as_ref()?;
        annotations::load(&self.dir)
            .ok()?
            .get(id)
            .map(|annotation| annotation.start_mach_ns)
    }

    /// Open a region now. Returns its id.
    pub fn start(&mut self) -> eyre::Result<String> {
        if let Some(id) = &self.open {
            return Ok(id.clone());
        }

        let id = annotations::add(
            &self.dir,
            New {
                kind: Some(Kind::Bug),
                start_mach_ns: clock::mach_now_ns(),
                author: Some(AUTHOR_RECORDER_MARK.to_string()),
                ..New::default()
            },
        )?;

        self.open = Some(id.clone());
        self.marked += 1;
        Ok(id)
    }

    /// Close the open region now, with what the user said about it if anything.
    pub fn end(&mut self, text: &str) -> eyre::Result<()> {
        let Some(id) = self.open.clone() else {
            return Ok(());
        };

        let text = text.trim();
        annotations::edit(
            &self.dir,
            &id,
            Patch {
                end_mach_ns: Some(clock::mach_now_ns()),
                text: (!text.is_empty()).then(|| text.to_string()),
                ..Patch::default()
            },
        )?;

        self.open = None;
        Ok(())
    }

    /// Recording is stopping with a region still open: keep it open-ended (the bug was still
    /// there when the recording stopped), but do not lose what the user had typed about it.
    pub fn finish(&mut self, text: &str) -> eyre::Result<()> {
        let Some(id) = self.open.take() else {
            return Ok(());
        };

        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }

        annotations::edit(
            &self.dir,
            &id,
            Patch {
                text: Some(text.to_string()),
                ..Patch::default()
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "komorebi-recorder-marker-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn start_then_end_marks_a_region_with_the_users_note() {
        let dir = temp("region");
        let mut marker = Marker::new(dir.clone());
        assert!(!marker.is_open());

        let id = marker.start().expect("start");
        assert!(marker.is_open());
        assert!(marker.open_since_mach_ns().is_some());

        // on disk the moment it was started, before anything else happens
        let started = annotations::load(&dir).expect("load");
        let region = started.get(&id).expect("region");
        assert!(region.open_ended());
        assert_eq!(region.author, AUTHOR_RECORDER_MARK);

        std::thread::sleep(std::time::Duration::from_millis(5));
        marker
            .end("  tiles overlap after unplugging  ")
            .expect("end");
        assert!(!marker.is_open());

        let ended = annotations::load(&dir).expect("load");
        let region = ended.get(&id).expect("region");
        assert!(!region.open_ended());
        assert!(region.end_mach_ns.expect("end") > region.start_mach_ns);
        assert_eq!(region.text, "tiles overlap after unplugging");
        assert_eq!(marker.marked(), 1);
    }

    #[test]
    fn starting_twice_does_not_open_two_regions() {
        let dir = temp("twice");
        let mut marker = Marker::new(dir.clone());
        let first = marker.start().expect("first");
        let second = marker.start().expect("second");

        assert_eq!(first, second);
        assert_eq!(annotations::load(&dir).expect("load").annotations.len(), 1);
    }

    #[test]
    fn ending_without_a_start_does_nothing() {
        let dir = temp("no-start");
        let mut marker = Marker::new(dir.clone());
        marker.end("x").expect("end");
        assert!(
            annotations::load(&dir)
                .expect("load")
                .annotations
                .is_empty()
        );
    }

    #[test]
    fn several_regions_can_be_marked_in_one_recording() {
        let dir = temp("several");
        let mut marker = Marker::new(dir.clone());
        for _ in 0..3 {
            marker.start().expect("start");
            marker.end("").expect("end");
        }

        let loaded = annotations::load(&dir).expect("load");
        assert_eq!(loaded.annotations.len(), 3);
        assert!(loaded.annotations.iter().all(|region| !region.open_ended()));
        assert_eq!(marker.marked(), 3);
    }

    #[test]
    fn stopping_with_a_region_open_leaves_it_open_ended_but_keeps_the_note() {
        let dir = temp("finish");
        let mut marker = Marker::new(dir.clone());
        let id = marker.start().expect("start");

        marker
            .finish("still broken when I stopped")
            .expect("finish");

        let loaded = annotations::load(&dir).expect("load");
        let region = loaded.get(&id).expect("region");
        assert!(region.open_ended());
        assert_eq!(region.text, "still broken when I stopped");
        assert!(!marker.is_open());
    }

    #[test]
    fn a_region_open_when_the_recorder_dies_is_still_on_disk() {
        let dir = temp("crash");
        {
            let mut marker = Marker::new(dir.clone());
            marker.start().expect("start");
            // the process is killed here: nothing more is written
        }

        let loaded = annotations::load(&dir).expect("load");
        assert_eq!(loaded.annotations.len(), 1);
        assert!(loaded.annotations[0].open_ended());
        assert!(loaded.annotations[0].needs_description());
        assert!(loaded.warnings.is_empty());
    }
}
