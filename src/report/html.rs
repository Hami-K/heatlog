//! Self-contained HTML report: summary numbers plus inline-SVG time charts.
//! No scripts, no external resources, so the file opens anywhere.

use std::fmt::Write as _;

use anyhow::Result;

use super::{cpu_throttle_flags, episodes, fmt_time, Series, Summary};
use crate::session::Session;

/// More SVG points than this per chart just bloats the file, so longer
/// sessions are decimated (keeping each bucket's min and max, which
/// preserves spikes).
const MAX_POINTS: usize = 1200;

const CHART_W: f64 = 860.0;
const CHART_H: f64 = 220.0;
const MARGIN_L: f64 = 56.0;
const MARGIN_R: f64 = 12.0;
const MARGIN_T: f64 = 10.0;
const MARGIN_B: f64 = 26.0;

pub fn render(session: &Session, summary: &Summary) -> Result<String> {
    let samples = &session.samples;
    let t0 = samples.first().map(|s| s.t_ms).unwrap_or(0);
    let offsets: Vec<u64> = samples
        .iter()
        .map(|s| (s.t_ms - t0).max(0) as u64 / 1000)
        .collect();
    let duration = summary.duration_secs;

    let gpu_regions = regions(
        &samples
            .iter()
            .map(|s| s.gpu.as_ref().is_some_and(|g| !g.throttle.is_empty()))
            .collect::<Vec<_>>(),
        &offsets,
        summary.interval_secs,
    );
    let cpu_regions = cpu_throttle_flags(samples)
        .map(|(_, flags)| regions(&flags, &offsets, summary.interval_secs))
        .unwrap_or_default();

    let pick = |f: &dyn Fn(&crate::session::Sample) -> Option<f64>| -> Vec<(u64, f64)> {
        samples
            .iter()
            .zip(&offsets)
            .filter_map(|(s, &t)| f(s).map(|v| (t, v)))
            .collect()
    };

    let mut charts = String::new();
    let gpu_note = "Shaded: GPU throttling (reported by the driver).";
    let cpu_note = "Shaded: estimated CPU clock drop under load. Heuristic, not a driver signal.";
    for (title, unit, points, bands, class, note) in [
        (
            "GPU temperature",
            "C",
            pick(&|s| s.gpu.as_ref().map(|g| f64::from(g.temp_c))),
            &gpu_regions,
            "gpu",
            gpu_note,
        ),
        (
            "GPU core clock",
            "MHz",
            pick(&|s| s.gpu.as_ref().map(|g| f64::from(g.core_mhz))),
            &gpu_regions,
            "gpu",
            gpu_note,
        ),
        (
            "GPU power draw",
            "W",
            pick(&|s| s.gpu.as_ref().map(|g| g.power_w)),
            &gpu_regions,
            "gpu",
            gpu_note,
        ),
        (
            "CPU usage",
            "%",
            pick(&|s| Some(f64::from(s.cpu.usage_pct))),
            &cpu_regions,
            "cpu",
            cpu_note,
        ),
        (
            "CPU fastest-core clock",
            "MHz",
            pick(&|s| s.cpu.per_core_mhz.iter().copied().max().map(|m| m as f64)),
            &cpu_regions,
            "cpu",
            cpu_note,
        ),
        (
            "CPU temperature",
            "C",
            pick(&|s| s.cpu_temp_c.map(f64::from)),
            &cpu_regions,
            "cpu",
            cpu_note,
        ),
    ] {
        if points.is_empty() {
            continue;
        }
        let shaded = !bands.is_empty();
        write!(
            charts,
            "<figure><figcaption>{title} ({unit})</figcaption>{}{}</figure>",
            chart_svg(&points, duration, bands, class),
            if shaded {
                format!("<p class=\"note\">{note}</p>")
            } else {
                String::new()
            }
        )?;
    }

    Ok(format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>heatlog {started}</title><style>{CSS}</style></head><body>\n\
         <h1>heatlog session</h1>\n{summary_html}\n{charts}\n</body></html>\n",
        started = summary.started.format("%Y-%m-%d %H:%M"),
        summary_html = summary_html(summary),
    ))
}

const CSS: &str = "\
:root{color-scheme:light dark;--bg:#ffffff;--ink:#1f2937;--muted:#6b7280;\
--grid:#e5e7eb;--line:#2563eb;--band-gpu:rgba(220,38,38,.16);--band-cpu:rgba(217,119,6,.16)}\
@media (prefers-color-scheme:dark){:root{--bg:#111827;--ink:#e5e7eb;--muted:#9ca3af;\
--grid:#374151;--line:#60a5fa;--band-gpu:rgba(248,113,113,.22);--band-cpu:rgba(251,191,36,.20)}}\
body{background:var(--bg);color:var(--ink);font:14px/1.5 system-ui,sans-serif;\
max-width:920px;margin:0 auto;padding:16px}\
h1{font-size:20px}h2{font-size:15px;margin:18px 0 6px}\
table{border-collapse:collapse}td{padding:1px 14px 1px 0}\
td.num{text-align:right;font-variant-numeric:tabular-nums}\
.meta,.note,.muted{color:var(--muted)}.note{font-size:12px;margin:2px 0 0}\
figure{margin:20px 0 0}figcaption{font-weight:600;margin-bottom:4px}\
svg{width:100%;height:auto;display:block}\
svg text{fill:var(--muted);font-size:11px}\
.grid{stroke:var(--grid);stroke-width:1}\
.series{stroke:var(--line);stroke-width:2;fill:none;stroke-linejoin:round}\
.band-gpu{fill:var(--band-gpu)}.band-cpu{fill:var(--band-cpu)}\
ul{margin:4px 0;padding-left:20px}";

