//! Desktop app: session list with verdict colours, and a session view
//! mirroring the HTML report (verdict, charts, technical details, export).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use eframe::egui::{self, Color32, RichText};
use egui_plot::{HLine, Line, Plot, PlotPoints, Polygon};

use heatlog::report::charts;
use heatlog::report::{self, fmt_time, Series, Summary};
use heatlog::session::{self, Session};

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([980.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "heatlog",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

struct Row {
    path: PathBuf,
    started: String,
    duration: String,
    game: String,
    verdict: Option<&'static str>,
}

struct ChartView {
    title: &'static str,
    unit: &'static str,
    explain: String,
    points: Vec<[f64; 2]>,
    bands: Vec<(f64, f64)>,
    band_color: Color32,
    y_lo: f64,
    y_hi: f64,
    hot_from: Option<f64>,
}

struct Loaded {
    path: PathBuf,
    session: Session,
    summary: Summary,
    verdict_class: &'static str,
    verdict_text: String,
    charts: Vec<ChartView>,
    export_status: Option<String>,
}

enum Screen {
    List,
    View(Box<Loaded>),
}

struct App {
    rows: Vec<Row>,
    verdicts: Option<Receiver<(usize, &'static str)>>,
    screen: Screen,
    error: Option<String>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        match load_rows() {
            Ok(rows) => {
                let verdicts = analyze_in_background(&rows, cc.egui_ctx.clone());
                Self {
                    rows,
                    verdicts: Some(verdicts),
                    screen: Screen::List,
                    error: None,
                }
            }
            Err(e) => Self {
                rows: Vec::new(),
                verdicts: None,
                screen: Screen::List,
                error: Some(format!("{e:#}")),
            },
        }
    }
}

fn load_rows() -> anyhow::Result<Vec<Row>> {
    let cfg = heatlog::config::load_or_create()?;
    let mut files = session::list_files(&cfg.sessions_dir()?)?;
    files.reverse(); // newest first
    Ok(files
        .into_iter()
        .filter_map(|path| {
            let peek = session::peek(&path).ok()?;
            Some(Row {
                path,
                started: peek.meta.started.format("%Y-%m-%d %H:%M").to_string(),
                duration: fmt_time(peek.duration_secs),
                game: peek.meta.game.unwrap_or_else(|| "-".to_string()),
                verdict: None,
            })
        })
        .collect())
}

/// Verdicts need a full read and analysis per session, so they fill in from
/// a background thread while the list is already usable.
fn analyze_in_background(rows: &[Row], ctx: egui::Context) -> Receiver<(usize, &'static str)> {
    let (tx, rx) = mpsc::channel();
    // cloned so the worker owns its own path list
    let paths: Vec<PathBuf> = rows.iter().map(|r| r.path.clone()).collect();
    std::thread::spawn(move || {
        for (i, path) in paths.iter().enumerate() {
            let class = session::read(path)
                .ok()
                .and_then(|s| report::analyze(&s).ok())
                .map(|summary| report::verdict(&summary).0)
                .unwrap_or("na");
            if tx.send((i, class)).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    });
    rx
}

fn load_session(path: &Path) -> anyhow::Result<Loaded> {
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
        "good" => Color32::from_rgb(22, 163, 74),
        "warn" => Color32::from_rgb(217, 119, 6),
        "bad" => Color32::from_rgb(220, 38, 38),
        _ => Color32::GRAY,
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(rx) = &self.verdicts {
            while let Ok((i, class)) = rx.try_recv() {
                if let Some(row) = self.rows.get_mut(i) {
                    row.verdict = Some(class);
                }
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(error) = &self.error {
                ui.colored_label(Color32::from_rgb(220, 38, 38), error);
                return;
            }
            let open = match &mut self.screen {
                Screen::List => self.list_ui(ui),
                Screen::View(loaded) => {
                    let back = view_ui(ui, loaded);
                    if back {
                        self.screen = Screen::List;
                    }
                    None
                }
            };
            if let Some(path) = open {
                match load_session(&path) {
                    Ok(loaded) => self.screen = Screen::View(Box::new(loaded)),
                    Err(e) => self.error = Some(format!("{e:#}")),
                }
            }
        });
    }
}

impl App {
    fn list_ui(&self, ui: &mut egui::Ui) -> Option<PathBuf> {
        ui.heading("Sessions");
        ui.add_space(4.0);
        if self.rows.is_empty() {
            ui.label("No sessions recorded yet. Record one with: heatlog record");
            return None;
        }
        let mut open = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for row in &self.rows {
                ui.horizontal(|ui| {
                    let dot = match row.verdict {
                        Some(class) => RichText::new("●").color(verdict_color(class)),
                        None => RichText::new("○").color(Color32::GRAY),
                    };
                    ui.label(dot);
                    if ui.link(&row.started).clicked() {
                        open = Some(row.path.clone());
                    }
                    ui.label(RichText::new(&row.duration).weak());
                    ui.label(RichText::new(&row.game).weak());
                });
            }
        });
        open
    }
}

