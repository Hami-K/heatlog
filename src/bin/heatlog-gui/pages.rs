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

/// What to put in the Fan speed row. A laptop whose temperature comes
/// from LibreHardwareMonitor usually has no fan sensors there at all - LHM
/// reads fans off a Super I/O chip that laptops do not have - so the row
/// says where to look instead of showing nothing.
fn fan_speed_text(rpm: Option<u32>, source: Option<&str>) -> String {
    match (rpm, source) {
        (Some(rpm), _) => format!("{rpm} rpm"),
        (None, Some("LibreHardwareMonitor")) => {
            "not reported by LibreHardwareMonitor on this laptop - try HWiNFO".to_string()
        }
        (None, Some(source)) => format!("not reported by {source}"),
        (None, None) => "unavailable - see Settings".to_string(),
    }
}

pub fn cpu(ui: &mut egui::Ui, app: &App) {
    let name = app
        .info
        .as_ref()
        .map(|i| i.cpu_name.clone())
        .unwrap_or_else(|| "CPU".to_string());
    let source = app.info.as_ref().and_then(|i| i.cpu_temp_source);
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
        stats.push(("Fan speed", fan_speed_text(s.fastest_fan(), source)));
        match s.cpu_temp_c {
            Some(t) => stats.push(("Temperature", format!("{t:.0} °C"))),
            None => stats.push(("Temperature", "unavailable - see Settings".to_string())),
        }
    } else {
        stats.push(("Status", "starting sensors...".to_string()));
    }
    stat_grid(ui, "cpu-stats", &stats);
}

pub fn gpu(ui: &mut egui::Ui, app: &App) {
    let info = app.info.as_ref();
    let main = info.and_then(|i| i.gpu.as_ref());
    match (main, app.latest.as_ref().and_then(|s| s.gpu.as_ref())) {
        (Some(gpu_info), Some(g)) => {
            ui.heading(format!("{}  {}", icons::GRAPHICS_CARD, gpu_info.name));
            ui.add_space(6.0);
            source_note(ui, gpu_info);
            // A GPU with no temperature to show gets its usage charted
            // instead of an empty plot.
            match g.temp_c {
                Some(_) => live_plot(ui, "gpu", &app.hist.gpu_temp_c, Some(100.0), |v| {
                    format!("GPU {v:.0} °C")
                }),
                None => live_plot(ui, "gpu", &app.hist.gpu_util_pct, Some(100.0), |v| {
                    format!("GPU usage {v:.0}%")
                }),
            }
            ui.add_space(10.0);
            stat_grid(ui, "gpu-stats", &gpu_stats(g, gpu_info));

            if let Some(second) = info.and_then(|i| i.gpu2.as_ref()) {
                ui.add_space(16.0);
                ui.heading(format!("{}  {}", icons::GRAPHICS_CARD, second.name));
                ui.add_space(6.0);
                source_note(ui, second);
                match app.latest.as_ref().and_then(|s| s.gpu2.as_ref()) {
                    Some(g) => stat_grid(ui, "gpu2-stats", &gpu_stats(g, second)),
                    None => {
                        ui.label(RichText::new("No reading yet.").weak());
                    }
                }
            }
        }
        _ => {
            ui.heading(format!("{}  GPU", icons::GRAPHICS_CARD));
            ui.add_space(6.0);
            card(ui, |ui| {
                ui.label(
                    RichText::new(format!("{}  No GPU detected", icons::PLUGS))
                        .strong()
                        .size(15.0),
                );
                ui.label(
                    RichText::new(
                        "CPU and memory are still monitored and recorded. heatlog reads \
                         NVIDIA GPUs through NVIDIA's own library, and Intel and AMD GPUs \
                         through Windows' graphics counters.",
                    )
                    .weak(),
                );
            });
        }
    }
}

