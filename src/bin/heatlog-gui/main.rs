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

/// Rust-family copper red: the app's primary accent.
pub const ACCENT: Color32 = Color32::from_rgb(206, 66, 43);
pub const RED: Color32 = Color32::from_rgb(220, 38, 38);
pub const AMBER: Color32 = Color32::from_rgb(217, 119, 6);
pub const GREEN: Color32 = Color32::from_rgb(22, 163, 74);
const COOL_BLUE: Color32 = Color32::from_rgb(59, 130, 246);
/// 61 points at one per second = a 60-second window.
pub const HISTORY: usize = 61;

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
    Color32::from_rgb(
        channel(a.r(), b.r()),
        channel(a.g(), b.g()),
        channel(a.b(), b.b()),
    )
}

/// Usage gauges stay inside the brand accents: amber when quiet, rust when
/// pegged. (The heat scale with its cool blue is for temperatures only.)
pub fn usage_color(frac: f32) -> Color32 {
    lerp(AMBER, ACCENT, frac)
}

/// The heat scale: cool blue -> warm amber -> rust red.
pub fn heat_color(frac: f32) -> Color32 {
    if frac < 0.5 {
        lerp(COOL_BLUE, AMBER, frac * 2.0)
    } else {
        lerp(AMBER, ACCENT, (frac - 0.5) * 2.0)
    }
}

/// Where a temperature sits on the heat scale.
pub fn temp_frac(celsius: f64) -> f32 {
    ((celsius - 30.0) / 65.0) as f32
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1060.0, 740.0])
            .with_min_inner_size([760.0, 520.0])
            .with_icon(app_icon()),
        // A dashboard redraws a few times a second, so waiting for the
        // display's refresh buys nothing and some drivers burn CPU doing it.
        vsync: false,
        ..Default::default()
    };
    eframe::run_native(
        "heatlog",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

/// A simple original flame, drawn in code: an outer rust teardrop with an
/// amber core. No external image asset needed.
fn app_icon() -> egui::IconData {
    const SIZE: usize = 48;
    let in_triangle = |px: f32, py: f32, a: (f32, f32), b: (f32, f32), c: (f32, f32)| {
        let sign = |p: (f32, f32), q: (f32, f32), r: (f32, f32)| {
            (p.0 - r.0) * (q.1 - r.1) - (q.0 - r.0) * (p.1 - r.1)
        };
        let (d1, d2, d3) = (
            sign((px, py), a, b),
            sign((px, py), b, c),
            sign((px, py), c, a),
        );
        !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
    };
    let in_circle =
        |px: f32, py: f32, cx: f32, cy: f32, r: f32| (px - cx).powi(2) + (py - cy).powi(2) <= r * r;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (fx, fy) = (x as f32 / SIZE as f32, y as f32 / SIZE as f32);
            let outer = in_circle(fx, fy, 0.5, 0.62, 0.28)
                || in_triangle(fx, fy, (0.5, 0.08), (0.28, 0.56), (0.72, 0.56));
            let inner = in_circle(fx, fy, 0.5, 0.68, 0.15)
                || in_triangle(fx, fy, (0.5, 0.36), (0.38, 0.66), (0.62, 0.66));
            let color = if inner {
                Some(AMBER)
            } else if outer {
                Some(ACCENT)
            } else {
                None
            };
            if let Some(c) = color {
                let i = (y * SIZE + x) * 4;
                rgba[i] = c.r();
                rgba[i + 1] = c.g();
                rgba[i + 2] = c.b();
                rgba[i + 3] = 255;
            }
        }
    }
    egui::IconData {
        rgba,
        width: SIZE as u32,
        height: SIZE as u32,
    }
}

#[derive(PartialEq, Clone, Copy)]
pub enum Page {
    Overview,
    Cpu,
    Gpu,
    Memory,
    Recordings,
    Settings,
    /// CPU-temperature setup, reached from the Overview banner.
    TempSetup,
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

/// Settings as edited on the Settings page, saved on demand.
pub struct SettingsDraft {
    pub games: Vec<String>,
    pub new_game: String,
    pub interval_secs: u64,
    pub check_updates: bool,
    pub include_prereleases: bool,
    pub delete_after_days: u64,
    pub saved: Option<String>,
}

impl SettingsDraft {
    fn from(cfg: &heatlog::config::Config) -> Self {
        Self {
            games: cfg.games.clone(),
            new_game: String::new(),
            interval_secs: cfg.interval_secs.max(1),
            check_updates: cfg.check_updates,
            include_prereleases: cfg.include_prereleases,
            delete_after_days: cfg.delete_after_days,
            saved: None,
        }
    }
}

pub struct App {
    cmds: Sender<worker::Cmd>,
    events: Receiver<worker::Event>,
    pub info: Option<worker::Info>,
    pub latest: Option<Sample>,
    /// The sample before `latest`, and when `latest` arrived: numbers and
    /// gauges glide between the two instead of jumping.
    pub prev: Option<Sample>,
    pub sample_at: Instant,
    /// Whether the window has focus; drives how often it repaints.
    pub focused: bool,
    pub hist: History,
    pub page: Page,
    pub recording: Option<RecordingUi>,
    pub auto: bool,
    pub status: Option<String>,
    pub error: Option<String>,
    /// Something the user may want to act on, but not a failure.
    pub hint: Option<String>,
    pub recordings: pages::Recordings,
    pub view: Option<Box<view::Loaded>>,
    pub settings: SettingsDraft,
    pub confirm_delete: Option<std::path::PathBuf>,
    pub confirm_delete_all: bool,
    pub update: Option<heatlog::update::Available>,
    update_rx: Receiver<Option<heatlog::update::Available>>,
    pub installing: bool,
    pub install_error: Option<String>,
    install_rx: Option<Receiver<Result<(), String>>>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        cc.egui_ctx.set_fonts(fonts);
        apply_style(&cc.egui_ctx);

