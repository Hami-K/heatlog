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
use heatlog::sensors::cpu_temp::{self, Availability};
use heatlog::sensors::gpu::GpuInfo;
use heatlog::session::{Sample, SessionWriter};

pub enum Cmd {
    Record,
    Stop,
    SetAuto(bool),
    /// Re-run temperature-source detection (the setup page's Check again).
    Reprobe,
    Settings {
        games: Vec<String>,
        interval_secs: u64,
    },
}

pub struct Info {
    pub cpu_name: String,
    /// The main GPU and, on a machine that has one, the second GPU beside
    /// it. Kept whole rather than reduced to a name: the page has to say
    /// where the numbers come from and what this GPU cannot report.
    pub gpu: Option<GpuInfo>,
    pub gpu2: Option<GpuInfo>,
    pub ram_total_mib: u64,
    pub cpu_temp_source: Option<&'static str>,
    pub games: Vec<String>,
    pub interval_secs: u64,
    pub avail: Availability,
}

pub enum Event {
    Ready(Box<Info>),
    Tick(Box<Sample>),
    RecordingStarted {
        game: Option<String>,
    },
    RecordingStopped {
        path: PathBuf,
        samples: u64,
    },
    /// Stopped so soon that nothing was worth saving; the file is gone.
    RecordingDiscarded,
    Error(String),
    Hint(String),
}

pub fn spawn(ctx: egui::Context) -> (Sender<Cmd>, Receiver<Event>) {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    std::thread::spawn(move || run(ctx, cmd_rx, event_tx));
    (cmd_tx, event_rx)
}

/// A recording this short is a mis-click or a crash-landed game launch,
/// not a session worth keeping: two samples is the minimum any chart or
/// verdict can be built from.
const MIN_SAMPLES: u64 = 2;
const MIN_DURATION: Duration = Duration::from_secs(10);

fn too_short(samples: u64, elapsed: Duration) -> bool {
    samples < MIN_SAMPLES || elapsed < MIN_DURATION
}

struct Recording {
    writer: SessionWriter,
    started: Instant,
    samples: u64,
    game: Option<(Pid, String)>,
    /// Ticks since the last appended sample: the live view always samples
    /// every second, but the session keeps the configured interval.
    since_append: u64,
}

