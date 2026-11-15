//! Session view: verdict banner, charts with snapping tooltips, technical
//! details, HTML export.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Color32, RichText};
use egui_plot::{HLine, Line, Plot, PlotPoint, PlotPoints, Points, Polygon, Text};

use heatlog::report::charts::{self};
use heatlog::report::{self, fmt_time, Series, Summary};
use heatlog::session::{self, Session};

use crate::{card, ACCENT, AMBER, GREEN, RED};

pub struct ChartView {
    title: &'static str,
    unit: &'static str,
    explain: String,
    points: Vec<[f64; 2]>,
    bands: Vec<(f64, f64)>,
    band_color: Color32,
    y_lo: f64,
    y_hi: f64,
    hot_from: Option<f64>,
    phrase: fn(f64) -> String,
}

/// Plain-language hover text per chart, e.g. "CPU usage 10%".
fn phrase_for(title: &str) -> fn(f64) -> String {
    match title {
        "GPU temperature" => |v| format!("GPU {v:.0} °C"),
        "GPU speed" => |v| format!("GPU at {v:.2} GHz"),
        "GPU power" => |v| format!("GPU drawing {v:.0} W"),
        "CPU usage" => |v| format!("CPU usage {v:.0}%"),
        "CPU speed" => |v| format!("CPU at {v:.2} GHz"),
        "CPU temperature" => |v| format!("CPU {v:.0} °C"),
        _ => |v| format!("{v:.1}"),
    }
}

pub struct Loaded {
    path: PathBuf,
    session: Session,
    summary: Summary,
    verdict_class: &'static str,
    verdict_text: String,
    causes: Vec<report::causes::Cause>,
    charts: Vec<ChartView>,
    export_status: Option<String>,
}

pub fn load_session(path: &Path) -> anyhow::Result<Loaded> {
    let session = session::read(path)?;
    let summary = report::analyze(&session)?;
    let (verdict_class, verdict_text) = report::verdict(&summary);
    let charts = charts::build(&session, &summary)
        .into_iter()
        .map(|c| {
            let (y_lo, y_hi, hot_from) = c.y_domain();
            ChartView {
                title: c.title,
                unit: c.unit,
                points: charts::downsample(&c.points, charts::MAX_POINTS)
                    .into_iter()
                    .map(|(x, y)| [x, y])
                    .collect(),
                band_color: match c.band_class {
                    "gpu" => Color32::from_rgba_unmultiplied(220, 38, 38, 40),
                    _ => Color32::from_rgba_unmultiplied(217, 119, 6, 40),
                },
                bands: c.bands,
                explain: c.explain,
                y_lo,
                y_hi,
                hot_from,
                phrase: phrase_for(c.title),
            }
        })
        .collect();
    let causes = report::causes::causes(&session, &summary);
    Ok(Loaded {
        path: path.to_path_buf(),
        session,
        summary,
        verdict_class,
        verdict_text,
        causes,
        charts,
        export_status: None,
    })
}

fn verdict_color(class: &str) -> Color32 {
    match class {
        "good" => GREEN,
        "warn" => AMBER,
        "bad" => RED,
        _ => Color32::GRAY,
    }
}

/// Returns true when the back button was clicked.
pub fn session_view(ui: &mut egui::Ui, loaded: &mut Loaded) -> bool {
    let mut back = false;
    ui.horizontal(|ui| {
        if ui.button("< Recordings").clicked() {
            back = true;
        }
        ui.heading(loaded.summary.started.format("%Y-%m-%d %H:%M").to_string());
        if let Some(game) = &loaded.summary.game {
            ui.label(RichText::new(game).weak());
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Export HTML").clicked() {
                loaded.export_status = Some(export_html(loaded));
            }
            if let Some(status) = &loaded.export_status {
                ui.label(RichText::new(status).weak());
            }
        });
    });

    let color = verdict_color(loaded.verdict_class);
    egui::Frame::none()
        .fill(color.gamma_multiply(0.16))
        .stroke(egui::Stroke::new(1.0_f32, color))
        .inner_margin(10.0)
        .rounding(8.0)
        .show(ui, |ui| {
            ui.label(RichText::new(&loaded.verdict_text).size(14.0));
        });
    ui.add_space(6.0);

    if !loaded.causes.is_empty() {
        ui.add_space(8.0);
        ui.label(RichText::new("Possible causes").strong().size(15.0));
        ui.label(
            RichText::new("Read off the recording, so these are suggestions, not certainties.")
                .weak()
                .small(),
        );
        for cause in &loaded.causes {
            card(ui, |ui| {
                ui.label(RichText::new(&cause.headline).strong());
                ui.label(RichText::new(&cause.detail).weak());
            });
            ui.add_space(4.0);
        }
    }

    {
        for chart in &loaded.charts {
            ui.add_space(8.0);
            ui.label(RichText::new(format!("{} ({})", chart.title, chart.unit)).strong());
            plot_chart(ui, chart, loaded.summary.duration_secs as f64);
            ui.label(RichText::new(&chart.explain).weak().small());
        }
        ui.add_space(10.0);
        technical_ui(ui, &loaded.summary);
        ui.add_space(10.0);
    }
    back
}