/// Where a GPU's numbers come from, and what that costs in detail. Said
/// once per GPU rather than repeated on every row.
fn source_note(ui: &mut egui::Ui, info: &heatlog::sensors::gpu::GpuInfo) {
    let mut parts: Vec<String> = Vec::new();
    if info.integrated {
        parts.push("integrated".to_string());
    }
    if info.source == "Windows" {
        parts
            .push("read through Windows, which reports no clocks, power or throttling".to_string());
    }
    if info.experimental {
        parts.push(
            "experimental: this combination has never been tested on real hardware".to_string(),
        );
    }
    if parts.is_empty() {
        return;
    }
    let text = parts.join(" - ");
    ui.label(if info.experimental {
        RichText::new(text).color(AMBER).small()
    } else {
        RichText::new(text).weak().small()
    });
    ui.add_space(4.0);
}

/// Only the figures this GPU actually reports. A missing one is named as
/// not reported rather than shown as zero.
fn gpu_stats(
    g: &heatlog::session::GpuSample,
    info: &heatlog::sensors::gpu::GpuInfo,
) -> Vec<(&'static str, String)> {
    let mut stats = vec![("Utilization", format!("{}%", g.util_pct))];
    if let Some(temp) = g.temp_c {
        stats.push(("Temperature", format!("{temp} °C")));
    } else {
        stats.push(("Temperature", "not reported".to_string()));
    }
    if let Some(mhz) = g.core_mhz {
        stats.push(("Speed", format!("{:.2} GHz", f64::from(mhz) / 1000.0)));
    }
    if let Some(mhz) = g.mem_mhz {
        stats.push((
            "Memory speed",
            format!("{:.2} GHz", f64::from(mhz) / 1000.0),
        ));
    }
    if let Some(power) = g.power_w {
        stats.push(("Power draw", format!("{power:.0} W")));
    }
    match (info.vram_total_mib, g.shared_mem_used_mib) {
        // A card with memory of its own.
        (Some(total), _) => stats.push((
            "VRAM used",
            format!(
                "{:.1} of {:.0} GB",
                g.vram_used_mib as f64 / 1024.0,
                total as f64 / 1024.0
            ),
        )),
        // An integrated GPU borrows system memory; there is no total of
        // its own to show it against.
        (None, Some(shared)) => stats.push((
            "Memory used",
            format!(
                "{:.1} GB shared",
                (g.vram_used_mib + shared) as f64 / 1024.0
            ),
        )),
        (None, None) => stats.push((
            "Memory used",
            format!("{:.1} GB", g.vram_used_mib as f64 / 1024.0),
        )),
    }
    stats.push((
        "Throttling now",
        match &g.throttle {
            None => "not reported by this GPU".to_string(),
            Some(reasons) if reasons.is_empty() => "no".to_string(),
            Some(reasons) => reasons.join(", "),
        },
    ));
    stats
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

    let health = health(app);
    let needs_setup = app
        .info
        .as_ref()
        .is_some_and(|i| i.cpu_temp_source.is_none());
    card(ui, |ui| {
        // The row must be a horizontal first: a bare right-to-left layout
        // would claim the panel's whole height and push the page off.
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(health.icon())
                    .size(22.0)
                    .color(health.color()),
            );
            ui.label(RichText::new(&health.word).strong().size(16.0));
            // Button goes in the right-aligned remainder before the detail
            // text, so the text truncates instead of running underneath.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if needs_setup && ui.button("Set up CPU temp").clicked() {
                    nav = Some(crate::Page::TempSetup);
                }
                ui.add(egui::Label::new(RichText::new(&health.detail).weak()).truncate());
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
                if s.gpu.as_ref().is_some_and(|g| g.temp_c.is_some()) {
                    let temp =
                        app.smooth(|s| s.gpu.as_ref().and_then(|g| g.temp_c).unwrap_or(0) as f32);
                    gauges.push(G {
                        frac: temp_frac(f64::from(temp)),
                        color: heat_color(temp_frac(f64::from(temp))),
                        center: format!("{temp:.0} °C"),
                        label: "GPU temp",
                    });
                }
            }
            if s.cpu_temp_c.is_some() {
                let temp = app.smooth(|s| s.cpu_temp_c.unwrap_or(0.0));
                gauges.push(G {
                    frac: temp_frac(f64::from(temp)),
                    color: heat_color(temp_frac(f64::from(temp))),
                    center: format!("{temp:.0} °C"),
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
                                ui.label(RichText::new(format!("GPU peak {t:.0} °C")).weak());
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
                None => app.status_text().unwrap_or("not recording").to_string(),
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

pub struct Health {
    /// "starting", "none", "cool", "warm", "hot".
    pub level: &'static str,
    pub word: String,
    pub detail: String,
}

impl Health {
    fn color(&self) -> Color32 {
        match self.level {
            "cool" => GREEN,
            "warm" => AMBER,
            "hot" => RED,
            _ => Color32::GRAY,
        }
    }

    fn icon(&self) -> &'static str {
        match self.level {
            "cool" => icons::SNOWFLAKE,
            "hot" => icons::FLAME,
            "starting" => icons::HOURGLASS,
            _ => icons::THERMOMETER,
        }
    }
}

/// The banner never claims more than it knows: it names which chip it is
/// talking about when the other one has no sensor, and only says
/// temperatures look fine when every source this machine should have is
/// actually reporting.
fn health_of(
    gpu_temp: Option<f64>,
    cpu_temp: Option<f64>,
    throttling: bool,
    gpu_present: bool,
) -> Health {
    let missing_cpu = cpu_temp.is_none();
    let missing_gpu = gpu_present && gpu_temp.is_none();
    if gpu_temp.is_none() && cpu_temp.is_none() {
        return Health {
            level: "none",
            word: "Temperatures unavailable".to_string(),
            detail: "set up a sensor source to see them".to_string(),
        };
    }

    // Which chip the reading actually describes, when only one is known.
    let subject = match (gpu_temp.is_some(), cpu_temp.is_some()) {
        (true, false) => "GPU ",
        (false, true) => "CPU ",
        _ => "",
    };
    let gap = if missing_cpu {
        Some("CPU temperature not set up")
    } else if missing_gpu {
        Some("GPU temperature not reporting")
    } else {
        None
    };

    let (level, word, reading) =
        if throttling || gpu_temp.is_some_and(|t| t >= 85.0) || cpu_temp.is_some_and(|t| t >= 90.0)
        {
            let why = if throttling {
                "the GPU is slowing itself down"
            } else {
                "temperatures at throttle levels"
            };
            ("hot", format!("{subject}too hot - slowing down"), why)
        } else if gpu_temp.is_some_and(|t| t >= 75.0) || cpu_temp.is_some_and(|t| t >= 80.0) {
            (
                "warm",
                format!("{subject}getting warm"),
                "elevated but still safe",
            )
        } else {
            (
                "cool",
                format!("{subject}running cool"),
                "temperatures look fine",
            )
        };

    let mut word = word;
    // Sentence case, since the subject may be an acronym or nothing.
    if subject.is_empty() {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            word = first.to_uppercase().collect::<String>() + chars.as_str();
        }
    }
    Health {
        level,
        word,
        detail: match gap {
            Some(gap) => gap.to_string(),
            None => reading.to_string(),
        },
    }
}

fn health(app: &App) -> Health {
    let Some(s) = &app.latest else {
        return Health {
            level: "starting",
            word: "Starting".to_string(),
            detail: "waiting for the first sensor read".to_string(),
        };
    };
    health_of(
        s.gpu.as_ref().and_then(|g| g.temp_c).map(f64::from),
        s.cpu_temp_c.map(f64::from),
        s.gpu.as_ref().is_some_and(|g| {
            g.throttle
                .as_ref()
                .is_some_and(|reasons| !reasons.is_empty())
        }),
        app.info.as_ref().is_some_and(|i| i.gpu.is_some()),
    )
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

    // Copied out rather than borrowed: the cards below need &mut app.
    let (hwinfo_sharing, hwinfo_running, lhm_web, lhm_running, lhm_path, hwinfo_path) =
        match &app.info {
            Some(info) => (
                info.avail.hwinfo_sharing,
                info.avail.hwinfo_running,
                info.avail.lhm_web,
                info.avail.lhm_running,
                info.avail.lhm_path.clone(),
                info.avail.hwinfo_path.clone(),
            ),
            None => (false, false, false, false, None, None),
        };
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
        ui.label(
            RichText::new("LibreHardwareMonitor (recommended)")
                .strong()
                .size(15.0),
        );
        if lhm_web {
            ui.label(
                RichText::new(format!("{}  Web server reachable.", icons::CHECK)).color(GREEN),
            );
        } else if lhm_running {
            ui.label(
                RichText::new(format!(
                    "{}  Running, but its web server is off.",
                    icons::WARNING
                ))
                .color(AMBER),
            );
        } else {
            ui.label(RichText::new("Not running.").weak());
        }
        ui.add_space(4.0);
        match lhm_path.as_ref() {
            Some(path) => {
                ui.label(
                    RichText::new(
                        "Windows will ask for admin permission, because LibreHardwareMonitor \
                         needs it to read sensors.",
                    )
                    .weak()
                    .small(),
                );
                let path = path.clone();
                if ui
                    .button(format!("{}  Start LibreHardwareMonitor", icons::PLAY))
                    .clicked()
                {
                    match start_lhm(&path) {
                        Ok(()) => app
                            .set_status("Starting LibreHardwareMonitor - accept the admin prompt."),
                        Err(e) => app.error = Some(e),
                    }
                }
                ui.add_space(4.0);
                ui.label("Then, once it is open:");
                ui.label("1.  Options > Remote Web Server > Run, and leave it running.");
                ui.label(
                    RichText::new(
                        "Windows Firewall may ask about network access. Blocking it is fine: \
                         heatlog only reads from this PC.",
                    )
                    .weak()
                    .small(),
                );
            }
            _ => {
                ui.label("Not installed, or heatlog could not find it. To install it:");
                ui.label(
                    RichText::new("winget install LibreHardwareMonitor.LibreHardwareMonitor")
                        .monospace(),
                );
                ui.label(
                    RichText::new(
                        "That package is portable: it adds no Start menu entry, and it has to \
                         be run as administrator. Use Check again afterwards and heatlog will \
                         offer to start it for you.",
                    )
                    .weak()
                    .small(),
                );
            }
        }
    });
    ui.add_space(10.0);

    card(ui, |ui| {
        ui.label(RichText::new("HWiNFO (alternative)").strong().size(15.0));
        if hwinfo_sharing {
            ui.label(
                RichText::new(format!("{}  Running and sharing sensors.", icons::CHECK))
                    .color(GREEN),
            );
        } else if hwinfo_running {
            ui.label(
                RichText::new(format!(
                    "{}  Running, but not sharing sensors yet.",
                    icons::WARNING
                ))
                .color(AMBER),
            );
        } else if hwinfo_path.is_some() {
            ui.label(
                RichText::new(format!("{}  Installed, but not running.", icons::WARNING))
                    .color(AMBER),
            );
        } else {
            ui.label(RichText::new("Not installed.").weak());
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
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!app.installing && hwinfo_path.is_none(), |ui| {
                if ui
                    .button(format!("{}  Install HWiNFO", icons::DOWNLOAD_SIMPLE))
                    .on_disabled_hover_text("Already installed")
                    .clicked()
                {
                    app.start_hwinfo_install();
                }
            });
            if app.installing {
                ui.label(RichText::new("installing, this takes a minute...").weak());
            }
        });
        if let Some(error) = &app.install_error {
            ui.label(
                RichText::new("Automatic install didn't work - download HWiNFO from its official site instead.")
                    .color(AMBER),
            );
            ui.label(RichText::new(error).weak().small());
            if ui
                .button(format!(
                    "{}  Open hwinfo.com download page",
                    icons::ARROW_SQUARE_OUT
                ))
                .clicked()
            {
                open_url(HWINFO_DOWNLOAD);
            }
            ui.label(
                RichText::new("Pick the installer version, not the portable one.")
                    .weak()
                    .small(),
            );
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

    ui.label(
        RichText::new("This page checks again by itself every few seconds.")
            .weak()
            .small(),
    );
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui
            .button(format!("{}  Check again", icons::ARROWS_CLOCKWISE))
            .on_hover_text("Looks for both apps again, running or newly installed")
            .clicked()
        {
            app.send(worker::Cmd::Reprobe);
            app.set_status("Looking for a temperature source...");
        }
        if let Some(status) = app.status_text() {
            ui.label(RichText::new(status).weak());
        }
    });
    nav
}

