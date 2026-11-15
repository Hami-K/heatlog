//! The sidebar pages: Overview, CPU, GPU, Memory, Recordings, Settings,
//! and the CPU-temperature setup flow.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icons;
use egui_plot::{Line, Plot, PlotPoints};

use heatlog::report::{self, fmt_time};
use heatlog::session;

use crate::view::{hover_nearest, TimeFmt};
use crate::worker;
use crate::{card, heat_color, temp_frac, usage_color, App, ACCENT, AMBER, GREEN, RED};

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
        format!("CPU usage {v:.0}%")
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
            None => stats.push(("Temperature", "unavailable - see Settings".to_string())),
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

/// Round gauge: a 270-degree arc with a big number in the middle. The
/// caller passes an already-smoothed fraction, so drawing never drives
/// repaints.
fn gauge(ui: &mut egui::Ui, frac: f32, color: Color32, center: String, label: &str) {
    let size = 118.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let center_pos = rect.center();
    let radius = size * 0.40;
    // Screen y points down, so 135 degrees is the lower-left: the arc
    // sweeps up and over, leaving the classic gauge opening at the bottom.
    let start = 0.75 * std::f32::consts::PI;
    let sweep = 1.5 * std::f32::consts::PI;

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
    let frac = frac.clamp(0.0, 1.0);
    if frac > 0.01 {
        ui.painter().add(arc(sweep * frac, color, 7.0));
    }
    ui.painter().text(
        center_pos,
        egui::Align2::CENTER_CENTER,
        center,
        egui::FontId::proportional(21.0),
        ui.visuals().strong_text_color(),
    );
    ui.painter().text(
        center_pos + egui::vec2(0.0, radius * 0.82),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(12.0),
        ui.visuals().weak_text_color(),
    );
}

// ------------------------------------------------------------------ overview