fn plot_chart(ui: &mut egui::Ui, chart: &ChartView, duration: f64) {
    Plot::new(chart.title)
        .height(180.0)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_scroll(false)
        .allow_boxed_zoom(false)
        .show_x(false)
        .show_y(false)
        .include_x(0.0)
        .include_x(duration.max(1.0))
        .include_y(chart.y_lo)
        .include_y(chart.y_hi)
        .x_axis_formatter(|mark, _range| fmt_time(mark.value.max(0.0) as u64))
        .show(ui, |plot_ui| {
            for &(start, end) in &chart.bands {
                plot_ui.polygon(
                    Polygon::new(PlotPoints::from(vec![
                        [start, chart.y_lo],
                        [end, chart.y_lo],
                        [end, chart.y_hi],
                        [start, chart.y_hi],
                    ]))
                    .fill_color(chart.band_color)
                    .stroke(egui::Stroke::NONE),
                );
            }
            if let Some(hot) = chart.hot_from {
                plot_ui.hline(
                    HLine::new(hot).color(Color32::from_rgba_unmultiplied(220, 38, 38, 120)),
                );
            }
            plot_ui.line(
                Line::new(PlotPoints::from(chart.points.clone()))
                    .color(ACCENT)
                    .width(2.0_f32),
            );
            hover_nearest(plot_ui, &chart.points, TimeFmt::Offset, chart.phrase);
        });
}

pub enum TimeFmt {
    /// x counts seconds relative to now (live graphs): "12 s ago".
    Ago,
    /// x is the offset into a recording: "0:04".
    Offset,
}

/// Snap the hover to the nearest data point: a marker on the point and a
/// plain-language tooltip like "0:04 - CPU usage 10%" instead of raw cursor
/// coordinates.
pub fn hover_nearest(
    plot_ui: &mut egui_plot::PlotUi,
    points: &[[f64; 2]],
    time: TimeFmt,
    phrase: fn(f64) -> String,
) {
    if points.is_empty() || !plot_ui.response().hovered() {
        return;
    }
    let Some(pointer) = plot_ui.pointer_coordinate() else {
        return;
    };
    let nearest = *points
        .iter()
        .min_by(|a, b| {
            (a[0] - pointer.x)
                .abs()
                .total_cmp(&(b[0] - pointer.x).abs())
        })
        .expect("points is non-empty");

    plot_ui.points(
        Points::new(vec![nearest])
            .radius(4.0_f32)
            .color(ACCENT)
            .filled(true),
    );
    let when = match time {
        TimeFmt::Ago => {
            if nearest[0] >= -0.5 {
                "now".to_string()
            } else {
                format!("{:.0} s ago", -nearest[0])
            }
        }
        TimeFmt::Offset => fmt_time(nearest[0].max(0.0) as u64),
    };
    let text = format!("{when} - {}", (phrase)(nearest[1]));
    // Drawn inside the plot rather than as an egui tooltip: a tooltip is a
    // floating layer that animates and keeps asking for repaints while the
    // pointer rests on it, which costs more than the whole dashboard.
    let bounds = plot_ui.plot_bounds();
    let span = (bounds.max()[0] - bounds.min()[0]).max(1e-9);
    let height = (bounds.max()[1] - bounds.min()[1]).max(1e-9);
    // The readout is drawn inside the plot, so it has to dodge the edges:
    // to the left of a point near the right edge, and below a point near
    // the top, where there is no room above it.
    let near_right = (nearest[0] - bounds.min()[0]) / span > 0.7;
    let near_top = (nearest[1] - bounds.min()[1]) / height > 0.85;
    let anchor = match (near_right, near_top) {
        (false, false) => egui::Align2::LEFT_BOTTOM,
        (true, false) => egui::Align2::RIGHT_BOTTOM,
        (false, true) => egui::Align2::LEFT_TOP,
        (true, true) => egui::Align2::RIGHT_TOP,
    };
    let offset = height * 0.04;
    let y = if near_top {
        nearest[1] - offset
    } else {
        nearest[1] + offset
    };
    plot_ui.text(
        Text::new(
            PlotPoint::new(nearest[0], y),
            RichText::new(text).size(12.0).color(ui_strong(plot_ui)),
        )
        .anchor(anchor),
    );
}