/// Installs through winget's official package, or falls back to HWiNFO's
/// own download page. Runs in a visible console so the user can see the
/// admin prompt and the progress.
pub const HWINFO_DOWNLOAD: &str = "https://www.hwinfo.com/download/";

/// Runs winget and reports what actually happened. winget's download can
/// fail for reasons heatlog cannot fix (HWiNFO's site has answered 403),
/// so the result is reported rather than assumed, with the official
/// download page as the way out. No retrying.
pub fn install_hwinfo() -> Result<(), String> {
    let output = std::process::Command::new("winget")
        .args([
            "install",
            "--id",
            "REALiX.HWiNFO",
            "-e",
            "--accept-package-agreements",
            "--accept-source-agreements",
            "--disable-interactivity",
        ])
        .output()
        .map_err(|_| "winget is not available on this PC".to_string())?;
    if output.status.success() {
        return Ok(());
    }
    // winget prints its diagnosis to stdout, not stderr.
    let text = String::from_utf8_lossy(&output.stdout);
    let reason = text
        .lines()
        .rev()
        .find(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with(['-', '\\', '|', '/', '█'])
        })
        .unwrap_or("winget could not install HWiNFO")
        .trim()
        .to_string();
    Err(reason)
}

pub fn open_url(url: &str) {
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(url)
        .spawn();
}

