use std::time::{Duration, Instant};

use anyhow::Result;
use heatlog::session::Sample;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Block, Paragraph, Sparkline};
use ratatui::{DefaultTerminal, Frame};

pub struct Tui {
    terminal: DefaultTerminal,
    gpu_temp: Vec<u64>,
    gpu_util: Vec<u64>,
    cpu_usage: Vec<u64>,
    cpu_mhz: Vec<u64>,
}

impl Tui {
    pub fn start() -> Self {
        Self {
            terminal: ratatui::init(),
            gpu_temp: Vec::new(),
            gpu_util: Vec::new(),
            cpu_usage: Vec::new(),
            cpu_mhz: Vec::new(),
        }
    }

    /// Waits until `deadline`, returning true if the user asked to stop
    /// (q, Esc, or Ctrl-C, which raw mode delivers as a key event).
    pub fn wait(&mut self, deadline: Instant) -> Result<bool> {
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            if event::poll(deadline - now)? {
                if let Event::Key(key) = event::read()? {
                    let ctrl_c = key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL);
                    if key.kind == KeyEventKind::Press
                        && (key.code == KeyCode::Char('q') || key.code == KeyCode::Esc || ctrl_c)
                    {
                        return Ok(true);
                    }
                }
            }
        }
    }

    pub fn draw(
        &mut self,
        sample: &Sample,
        count: u64,
        elapsed: Duration,
        file: &str,
    ) -> Result<()> {
        if let Some(gpu) = &sample.gpu {
            push_capped(&mut self.gpu_temp, u64::from(gpu.temp_c));
            push_capped(&mut self.gpu_util, u64::from(gpu.util_pct));
        }
        push_capped(&mut self.cpu_usage, sample.cpu.usage_pct as u64);
        push_capped(
            &mut self.cpu_mhz,
            sample.cpu.per_core_mhz.iter().copied().max().unwrap_or(0),
        );

        let Self {
            terminal,
            gpu_temp,
            gpu_util,
            cpu_usage,
            cpu_mhz,
        } = self;
        terminal.draw(|frame| {
            let [header, gpu_row, cpu_row, footer] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Fill(1),
                Constraint::Fill(1),
                Constraint::Length(1),
            ])
            .areas(frame.area());

            let secs = elapsed.as_secs();
            frame.render_widget(
                Paragraph::new(format!(
                    "heatlog  {file}  {}:{:02}:{:02}  {count} samples  (q to stop)",
                    secs / 3600,
                    secs % 3600 / 60,
                    secs % 60
                )),
                header,
            );

            match &sample.gpu {
                Some(gpu) => {
                    let throttle = if gpu.throttle.is_empty() {
                        String::new()
                    } else {
                        format!("  THROTTLE: {}", gpu.throttle.join(", "))
                    };
                    let [left, right] =
                        Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)])
                            .areas(gpu_row);
                    spark(
                        frame,
                        left,
                        format!(
                            "GPU {} °C  {} MHz  {:.0} W{throttle}",
                            gpu.temp_c, gpu.core_mhz, gpu.power_w
                        ),
                        gpu_temp,
                        None,
                    );
                    spark(
                        frame,
                        right,
                        format!("GPU util {}%  VRAM {} MiB", gpu.util_pct, gpu.vram_used_mib),
                        gpu_util,
                        Some(100),
                    );
                }
                None => frame.render_widget(
                    Paragraph::new("GPU unavailable").block(Block::bordered()),
                    gpu_row,
                ),
            }

            let [left, right] =
                Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).areas(cpu_row);
            spark(
                frame,
                left,
                format!("CPU {:.0}%", sample.cpu.usage_pct),
                cpu_usage,
                Some(100),
            );
            spark(
                frame,
                right,
                format!(
                    "CPU fastest core {} MHz",
                    cpu_mhz.last().copied().unwrap_or(0)
                ),
                cpu_mhz,
                None,
            );

            let cpu_temp = match sample.cpu_temp_c {
                Some(t) => format!("CPU temp {t:.0} °C"),
                None => "CPU temp n/a".to_string(),
            };
            frame.render_widget(
                Paragraph::new(format!("{cpu_temp}  RAM {} MiB", sample.ram_used_mib)),
                footer,
            );
        })?;
        Ok(())
    }
}

/// Sparklines only show the newest points that fit on screen, so history
/// beyond any realistic terminal width is dead weight.
fn push_capped(history: &mut Vec<u64>, value: u64) {
    history.push(value);
    if history.len() > 1024 {
        let excess = history.len() - 1024;
        history.drain(..excess);
    }
}

fn spark(frame: &mut Frame, area: Rect, title: String, data: &[u64], max: Option<u64>) {
    // Show only the newest points that fit inside the borders.
    let width = area.width.saturating_sub(2) as usize;
    let tail = &data[data.len().saturating_sub(width)..];
    let mut widget = Sparkline::default()
        .block(Block::bordered().title(title))
        .data(tail);
    if let Some(max) = max {
        widget = widget.max(max);
    }
    frame.render_widget(widget, area);
}

impl Drop for Tui {
    /// Restores the terminal even on an error path mid-recording.
    fn drop(&mut self) {
        ratatui::restore();
    }
}