/// Returns a page to navigate to, if something on the page asked.
pub fn overview(ui: &mut egui::Ui, app: &App) -> Option<crate::Page> {
    let mut nav = None;
    ui.heading(format!("{}  Overview", icons::GAUGE));
    ui.add_space(6.0);

    let (color, icon, word, detail) = health(app);
    let needs_setup = app
        .info
        .as_ref()
        .is_some_and(|i| i.cpu_temp_source.is_none());
    card(ui, |ui| {
        // The row must be a horizontal first: a bare right-to-left layout
        // would claim the panel's whole height and push the page off.
        ui.horizontal(|ui| {
            ui.label(RichText::new(icon).size(22.0).color(color));
            ui.label(RichText::new(word).strong().size(16.0));
            // Button goes in the right-aligned remainder before the detail
            // text, so the text truncates instead of running underneath.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if needs_setup && ui.button("Set up").clicked() {
                    nav = Some(crate::Page::TempSetup);
                }
                ui.add(egui::Label::new(RichText::new(detail).weak()).truncate());
            });
        });
    });
    ui.add_space(10.0);

    // Full-width gauge row: one evenly spaced column per thing this
    // machine can actually measure.
    card(ui, |ui| {
        if let Some(s) = &app.latest {
            struct G {
                frac: f32,
                color: Color32,
                center: String,
                label: &'static str,
            }
            let usage = app.smooth(|s| s.cpu.usage_pct);
            let mut gauges = vec![G {
                frac: usage / 100.0,
                color: usage_color(usage / 100.0),
                center: format!("{usage:.0}%"),
                label: "CPU usage",
            }];
            let total_gb = app.info.as_ref().map(|i| i.ram_total_mib).unwrap_or(0) as f32 / 1024.0;
            if total_gb > 0.0 {
                let used_gb = app.smooth(|s| s.ram_used_mib as f32 / 1024.0);
                gauges.push(G {
                    frac: used_gb / total_gb,
                    color: usage_color(used_gb / total_gb),
                    center: format!("{used_gb:.1} GB"),
                    label: "Memory",
                });
            }
            if s.gpu.is_some() {
                let util = app.smooth(|s| s.gpu.as_ref().map_or(0.0, |g| g.util_pct as f32));
                gauges.push(G {
                    frac: util / 100.0,
                    color: usage_color(util / 100.0),
                    center: format!("{util:.0}%"),
                    label: "GPU usage",
                });
                let temp = app.smooth(|s| s.gpu.as_ref().map_or(0.0, |g| g.temp_c as f32));
                gauges.push(G {
                    frac: temp_frac(f64::from(temp)),
                    color: heat_color(temp_frac(f64::from(temp))),
                    center: format!("{temp:.0} C"),
                    label: "GPU temp",
                });
            }
            if s.cpu_temp_c.is_some() {
                let temp = app.smooth(|s| s.cpu_temp_c.unwrap_or(0.0));
                gauges.push(G {
                    frac: temp_frac(f64::from(temp)),
                    color: heat_color(temp_frac(f64::from(temp))),
                    center: format!("{temp:.0} C"),
                    label: "CPU temp",
                });
            }
            ui.columns(gauges.len(), |cols| {
                for (col, g) in cols.iter_mut().zip(&gauges) {
                    col.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                        gauge(ui, g.frac, g.color, g.center.clone(), g.label);
                    });
                }
            });
        } else {
            ui.label(RichText::new("starting sensors...").weak());
        }
    });
    ui.add_space(10.0);

    ui.columns(2, |cols| {
        card(&mut cols[0], |ui| {
            ui.label(RichText::new("Activity - last 60 seconds").weak());
            activity_chart(ui, app);
            ui.horizontal(|ui| {
                ui.label(RichText::new("--").color(ACCENT).strong());
                ui.label(RichText::new("CPU usage").weak().small());
                if !app.hist.gpu_util_pct.is_empty() {
                    ui.label(RichText::new("--").color(AMBER).strong());
                    ui.label(RichText::new("GPU usage").weak().small());
                }
            });
        });
        card(&mut cols[1], |ui| {
            ui.label(RichText::new(format!("{}  Latest recording", icons::FILM_STRIP)).weak());
            match app.recordings.rows.first() {
                Some(row) => {
                    ui.label(RichText::new(&row.started).strong().size(16.0));
                    let mut line = row.duration.clone();
                    if let Some(game) = &row.game {
                        line.push_str("  -  ");
                        line.push_str(game);
                    }
                    ui.label(RichText::new(line).weak());
                    if let Some(extra) = &row.extra {
                        ui.horizontal(|ui| {
                            verdict_pill(ui, extra.class);
                            if let Some(t) = extra.gpu_peak_c {
                                ui.label(RichText::new(format!("GPU peak {t:.0} C")).weak());
                            }
                        });
                    }
                    if ui.link("All recordings").clicked() {
                        nav = Some(crate::Page::Recordings);
                    }
                }
                None => {
                    ui.label(
                        RichText::new("No recordings yet - hit Start recording up top.").weak(),
                    );
                }
            }
            let status_line = match &app.recording {
                Some(rec) => format!(
                    "{}  recording - {}",
                    icons::RECORD,
                    fmt_time(rec.started.elapsed().as_secs())
                ),
                None => app
                    .status
                    .clone()
                    .unwrap_or_else(|| "not recording".to_string()),
            };
            let status_color = if app.recording.is_some() {
                RED
            } else {
                ui.visuals().weak_text_color()
            };
            ui.label(RichText::new(status_line).color(status_color).small());
        });
    });
    nav
}

