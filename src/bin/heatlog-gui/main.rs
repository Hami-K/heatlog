//! heatlog desktop app: live hardware monitoring, OBS-style recording, and
//! session reports, laid out like a modern Windows system tool.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod pages;
mod view;
mod worker;

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};

use heatlog::report::fmt_time;
use heatlog::session::Sample;

pub const ACCENT: Color32 = Color32::from_rgb(59, 130, 246);
pub const RED: Color32 = Color32::from_rgb(220, 38, 38);
pub const AMBER: Color32 = Color32::from_rgb(217, 119, 6);
pub const GREEN: Color32 = Color32::from_rgb(22, 163, 74);
/// Sidebar + hardware pages show the last 60 seconds; keep a little extra.
pub const HISTORY: usize = 90;

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1060.0, 740.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "heatlog",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

#[derive(PartialEq, Clone, Copy)]
pub enum Page {
    Overview,
    Cpu,
    Gpu,
    Memory,
    Recordings,
    Settings,
}

#[derive(Default)]
pub struct History {
    pub cpu_pct: VecDeque<f64>,
    pub cpu_ghz: VecDeque<f64>,
    pub cpu_temp_c: VecDeque<f64>,
    pub gpu_temp_c: VecDeque<f64>,
    pub gpu_util_pct: VecDeque<f64>,
    pub gpu_power_w: VecDeque<f64>,
    pub ram_gb: VecDeque<f64>,
}

fn push(series: &mut VecDeque<f64>, value: f64) {
    series.push_back(value);
    if series.len() > HISTORY {
        series.pop_front();
    }
}

pub struct RecordingUi {
    pub started: Instant,
    pub samples: u64,
    pub game: Option<String>,
}

pub struct App {
    cmds: Sender<worker::Cmd>,
    events: Receiver<worker::Event>,
    pub info: Option<worker::Info>,
    pub latest: Option<Sample>,
    pub hist: History,
    pub page: Page,
    pub recording: Option<RecordingUi>,
    pub auto: bool,
    pub status: Option<String>,
    pub error: Option<String>,
    pub recordings: pages::Recordings,
    pub view: Option<Box<view::Loaded>>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_style(&cc.egui_ctx);
        let (cmds, events) = worker::spawn(cc.egui_ctx.clone());
        Self {
            cmds,
            events,
            info: None,
            latest: None,
            hist: History::default(),
            page: Page::Overview,
            recording: None,
            auto: false,
            status: None,
            error: None,
            recordings: pages::Recordings::load(&cc.egui_ctx),
            view: None,
        }
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                worker::Event::Ready(info) => self.info = Some(*info),
                worker::Event::Tick(sample) => {
                    push(&mut self.hist.cpu_pct, f64::from(sample.cpu.usage_pct));
                    push(
                        &mut self.hist.cpu_ghz,
                        sample.cpu.per_core_mhz.iter().copied().max().unwrap_or(0) as f64 / 1000.0,
                    );
                    if let Some(t) = sample.cpu_temp_c {
                        push(&mut self.hist.cpu_temp_c, f64::from(t));
                    }
                    if let Some(gpu) = &sample.gpu {
                        push(&mut self.hist.gpu_temp_c, f64::from(gpu.temp_c));
                        push(&mut self.hist.gpu_util_pct, f64::from(gpu.util_pct));
                        push(&mut self.hist.gpu_power_w, gpu.power_w);
                    }
                    push(&mut self.hist.ram_gb, sample.ram_used_mib as f64 / 1024.0);
                    if let Some(rec) = &mut self.recording {
                        rec.samples += 1;
                    }
                    self.latest = Some(*sample);
                }
                worker::Event::RecordingStarted { game } => {
                    self.status = None;
                    self.recording = Some(RecordingUi {
                        started: Instant::now(),
                        samples: 0,
                        game,
                    });
                }
                worker::Event::RecordingStopped { path, samples } => {
                    self.recording = None;
                    self.status = Some(format!(
                        "Saved {samples} samples to {}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    self.recordings = pages::Recordings::load(ctx);
                }
                worker::Event::Error(e) => self.error = Some(e),
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);
        self.recordings.drain();
        if self.recording.is_some() {
            // keep the pulsing dot animating between samples
            ctx.request_repaint_after(Duration::from_millis(90));
        }

        self.top_bar(ctx);
        self.sidebar(ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(egui::Margin::same(16.0)))
            .show(ctx, |ui| {
                if let Some(error) = &self.error {
                    let error = error.clone();
                    ui.horizontal(|ui| {
                        ui.colored_label(RED, &error);
                        if ui.small_button("dismiss").clicked() {
                            self.error = None;
                        }
                    });
                    ui.add_space(6.0);
                }
                if self.view.is_some() {
                    let mut back = false;
                    if let Some(loaded) = &mut self.view {
                        back = view::session_view(ui, loaded);
                    }
                    if back {
                        self.view = None;
                    }
                    return;
                }
                match self.page {
                    Page::Overview => pages::overview(ui, self),
                    Page::Cpu => pages::cpu(ui, self),
                    Page::Gpu => pages::gpu(ui, self),
                    Page::Memory => pages::memory(ui, self),
                    Page::Recordings => {
                        if let Some(open) = pages::recordings(ui, &self.recordings) {
                            match view::load_session(&open) {
                                Ok(loaded) => self.view = Some(Box::new(loaded)),
                                Err(e) => self.error = Some(format!("{e:#}")),
                            }
                        }
                    }
                    Page::Settings => pages::settings(ui, self),
                }
            });
    }
}

