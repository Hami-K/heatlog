//! Session view: verdict banner, charts with snapping tooltips, technical
//! details, HTML export.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Color32, RichText};
use egui_plot::{HLine, Line, Plot, PlotPoints, Points, Polygon};

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

/// Plain-language hover text per chart, e.g. "CPU 10% busy".
fn phrase_for(title: &str) -> fn(f64) -> String {
    match title {
        "GPU temperature" => |v| format!("GPU {v:.0} C"),
        "GPU speed" => |v| format!("GPU at {v:.2} GHz"),
        "GPU power" => |v| format!("GPU drawing {v:.0} W"),
        "CPU usage" => |v| format!("CPU {v:.0}% busy"),
        "CPU speed" => |v| format!("CPU at {v:.2} GHz"),
        "CPU temperature" => |v| format!("CPU {v:.0} C"),
        _ => |v| format!("{v:.1}"),
    }
}

pub struct Loaded {
    path: PathBuf,
    session: Session,
    summary: Summary,
    verdict_class: &'static str,
    verdict_text: String,
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
    Ok(Loaded {
        path: path.to_path_buf(),
        session,
        summary,
        verdict_class,
        verdict_text,
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

    egui::ScrollArea::vertical().show(ui, |ui| {
        for chart in &loaded.charts {
            ui.add_space(8.0);
            ui.label(RichText::new(format!("{} ({})", chart.title, chart.unit)).strong());
            plot_chart(ui, chart, loaded.summary.duration_secs as f64);
            ui.label(RichText::new(&chart.explain).weak().small());
        }
        ui.add_space(10.0);
        technical_ui(ui, &loaded.summary);
        ui.add_space(10.0);
    });
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
/// plain-language tooltip like "0:04 · CPU 10% busy" instead of raw cursor
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
    let text = format!("{when} · {}", (phrase)(nearest[1]));
    egui::show_tooltip_at_pointer(
        plot_ui.ctx(),
        egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("plot-tip-layer")),
        egui::Id::new("plot-tip"),
        |ui| {
            // One line, sized to content: the default tooltip width made
            // the text wrap mid-number.
            ui.add(egui::Label::new(text).wrap_mode(egui::TextWrapMode::Extend));
        },
    );
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
                    plain(ui, "Temperature", &gpu.temp_c, "C");
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
                    plain(ui, "Temperature", temp, "C");
                }
            });
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
