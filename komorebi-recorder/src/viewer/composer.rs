//! Marking a bug region: where it starts and ends, and what is wrong.
//!
//! This is the most important thing the viewer writes. The region and the words are what an
//! investigation starts from, so the form asks for the words, makes the times easy to take from the
//! video, and shows plainly that a region without a description is not much use.

use super::app::Open;
use super::app::Tab;
use super::theme;
use crate::analysis::envelope;
use crate::annotations;
use crate::annotations::AnnotationView;
use crate::annotations::Kind;
use crate::annotations::New;
use crate::annotations::Patch;
use crate::timecode;
use eframe::egui;

/// The form for one bug region.
pub struct Composer {
    /// The id of the region being changed; `None` for a new one.
    pub editing: Option<String>,
    pub title: String,
    pub text: String,
    pub expected: String,
    /// Timecodes, as typed.
    pub start: String,
    pub end: String,
    /// It was still going on when the recording ended.
    pub open_ended: bool,
    /// Which displays it was seen on; none ticked means any.
    pub displays: Vec<(u32, bool)>,
    pub error: Option<String>,
    /// Focus the description when the form first shows.
    first: bool,
}

impl Composer {
    /// A blank form for a region from `start_s` to `end_s`.
    #[must_use]
    pub fn new(start_s: f64, end_s: f64, displays: &[u32]) -> Self {
        Self {
            editing: None,
            title: String::new(),
            text: String::new(),
            expected: String::new(),
            start: timecode::format(start_s),
            end: timecode::format(end_s),
            open_ended: false,
            displays: displays.iter().map(|id| (*id, false)).collect(),
            error: None,
            first: true,
        }
    }

    /// The form for changing an existing region.
    #[must_use]
    pub fn editing(bug: &AnnotationView, displays: &[u32]) -> Self {
        Self {
            editing: Some(bug.id.clone()),
            title: bug.title.clone(),
            text: bug.text.clone(),
            expected: bug.expected.clone().unwrap_or_default(),
            start: timecode::format(bug.start_video_s),
            end: timecode::format(bug.end_video_s),
            open_ended: bug.open_ended,
            displays: displays
                .iter()
                .map(|id| (*id, bug.displays.contains(id)))
                .collect(),
            error: None,
            first: true,
        }
    }
}

impl Open {
    /// Open the form for a new region: from the selection if there is one, else from the playhead.
    pub fn open_composer(&mut self) {
        let duration = self.loaded.duration_s;
        let (start, end) = self.selection.unwrap_or_else(|| {
            let position = self.position();
            (position, (position + 5.0).min(duration))
        });
        let displays: Vec<u32> = self
            .loaded
            .displays
            .iter()
            .map(|display| display.id)
            .collect();
        self.composer = Some(Composer::new(start, end, &displays));
        self.tab = Tab::Bugs;
    }

    /// Open the form for changing region `id`.
    pub fn open_composer_for(&mut self, id: &str) {
        let displays: Vec<u32> = self
            .loaded
            .displays
            .iter()
            .map(|display| display.id)
            .collect();
        if let Some(bug) = self.loaded.bugs().find(|bug| bug.id == id) {
            self.composer = Some(Composer::editing(bug, &displays));
        }
    }

