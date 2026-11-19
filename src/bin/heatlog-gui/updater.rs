//! Updating heatlog from inside heatlog: no installer, no setup screen.
//! The new program files are downloaded, checked against the release's
//! published checksums and swapped in place, and the user is offered a
//! restart. Also the "what's new" dialog shown once afterwards.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

use eframe::egui::{self, Color32, RichText};
use heatlog::update::{self, Available, Progress};

use crate::{ACCENT, GREEN, RED};

const RELEASES_PAGE: &str = "https://github.com/Hami-K/heatlog/releases";

pub enum Phase {
    Downloading { got: u64, total: u64 },
    Installing,
    Installed,
    Failed(String),
}

/// What the user can ask of the dialog.
pub enum Action {
    Nothing,
    Close,
    Restart,
}

pub struct Install {
    version: String,
    phase: Phase,
    cancel: Arc<AtomicBool>,
    rx: Receiver<Phase>,
}

/// Starts the download on a thread of its own; the UI only watches.
pub fn start(available: &Available, ctx: &egui::Context) -> Install {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let version = available.version.clone();
    let flag = Arc::clone(&cancel);
    // Cloned because the thread outlives this call; an Available is three
    // short strings and a handful of asset names.
    let available = available.clone();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let send = |phase| {
            let _ = tx.send(phase);
            ctx.request_repaint();
        };
        let result = update::install_in_place(&available, &flag, |progress| match progress {
            Progress::Downloading { got, total } => send(Phase::Downloading { got, total }),
            Progress::Installing => send(Phase::Installing),
        });
        send(match result {
            Ok(()) => Phase::Installed,
            Err(e) => Phase::Failed(format!("{e:#}")),
        });
    });
    Install {
        version,
        phase: Phase::Downloading { got: 0, total: 0 },
        cancel,
        rx,
    }
}

impl Install {
    pub fn drain(&mut self) {
        while let Ok(phase) = self.rx.try_recv() {
            self.phase = phase;
        }
    }

    /// True while heatlog's own files are being fetched or replaced, which
    /// is no time to start a recording.
    pub fn busy(&self) -> bool {
        matches!(self.phase, Phase::Downloading { .. } | Phase::Installing)
    }

    pub fn installed(&self) -> bool {
        matches!(self.phase, Phase::Installed)
    }
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

/// The update dialog, in heatlog's own theme.
pub fn dialog(ctx: &egui::Context, install: &mut Install) -> Action {
    dim(ctx, "update-dim");
    let mut action = Action::Nothing;
    egui::Window::new("update")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(420.0)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            match &install.phase {
                Phase::Downloading { got, total } => {
                    ui.label(
                        RichText::new(format!("Downloading heatlog {}", install.version))
                            .strong()
                            .size(16.0),
                    );
                    ui.add_space(8.0);
                    let frac = if *total > 0 {
                        *got as f32 / *total as f32
                    } else {
                        0.0
                    };
                    ui.add(
                        egui::ProgressBar::new(frac)
                            .fill(ACCENT)
                            .desired_height(10.0),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(if *total > 0 {
                            format!("{} of {} from GitHub", mb(*got), mb(*total))
                        } else {
                            "Starting...".to_string()
                        })
                        .weak(),
                    );
                    ui.add_space(10.0);
                    if ui.button("Cancel").clicked() {
                        install.cancel.store(true, Ordering::SeqCst);
                    }
                }
                Phase::Installing => {
                    ui.label(
                        RichText::new(format!("Installing heatlog {}", install.version))
                            .strong()
                            .size(16.0),
                    );
                    ui.add_space(8.0);
                    ui.add(
                        egui::ProgressBar::new(1.0)
                            .fill(ACCENT)
                            .desired_height(10.0),
                    );
                    ui.add_space(6.0);
                    ui.label(RichText::new("Replacing heatlog's files.").weak());
                }
                Phase::Installed => {
                    ui.label(
                        RichText::new("Update installed")
                            .color(GREEN)
                            .strong()
                            .size(16.0),
                    );
                    ui.add_space(6.0);
                    ui.label(format!(
                        "heatlog {} is ready. Restart to finish.",
                        install.version
                    ));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Restart heatlog").clicked() {
                            action = Action::Restart;
                        }
                        if ui.button("Later").clicked() {
                            action = Action::Close;
                        }
                    });
                }
                Phase::Failed(reason) => {
                    let cancelled = reason.contains("cancelled");
                    ui.label(
                        RichText::new(if cancelled {
                            "Update cancelled"
                        } else {
                            "Update failed"
                        })
                        .color(if cancelled { ACCENT } else { RED })
                        .strong()
                        .size(16.0),
                    );
                    ui.add_space(6.0);
                    ui.label(format!(
                        "heatlog is still version {}, and still works.",
                        update::current_version()
                    ));
                    if !cancelled {
                        ui.add_space(4.0);
                        ui.label(RichText::new(reason).weak().small());
                    }
                    ui.add_space(10.0);
                    if ui.button("Close").clicked() {
                        action = Action::Close;
                    }
                }
            }
            ui.add_space(4.0);
        });
    action
}

/// Everything that changed since the version the user was on.
pub struct WhatsNew {
    pub from: String,
    pub notes: Vec<(String, String)>,
}

/// Fetches the notes off the UI thread. An empty list (offline, say) still
/// gets a dialog, just a shorter one.
pub fn spawn_notes(from: &str, ctx: &egui::Context) -> Receiver<Vec<(String, String)>> {
    let (tx, rx) = mpsc::channel();
    let from = from.to_string();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(update::notes_since(&from).unwrap_or_default());
        ctx.request_repaint();
    });
    rx
}

/// Shown once, on the first start after an update actually installed.
/// Returns true when the user dismisses it.
pub fn whats_new_dialog(ctx: &egui::Context, whats_new: &WhatsNew) -> bool {
    dim(ctx, "whats-new-dim");
    let mut dismissed = false;
    egui::Window::new("whats-new")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(460.0)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!(
                    "What's new in heatlog {}",
                    update::current_version()
                ))
                .strong()
                .size(17.0),
            );
            ui.label(
                RichText::new(format!("You were on {}.", whats_new.from))
                    .weak()
                    .small(),
            );
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .show(ui, |ui| {
                    if whats_new.notes.is_empty() {
                        ui.label(
                            "heatlog updated successfully. The release notes could not be \
                             fetched - they are on GitHub.",
                        );
                    }
                    for (tag, body) in &whats_new.notes {
                        ui.label(RichText::new(tag).strong().size(14.0));
                        ui.add_space(2.0);
                        ui.label(if body.is_empty() {
                            "No notes were written for this release."
                        } else {
                            body
                        });
                        ui.add_space(10.0);
                    }
                });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Got it").clicked() {
                    dismissed = true;
                }
                if ui.link("View all release notes").clicked() {
                    crate::pages::open_url(RELEASES_PAGE);
                }
            });
            ui.add_space(4.0);
        });
    dismissed
}

/// Darkens the app behind a dialog, so it reads as one thing to deal with.
/// Painted above the panels but below windows, which egui draws last.
fn dim(ctx: &egui::Context, id: &str) {
    ctx.layer_painter(egui::LayerId::new(
        egui::Order::PanelResizeLine,
        egui::Id::new(id),
    ))
    .rect_filled(ctx.screen_rect(), 0.0, Color32::from_black_alpha(150));
}
