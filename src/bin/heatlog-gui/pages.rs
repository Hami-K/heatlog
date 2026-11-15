//! The sidebar pages: Overview, CPU, GPU, Memory, Recordings, Settings.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use eframe::egui::{self, Color32, RichText};
use egui_plot::{Line, Plot, PlotPoints};

use heatlog::report::{self, fmt_time};
use heatlog::session;

use crate::view::hover_nearest;
use crate::{card, App, ACCENT, AMBER, GREEN, RED};

// ---------------------------------------------------------------- live pages

/// Big 60-second live graph with filled area.
fn live_plot(ui: &mut egui::Ui, id: &str, data: &VecDeque<f64>, unit: &str, y_max: Option<f64>) {
    let points: Vec<[f64; 2]> = data
        .iter()
        .enumerate()
        .map(|(i, &v)| [i as f64 - (data.len() as f64 - 1.0), v])
        .collect();
    let mut plot = Plot::new(format!("live-{id}"))
        .height(240.0)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_scroll(false)
        .allow_boxed_zoom(false)
        .show_x(false)
        .show_y(false)
        .include_x(-60.0)
        .include_x(0.0)
        .include_y(0.0)
        .x_axis_formatter(|mark, _| {
            let ago = -mark.value;
            if ago <= 0.5 {
                "now".to_string()
            } else {
                format!("{ago:.0}s")
            }
        });
    if let Some(y) = y_max {
        plot = plot.include_y(y);
    }
    let unit = unit.to_string();
    plot.show(ui, |plot_ui| {
        plot_ui.line(
            Line::new(PlotPoints::from(points.clone()))
                .color(ACCENT)
                .width(2.0_f32)
                .fill(0.0_f32),
        );
        hover_nearest(plot_ui, &points, &unit, false);
    });
}

fn stat_grid(ui: &mut egui::Ui, id: &str, stats: &[(&str, String)]) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing(egui::vec2(24.0, 6.0))
        .show(ui, |ui| {
            for (label, value) in stats {
                ui.label(RichText::new(*label).weak());
                ui.label(RichText::new(value).strong());
                ui.end_row();
            }
        });
}

pub fn cpu(ui: &mut egui::Ui, app: &App) {
    let name = app
        .info
        .as_ref()
        .map(|i| i.cpu_name.clone())
        .unwrap_or_else(|| "CPU".to_string());
    ui.heading(name);
    ui.add_space(6.0);
    live_plot(ui, "cpu", &app.hist.cpu_pct, "%", Some(100.0));
    ui.add_space(10.0);

    let mut stats: Vec<(&str, String)> = Vec::new();
    if let Some(s) = &app.latest {
        stats.push(("Utilization", format!("{:.0}%", s.cpu.usage_pct)));
        stats.push((
            "Speed (fastest core)",
            format!(
                "{:.2} GHz",
                s.cpu.per_core_mhz.iter().copied().max().unwrap_or(0) as f64 / 1000.0
            ),
        ));
        stats.push(("Logical processors", s.cpu.per_core_mhz.len().to_string()));
        match s.cpu_temp_c {
            Some(t) => stats.push(("Temperature", format!("{t:.0} C"))),
            None => stats.push((
                "Temperature",
                match app.info.as_ref().and_then(|i| i.cpu_temp_source) {
                    Some(_) => "reading...".to_string(),
                    None => "unavailable (start LibreHardwareMonitor)".to_string(),
                },
            )),
        }
    } else {
        stats.push(("Status", "starting sensors...".to_string()));
    }
    stat_grid(ui, "cpu-stats", &stats);
}