    /// Write the form to `annotations.jsonl`. On a mistake in the form, says what and keeps it open.
    pub fn save_composer(&mut self) -> Result<(), String> {
        let Some(form) = &self.composer else {
            return Ok(());
        };

        let manifest = &self.loaded.analysis.manifest;
        let duration = self.loaded.duration_s;
        let parse = |what: &str, text: &str| -> Result<f64, String> {
            timecode::parse(text)
                .map_err(|_| format!("{what} is not a time like 12.5 or 0:12.500"))
                .and_then(|seconds| {
                    if (-0.001..=duration + 0.001).contains(&seconds) {
                        Ok(seconds.clamp(0.0, duration))
                    } else {
                        Err(format!(
                            "{what} is outside the recording (0 to {})",
                            timecode::format(duration)
                        ))
                    }
                })
        };

        let start_s = parse("the start", &form.start)?;
        let end_s = if form.open_ended {
            None
        } else {
            Some(parse("the end", &form.end)?)
        };
        if let Some(end) = end_s
            && end < start_s
        {
            return Err("the end is before the start".to_string());
        }
        let text = form.text.trim().to_string();
        if text.is_empty() {
            return Err("say what goes wrong: whoever investigates reads this first".to_string());
        }

        let displays: Vec<u32> = form
            .displays
            .iter()
            .filter(|(_, ticked)| *ticked)
            .map(|(id, _)| *id)
            .collect();
        let expected = form.expected.trim().to_string();
        let start_ns = manifest.mach_at_video_seconds(start_s);
        let end_ns = end_s.map(|end| manifest.mach_at_video_seconds(end));

        let dir = self.loaded.dir.clone();
        let saved = match &form.editing {
            Some(id) => {
                let id = id.clone();
                annotations::edit(
                    &dir,
                    &id,
                    Patch {
                        start_mach_ns: Some(start_ns),
                        end_mach_ns: end_ns,
                        title: Some(form.title.trim().to_string()),
                        text: Some(text),
                        expected: Some(expected),
                        displays: Some(displays),
                        refs: None,
                    },
                )
                .map(|()| id)
            }
            None => annotations::add(
                &dir,
                New {
                    kind: Some(Kind::Bug),
                    start_mach_ns: start_ns,
                    end_mach_ns: end_ns,
                    title: form.title.trim().to_string(),
                    text,
                    expected: (!expected.is_empty()).then_some(expected),
                    displays,
                    author: Some(annotations::AUTHOR_USER.to_string()),
                    about: None,
                    refs: Vec::new(),
                },
            ),
        }
        .map_err(|error| format!("{error:#}"))?;

        self.loaded
            .reload_annotations()
            .map_err(|error| format!("{error:#}"))?;
        self.selected_bug = Some(saved);
        self.selection = None;
        self.composer = None;
        self.say("saved: this is what an investigation will start from");
        Ok(())
    }

    /// Remove an annotation, keeping it to bring back.
    pub fn delete_annotation(&mut self, id: &str) {
        let Some(gone) = self
            .loaded
            .annotations
            .iter()
            .find(|annotation| annotation.id == id)
            .cloned()
        else {
            return;
        };

        match annotations::delete(&self.loaded.dir, id) {
            Ok(()) => {
                let _ = self.loaded.reload_annotations();
                if self.selected_bug.as_deref() == Some(id) {
                    self.selected_bug = None;
                }
                self.deleted.push(gone);
                self.say("deleted (⌘Z brings it back)");
            }
            Err(error) => self.say(format!("could not delete: {error:#}")),
        }
    }

    /// Bring back the annotation deleted last.
    pub fn undo_delete(&mut self) {
        let Some(back) = self.deleted.pop() else {
            return;
        };
        let manifest = &self.loaded.analysis.manifest;
        let result = annotations::add(
            &self.loaded.dir,
            New {
                kind: Some(back.kind),
                start_mach_ns: back.start_mach_ns,
                end_mach_ns: back.end_mach_ns,
                title: back.title.clone(),
                text: back.text.clone(),
                expected: back.expected.clone(),
                displays: back.displays.clone(),
                author: Some(back.author.clone()),
                about: back.about.clone(),
                refs: back.refs.clone(),
            },
        );
        let _ = manifest;
        match result {
            Ok(id) => {
                let _ = self.loaded.reload_annotations();
                self.selected_bug = Some(id);
                self.say("brought back");
            }
            Err(error) => self.say(format!("could not bring it back: {error:#}")),
        }
    }

