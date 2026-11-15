//! Background thread owning the sensors (WMI handles must stay on one
//! thread): samples every second for the live view, and doubles as the
//! recording engine when told to.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui;
use sysinfo::{Pid, ProcessesToUpdate, System};

use heatlog::config::{self, Config};
use heatlog::recorder::{self, Sensors};
use heatlog::session::{Sample, SessionWriter};

pub enum Cmd {
    Record,
    Stop,
    SetAuto(bool),
}

pub struct Info {
    pub cpu_name: String,
    pub gpu_name: Option<String>,
    pub ram_total_mib: u64,
    pub cpu_temp_source: Option<&'static str>,
    pub games: Vec<String>,
}

pub enum Event {
    Ready(Box<Info>),
    Tick(Box<Sample>),
    RecordingStarted { game: Option<String> },
    RecordingStopped { path: PathBuf, samples: u64 },
    Error(String),
}

pub fn spawn(ctx: egui::Context) -> (Sender<Cmd>, Receiver<Event>) {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    std::thread::spawn(move || run(ctx, cmd_rx, event_tx));
    (cmd_tx, event_rx)
}

struct Recording {
    writer: SessionWriter,
    samples: u64,
    game: Option<(Pid, String)>,
}

fn run(ctx: egui::Context, cmds: Receiver<Cmd>, events: Sender<Event>) {
    let send = |event: Event| {
        let _ = events.send(event);
        ctx.request_repaint();
    };

    let cfg = match config::load_or_create() {
        Ok(cfg) => cfg,
        Err(e) => {
            send(Event::Error(format!("config: {e:#}")));
            Config::default()
        }
    };
    let sessions_dir = cfg
        .sessions_dir()
        .unwrap_or_else(|_| PathBuf::from("sessions"));

    let mut sensors = Sensors::init(&cfg.lhm_url);
    send(Event::Ready(Box::new(Info {
        cpu_name: sensors.cpu_name().to_string(),
        gpu_name: sensors.gpu_info().map(|g| g.name.clone()),
        ram_total_mib: sensors.ram_total_mib(),
        cpu_temp_source: sensors.cpu_temp_source(),
        games: cfg.games.clone(),
    })));

    let mut auto = false;
    let mut sys = System::new();
    let mut recording: Option<Recording> = None;
    let started = Instant::now();
    let mut tick: u32 = 1;

    loop {
        // Absolute schedule, like the CLI: read time must not stretch the
        // interval. Live view always samples at 1s; recordings therefore
        // also run at 1s from the GUI.
        let mut target = started + Duration::from_secs(1) * tick;
        let now = Instant::now();
        while target <= now {
            tick += 1;
            target = started + Duration::from_secs(1) * tick;
        }
        std::thread::sleep(target - now);
        tick += 1;

        loop {
            match cmds.try_recv() {
                Ok(Cmd::Record) => {
                    start_recording(&mut recording, &sensors, &sessions_dir, None, &send)
                }
                Ok(Cmd::Stop) => stop_recording(&mut recording, &send),
                Ok(Cmd::SetAuto(enabled)) => auto = enabled,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return, // UI closed
            }
        }

        let sample = sensors.read_sample();
        if let Some(rec) = &mut recording {
            if let Err(e) = rec.writer.append(&sample) {
                send(Event::Error(format!("recording failed: {e:#}")));
                stop_recording(&mut recording, &send);
            } else {
                rec.samples += 1;
                let game_gone = rec.game.as_ref().is_some_and(|(pid, _)| {
                    sys.refresh_processes(ProcessesToUpdate::All, true);
                    sys.process(*pid).is_none()
                });
                if game_gone {
                    stop_recording(&mut recording, &send);
                }
            }
        } else if auto && !cfg.games.is_empty() {
            if let Some(found) = recorder::scan_for_game(&mut sys, &cfg.games) {
                start_recording(&mut recording, &sensors, &sessions_dir, Some(found), &send);
            }
        }
        send(Event::Tick(Box::new(sample)));
    }
}

fn start_recording(
    recording: &mut Option<Recording>,
    sensors: &Sensors,
    sessions_dir: &std::path::Path,
    game: Option<(Pid, String)>,
    send: &impl Fn(Event),
) {
    if recording.is_some() {
        return;
    }
    let game_name = game.as_ref().map(|(_, name)| name.clone());
    let meta = sensors.session_meta(1, game_name.clone());
    match SessionWriter::create(sessions_dir, &meta) {
        Ok(writer) => {
            *recording = Some(Recording {
                writer,
                samples: 0,
                game,
            });
            send(Event::RecordingStarted { game: game_name });
        }
        Err(e) => send(Event::Error(format!("could not start recording: {e:#}"))),
    }
}

fn stop_recording(recording: &mut Option<Recording>, send: &impl Fn(Event)) {
    if let Some(rec) = recording.take() {
        send(Event::RecordingStopped {
            path: rec.writer.path().to_path_buf(),
            samples: rec.samples,
        });
    }
}