fn summary_html(s: &Summary) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "<p class=\"meta\">{}{} &middot; duration {} &middot; {} samples @ {}s \
         &middot; RAM peak {} MiB</p>",
        s.started.format("%Y-%m-%d %H:%M:%S"),
        s.game
            .as_deref()
            .map(|g| format!(" &middot; {g}"))
            .unwrap_or_default(),
        fmt_time(s.duration_secs),
        s.sample_count,
        s.interval_secs,
        s.ram_peak_mib
    );

    let row = |label: &str, series: &Series, unit: &str| {
        format!(
            "<tr><td>{label}</td><td class=\"num\">avg {:.0} {unit}</td>\
             <td class=\"num\">peak {:.0} {unit}</td><td class=\"muted\">at {}</td></tr>",
            series.avg,
            series.peak,
            fmt_time(series.peak_at_secs)
        )
    };

    if let Some(gpu) = &s.gpu {
        let _ = write!(
            out,
            "<h2>GPU &mdash; {}</h2><table>{}{}{}{}{}</table>",
            s.gpu_name.as_deref().unwrap_or("unknown"),
            row("Temperature", &gpu.temp_c, "C"),
            row("Core clock", &gpu.core_mhz, "MHz"),
            row("Utilisation", &gpu.util_pct, "%"),
            row("Power draw", &gpu.power_w, "W"),
            row("VRAM used", &gpu.vram_used_mib, "MiB"),
        );
        if gpu.throttle.is_empty() {
            out.push_str("<p>Throttling: none</p>");
        } else {
            out.push_str("<p>Throttling</p><ul>");
            for t in &gpu.throttle {
                let _ = write!(
                    out,
                    "<li>{}: {} episode(s), {} total, longest at {}</li>",
                    t.reason,
                    t.episodes,
                    fmt_time(t.seconds),
                    fmt_time(t.longest_at_secs)
                );
            }
            out.push_str("</ul>");
        }
    } else {
        out.push_str("<h2>GPU</h2><p>not recorded</p>");
    }

    let _ = write!(
        out,
        "<h2>CPU &mdash; {}</h2><table>{}{}",
        s.cpu_name,
        row("Usage", &s.cpu.usage_pct, "%"),
        row("Fastest core", &s.cpu.max_core_mhz, "MHz"),
    );
    if let Some(temp) = &s.cpu_temp {
        let _ = write!(
            out,
            "{}",
            row(
                &format!(
                    "Temperature ({})",
                    s.cpu_temp_source.as_deref().unwrap_or("unknown source")
                ),
                temp,
                "C"
            )
        );
    }
    out.push_str("</table>");
    match &s.cpu.throttle {
        Some(t) if t.episodes > 0 => {
            let _ = write!(
                out,
                "<p>Clock drops under load (heuristic): {} episode(s), {} total, \
                 longest at {}.<br><span class=\"note\">Fastest core below 75% of the \
                 {}-MHz loaded baseline while usage &gt; 80%. Windows exposes no direct \
                 CPU throttle signal, so treat this as an estimate.</span></p>",
                t.episodes,
                fmt_time(t.seconds),
                fmt_time(t.longest_at_secs),
                t.baseline_mhz
            );
        }
        Some(_) => out.push_str("<p>Clock drops under load (heuristic): none detected</p>"),
        None => {
            out.push_str("<p>Clock drops under load (heuristic): not enough high-load samples</p>")
        }
    }
    out
}