impl App {
    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top")
            .frame(
                egui::Frame::side_top_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(16.0, 10.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    match &self.recording {
                        None => {
                            let button = egui::Button::new(
                                RichText::new("\u{23FA}  Start recording")
                                    .size(15.0)
                                    .color(Color32::WHITE),
                            )
                            .fill(ACCENT)
                            .min_size(egui::vec2(180.0, 34.0));
                            if ui.add(button).clicked() {
                                let _ = self.cmds.send(worker::Cmd::Record);
                            }
                        }
                        Some(rec) => {
                            let pulse = ((ui.input(|i| i.time) * 3.5).sin() * 0.5 + 0.5) as f32;
                            let label = format!(
                                "\u{23F9}  Stop recording   {}",
                                fmt_time(rec.started.elapsed().as_secs())
                            );
                            let button = egui::Button::new(
                                RichText::new(label).size(15.0).color(Color32::WHITE),
                            )
                            .fill(RED)
                            .min_size(egui::vec2(220.0, 34.0));
                            if ui.add(button).clicked() {
                                let _ = self.cmds.send(worker::Cmd::Stop);
                            }
                            ui.label(
                                RichText::new("\u{25CF}")
                                    .size(16.0)
                                    .color(RED.gamma_multiply(0.45 + 0.55 * pulse)),
                            );
                            if let Some(game) = &rec.game {
                                ui.label(RichText::new(format!("recording {game}")).weak());
                            }
                        }
                    }

                    let games = self
                        .info
                        .as_ref()
                        .map(|i| i.games.clone())
                        .unwrap_or_default();
                    ui.add_space(12.0);
                    ui.add_enabled_ui(!games.is_empty(), |ui| {
                        if ui
                            .checkbox(&mut self.auto, "Auto-record")
                            .on_hover_text(if games.is_empty() {
                                "Add game process names in Settings first".to_string()
                            } else {
                                format!("Starts when one of these launches: {}", games.join(", "))
                            })
                            .changed()
                        {
                            let _ = self.cmds.send(worker::Cmd::SetAuto(self.auto));
                        }
                    });
                    if self.auto && self.recording.is_none() {
                        ui.label(
                            RichText::new(format!("Watching for: {}", games.join(", "))).weak(),
                        );
                    }

                    if let Some(status) = &self.status {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(status).weak());
                        });
                    }
                });
            });
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("nav")
            .exact_width(200.0)
            .resizable(false)
            .show(ctx, |ui| {
                ui.add_space(8.0);
                let cpu_sub = self.latest.as_ref().map(|s| {
                    format!(
                        "{:.0}%  ·  {:.2} GHz",
                        s.cpu.usage_pct,
                        s.cpu.per_core_mhz.iter().copied().max().unwrap_or(0) as f64 / 1000.0
                    )
                });
                let gpu_sub = match self.latest.as_ref().and_then(|s| s.gpu.as_ref()) {
                    Some(gpu) => Some(format!("{} C  ·  {}%", gpu.temp_c, gpu.util_pct)),
                    None => Some("not detected".to_string()),
                };
                let ram_total = self.info.as_ref().map(|i| i.ram_total_mib).unwrap_or(0);
                let mem_sub = self.latest.as_ref().map(|s| {
                    let used = s.ram_used_mib as f64 / 1024.0;
                    let total = ram_total as f64 / 1024.0;
                    if total > 0.0 {
                        format!("{used:.1}/{total:.1} GB ({:.0}%)", 100.0 * used / total)
                    } else {
                        format!("{used:.1} GB")
                    }
                });

                let items = [
                    NavItem::plain(Page::Overview, "Overview"),
                    NavItem {
                        page: Page::Cpu,
                        title: "CPU",
                        sub: cpu_sub,
                        graph: Some(&self.hist.cpu_pct),
                        y_max: Some(100.0),
                    },
                    NavItem {
                        page: Page::Gpu,
                        title: "GPU",
                        sub: gpu_sub,
                        graph: Some(&self.hist.gpu_temp_c),
                        y_max: Some(100.0),
                    },
                    NavItem {
                        page: Page::Memory,
                        title: "Memory",
                        sub: mem_sub,
                        graph: Some(&self.hist.ram_gb),
                        y_max: Some((ram_total as f64 / 1024.0).max(1.0)),
                    },
                    NavItem::plain(Page::Recordings, "Recordings"),
                    NavItem::plain(Page::Settings, "Settings"),
                ];
                for item in items {
                    if nav_item(ui, self.page == item.page, &item) {
                        self.page = item.page;
                        self.view = None;
                    }
                    ui.add_space(2.0);
                }
            });
    }
}