/// Starts LibreHardwareMonitor with administrator rights, which it needs
/// to read sensors. Returns the path it launched.
pub fn start_lhm(path: &std::path::Path) -> Result<(), String> {
    std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!("Start-Process -Verb RunAs -FilePath '{}'", path.display()),
        ])
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not start LibreHardwareMonitor: {e}"))
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
                                .filter_map(|x| x.gpu.as_ref().and_then(|g| g.temp_c))
                                .map(f64::from)
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
                            gpu_peak_c: summary
                                .gpu
                                .as_ref()
                                .and_then(|g| g.temp_c.as_ref())
                                .map(|t| t.peak),
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
    {
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
                                        peaks.push(format!("GPU peak {t:.0} °C"));
                                    }
                                    if let Some(t) = extra.cpu_peak_c {
                                        peaks.push(format!("CPU peak {t:.0} °C"));
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
    }
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

    {
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
            ui.label(RichText::new("Slowdown causes").strong());
            ui.checkbox(
                &mut app.settings.record_processes,
                "Record which programs were busy",
            );
            ui.label(
                RichText::new(
                    "Lets reports suggest what caused a slowdown, for example a browser                      using the processor while a game stuttered. Program names are written                      into the recording files on this PC and sent nowhere. Turn this off to                      record hardware readings only.",
                )
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
    }
    nav
}