/// Returns true when the back button was clicked.
fn view_ui(ui: &mut egui::Ui, loaded: &mut Loaded) -> bool {
    let mut back = false;
    ui.horizontal(|ui| {
        if ui.button("< Sessions").clicked() {
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
        .fill(color.gamma_multiply(0.15))
        .stroke(egui::Stroke::new(1.0_f32, color))
        .inner_margin(10.0)
        .rounding(6.0)
        .show(ui, |ui| {
            ui.label(&loaded.verdict_text);
        });
    ui.add_space(6.0);

    egui::ScrollArea::vertical().show(ui, |ui| {
        for chart in &loaded.charts {
            ui.add_space(8.0);
            ui.label(RichText::new(format!("{} ({})", chart.title, chart.unit)).strong());
            plot_chart(ui, chart, loaded.summary.duration_secs as f64);
            ui.label(RichText::new(&chart.explain).weak().small());
        }
        ui.add_space(8.0);
        technical_ui(ui, &loaded.summary);
    });
    back
}

fn plot_chart(ui: &mut egui::Ui, chart: &ChartView, duration: f64) {
    Plot::new(chart.title)
        .height(170.0)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_scroll(false)
        .allow_boxed_zoom(false)
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
                    HLine::new(hot)
                        .color(Color32::from_rgba_unmultiplied(220, 38, 38, 120))
                        .name("hot"),
                );
            }
            plot_ui.line(
                Line::new(PlotPoints::from(chart.points.clone()))
                    .color(Color32::from_rgb(37, 99, 235))
                    .width(2.0_f32),
            );
        });
}

fn technical_ui(ui: &mut egui::Ui, s: &Summary) {
    egui::CollapsingHeader::new("Technical details").show(ui, |ui| {
        let row = |ui: &mut egui::Ui, label: &str, series: &Series, unit: &str| {
            ui.label(label);
            ui.label(format!("avg {:.0} {unit}", series.avg));
            ui.label(format!("peak {:.0} {unit}", series.peak));
            ui.label(format!("at {}", fmt_time(series.peak_at_secs)));
            ui.end_row();
        };

        if let Some(gpu) = &s.gpu {
            ui.label(RichText::new(s.gpu_name.as_deref().unwrap_or("GPU")).strong());
            egui::Grid::new("gpu").striped(true).show(ui, |ui| {
                row(ui, "Temperature", &gpu.temp_c, "C");
                row(ui, "Core clock", &gpu.core_mhz, "MHz");
                row(ui, "Utilisation", &gpu.util_pct, "%");
                row(ui, "Power draw", &gpu.power_w, "W");
                row(ui, "VRAM used", &gpu.vram_used_mib, "MiB");
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

        ui.add_space(6.0);
        ui.label(RichText::new(&s.cpu_name).strong());
        egui::Grid::new("cpu").striped(true).show(ui, |ui| {
            row(ui, "Usage", &s.cpu.usage_pct, "%");
            row(ui, "Fastest core", &s.cpu.max_core_mhz, "MHz");
            if let Some(temp) = &s.cpu_temp {
                row(ui, "Temperature", temp, "C");
            }
        });
        if let Some(t) = &s.cpu.throttle {
            ui.label(format!(
                "Estimated clock drops under load: {} episode(s), {} total ({}-MHz baseline)",
                t.episodes,
                fmt_time(t.seconds),
                t.baseline_mhz
            ));
        }
        ui.label(format!("RAM peak: {} MiB", s.ram_peak_mib));
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