fn activity_chart(ui: &mut egui::Ui, app: &App) {
    let series = |data: &VecDeque<f64>| -> Vec<[f64; 2]> {
        data.iter()
            .enumerate()
            .map(|(i, &v)| [i as f64 - (data.len() as f64 - 1.0), v])
            .collect()
    };
    Plot::new("activity")
        .height(140.0)
        .show_axes(false)
        .show_grid(false)
        .show_x(false)
        .show_y(false)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_scroll(false)
        .allow_boxed_zoom(false)
        .include_x(-60.0)
        .include_x(0.0)
        .include_y(0.0)
        .include_y(100.0)
        .show(ui, |plot_ui| {
            plot_ui.line(
                Line::new(PlotPoints::from(series(&app.hist.cpu_pct)))
                    .color(ACCENT)
                    .width(2.0_f32)
                    .fill(0.0_f32),
            );
            if !app.hist.gpu_util_pct.is_empty() {
                plot_ui.line(
                    Line::new(PlotPoints::from(series(&app.hist.gpu_util_pct)))
                        .color(AMBER)
                        .width(2.0_f32),
                );
            }
        });
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
            "set up a sensor source to see them".to_string(),
        );
    }
    if throttling || gpu_temp.is_some_and(|t| t >= 85) || cpu_temp.is_some_and(|t| t >= 90.0) {
        let why = if throttling {
            "the GPU is slowing itself down".to_string()
        } else {
            "temperatures at throttle levels".to_string()
        };
        (RED, icons::FLAME, "Too hot - slowing down", why)
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

// ------------------------------------------------- CPU temperature setup

/// Guided setup for a CPU temperature source. heatlog never changes another
/// app's settings; it installs through winget and explains the two clicks
/// the user has to make themselves.
pub fn temp_setup(ui: &mut egui::Ui, app: &mut App) -> Option<crate::Page> {
    let mut nav = None;
    if ui.button("< Overview").clicked() {
        nav = Some(crate::Page::Overview);
    }
    ui.heading(format!("{}  Set up CPU temperature", icons::THERMOMETER));
    ui.add_space(6.0);

    let avail = app.info.as_ref().map(|i| &i.avail);
    if let Some(source) = app.info.as_ref().and_then(|i| i.cpu_temp_source) {
        card(ui, |ui| {
            ui.label(
                RichText::new(format!(
                    "{}  CPU temperature is working, via {source}.",
                    icons::CHECK_CIRCLE
                ))
                .color(GREEN)
                .strong(),
            );
        });
        ui.add_space(8.0);
    }

    card(ui, |ui| {
        ui.label(
            "Windows has no unprivileged way to read CPU temperature, so heatlog borrows the \
             reading from a monitoring app that already ships a signed driver. Either of these \
             works; HWiNFO is the smoother one.",
        );
    });
    ui.add_space(10.0);

    card(ui, |ui| {
        ui.label(RichText::new("HWiNFO (recommended)").strong().size(15.0));
        match avail {
            Some(a) if a.hwinfo_sharing => {
                ui.label(
                    RichText::new(format!("{}  Running and sharing sensors.", icons::CHECK))
                        .color(GREEN),
                );
            }
            Some(a) if a.hwinfo_running => {
                ui.label(
                    RichText::new(format!(
                        "{}  Running, but not sharing sensors yet.",
                        icons::WARNING
                    ))
                    .color(AMBER),
                );
            }
            _ => {
                ui.label(RichText::new("Not running.").weak());
            }
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Windows will ask for admin permission during the install, because HWiNFO \
                 installs a hardware-monitoring driver.",
            )
            .weak()
            .small(),
        );
        if ui
            .button(format!("{}  Install HWiNFO", icons::DOWNLOAD_SIMPLE))
            .clicked()
        {
            app.status = Some(install_hwinfo());
        }
        ui.add_space(6.0);
        ui.label("Then, in HWiNFO:");
        ui.label("1.  Start it in Sensors-only mode.");
        ui.label("2.  Settings > check \"Shared Memory Support\".");
        ui.label("3.  Leave HWiNFO running while you game.");
        ui.label(
            RichText::new(
                "The free version switches Shared Memory Support off again after 12 hours; \
                 heatlog will say so when that happens.",
            )
            .weak()
            .small(),
        );
    });
    ui.add_space(10.0);

    card(ui, |ui| {
        ui.label(
            RichText::new("LibreHardwareMonitor (alternative)")
                .strong()
                .size(15.0),
        );
        match avail {
            Some(a) if a.lhm_web => {
                ui.label(
                    RichText::new(format!("{}  Web server reachable.", icons::CHECK)).color(GREEN),
                );
            }
            Some(a) if a.lhm_running => {
                ui.label(
                    RichText::new(format!(
                        "{}  Running, but its web server is off.",
                        icons::WARNING
                    ))
                    .color(AMBER),
                );
            }
            _ => {
                ui.label(RichText::new("Not running.").weak());
            }
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new("winget install LibreHardwareMonitor.LibreHardwareMonitor").monospace(),
        );
        ui.label("Then enable Options > Remote Web Server and leave it running.");
    });
    ui.add_space(10.0);

    ui.horizontal(|ui| {
        if ui
            .button(format!("{}  Check again", icons::ARROWS_CLOCKWISE))
            .clicked()
        {
            app.send(worker::Cmd::Reprobe);
            app.status = Some("Looking for a temperature source...".to_string());
        }
        if let Some(status) = &app.status {
            ui.label(RichText::new(status).weak());
        }
    });
    nav
}