    /// Move one edge of a bug region, from dragging it on the timeline.
    pub fn move_bug_edge(&mut self, id: &str, moving_start: bool, to_s: f64) -> Result<(), String> {
        let manifest = &self.loaded.analysis.manifest;
        let ns = manifest.mach_at_video_seconds(to_s.clamp(0.0, self.loaded.duration_s));
        let patch = if moving_start {
            Patch {
                start_mach_ns: Some(ns),
                ..Patch::default()
            }
        } else {
            Patch {
                end_mach_ns: Some(ns),
                ..Patch::default()
            }
        };

        annotations::edit(&self.loaded.dir, id, patch).map_err(
            |error| match envelope::exit_kind(&error) {
                envelope::ExitKind::Usage => "a region cannot end before it starts".to_string(),
                _ => format!("{error:#}"),
            },
        )?;
        self.loaded
            .reload_annotations()
            .map_err(|error| format!("{error:#}"))
    }

    pub fn composer_window(&mut self, ctx: &egui::Context) {
        let Some(form) = self.composer.as_mut() else {
            return;
        };

        let position = self.playback.position();
        let mut save = false;
        let mut cancel = false;
        let mut seek: Option<String> = None;
        let mut open = true;
        let title = if form.editing.is_some() {
            "Edit bug region"
        } else {
            "Mark a bug region"
        };

        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(460.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(
                        "Mark the stretch of the recording where the bug is visible, and say what it looks like. \
                         Whoever investigates it starts from exactly this.",
                    )
                    .color(theme::TEXT_DIM),
                );
                ui.add_space(8.0);

                ui.label("What goes wrong");
                let description = ui.add(
                    egui::TextEdit::multiline(&mut form.text)
                        .hint_text("e.g. after unplugging the monitor, my browser window disappears instead of moving to the laptop screen")
                        .desired_rows(4)
                        .desired_width(f32::INFINITY),
                );
                if form.first {
                    description.request_focus();
                    form.first = false;
                }

                ui.add_space(4.0);
                ui.label("What you expected instead (optional)");
                ui.add(
                    egui::TextEdit::multiline(&mut form.expected)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY),
                );

                ui.add_space(4.0);
                ui.label("A short title (optional)");
                ui.add(egui::TextEdit::singleline(&mut form.title).desired_width(f32::INFINITY));

                ui.add_space(8.0);
                egui::Grid::new("times").num_columns(4).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("starts");
                    ui.add(egui::TextEdit::singleline(&mut form.start).desired_width(90.0).font(egui::TextStyle::Monospace));
                    if ui.button("= playhead").clicked() {
                        form.start = timecode::format(position);
                    }
                    if ui.button("go").on_hover_text("show this moment").clicked() {
                        seek = Some(form.start.clone());
                    }
                    ui.end_row();

                    ui.label("ends");
                    ui.add_enabled(
                        !form.open_ended,
                        egui::TextEdit::singleline(&mut form.end).desired_width(90.0).font(egui::TextStyle::Monospace),
                    );
                    if ui.add_enabled(!form.open_ended, egui::Button::new("= playhead")).clicked() {
                        form.end = timecode::format(position);
                    }
                    if ui.add_enabled(!form.open_ended, egui::Button::new("go")).clicked() {
                        seek = Some(form.end.clone());
                    }
                    ui.end_row();
                });
                ui.checkbox(&mut form.open_ended, "still happening when the recording ended");

                if form.displays.len() > 1 {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("seen on");
                        for (id, ticked) in &mut form.displays {
                            ui.checkbox(ticked, format!("display {id}"));
                        }
                        ui.label(egui::RichText::new("(none ticked: any)").small().color(theme::TEXT_DIM));
                    });
                }

                if let Some(error) = &form.error {
                    ui.add_space(6.0);
                    ui.colored_label(theme::BUG, error);
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new(egui::RichText::new("Save").color(egui::Color32::WHITE)).fill(egui::Color32::from_rgb(150, 40, 40)))
                        .clicked()
                    {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if let Some(text) = seek
            && let Ok(seconds) = timecode::parse(&text)
        {
            self.playback.pause();
            self.playback.seek(seconds);
        }
        if cancel || !open {
            self.composer = None;
        } else if save
            && let Err(message) = self.save_composer()
            && let Some(form) = self.composer.as_mut()
        {
            form.error = Some(message);
        }
    }
}