        let cfg = heatlog::config::load_or_create().unwrap_or_default();
        // Housekeeping before the list is built, so pruned sessions never
        // show up in it.
        let pruned = cfg
            .sessions_dir()
            .and_then(|dir| heatlog::session::delete_older_than(&dir, cfg.delete_after_days))
            .unwrap_or(0);
        let update_rx = spawn_update_check(&cfg, cc.egui_ctx.clone());
        let (cmds, events) = worker::spawn(cc.egui_ctx.clone());
        Self {
            cmds,
            events,
            settings: SettingsDraft::from(&cfg),
            confirm_delete: None,
            confirm_delete_all: false,
            update: None,
            update_rx,
            installing: false,
            install_error: None,
            install_rx: None,
            status: (pruned > 0).then(|| format!("Deleted {pruned} old recording(s)")),
            info: None,
            latest: None,
            prev: None,
            sample_at: Instant::now(),
            focused: true,
            hist: History::default(),
            page: Page::Overview,
            recording: None,
            auto: false,
            error: None,
            hint: None,
            recordings: pages::Recordings::load(&cc.egui_ctx),
            view: None,
        }
    }

    /// Installs HWiNFO on a thread of its own: winget takes a minute and
    /// must not stall the window or the sampling loop.
    pub fn start_hwinfo_install(&mut self) {
        if self.installing {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.installing = true;
        self.install_error = None;
        self.status = None;
        self.install_rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(pages::install_hwinfo());
        });
    }

    pub fn send(&self, cmd: worker::Cmd) {
        let _ = self.cmds.send(cmd);
    }

    pub fn recordings_bytes(&self) -> u64 {
        heatlog::config::load_or_create()
            .and_then(|cfg| cfg.sessions_dir())
            .map(|dir| heatlog::session::total_bytes(&dir))
            .unwrap_or(0)
    }

    pub fn save_settings(&mut self) {
        let mut cfg = heatlog::config::load_or_create().unwrap_or_default();
        cfg.games = self
            .settings
            .games
            .iter()
            .map(|g| g.trim().to_string())
            .filter(|g| !g.is_empty())
            .collect();
        cfg.interval_secs = self.settings.interval_secs.max(1);
        cfg.check_updates = self.settings.check_updates;
        cfg.include_prereleases = self.settings.include_prereleases;
        cfg.delete_after_days = self.settings.delete_after_days;
        match heatlog::config::save(&cfg) {
            Ok(()) => {
                self.settings.saved = Some("Saved".to_string());
                self.send(worker::Cmd::Settings {
                    games: cfg.games,
                    interval_secs: cfg.interval_secs,
                });
            }
            Err(e) => self.error = Some(format!("could not save settings: {e:#}")),
        }
    }

    /// Downloads and starts the installer, then closes the window so the
    /// files it replaces are not in use. Without an installer asset (a
    /// portable build) the release page opens instead.
    fn start_update(&mut self, ctx: &egui::Context, available: &heatlog::update::Available) {
        match &available.installer {
            Some(_) => match heatlog::update::run_installer(available) {
                Ok(_) => {
                    self.status = Some("Installer started - heatlog will close".to_string());
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Err(e) => self.error = Some(format!("update failed: {e:#}")),
            },
            None => {
                let _ = std::process::Command::new("cmd")
                    .args(["/C", "start", ""])
                    .arg(&available.page_url)
                    .spawn();
                self.update = None;
            }
        }
    }

    pub fn delete_recording(&mut self, path: &std::path::Path, ctx: &egui::Context) {
        self.confirm_delete = None;
        match heatlog::session::delete(path) {
            Ok(()) => {
                self.status = Some("Recording deleted".to_string());
                self.recordings = pages::Recordings::load(ctx);
            }
            Err(e) => self.error = Some(format!("could not delete: {e:#}")),
        }
    }

    pub fn delete_all_recordings(&mut self, ctx: &egui::Context) {
        self.confirm_delete_all = false;
        let paths: Vec<std::path::PathBuf> = self
            .recordings
            .rows
            .iter()
            .map(|r| r.path.clone())
            .collect();
        let total = paths.len();
        for path in paths {
            if let Err(e) = heatlog::session::delete(&path) {
                self.error = Some(format!("could not delete: {e:#}"));
                break;
            }
        }
        self.status = Some(format!("Deleted {total} recording(s)"));
        self.recordings = pages::Recordings::load(ctx);
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                worker::Event::Ready(info) => {
                    // A fresh probe answers whatever the status line was
                    // waiting for, so it must not linger.
                    if self.recording.is_none() {
                        self.status = None;
                    }
                    self.info = Some(*info);
                }
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
                    self.prev = self.latest.take();
                    self.sample_at = Instant::now();
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
                worker::Event::Hint(hint) => self.hint = Some(hint),
            }
        }
    }

    /// How far the display has glided from the previous sample to the
    /// latest one, eased: 0 just after a sample arrives, 1 once settled.
    /// Gliding costs extra frames, so it only happens while someone is
    /// looking: in the background the numbers simply step once a second.
    pub fn blend(&self) -> f32 {
        if !self.focused {
            return 1.0;
        }
        let t = (self.sample_at.elapsed().as_secs_f32() / 0.4).clamp(0.0, 1.0);
        // smoothstep
        t * t * (3.0 - 2.0 * t)
    }

    /// A value glided between the two most recent samples.
    pub fn smooth(&self, pick: impl Fn(&Sample) -> f32) -> f32 {
        let Some(latest) = &self.latest else {
            return 0.0;
        };
        let now = pick(latest);
        match &self.prev {
            Some(prev) => {
                let was = pick(prev);
                was + (now - was) * self.blend()
            }
            None => now,
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);
        self.recordings.drain();
        if let Ok(found) = self.update_rx.try_recv() {
            self.update = found;
        }
        if let Some(rx) = &self.install_rx {
            if let Ok(result) = rx.try_recv() {
                self.installing = false;
                self.install_rx = None;
                match result {
                    Ok(()) => {
                        self.install_error = None;
                        self.status = Some(
                            "HWiNFO installed - start it, tick Shared Memory Support, then                              use Check again."
                                .to_string(),
                        );
                        self.send(worker::Cmd::Reprobe);
                    }
                    Err(reason) => {
                        self.status = None;
                        self.install_error = Some(reason);
                    }
                }
            }
        }
        if ctx.input(|i| i.viewport().minimized) == Some(true) {
            return; // nothing to draw, and no repaints to schedule
        }

        // A monitor must be cheap: the window repaints when a sample
        // arrives (once a second, from the worker), and only briefly in
        // between while a value is still gliding or the record dot is
        // pulsing. Nothing here runs at the display's refresh rate.
        self.focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        if self.blend() < 1.0 {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        if self.recording.is_some() && self.focused {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::R)) {
            let _ = self.cmds.send(if self.recording.is_some() {
                worker::Cmd::Stop
            } else {
                worker::Cmd::Record
            });
        }

        self.top_bar(ctx);
        self.sidebar(ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(egui::Margin::same(16.0)))
            .show(ctx, |ui| {
                if let Some(available) = self.update.clone() {
                    card(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "Update available: {}{}",
                                    available.version,
                                    if available.prerelease {
                                        " (pre-release)"
                                    } else {
                                        ""
                                    }
                                ))
                                .strong(),
                            );
                            // Never mid-recording: the installer replaces
                            // the running files.
                            let recording = self.recording.is_some();
                            ui.add_enabled_ui(!recording, |ui| {
                                if ui
                                    .button("Update now")
                                    .on_disabled_hover_text("Stop recording first")
                                    .clicked()
                                {
                                    self.start_update(ctx, &available);
                                }
                            });
                            if ui.button("Later").clicked() {
                                self.update = None;
                            }
                        });
                    });
                    ui.add_space(6.0);
                }
                if let Some(hint) = &self.hint {
                    let hint = hint.clone();
                    ui.horizontal(|ui| {
                        ui.colored_label(AMBER, &hint);
                        if ui.small_button("dismiss").clicked() {
                            self.hint = None;
                        }
                    });
                    ui.add_space(6.0);
                }
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
                    Page::Overview => {
                        if let Some(page) = pages::overview(ui, self) {
                            self.page = page;
                        }
                    }
                    Page::TempSetup => {
                        if let Some(page) = pages::temp_setup(ui, self) {
                            self.page = page;
                        }
                    }
                    Page::Cpu => pages::cpu(ui, self),
                    Page::Gpu => pages::gpu(ui, self),
                    Page::Memory => pages::memory(ui, self),
                    Page::Recordings => {
                        if let Some(open) = pages::recordings(ui, self) {
                            match view::load_session(&open) {
                                Ok(loaded) => self.view = Some(Box::new(loaded)),
                                Err(e) => self.error = Some(format!("{e:#}")),
                            }
                        }
                    }
                    Page::Settings => {
                        if let Some(page) = pages::settings(ui, self) {
                            self.page = page;
                        }
                    }
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
                    use egui_phosphor::regular as icons;
                    match &self.recording {
                        None => {
                            let button = egui::Button::new(
                                RichText::new(format!("{}  Start recording", icons::RECORD))
                                    .size(15.0)
                                    .color(Color32::WHITE),
                            )
                            .fill(RED)
                            .min_size(egui::vec2(180.0, 34.0));
                            if ui.add(button).clicked() {
                                let _ = self.cmds.send(worker::Cmd::Record);
                            }
                        }
                        Some(rec) => {
                            let pulse = ((ui.input(|i| i.time) * 3.5).sin() * 0.5 + 0.5) as f32;
                            let label = format!(
                                "{}  Stop recording   {}",
                                icons::STOP,
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
                    Some(gpu) => Some(format!("{} °C  ·  {}%", gpu.temp_c, gpu.util_pct)),
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

                use egui_phosphor::regular as icons;
                let gpu_graph = (!self.hist.gpu_temp_c.is_empty()).then_some(&self.hist.gpu_temp_c);
                let items = [
                    NavItem::plain(Page::Overview, icons::GAUGE, "Overview"),
                    NavItem {
                        page: Page::Cpu,
                        icon: icons::CPU,
                        title: "CPU",
                        sub: cpu_sub,
                        graph: Some(&self.hist.cpu_pct),
                        y_max: Some(100.0),
                    },
                    NavItem {
                        page: Page::Gpu,
                        icon: icons::GRAPHICS_CARD,
                        title: "GPU",
                        sub: gpu_sub,
                        graph: gpu_graph,
                        y_max: Some(100.0),
                    },
                    NavItem {
                        page: Page::Memory,
                        icon: icons::MEMORY,
                        title: "Memory",
                        sub: mem_sub,
                        graph: Some(&self.hist.ram_gb),
                        y_max: Some((ram_total as f64 / 1024.0).max(1.0)),
                    },
                    NavItem::plain(Page::Recordings, icons::FILM_STRIP, "Recordings"),
                    NavItem::plain(Page::Settings, icons::GEAR, "Settings"),
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
    icon: &'static str,
    title: &'static str,
    sub: Option<String>,
    graph: Option<&'a VecDeque<f64>>,
    y_max: Option<f64>,
}

impl NavItem<'_> {
    fn plain(page: Page, icon: &'static str, title: &'static str) -> Self {
        Self {
            page,
            icon,
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
            ui.label(
                RichText::new(format!("{}  {}", item.icon, item.title))
                    .strong()
                    .size(14.0),
            );
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
            // Slightly warm surfaces instead of neutral grey.
            if theme == egui::Theme::Dark {
                style.visuals.window_fill = Color32::from_rgb(26, 22, 19);
                style.visuals.panel_fill = Color32::from_rgb(26, 22, 19);
                style.visuals.extreme_bg_color = Color32::from_rgb(19, 16, 14);
                style.visuals.faint_bg_color = Color32::from_rgb(38, 32, 27);
                style.visuals.widgets.noninteractive.bg_stroke.color =
                    Color32::from_rgb(62, 52, 44);
            } else {
                style.visuals.window_fill = Color32::from_rgb(251, 247, 243);
                style.visuals.panel_fill = Color32::from_rgb(251, 247, 243);
                style.visuals.faint_bg_color = Color32::from_rgb(243, 236, 229);
                style.visuals.widgets.noninteractive.bg_stroke.color =
                    Color32::from_rgb(224, 213, 203);
            }
        });
    }
}

/// Asks GitHub for a newer release, off the UI thread. Opt-out via
/// settings; nothing else here ever reaches the internet.
fn spawn_update_check(
    cfg: &heatlog::config::Config,
    ctx: egui::Context,
) -> Receiver<Option<heatlog::update::Available>> {
    let (tx, rx) = std::sync::mpsc::channel();
    if cfg.check_updates {
        let prereleases = cfg.include_prereleases;
        std::thread::spawn(move || {
            let found = heatlog::update::check(prereleases).unwrap_or(None);
            let _ = tx.send(found);
            ctx.request_repaint();
        });
    }
    rx
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
        .show(ui, |ui| {
            // Cards span the column they are in, so a page of them lines
            // up instead of each shrinking to its own content.
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}