pub fn gpu(ui: &mut egui::Ui, app: &App) {
    let name = app.info.as_ref().and_then(|i| i.gpu_name.clone());
    match (&name, app.latest.as_ref().and_then(|s| s.gpu.as_ref())) {
        (Some(name), Some(g)) => {
            ui.heading(name);
            ui.add_space(6.0);
            live_plot(ui, "gpu", &app.hist.gpu_temp_c, " C", Some(100.0));
            ui.add_space(10.0);
            let stats = vec![
                ("Temperature", format!("{} C", g.temp_c)),
                ("Utilization", format!("{}%", g.util_pct)),
                (
                    "Speed",
                    format!("{:.2} GHz", f64::from(g.core_mhz) / 1000.0),
                ),
                (
                    "Memory speed",
                    format!("{:.2} GHz", f64::from(g.mem_mhz) / 1000.0),
                ),
                ("Power draw", format!("{:.0} W", g.power_w)),
                (
                    "VRAM used",
                    format!("{:.1} GB", g.vram_used_mib as f64 / 1024.0),
                ),
                (
                    "Throttling now",
                    if g.throttle.is_empty() {
                        "no".to_string()
                    } else {
                        g.throttle.join(", ")
                    },
                ),
            ];
            stat_grid(ui, "gpu-stats", &stats);
        }
        _ => {
            ui.heading("GPU");
            ui.add_space(6.0);
            card(ui, |ui| {
                ui.label("No NVIDIA GPU detected on this machine.");
                ui.label(
                    RichText::new(
                        "CPU and memory are still monitored and recorded. On a machine with \
                         an NVIDIA GPU, temperature, clocks, power and throttle reasons appear here.",
                    )
                    .weak(),
                );
            });
        }
    }
}

pub fn memory(ui: &mut egui::Ui, app: &App) {
    ui.heading("Memory");
    ui.add_space(6.0);
    let total_gb = app.info.as_ref().map(|i| i.ram_total_mib as f64 / 1024.0);
    live_plot(ui, "mem", &app.hist.ram_gb, " GB", total_gb);
    ui.add_space(10.0);
    if let Some(s) = &app.latest {
        let used = s.ram_used_mib as f64 / 1024.0;
        let mut stats = vec![("In use", format!("{used:.1} GB"))];
        if let Some(total) = total_gb {
            stats.push(("Total", format!("{total:.1} GB")));
            stats.push(("Used", format!("{:.0}%", 100.0 * used / total)));
        }
        stat_grid(ui, "mem-stats", &stats);
    }
}

// ------------------------------------------------------------------ overview

pub fn overview(ui: &mut egui::Ui, app: &App) {
    ui.heading("Overview");
    ui.add_space(6.0);

    let (color, word, detail) = health(app);
    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("\u{25CF}").size(18.0).color(color));
            ui.label(RichText::new(word).strong().size(16.0));
            ui.label(RichText::new(detail).weak());
        });
    });
    ui.add_space(8.0);

    ui.columns(2, |cols| {
        card(&mut cols[0], |ui| {
            ui.label(RichText::new("CPU").weak());
            match &app.latest {
                Some(s) => {
                    ui.label(
                        RichText::new(format!(
                            "{:.0}%  ·  {:.2} GHz",
                            s.cpu.usage_pct,
                            s.cpu.per_core_mhz.iter().copied().max().unwrap_or(0) as f64 / 1000.0
                        ))
                        .strong()
                        .size(20.0),
                    );
                    if let Some(t) = s.cpu_temp_c {
                        ui.label(format!("{t:.0} C"));
                    }
                }
                None => {
                    ui.label(RichText::new("starting sensors...").size(20.0).weak());
                }
            }
        });
        card(&mut cols[1], |ui| {
            ui.label(RichText::new("GPU").weak());
            match app.latest.as_ref().and_then(|s| s.gpu.as_ref()) {
                Some(g) => {
                    ui.label(
                        RichText::new(format!("{} C  ·  {}%", g.temp_c, g.util_pct))
                            .strong()
                            .size(20.0),
                    );
                    ui.label(if g.throttle.is_empty() {
                        "not throttling".to_string()
                    } else {
                        format!("THROTTLING: {}", g.throttle.join(", "))
                    });
                }
                None => {
                    ui.label(RichText::new("not detected").size(20.0).weak());
                }
            }
        });
    });
    ui.add_space(8.0);
    ui.columns(2, |cols| {
        card(&mut cols[0], |ui| {
            ui.label(RichText::new("Memory").weak());
            let total = app.info.as_ref().map(|i| i.ram_total_mib).unwrap_or(0) as f64 / 1024.0;
            match &app.latest {
                Some(s) => {
                    let used = s.ram_used_mib as f64 / 1024.0;
                    let pct = if total > 0.0 {
                        format!(" ({:.0}%)", 100.0 * used / total)
                    } else {
                        String::new()
                    };
                    ui.label(
                        RichText::new(format!("{used:.1} / {total:.1} GB{pct}"))
                            .strong()
                            .size(20.0),
                    );
                }
                None => {
                    ui.label(RichText::new("...").size(20.0).weak());
                }
            }
        });
        card(&mut cols[1], |ui| {
            ui.label(RichText::new("Recording").weak());
            match &app.recording {
                Some(rec) => {
                    ui.label(
                        RichText::new(format!(
                            "\u{25CF} {}",
                            fmt_time(rec.started.elapsed().as_secs())
                        ))
                        .strong()
                        .size(20.0)
                        .color(RED),
                    );
                    if let Some(game) = &rec.game {
                        ui.label(game);
                    }
                }
                None => {
                    ui.label(RichText::new("not recording").size(20.0).weak());
                    if let Some(status) = &app.status {
                        ui.label(RichText::new(status).weak());
                    }
                }
            }
        });
    });
}