#[cfg(test)]
mod tests {
    use super::health_of;

    #[test]
    fn a_missing_source_is_named_not_glossed_over() {
        // GPU reporting, no CPU temperature source: say which is which.
        let partial = health_of(Some(49.0), None, false, true);
        assert_eq!(partial.level, "cool");
        assert_eq!(partial.word, "GPU running cool");
        assert_eq!(partial.detail, "CPU temperature not set up");

        // Both present and cool: only then is the all-clear honest.
        let full = health_of(Some(49.0), Some(67.0), false, true);
        assert_eq!(full.word, "Running cool");
        assert_eq!(full.detail, "temperatures look fine");

        // CPU only, on a machine with no GPU at all: no phantom gap.
        let cpu_only = health_of(None, Some(67.0), false, false);
        assert_eq!(cpu_only.word, "CPU running cool");
        assert_eq!(cpu_only.detail, "temperatures look fine");

        // Nothing at all.
        assert_eq!(health_of(None, None, false, false).level, "none");
    }

    #[test]
    fn heat_still_wins_over_a_missing_source() {
        let hot = health_of(Some(88.0), None, true, true);
        assert_eq!(hot.level, "hot");
        assert_eq!(hot.word, "GPU too hot - slowing down");
        // The gap is still reported rather than the reason.
        assert_eq!(hot.detail, "CPU temperature not set up");
    }
}