/// Installs through winget's official package, or falls back to HWiNFO's
/// own download page. Runs in a visible console so the user can see the
/// admin prompt and the progress.
fn install_hwinfo() -> String {
    let winget = std::process::Command::new("cmd")
        .args([
            "/C",
            "start",
            "",
            "cmd",
            "/K",
            "winget install --id REALiX.HWiNFO -e --accept-package-agreements \
             --accept-source-agreements",
        ])
        .spawn();
    match winget {
        Ok(_) => "Installing HWiNFO in a new window - accept the admin prompt, then come \
                  back and use Check again."
            .to_string(),
        Err(_) => {
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", "", "https://www.hwinfo.com/download/"])
                .spawn();
            "winget is not available, so HWiNFO's download page was opened instead.".to_string()
        }
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
        .width(130.0)
        .height(28.0)
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

pub fn recordings(ui: &mut egui::Ui, app: &mut App) -> Option<PathBuf> {
    ui.horizontal(|ui| {
        ui.heading(format!("{}  Recordings", icons::FILM_STRIP));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if app.confirm_delete_all {
                ui.label(
                    RichText::new(format!(
                        "Delete all {} recordings?",
                        app.recordings.rows.len()
                    ))
                    .color(AMBER),
                );
                if ui.button("Yes, delete all").clicked() {
                    app.delete_all_recordings(ui.ctx());
                }
                if ui.button("Cancel").clicked() {
                    app.confirm_delete_all = false;
                }
            } else if !app.recordings.rows.is_empty()
                && ui.button(format!("{}  Delete all", icons::TRASH)).clicked()
            {
                app.confirm_delete_all = true;
            }
        });
    });
    ui.add_space(6.0);
    if let Some(error) = &app.recordings.error {
        ui.colored_label(RED, error);
        return None;
    }
    if app.recordings.rows.is_empty() {
        ui.label(
            "No recordings yet. Hit Start recording, or turn on auto-record and launch a game.",
        );
        return None;
    }

    let mut open = None;
    let mut delete = None;
    let mut confirm = app.confirm_delete.clone();
    egui::ScrollArea::vertical().show(ui, |ui| {
        for row in &app.recordings.rows {
            let pending = confirm.as_deref() == Some(row.path.as_path());
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
                                line.push_str("  -  ");
                                line.push_str(game);
                            }
                            ui.label(RichText::new(line).weak());
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if pending {
                                ui.label(RichText::new("Delete?").color(AMBER));
                                if ui.button("Yes").clicked() {
                                    delete = Some(row.path.clone());
                                }
                                if ui.button("No").clicked() {
                                    confirm = None;
                                }
                            } else {
                                if ui
                                    .button(RichText::new(icons::TRASH))
                                    .on_hover_text("Delete this recording")
                                    .clicked()
                                {
                                    confirm = Some(row.path.clone());
                                }
                                if let Some(extra) = &row.extra {
                                    verdict_pill(ui, extra.class);
                                    let mut peaks = Vec::new();
                                    if let Some(t) = extra.gpu_peak_c {
                                        peaks.push(format!("GPU peak {t:.0} C"));
                                    }
                                    if let Some(t) = extra.cpu_peak_c {
                                        peaks.push(format!("CPU peak {t:.0} C"));
                                    }
                                    if !peaks.is_empty() {
                                        ui.label(RichText::new(peaks.join("  -  ")).weak());
                                    }
                                    row_spark(ui, &row.started, &extra.spark);
                                } else {
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
            // A click on the card opens it, unless a button took the click.
            if response.clicked() && !pending {
                open = Some(row.path.clone());
            }
            ui.add_space(4.0);
        }
    });
    app.confirm_delete = confirm;
    if let Some(path) = delete {
        app.delete_recording(&path, ui.ctx());
    }
    open
}

// ------------------------------------------------------------------ settings

pub fn settings(ui: &mut egui::Ui, app: &mut App) -> Option<crate::Page> {
    let mut nav = None;
    ui.heading(format!("{}  Settings", icons::GEAR));
    ui.add_space(6.0);

    egui::ScrollArea::vertical().show(ui, |ui| {
        card(ui, |ui| {
            ui.label(RichText::new("Auto-record these games").strong());
            ui.label(
                RichText::new(
                    "Find a game's process name in Task Manager (Details tab, Name column) \
                     while it is running.",
                )
                .weak()
                .small(),
            );
            let mut remove = None;
            for (i, game) in app.settings.games.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(game);
                    if ui.button(icons::TRASH).clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                app.settings.games.remove(i);
            }
            ui.horizontal(|ui| {
                let hint = ui.add(
                    egui::TextEdit::singleline(&mut app.settings.new_game)
                        .hint_text("RainbowSix.exe"),
                );
                let submitted = hint.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Add").clicked() || submitted)
                    && !app.settings.new_game.trim().is_empty()
                {
                    let name = app.settings.new_game.trim().to_string();
                    app.settings.games.push(name);
                    app.settings.new_game.clear();
                }
            });
        });
        ui.add_space(8.0);

        card(ui, |ui| {
            ui.label(RichText::new("Recording").strong());
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut app.settings.interval_secs)
                        .range(1..=60)
                        .suffix(" s"),
                );
                ui.label(RichText::new("between samples").weak());
            });
        });
        ui.add_space(8.0);

        card(ui, |ui| {
            ui.label(RichText::new("Storage").strong());
            let used = app.recordings_bytes();
            ui.label(format!(
                "{} recordings, {:.1} MB on disk",
                app.recordings.rows.len(),
                used as f64 / (1024.0 * 1024.0)
            ));
            ui.horizontal(|ui| {
                let mut enabled = app.settings.delete_after_days > 0;
                if ui
                    .checkbox(&mut enabled, "Auto-delete old recordings")
                    .changed()
                {
                    app.settings.delete_after_days = if enabled { 30 } else { 0 };
                }
                if enabled {
                    ui.label(RichText::new("older than").weak());
                    ui.add(
                        egui::DragValue::new(&mut app.settings.delete_after_days)
                            .range(1..=365)
                            .suffix(" days"),
                    );
                }
            });
            ui.label(
                RichText::new("Deleted recordings go to the Recycle Bin.")
                    .weak()
                    .small(),
            );
        });
        ui.add_space(8.0);

        card(ui, |ui| {
            ui.label(RichText::new("Updates").strong());
            ui.checkbox(
                &mut app.settings.check_updates,
                "Check GitHub for updates on start-up",
            );
            ui.checkbox(
                &mut app.settings.include_prereleases,
                "Include pre-releases (early versions, less tested)",
            );
            ui.label(
                RichText::new(format!(
                    "This version: {}. The check is the only internet request heatlog makes.",
                    heatlog::update::current_version()
                ))
                .weak()
                .small(),
            );
        });
        ui.add_space(8.0);

        card(ui, |ui| {
            ui.label(RichText::new("CPU temperature").strong());
            match app.info.as_ref().and_then(|i| i.cpu_temp_source) {
                Some(source) => {
                    ui.label(format!("Reading from {source}."));
                }
                None => {
                    ui.label(RichText::new("No source found.").weak());
                }
            }
            if ui.button("Open setup").clicked() {
                nav = Some(crate::Page::TempSetup);
            }
        });
        ui.add_space(10.0);

        ui.horizontal(|ui| {
            if ui.button(RichText::new("Save settings").strong()).clicked() {
                app.save_settings();
            }
            if let Some(saved) = &app.settings.saved {
                ui.label(RichText::new(saved).weak());
            }
        });
        let path = heatlog::config::config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        ui.add_space(4.0);
        ui.label(RichText::new(format!("Stored in {path}")).weak().small());
    });
    nav
}