/// The theme's strong text colour, for labels drawn inside a plot.
fn ui_strong(plot_ui: &egui_plot::PlotUi) -> Color32 {
    plot_ui.ctx().style().visuals.strong_text_color()
}

fn technical_ui(ui: &mut egui::Ui, s: &Summary) {
    egui::CollapsingHeader::new("Technical details").show(ui, |ui| {
        card(ui, |ui| {
            // Friendly units here too: GHz and GB, not MHz and MiB.
            let plain = |ui: &mut egui::Ui, label: &str, series: &Series, unit: &str| {
                ui.label(RichText::new(label).weak());
                ui.label(format!("avg {:.0} {unit}", series.avg));
                ui.label(format!("peak {:.0} {unit}", series.peak));
                ui.label(RichText::new(format!("at {}", fmt_time(series.peak_at_secs))).weak());
                ui.end_row();
            };
            let ghz = |ui: &mut egui::Ui, label: &str, series: &Series| {
                ui.label(RichText::new(label).weak());
                ui.label(format!("avg {:.2} GHz", series.avg / 1000.0));
                ui.label(format!("peak {:.2} GHz", series.peak / 1000.0));
                ui.label(RichText::new(format!("at {}", fmt_time(series.peak_at_secs))).weak());
                ui.end_row();
            };
            let gb = |ui: &mut egui::Ui, label: &str, series: &Series| {
                ui.label(RichText::new(label).weak());
                ui.label(format!("avg {:.1} GB", series.avg / 1024.0));
                ui.label(format!("peak {:.1} GB", series.peak / 1024.0));
                ui.label(RichText::new(format!("at {}", fmt_time(series.peak_at_secs))).weak());
                ui.end_row();
            };

            if let Some(gpu) = &s.gpu {
                ui.label(RichText::new(s.gpu_name.as_deref().unwrap_or("GPU")).strong());
                egui::Grid::new("gpu-tech").striped(true).show(ui, |ui| {
                    plain(ui, "Temperature", &gpu.temp_c, "°C");
                    ghz(ui, "Speed", &gpu.core_mhz);
                    plain(ui, "Utilization", &gpu.util_pct, "%");
                    plain(ui, "Power draw", &gpu.power_w, "W");
                    gb(ui, "VRAM used", &gpu.vram_used_mib);
                });
                for t in &gpu.throttle {
                    ui.label(format!(
                        "{}: {} episode(s), {} total, longest at {}",
                        t.reason,
                        t.episodes,
                        fmt_time(t.seconds),
                        fmt_time(t.longest_at_secs)
                    ));
                }
            } else {
                ui.label("No NVIDIA GPU detected");
            }

            ui.add_space(8.0);
            ui.label(RichText::new(&s.cpu_name).strong());
            egui::Grid::new("cpu-tech").striped(true).show(ui, |ui| {
                plain(ui, "Usage", &s.cpu.usage_pct, "%");
                ghz(ui, "Speed (fastest core)", &s.cpu.max_core_mhz);
                if let Some(temp) = &s.cpu_temp {
                    plain(ui, "Temperature", temp, "°C");
                }
            });
            if let Some(fan) = &s.fan_rpm {
                ui.add_space(8.0);
                ui.label(RichText::new("Fans").strong());
                egui::Grid::new("fan-tech").striped(true).show(ui, |ui| {
                    plain(ui, "Speed", fan, "rpm");
                });
                let note = match s.cooling {
                    report::Cooling::AtLimit => {
                        "Fans were at full speed while the GPU throttled: cooling limit reached."
                    }
                    report::Cooling::FansHeldBack => {
                        "The GPU throttled while the fans still had room to spin faster."
                    }
                    report::Cooling::Unknown => "",
                };
                if !note.is_empty() {
                    ui.label(RichText::new(note).weak());
                }
            }
            if let Some(t) = &s.cpu.throttle {
                ui.label(format!(
                    "Estimated clock drops under load: {} episode(s), {} total \
                     ({:.1}-GHz loaded baseline)",
                    t.episodes,
                    fmt_time(t.seconds),
                    t.baseline_mhz as f64 / 1000.0
                ));
            }
            ui.label(format!(
                "RAM peak: {:.1} GB",
                s.ram_peak_mib as f64 / 1024.0
            ));
        });
    });
}

fn export_html(loaded: &Loaded) -> String {
    let path = loaded.path.with_extension("html");
    match report::html::render(&loaded.session, &loaded.summary)
        .and_then(|html| std::fs::write(&path, html).map_err(Into::into))
    {
        Ok(()) => {
            // `start` must go through cmd; the empty "" is its window title.
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", ""])
                .arg(&path)
                .spawn();
            format!("Wrote {}", path.display())
        }
        Err(e) => format!("Export failed: {e:#}"),
    }
}