struct NavItem<'a> {
    page: Page,
    title: &'static str,
    sub: Option<String>,
    graph: Option<&'a VecDeque<f64>>,
    y_max: Option<f64>,
}

impl NavItem<'_> {
    fn plain(page: Page, title: &'static str) -> Self {
        Self {
            page,
            title,
            sub: None,
            graph: None,
            y_max: None,
        }
    }
}

fn nav_item(ui: &mut egui::Ui, selected: bool, item: &NavItem) -> bool {
    let fill = if selected {
        ACCENT.gamma_multiply(0.18)
    } else {
        Color32::TRANSPARENT
    };
    let response = egui::Frame::none()
        .fill(fill)
        .rounding(6.0)
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(item.title).strong().size(14.0));
            if let Some(graph) = item.graph {
                mini_plot(ui, item.title, graph, item.y_max);
            }
            if let Some(sub) = &item.sub {
                ui.label(RichText::new(sub).weak().size(12.0));
            }
        })
        .response
        .interact(egui::Sense::click());
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.clicked()
}

fn mini_plot(ui: &mut egui::Ui, id: &str, data: &VecDeque<f64>, y_max: Option<f64>) {
    use egui_plot::{Line, Plot, PlotPoints};
    let points: Vec<[f64; 2]> = data
        .iter()
        .enumerate()
        .map(|(i, &v)| [i as f64 - (HISTORY as f64 - 1.0), v])
        .collect();
    let mut plot = Plot::new(format!("mini-{id}"))
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
        .include_x(-(HISTORY as f64 - 1.0))
        .include_x(0.0)
        .include_y(0.0);
    if let Some(y) = y_max {
        plot = plot.include_y(y);
    }
    plot.show(ui, |plot_ui| {
        plot_ui.line(
            Line::new(PlotPoints::from(points))
                .color(ACCENT)
                .width(1.5_f32)
                .fill(0.0_f32),
        );
    });
}

fn apply_style(ctx: &egui::Context) {
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.style_mut_of(theme, |style| {
            style.spacing.item_spacing = egui::vec2(10.0, 8.0);
            style.spacing.button_padding = egui::vec2(12.0, 6.0);
            style.visuals.widgets.noninteractive.rounding = egui::Rounding::same(6.0);
            style.visuals.widgets.inactive.rounding = egui::Rounding::same(6.0);
            style.visuals.widgets.hovered.rounding = egui::Rounding::same(6.0);
            style.visuals.widgets.active.rounding = egui::Rounding::same(6.0);
            style.visuals.widgets.open.rounding = egui::Rounding::same(6.0);
            style.visuals.selection.bg_fill = ACCENT;
            style.visuals.hyperlink_color = ACCENT;
        });
    }
}

/// Card container used across pages.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::none()
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(
            1.0_f32,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        ))
        .rounding(8.0)
        .inner_margin(12.0)
        .show(ui, |ui| add(ui))
        .inner
}