fn health(app: &App) -> (Color32, &'static str, String) {
    let Some(s) = &app.latest else {
        return (
            Color32::GRAY,
            "Starting",
            "waiting for first sensor read".to_string(),
        );
    };
    let gpu_temp = s.gpu.as_ref().map(|g| g.temp_c);
    let throttling = s.gpu.as_ref().is_some_and(|g| !g.throttle.is_empty());
    let cpu_temp = s.cpu_temp_c;

    if throttling || gpu_temp.is_some_and(|t| t >= 85) || cpu_temp.is_some_and(|t| t >= 90.0) {
        let why = if throttling {
            "GPU is throttling".to_string()
        } else {
            "temperatures at throttle levels".to_string()
        };
        (RED, "Hot", why)
    } else if gpu_temp.is_some_and(|t| t >= 75) || cpu_temp.is_some_and(|t| t >= 80.0) {
        (AMBER, "Warm", "temperatures elevated but safe".to_string())
    } else {
        (GREEN, "Normal", "temperatures look fine".to_string())
    }
}

// ---------------------------------------------------------------- recordings

pub struct RowExtra {
    pub class: &'static str,
    pub gpu_peak_c: Option<f64>,
    pub cpu_peak_c: Option<f64>,
}

pub struct Row {
    pub path: PathBuf,
    pub started: String,
    pub duration: String,
    pub game: Option<String>,
    pub extra: Option<RowExtra>,
}

pub struct Recordings {
    pub rows: Vec<Row>,
    rx: Option<Receiver<(usize, RowExtra)>>,
    pub error: Option<String>,
}

