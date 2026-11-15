//! The sidebar pages: Overview, CPU, GPU, Memory, Recordings, Settings.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icons;
use egui_plot::{Line, Plot, PlotPoints};

use heatlog::report::{self, fmt_time};
use heatlog::session;

use crate::view::{hover_nearest, TimeFmt};
use crate::{card, heat_color, temp_frac, App, ACCENT, AMBER, GREEN, RED};

// ---------------------------------------------------------------- live pages

/// Big 60-second live graph with filled area and a plain-language tooltip.
fn live_plot(
    ui: &mut egui::Ui,
    id: &str,
    data: &VecDeque<f64>,
    y_max: Option<f64>,
    phrase: fn(f64) -> String,
) {
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
    plot.show(ui, |plot_ui| {
        plot_ui.line(
            Line::new(PlotPoints::from(points.clone()))
                .color(ACCENT)
                .width(2.0_f32)
                .fill(0.0_f32),
        );
        hover_nearest(plot_ui, &points, TimeFmt::Ago, phrase);
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
    ui.heading(format!("{}  {name}", icons::CPU));
    ui.add_space(6.0);
    live_plot(ui, "cpu", &app.hist.cpu_pct, Some(100.0), |v| {
        format!("CPU {v:.0}% busy")
    });
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
                "unavailable (start LibreHardwareMonitor's web server)".to_string(),
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
            ui.heading(format!("{}  {name}", icons::GRAPHICS_CARD));
            ui.add_space(6.0);
            live_plot(ui, "gpu", &app.hist.gpu_temp_c, Some(100.0), |v| {
                format!("GPU {v:.0} C")
            });
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
            ui.heading(format!("{}  GPU", icons::GRAPHICS_CARD));
            ui.add_space(6.0);
            card(ui, |ui| {
                ui.label(
                    RichText::new(format!("{}  No NVIDIA GPU detected", icons::PLUGS))
                        .strong()
                        .size(15.0),
                );
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
    ui.heading(format!("{}  Memory", icons::MEMORY));
    ui.add_space(6.0);
    let total_gb = app.info.as_ref().map(|i| i.ram_total_mib as f64 / 1024.0);
    live_plot(ui, "mem", &app.hist.ram_gb, total_gb, |v| {
        format!("{v:.1} GB in use")
    });
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

// -------------------------------------------------------------------- gauges

/// Round gauge: a 270-degree arc in the given color with a big number in
/// the middle. The fraction is animated, so changes glide.
fn gauge(ui: &mut egui::Ui, id: &str, frac: f32, color: Color32, center: String, label: &str) {
    let frac =
        ui.ctx()
            .animate_value_with_time(egui::Id::new(("gauge", id)), frac.clamp(0.0, 1.0), 0.5);
    let size = 120.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let center_pos = rect.center();
    let radius = size * 0.42;
    // Screen y points down, so 135 degrees is the lower-left: the arc
    // sweeps up and over, leaving the classic gauge opening at the bottom.
    let start = 0.75 * std::f32::consts::PI;
    let sweep = 1.5 * std::f32::consts::PI; // 270 deg

    let arc = |to: f32, stroke_color: Color32, width: f32| {
        let n = 48;
        let points: Vec<egui::Pos2> = (0..=n)
            .map(|i| {
                let angle = start + to * i as f32 / n as f32;
                center_pos + radius * egui::vec2(angle.cos(), angle.sin())
            })
            .collect();
        egui::Shape::line(points, egui::Stroke::new(width, stroke_color))
    };
    let track = ui.visuals().widgets.noninteractive.bg_stroke.color;
    ui.painter().add(arc(sweep, track, 7.0));
    if frac > 0.01 {
        ui.painter().add(arc(sweep * frac, color, 7.0));
    }
    ui.painter().text(
        center_pos,
        egui::Align2::CENTER_CENTER,
        center,
        egui::FontId::proportional(22.0),
        ui.visuals().strong_text_color(),
    );
    ui.painter().text(
        center_pos + egui::vec2(0.0, radius * 0.78),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(12.0),
        ui.visuals().weak_text_color(),
    );
}

// ------------------------------------------------------------------ overview

pub fn overview(ui: &mut egui::Ui, app: &App) {
    ui.heading(format!("{}  Overview", icons::GAUGE));
    ui.add_space(6.0);

    let (color, icon, word, detail) = health(app);
    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(icon).size(22.0).color(color));
            ui.label(RichText::new(word).strong().size(17.0));
            ui.label(RichText::new(detail).weak());
        });
    });
    ui.add_space(10.0);

    // Gauge row: whatever this machine can actually measure.
    card(ui, |ui| {
        ui.horizontal(|ui| {
            if let Some(s) = &app.latest {
                let cpu_frac = s.cpu.usage_pct / 100.0;
                gauge(
                    ui,
                    "cpu-busy",
                    cpu_frac,
                    heat_color(cpu_frac),
                    format!(
                        "{:.0}%",
                        f64::from(animated(ui, "cpu-busy-n", s.cpu.usage_pct))
                    ),
                    "CPU busy",
                );
                if let Some(t) = s.cpu_temp_c {
                    gauge(
                        ui,
                        "cpu-temp",
                        temp_frac(f64::from(t)),
                        heat_color(temp_frac(f64::from(t))),
                        format!("{:.0} C", animated(ui, "cpu-temp-n", t)),
                        "CPU temp",
                    );
                }
                if let Some(g) = &s.gpu {
                    let util = g.util_pct as f32 / 100.0;
                    gauge(
                        ui,
                        "gpu-busy",
                        util,
                        heat_color(util),
                        format!("{:.0}%", animated(ui, "gpu-busy-n", g.util_pct as f32)),
                        "GPU busy",
                    );
                    gauge(
                        ui,
                        "gpu-temp",
                        temp_frac(f64::from(g.temp_c)),
                        heat_color(temp_frac(f64::from(g.temp_c))),
                        format!("{:.0} C", animated(ui, "gpu-temp-n", g.temp_c as f32)),
                        "GPU temp",
                    );
                }
            } else {
                ui.label(RichText::new("starting sensors...").weak());
            }
        });
    });
    ui.add_space(10.0);

    ui.columns(2, |cols| {
        card(&mut cols[0], |ui| {
            ui.label(RichText::new(format!("{}  Memory", icons::MEMORY)).weak());
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
            ui.label(RichText::new(format!("{}  Recording", icons::RECORD)).weak());
            match &app.recording {
                Some(rec) => {
                    ui.label(
                        RichText::new(format!(
                            "{}  {}",
                            icons::RECORD,
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

fn animated(ui: &egui::Ui, id: &str, target: f32) -> f32 {
    ui.ctx()
        .animate_value_with_time(egui::Id::new(("num", id)), target, 0.5)
}

fn health(app: &App) -> (Color32, &'static str, &'static str, String) {
    let Some(s) = &app.latest else {
        return (
            Color32::GRAY,
            icons::HOURGLASS,
            "Starting",
            "waiting for the first sensor read".to_string(),
        );
    };
    let gpu_temp = s.gpu.as_ref().map(|g| g.temp_c);
    let throttling = s.gpu.as_ref().is_some_and(|g| !g.throttle.is_empty());
    let cpu_temp = s.cpu_temp_c;

    if gpu_temp.is_none() && cpu_temp.is_none() {
        // No temperature source at all: saying "fine" would be a guess.
        return (
            Color32::GRAY,
            icons::THERMOMETER,
            "Temperatures unavailable",
            "no GPU detected and no CPU temperature source — usage and clocks are still live"
                .to_string(),
        );
    }
    if throttling || gpu_temp.is_some_and(|t| t >= 85) || cpu_temp.is_some_and(|t| t >= 90.0) {
        let why = if throttling {
            "the GPU is slowing itself down".to_string()
        } else {
            "temperatures at throttle levels".to_string()
        };
        (RED, icons::FLAME, "Too hot — slowing down", why)
    } else if gpu_temp.is_some_and(|t| t >= 75) || cpu_temp.is_some_and(|t| t >= 80.0) {
        (
            AMBER,
            icons::THERMOMETER,
            "Getting warm",
            "elevated but still safe".to_string(),
        )
    } else {
        (
            GREEN,
            icons::SNOWFLAKE,
            "Running cool",
            "temperatures look fine".to_string(),
        )
    }
}

// ---------------------------------------------------------------- recordings

pub struct RowExtra {
    pub class: &'static str,
    pub gpu_peak_c: Option<f64>,
    pub cpu_peak_c: Option<f64>,
    /// Small preview: GPU temperature over the session when a GPU was
    /// recorded, otherwise CPU usage.
    pub spark: Vec<f64>,
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
    /// Lists via the cheap peek; verdicts, peaks and sparklines need full
    /// analysis, so they fill in from a background thread.
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
                    .and_then(|s| {
                        let summary = report::analyze(&s).ok()?;
                        let series: Vec<f64> = if s.samples.iter().any(|x| x.gpu.is_some()) {
                            s.samples
                                .iter()
                                .filter_map(|x| x.gpu.as_ref().map(|g| f64::from(g.temp_c)))
                                .collect()
                        } else {
                            s.samples
                                .iter()
                                .map(|x| f64::from(x.cpu.usage_pct))
                                .collect()
                        };
                        let step = (series.len() / 60).max(1);
                        Some(RowExtra {
                            class: report::verdict(&summary).0,
                            gpu_peak_c: summary.gpu.as_ref().map(|g| g.temp_c.peak),
                            cpu_peak_c: summary.cpu_temp.as_ref().map(|t| t.peak),
                            spark: series.iter().step_by(step).copied().collect(),
                        })
                    })
                    .unwrap_or(RowExtra {
                        class: "na",
                        gpu_peak_c: None,
                        cpu_peak_c: None,
                        spark: Vec::new(),
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

fn row_spark(ui: &mut egui::Ui, id: &str, data: &[f64]) {
    if data.is_empty() {
        return;
    }
    let points: Vec<[f64; 2]> = data
        .iter()
        .enumerate()
        .map(|(i, &v)| [i as f64, v])
        .collect();
    Plot::new(format!("spark-{id}"))
        .width(140.0)
        .height(30.0)
        .show_axes(false)
        .show_grid(false)
        .show_background(false)
        .show_x(false)
        .show_y(false)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_scroll(false)
        .allow_boxed_zoom(false)
        .show(ui, |plot_ui| {
            plot_ui.line(
                Line::new(PlotPoints::from(points))
                    .color(AMBER)
                    .width(1.5_f32)
                    .fill(0.0_f32),
            );
        });
}

pub fn recordings(ui: &mut egui::Ui, recordings: &Recordings) -> Option<PathBuf> {
    ui.heading(format!("{}  Recordings", icons::FILM_STRIP));
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
                                    row_spark(ui, &row.started, &extra.spark);
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
    ui.heading(format!("{}  Settings", icons::GEAR));
    ui.add_space(6.0);
    let path = heatlog::config::config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    card(ui, |ui| {
        ui.label("Settings are read from the config file at startup:");
        ui.label(RichText::new(&path).monospace());
        if ui
            .button(format!("{}  Open folder", icons::FOLDER_OPEN))
            .clicked()
        {
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