fn run(ctx: egui::Context, cmds: Receiver<Cmd>, events: Sender<Event>) {
    let send = |event: Event| {
        let _ = events.send(event);
        ctx.request_repaint();
    };

    let mut cfg = match config::load_or_create() {
        Ok(cfg) => cfg,
        Err(e) => {
            send(Event::Error(format!("config: {e:#}")));
            Config::default()
        }
    };
    let sessions_dir = cfg
        .sessions_dir()
        .unwrap_or_else(|_| PathBuf::from("sessions"));

    let mut sensors = Sensors::init(&cfg.lhm_url, cfg.record_processes);
    let info = |sensors: &Sensors, cfg: &Config| {
        Event::Ready(Box::new(Info {
            cpu_name: sensors.cpu_name().to_string(),
            gpu: sensors.gpu_info().cloned(),
            gpu2: sensors.gpu2_info().cloned(),
            ram_total_mib: sensors.ram_total_mib(),
            cpu_temp_source: sensors.cpu_temp_source(),
            games: cfg.games.clone(),
            interval_secs: cfg.interval_secs.max(1),
            avail: cpu_temp::availability(&cfg.lhm_url),
        }))
    };
    send(info(&sensors, &cfg));

    let mut auto = false;
    let mut sys = System::new();
    let mut recording: Option<Recording> = None;
    let mut temp_worked = false;
    let mut hinted = false;
    let started = Instant::now();
    let mut tick: u32 = 1;

    loop {
        // Absolute schedule, like the CLI: read time must not stretch the
        // interval.
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
                Ok(Cmd::Record) => start_recording(
                    &mut recording,
                    &sensors,
                    &sessions_dir,
                    cfg.interval_secs.max(1),
                    None,
                    &send,
                ),
                Ok(Cmd::Stop) => stop_recording(&mut recording, &send),
                Ok(Cmd::SetAuto(enabled)) => auto = enabled,
                Ok(Cmd::Reprobe) => {
                    sensors.reprobe_temp(&cfg.lhm_url);
                    temp_worked = false;
                    hinted = false;
                    send(info(&sensors, &cfg));
                }
                Ok(Cmd::Settings {
                    games,
                    interval_secs,
                }) => {
                    cfg.games = games;
                    cfg.interval_secs = interval_secs;
                    send(info(&sensors, &cfg));
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return, // UI closed
            }
        }

        // Someone starting HWiNFO or LibreHardwareMonitor should not have
        // to come back and press a button: look again now and then until a
        // source turns up.
        if sensors.cpu_temp_source().is_none() && tick.is_multiple_of(15) {
            sensors.reprobe_temp(&cfg.lhm_url);
            if sensors.cpu_temp_source().is_some() {
                send(info(&sensors, &cfg));
            }
        }

        let sample = sensors.read_sample();
        match sample.cpu_temp_c {
            Some(_) => temp_worked = true,
            None if temp_worked && !hinted => {
                hinted = true;
                // HWiNFO's free version switches shared memory off after
                // 12 hours of use, which otherwise just looks broken.
                send(Event::Hint(match sensors.cpu_temp_source() {
                    Some("HWiNFO") => "HWiNFO stopped sharing sensors. Re-enable Shared Memory \
                         Support in HWiNFO's settings (the free version turns it off after 12 \
                         hours), then use Check again on the Settings page."
                        .to_string(),
                    _ => "The CPU temperature source stopped responding.".to_string(),
                }));
            }
            None => {}
        }

        if let Some(rec) = &mut recording {
            rec.since_append += 1;
            let due = rec.since_append >= cfg.interval_secs.max(1);
            if due {
                rec.since_append = 0;
                if let Err(e) = rec.writer.append(&sample) {
                    send(Event::Error(format!("recording failed: {e:#}")));
                    stop_recording(&mut recording, &send);
                } else {
                    rec.samples += 1;
                }
            }
            if let Some(rec) = &recording {
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
                start_recording(
                    &mut recording,
                    &sensors,
                    &sessions_dir,
                    cfg.interval_secs.max(1),
                    Some(found),
                    &send,
                );
            }
        }
        send(Event::Tick(Box::new(sample)));
    }
}

fn start_recording(
    recording: &mut Option<Recording>,
    sensors: &Sensors,
    sessions_dir: &std::path::Path,
    interval_secs: u64,
    game: Option<(Pid, String)>,
    send: &impl Fn(Event),
) {
    if recording.is_some() {
        return;
    }
    let game_name = game.as_ref().map(|(_, name)| name.clone());
    let meta = sensors.session_meta(interval_secs, game_name.clone());
    match SessionWriter::create(sessions_dir, &meta) {
        Ok(writer) => {
            *recording = Some(Recording {
                writer,
                started: Instant::now(),
                samples: 0,
                game,
                since_append: interval_secs.saturating_sub(1), // record promptly
            });
            send(Event::RecordingStarted { game: game_name });
        }
        Err(e) => send(Event::Error(format!("could not start recording: {e:#}"))),
    }
}

fn stop_recording(recording: &mut Option<Recording>, send: &impl Fn(Event)) {
    let Some(rec) = recording.take() else {
        return;
    };
    let path = rec.writer.path().to_path_buf();
    let (samples, elapsed) = (rec.samples, rec.started.elapsed());
    // Dropping the writer first closes the file, so it can be deleted.
    drop(rec);
    if too_short(samples, elapsed) {
        // Removed outright, not to the recycle bin: heatlog made this
        // file seconds ago and never showed it to anyone.
        let _ = std::fs::remove_file(&path);
        send(Event::RecordingDiscarded);
        return;
    }
    send(Event::RecordingStopped { path, samples });
}

#[cfg(test)]
mod tests {
    use super::{too_short, Duration};

    #[test]
    fn only_a_real_session_is_kept() {
        // A mis-click: stopped after a second or two.
        assert!(too_short(1, Duration::from_secs(1)));
        assert!(too_short(2, Duration::from_secs(4)));
        // Long enough, but the recording interval was slower than it.
        assert!(too_short(1, Duration::from_secs(30)));
        // The shortest session worth keeping.
        assert!(!too_short(2, Duration::from_secs(10)));
        assert!(!too_short(120, Duration::from_secs(120)));
    }
}