impl Recordings {
    /// Lists via the cheap peek; verdicts and peaks need full analysis, so
    /// they fill in from a background thread.
    pub fn load(ctx: &egui::Context) -> Self {
        let files = heatlog::config::load_or_create()
            .and_then(|cfg| cfg.sessions_dir())
            .and_then(|dir| session::list_files(&dir));
        let mut files = match files {
            Ok(files) => files,
            Err(e) => {
                return Self {
                    rows: Vec::new(),
                    rx: None,
                    error: Some(format!("{e:#}")),
                }
            }
        };
        files.reverse(); // newest first
        let rows: Vec<Row> = files
            .into_iter()
            .filter_map(|path| {
                let peek = session::peek(&path).ok()?;
                Some(Row {
                    path,
                    started: peek.meta.started.format("%Y-%m-%d %H:%M").to_string(),
                    duration: fmt_time(peek.duration_secs),
                    game: peek.meta.game,
                    extra: None,
                })
            })
            .collect();

        let (tx, rx) = mpsc::channel();
        // cloned so the worker owns its own path list
        let paths: Vec<PathBuf> = rows.iter().map(|r| r.path.clone()).collect();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for (i, path) in paths.iter().enumerate() {
                let extra = session::read(path)
                    .ok()
                    .and_then(|s| report::analyze(&s).ok())
                    .map(|summary| RowExtra {
                        class: report::verdict(&summary).0,
                        gpu_peak_c: summary.gpu.as_ref().map(|g| g.temp_c.peak),
                        cpu_peak_c: summary.cpu_temp.as_ref().map(|t| t.peak),
                    })
                    .unwrap_or(RowExtra {
                        class: "na",
                        gpu_peak_c: None,
                        cpu_peak_c: None,
                    });
                if tx.send((i, extra)).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        Self {
            rows,
            rx: Some(rx),
            error: None,
        }
    }

    pub fn drain(&mut self) {
        if let Some(rx) = &self.rx {
            while let Ok((i, extra)) = rx.try_recv() {
                if let Some(row) = self.rows.get_mut(i) {
                    row.extra = Some(extra);
                }
            }
        }
    }
}

pub fn verdict_pill(ui: &mut egui::Ui, class: &str) {
    let (color, label) = match class {
        "good" => (GREEN, "No throttling"),
        "warn" => (AMBER, "Ran warm"),
        "bad" => (RED, "Throttled"),
        _ => (Color32::GRAY, "Low load"),
    };
    egui::Frame::none()
        .fill(color)
        .rounding(10.0)
        .inner_margin(egui::Margin::symmetric(8.0, 3.0))
        .show(ui, |ui| {
            ui.label(RichText::new(label).color(Color32::WHITE).size(12.0));
        });
}

pub fn recordings(ui: &mut egui::Ui, recordings: &Recordings) -> Option<PathBuf> {
    ui.heading("Recordings");
    ui.add_space(6.0);
    if let Some(error) = &recordings.error {
        ui.colored_label(RED, error);
        return None;
    }
    if recordings.rows.is_empty() {
        ui.label(
            "No recordings yet. Hit Start recording, or turn on auto-record and launch a game.",
        );
        return None;
    }
    let mut open = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        for row in &recordings.rows {
            let response = egui::Frame::none()
                .fill(ui.visuals().faint_bg_color)
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    ui.visuals().widgets.noninteractive.bg_stroke.color,
                ))
                .rounding(8.0)
                .inner_margin(12.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&row.started).strong());
                            let mut line = row.duration.clone();
                            if let Some(game) = &row.game {
                                line.push_str("  ·  ");
                                line.push_str(game);
                            }
                            ui.label(RichText::new(line).weak());
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            match &row.extra {
                                Some(extra) => {
                                    verdict_pill(ui, extra.class);
                                    let mut peaks = Vec::new();
                                    if let Some(t) = extra.gpu_peak_c {
                                        peaks.push(format!("GPU peak {t:.0} C"));
                                    }
                                    if let Some(t) = extra.cpu_peak_c {
                                        peaks.push(format!("CPU peak {t:.0} C"));
                                    }
                                    if !peaks.is_empty() {
                                        ui.label(RichText::new(peaks.join("  ·  ")).weak());
                                    }
                                }
                                None => {
                                    ui.label(RichText::new("analyzing...").weak());
                                }
                            }
                        });
                    });
                })
                .response
                .interact(egui::Sense::click());
            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if response.clicked() {
                open = Some(row.path.clone());
            }
            ui.add_space(4.0);
        }
    });
    open
}

// ------------------------------------------------------------------ settings

pub fn settings(ui: &mut egui::Ui, app: &App) {
    ui.heading("Settings");
    ui.add_space(6.0);
    let path = heatlog::config::config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    card(ui, |ui| {
        ui.label("Settings are read from the config file at startup:");
        ui.label(RichText::new(&path).monospace());
        if ui.button("Open folder").clicked() {
            if let Some(dir) = std::path::Path::new(&path).parent() {
                let _ = std::process::Command::new("explorer").arg(dir).spawn();
            }
        }
        ui.add_space(6.0);
        if let Some(info) = &app.info {
            let games = if info.games.is_empty() {
                "none configured".to_string()
            } else {
                info.games.join(", ")
            };
            stat_grid(
                ui,
                "settings",
                &[
                    ("Games for auto-record", games),
                    (
                        "CPU temperature source",
                        info.cpu_temp_source.unwrap_or("unavailable").to_string(),
                    ),
                ],
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Find a game's process name in Task Manager (Details tab, Name column) \
                     while it is running, add it to `games` in the config file, then restart \
                     the app. In-app editing is coming in the next update.",
                )
                .weak(),
            );
        }
    });
}