fn chart_svg(
    points: &[(u64, f64)],
    duration: u64,
    bands: &[(u64, u64)],
    band_class: &str,
) -> String {
    let points = downsample(points, MAX_POINTS);
    let (mut lo, mut hi) = points
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &(_, v)| {
            (lo.min(v), hi.max(v))
        });
    if hi - lo < 1e-9 {
        lo -= 1.0;
        hi += 1.0;
    }
    let pad = (hi - lo) * 0.05;
    let (lo, hi) = (lo - pad, hi + pad);

    let plot_w = CHART_W - MARGIN_L - MARGIN_R;
    let plot_h = CHART_H - MARGIN_T - MARGIN_B;
    let x = |t: u64| MARGIN_L + plot_w * t as f64 / duration.max(1) as f64;
    let y = |v: f64| MARGIN_T + plot_h * (1.0 - (v - lo) / (hi - lo));

    let mut svg = format!(
        "<svg viewBox=\"0 0 {CHART_W} {CHART_H}\" role=\"img\" \
         xmlns=\"http://www.w3.org/2000/svg\">"
    );

    for &(start, end) in bands {
        let _ = write!(
            svg,
            "<rect class=\"band-{band_class}\" x=\"{:.1}\" y=\"{MARGIN_T}\" \
             width=\"{:.1}\" height=\"{plot_h:.1}\"/>",
            x(start),
            (x(end) - x(start)).max(1.0),
        );
    }

    for tick in ticks(lo, hi, 4) {
        let ty = y(tick);
        let _ = write!(
            svg,
            "<line class=\"grid\" x1=\"{MARGIN_L}\" y1=\"{ty:.1}\" \
             x2=\"{:.1}\" y2=\"{ty:.1}\"/>\
             <text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\">{}</text>",
            CHART_W - MARGIN_R,
            MARGIN_L - 6.0,
            ty + 4.0,
            format_tick(tick),
        );
    }
    for tick in ticks(0.0, duration as f64, 6) {
        let tick = tick as u64;
        let tx = x(tick);
        let _ = write!(
            svg,
            "<text x=\"{tx:.1}\" y=\"{:.1}\" text-anchor=\"middle\">{}</text>",
            CHART_H - 8.0,
            fmt_time(tick),
        );
    }

    let mut path = String::with_capacity(points.len() * 12);
    for (t, v) in &points {
        let _ = write!(path, "{:.1},{:.1} ", x(*t), y(*v));
    }
    let _ = write!(
        svg,
        "<polyline class=\"series\" points=\"{}\"/></svg>",
        path.trim_end()
    );
    svg
}

fn format_tick(v: f64) -> String {
    if v.abs() >= 100.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

/// Min/max decimation: each bucket contributes its lowest and highest value
/// in time order, so peaks survive however long the session is.
fn downsample(points: &[(u64, f64)], max_points: usize) -> Vec<(u64, f64)> {
    let buckets = max_points / 2;
    if points.len() <= max_points || buckets == 0 {
        return points.to_vec();
    }
    let mut out = Vec::with_capacity(buckets * 2);
    for chunk in points.chunks(points.len().div_ceil(buckets)) {
        let mut min = chunk[0];
        let mut max = chunk[0];
        for &p in chunk {
            if p.1 < min.1 {
                min = p;
            }
            if p.1 > max.1 {
                max = p;
            }
        }
        if min.0 <= max.0 {
            out.push(min);
            if max.0 != min.0 {
                out.push(max);
            }
        } else {
            out.push(max);
            out.push(min);
        }
    }
    out
}

/// Flagged samples as (start, end) second offsets, consecutive runs merged.
fn regions(flags: &[bool], offsets: &[u64], interval: u64) -> Vec<(u64, u64)> {
    episodes(flags)
        .into_iter()
        .map(|(start, len)| (offsets[start], offsets[start + len - 1] + interval))
        .collect()
}

/// Round-value axis ticks covering [lo, hi] with roughly `target` steps.
fn ticks(lo: f64, hi: f64, target: usize) -> Vec<f64> {
    let raw = (hi - lo) / target.max(1) as f64;
    let magnitude = 10f64.powf(raw.log10().floor());
    let normalized = raw / magnitude;
    let step = magnitude
        * if normalized <= 1.0 {
            1.0
        } else if normalized <= 2.0 {
            2.0
        } else if normalized <= 5.0 {
            5.0
        } else {
            10.0
        };
    let mut v = (lo / step).ceil() * step;
    let mut out = Vec::new();
    while v <= hi {
        out.push(v);
        v += step;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsample_keeps_extremes_and_shrinks() {
        let points: Vec<(u64, f64)> = (0..10_000)
            .map(|i| (i, (i as f64 / 50.0).sin() * 40.0))
            .map(|(i, v)| (i, if i == 7777 { 99.0 } else { v }))
            .collect();
        let out = downsample(&points, 1200);
        assert!(out.len() <= 1200);
        assert!(out.iter().any(|&(t, v)| t == 7777 && v == 99.0));
        assert!(
            out.windows(2).all(|w| w[0].0 <= w[1].0),
            "stays in time order"
        );
    }

    #[test]
    fn downsample_leaves_short_series_alone() {
        let points: Vec<(u64, f64)> = (0..100).map(|i| (i, i as f64)).collect();
        assert_eq!(downsample(&points, 1200), points);
    }

    #[test]
    fn regions_merge_consecutive_flags() {
        let flags = [false, true, true, false, false, true];
        let offsets = [0, 1, 2, 3, 4, 5];
        assert_eq!(regions(&flags, &offsets, 1), vec![(1, 3), (5, 6)]);
    }

    #[test]
    fn ticks_are_round_and_cover_range() {
        let t = ticks(13.0, 87.0, 4);
        assert_eq!(t, vec![20.0, 40.0, 60.0, 80.0]);
        assert!(ticks(0.0, 1.0, 4).len() >= 3);
    }
}
