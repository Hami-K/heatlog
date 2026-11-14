//! Self-contained HTML report: a plain-language verdict and charts up front,
//! full numbers in a collapsible technical section. One offline file; the
//! only script is a small inline hover tooltip.

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

enum YAxis {
    /// 0 to 100.
    Pct,
    /// 0 to a padded data maximum.
    FromZero,
    /// Padded data range, extended so the shaded "hot" zone is visible.
    Temp { hot_from: f64 },
}

struct Chart<'a> {
    title: &'a str,
    unit: &'a str,
    explain: String,
    points: Vec<(f64, f64)>,
    bands: &'a [(f64, f64)],
    band_class: &'a str,
    y: YAxis,
}

pub fn render(session: &Session, summary: &Summary) -> Result<String> {
    let samples = &session.samples;
    let t0 = samples.first().map(|s| s.t_ms).unwrap_or(0);
    // Millisecond precision: rounding to whole seconds made jittered
    // neighbours share an x position, drawing vertical jumps.
    let xs: Vec<f64> = samples.iter().map(|s| secs_f64(s.t_ms - t0)).collect();
    let duration = summary.duration_secs as f64;

    let gpu_flags: Vec<bool> = samples
        .iter()
        .map(|s| s.gpu.as_ref().is_some_and(|g| !g.throttle.is_empty()))
        .collect();
    let gpu_bands = regions(&gpu_flags, &xs, summary.interval_secs as f64);
    let cpu_bands = cpu_throttle_flags(samples)
        .map(|(_, flags)| regions(&flags, &xs, summary.interval_secs as f64))
        .unwrap_or_default();

    let pick = |f: &dyn Fn(&crate::session::Sample) -> Option<f64>| -> Vec<(f64, f64)> {
        samples
            .iter()
            .zip(&xs)
            .filter_map(|(s, &x)| f(s).map(|v| (x, v)))
            .collect()
    };

    let gpu_shaded = "Red shading marks moments the GPU slowed itself down.";
    let cpu_shaded = "Amber shading marks estimated CPU slowdowns under load.";
    let explain = |base: &str, bands: &[(f64, f64)], shaded_note: &str| {
        if bands.is_empty() {
            base.to_string()
        } else {
            format!("{base} {shaded_note}")
        }
    };

    let charts = [
        Chart {
            title: "GPU temperature",
            unit: "C",
            explain: explain(
                "How hot the graphics chip is. Staying out of the shaded hot zone is good.",
                &gpu_bands,
                gpu_shaded,
            ),
            points: pick(&|s| s.gpu.as_ref().map(|g| f64::from(g.temp_c))),
            bands: &gpu_bands,
            band_class: "gpu",
            y: YAxis::Temp { hot_from: 85.0 },
        },
        Chart {
            title: "GPU speed",
            unit: "GHz",
            explain: explain(
                "How fast the GPU is running. A steady line under load is good; sharp dips can mean throttling.",
                &gpu_bands,
                gpu_shaded,
            ),
            points: pick(&|s| s.gpu.as_ref().map(|g| f64::from(g.core_mhz) / 1000.0)),
            bands: &gpu_bands,
            band_class: "gpu",
            y: YAxis::FromZero,
        },
        Chart {
            title: "GPU power",
            unit: "W",
            explain: explain(
                "Electricity the GPU draws. Sitting flat at the same ceiling usually means its power limit.",
                &gpu_bands,
                gpu_shaded,
            ),
            points: pick(&|s| s.gpu.as_ref().map(|g| g.power_w)),
            bands: &gpu_bands,
            band_class: "gpu",
            y: YAxis::FromZero,
        },
        Chart {
            title: "CPU usage",
            unit: "%",
            explain: explain(
                "How busy the processor is, from 0 to 100%.",
                &cpu_bands,
                cpu_shaded,
            ),
            points: pick(&|s| Some(f64::from(s.cpu.usage_pct))),
            bands: &cpu_bands,
            band_class: "cpu",
            y: YAxis::Pct,
        },
        Chart {
            title: "CPU speed",
            unit: "GHz",
            explain: explain(
                "Speed of the fastest processor core. Drops while usage is high suggest heat or power limits (estimated).",
                &cpu_bands,
                cpu_shaded,
            ),
            points: pick(&|s| {
                s.cpu
                    .per_core_mhz
                    .iter()
                    .copied()
                    .max()
                    .map(|m| m as f64 / 1000.0)
            }),
            bands: &cpu_bands,
            band_class: "cpu",
            y: YAxis::FromZero,
        },
        Chart {
            title: "CPU temperature",
            unit: "C",
            explain: explain(
                "Processor temperature. Staying out of the shaded hot zone is good.",
                &cpu_bands,
                cpu_shaded,
            ),
            points: pick(&|s| s.cpu_temp_c.map(f64::from)),
            bands: &cpu_bands,
            band_class: "cpu",
            y: YAxis::Temp { hot_from: 90.0 },
        },
    ];

    let mut charts_html = String::new();
    for chart in &charts {
        if chart.points.is_empty() {
            continue;
        }
        write!(
            charts_html,
            "<figure><figcaption>{} ({})</figcaption>{}<p class=\"note\">{}</p></figure>",
            chart.title,
            chart.unit,
            chart_svg(chart, duration),
            chart.explain,
        )?;
    }

    let (verdict_class, verdict_text) = verdict(summary);
    Ok(format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>heatlog {started}</title><style>{CSS}</style></head><body>\n\
         <h1>heatlog session</h1>\n\
         <p class=\"meta\">{meta}</p>\n\
         <div class=\"verdict {verdict_class}\">{verdict_text}</div>\n\
         <details><summary>What is throttling?</summary><p>When a chip gets too hot \
         or draws more power than its limit allows, it protects itself by slowing \
         down. That is throttling. A little is normal in a thin laptop; a lot means \
         worse performance and is worth investigating (dust, fan settings, or an \
         aggressive power plan). The GPU reports throttling directly; for the CPU, \
         Windows offers no such signal, so heatlog estimates it from clock drops \
         during heavy load.</p></details>\n\
         {headline}\n{charts_html}\n\
         <details><summary>Technical details</summary>{technical}</details>\n\
         {JS}</body></html>\n",
        started = summary.started.format("%Y-%m-%d %H:%M"),
        meta = meta_line(summary),
        headline = headline(summary),
        technical = technical(summary),
    ))
}

fn secs_f64(ms: i64) -> f64 {
    ms.max(0) as f64 / 1000.0
}

fn meta_line(s: &Summary) -> String {
    format!(
        "{}{} &middot; duration {} &middot; {} samples @ {}s",
        s.started.format("%Y-%m-%d %H:%M:%S"),
        s.game
            .as_deref()
            .map(|g| format!(" &middot; {g}"))
            .unwrap_or_default(),
        fmt_time(s.duration_secs),
        s.sample_count,
        s.interval_secs
    )
}

/// Which verdict colour the session earns. Thermal throttling is the red
/// case; power-limit throttling, estimated CPU drops, or merely running hot
/// are amber.
fn verdict_level(
    thermal_episodes: usize,
    other_episodes: usize,
    cpu_estimated: usize,
    gpu_peak_c: Option<f64>,
) -> &'static str {
    if thermal_episodes > 0 {
        "bad"
    } else if other_episodes > 0 || cpu_estimated > 0 || gpu_peak_c.is_some_and(|p| p >= 85.0) {
        "warn"
    } else {
        "good"
    }
}

fn verdict(s: &Summary) -> (&'static str, String) {
    let (mut thermal, mut other) = (0usize, 0usize);
    let mut thermal_at = None;
    if let Some(gpu) = &s.gpu {
        for t in &gpu.throttle {
            if t.reason.contains("thermal") || t.reason.contains("slowdown") {
                thermal += t.episodes;
                thermal_at = Some(t.longest_at_secs);
            } else {
                other += t.episodes;
            }
        }
    }
    let cpu_est = s.cpu.throttle.as_ref().map_or(0, |t| t.episodes);
    let gpu_peak = s.gpu.as_ref().map(|g| g.temp_c.peak);
    let class = verdict_level(thermal, other, cpu_est, gpu_peak);

    let mut lines = Vec::new();
    if thermal > 0 {
        let when = thermal_at
            .map(|t| {
                if s.duration_secs >= 120 {
                    format!(", mostly around minute {}", t / 60)
                } else {
                    format!(", around {}", fmt_time(t))
                }
            })
            .unwrap_or_default();
        lines.push(format!(
            "Your GPU slowed itself down {thermal} time(s) to avoid overheating{when}."
        ));
    }
    if other > 0 {
        lines.push(format!(
            "Your GPU hit its power limit {other} time(s) — common on gaming laptops."
        ));
    }
    if cpu_est > 0 {
        lines.push(format!(
            "Your CPU appears to have slowed down {cpu_est} time(s) under heavy load (estimated)."
        ));
    }
    if lines.is_empty() {
        lines.push(match gpu_peak {
            Some(p) if p >= 85.0 => {
                format!("Your GPU ran hot (peak {p:.0} C) but did not slow itself down.")
            }
            Some(_) => "Your laptop stayed cool — no throttling detected.".to_string(),
            None => {
                "No slowdowns detected. (No NVIDIA GPU was recorded in this session.)".to_string()
            }
        });
    }
    (class, lines.join(" "))
}

fn gb(mib: u64) -> f64 {
    // GiB displayed as "GB", matching how Windows itself labels memory.
    mib as f64 / 1024.0
}

fn headline(s: &Summary) -> String {
    let mut out = String::from("<ul class=\"facts\">");
    if let Some(gpu) = &s.gpu {
        let _ = write!(
            out,
            "<li>GPU peaked at {:.0} C and {:.1} GHz, drawing up to {:.0} W</li>",
            gpu.temp_c.peak,
            gpu.core_mhz.peak / 1000.0,
            gpu.power_w.peak
        );
    } else {
        out.push_str("<li>No NVIDIA GPU detected — CPU and memory only</li>");
    }
    let _ = write!(
        out,
        "<li>CPU peaked at {:.1} GHz and {:.0}% busy</li>",
        s.cpu.max_core_mhz.peak / 1000.0,
        s.cpu.usage_pct.peak
    );
    if let Some(temp) = &s.cpu_temp {
        let _ = write!(out, "<li>CPU temperature peaked at {:.0} C</li>", temp.peak);
    }
    match s.ram_total_mib {
        Some(total) if total > 0 => {
            let _ = write!(
                out,
                "<li>Memory: {:.1} of {:.0} GB used ({:.0}%)</li>",
                gb(s.ram_peak_mib),
                gb(total),
                100.0 * s.ram_peak_mib as f64 / total as f64
            );
        }
        _ => {
            let _ = write!(out, "<li>Memory: {:.1} GB used</li>", gb(s.ram_peak_mib));
        }
    }
    out.push_str("</ul>");
    out
}

fn technical(s: &Summary) -> String {
    let mut out = String::new();
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
        out.push_str("<h2>GPU</h2><p>No NVIDIA GPU detected</p>");
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
                "<p>Estimated clock drops under load: {} episode(s), {} total, \
                 longest at {}.<br><span class=\"note\">Fastest core below 75% of the \
                 {}-MHz loaded baseline while usage &gt; 80%. Windows exposes no direct \
                 CPU throttle signal, so this is an estimate.</span></p>",
                t.episodes,
                fmt_time(t.seconds),
                fmt_time(t.longest_at_secs),
                t.baseline_mhz
            );
        }
        Some(_) => out.push_str("<p>Estimated clock drops under load: none detected</p>"),
        None => out.push_str(
            "<p>Estimated clock drops under load: not enough high-load samples to judge</p>",
        ),
    }
    let _ = write!(out, "<p>RAM peak: {} MiB</p>", s.ram_peak_mib);
    out
}

fn chart_svg(c: &Chart, duration: f64) -> String {
    let points = downsample(&c.points, MAX_POINTS);
    let (mut data_lo, mut data_hi) = points
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &(_, v)| {
            (lo.min(v), hi.max(v))
        });
    if data_hi - data_lo < 1e-9 {
        data_lo -= 1.0;
        data_hi += 1.0;
    }
    let pad = (data_hi - data_lo) * 0.05;
    let (lo, hi, hot_from) = match c.y {
        YAxis::Pct => (0.0, 100.0, None),
        YAxis::FromZero => (0.0, data_hi + pad, None),
        YAxis::Temp { hot_from } => (
            (data_lo - pad).min(40.0),
            (data_hi + pad).max(hot_from + 10.0),
            Some(hot_from),
        ),
    };

    let plot_w = CHART_W - MARGIN_L - MARGIN_R;
    let plot_h = CHART_H - MARGIN_T - MARGIN_B;
    let x = |t: f64| MARGIN_L + plot_w * t / duration.max(1.0);
    let y = |v: f64| MARGIN_T + plot_h * (1.0 - (v - lo) / (hi - lo));

    let mut svg = format!(
        "<svg viewBox=\"0 0 {CHART_W} {CHART_H}\" role=\"img\" \
         data-lo=\"{lo}\" data-hi=\"{hi}\" data-dur=\"{duration}\" data-unit=\"{}\" \
         xmlns=\"http://www.w3.org/2000/svg\">",
        c.unit
    );

    if let Some(hot) = hot_from {
        let _ = write!(
            svg,
            "<rect class=\"band-hot\" x=\"{MARGIN_L}\" y=\"{MARGIN_T}\" width=\"{plot_w:.1}\" \
             height=\"{:.1}\"/><text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\">hot</text>",
            (y(hot) - MARGIN_T).max(0.0),
            CHART_W - MARGIN_R - 4.0,
            y(hot) - 4.0,
        );
    }
    for &(start, end) in c.bands {
        let _ = write!(
            svg,
            "<rect class=\"band-{}\" x=\"{:.1}\" y=\"{MARGIN_T}\" \
             width=\"{:.1}\" height=\"{plot_h:.1}\"/>",
            c.band_class,
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
    for tick in ticks(0.0, duration, 6) {
        let tx = x(tick);
        let _ = write!(
            svg,
            "<text x=\"{tx:.1}\" y=\"{:.1}\" text-anchor=\"middle\">{}</text>",
            CHART_H - 8.0,
            fmt_time(tick as u64),
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
fn downsample(points: &[(f64, f64)], max_points: usize) -> Vec<(f64, f64)> {
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
fn regions(flags: &[bool], xs: &[f64], interval: f64) -> Vec<(f64, f64)> {
    episodes(flags)
        .into_iter()
        .map(|(start, len)| (xs[start], xs[start + len - 1] + interval))
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

const CSS: &str = "\
:root{color-scheme:light dark;--bg:#ffffff;--ink:#1f2937;--muted:#6b7280;\
--grid:#e5e7eb;--line:#2563eb;--band-gpu:rgba(220,38,38,.16);--band-cpu:rgba(217,119,6,.16);\
--band-hot:rgba(220,38,38,.07);\
--good-bg:rgba(22,163,74,.12);--good-bd:#16a34a;--warn-bg:rgba(217,119,6,.12);\
--warn-bd:#b45309;--bad-bg:rgba(220,38,38,.12);--bad-bd:#dc2626}\
@media (prefers-color-scheme:dark){:root{--bg:#111827;--ink:#e5e7eb;--muted:#9ca3af;\
--grid:#374151;--line:#60a5fa;--band-gpu:rgba(248,113,113,.22);--band-cpu:rgba(251,191,36,.20);\
--band-hot:rgba(248,113,113,.10);\
--good-bg:rgba(74,222,128,.12);--good-bd:#4ade80;--warn-bg:rgba(251,191,36,.12);\
--warn-bd:#fbbf24;--bad-bg:rgba(248,113,113,.14);--bad-bd:#f87171}}\
body{background:var(--bg);color:var(--ink);font:14px/1.5 system-ui,sans-serif;\
max-width:920px;margin:0 auto;padding:16px}\
h1{font-size:20px}h2{font-size:15px;margin:18px 0 6px}\
table{border-collapse:collapse}td{padding:1px 14px 1px 0}\
td.num{text-align:right;font-variant-numeric:tabular-nums}\
.meta,.note,.muted{color:var(--muted)}.note{font-size:12px;margin:2px 0 0}\
.verdict{padding:12px 16px;border-radius:8px;border-left:4px solid;margin:12px 0;\
font-size:15px}\
.verdict.good{background:var(--good-bg);border-color:var(--good-bd)}\
.verdict.warn{background:var(--warn-bg);border-color:var(--warn-bd)}\
.verdict.bad{background:var(--bad-bg);border-color:var(--bad-bd)}\
.facts{margin:10px 0;padding-left:20px}\
details{margin:10px 0}summary{cursor:pointer;color:var(--muted)}\
details[open] summary{margin-bottom:6px}\
figure{margin:20px 0 0}figcaption{font-weight:600;margin-bottom:4px}\
svg{width:100%;height:auto;display:block}\
svg text{fill:var(--muted);font-size:11px}\
.grid{stroke:var(--grid);stroke-width:1}\
.series{stroke:var(--line);stroke-width:2;fill:none;stroke-linejoin:round}\
.band-gpu{fill:var(--band-gpu)}.band-cpu{fill:var(--band-cpu)}.band-hot{fill:var(--band-hot)}\
#tip{position:fixed;background:var(--bg);border:1px solid var(--grid);\
padding:2px 8px;border-radius:4px;font-size:12px;pointer-events:none;\
font-variant-numeric:tabular-nums}\
ul{margin:4px 0;padding-left:20px}";

/// Hover tooltip (time + value at the nearest point). Inline and
/// dependency-free, so the file still opens offline.
const JS: &str = "<script>(function(){\
var ML=56,MR=12,MT=10,MB=26,W=860,H=220;\
var tip=document.createElement('div');tip.id='tip';tip.hidden=true;\
document.body.appendChild(tip);\
function fmt(t){t=Math.round(t);var h=Math.floor(t/3600),m=Math.floor(t%3600/60),s=t%60;\
function p(n){return String(n).padStart(2,'0')}\
return h?h+':'+p(m)+':'+p(s):m+':'+p(s)}\
document.querySelectorAll('svg[data-dur]').forEach(function(svg){\
var lo=+svg.dataset.lo,hi=+svg.dataset.hi,dur=+svg.dataset.dur,unit=svg.dataset.unit;\
var pl=svg.querySelector('polyline.series');if(!pl)return;\
var pts=pl.getAttribute('points').trim().split(/\\s+/).map(function(p){\
var a=p.split(',');return[+a[0],+a[1]]});\
svg.addEventListener('mousemove',function(e){\
var r=svg.getBoundingClientRect();\
var fx=(e.clientX-r.left)/r.width*W;\
var best=null,bd=1e9;\
for(var i=0;i<pts.length;i++){var d=Math.abs(pts[i][0]-fx);if(d<bd){bd=d;best=pts[i]}}\
if(!best||bd>40){tip.hidden=true;return}\
var t=(best[0]-ML)/(W-ML-MR)*dur;\
var v=hi-(best[1]-MT)/(H-MT-MB)*(hi-lo);\
tip.textContent=fmt(t)+'  '+(Math.abs(v)>=100?v.toFixed(0):v.toFixed(1))+' '+unit;\
tip.style.left=(e.clientX+14)+'px';tip.style.top=(e.clientY+14)+'px';tip.hidden=false});\
svg.addEventListener('mouseleave',function(){tip.hidden=true})})\
})()</script>";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x_positions_keep_millisecond_precision() {
        // Regression: truncating to whole seconds put jittered neighbours
        // (1.02s, 1.98s) on the same x, drawing vertical jumps.
        let t_ms = [0i64, 1020, 1980, 3000, 4010];
        let xs: Vec<f64> = t_ms.iter().map(|&ms| secs_f64(ms)).collect();
        assert!(
            xs.windows(2).all(|w| w[0] < w[1]),
            "strictly increasing: {xs:?}"
        );
        assert_eq!(xs[1], 1.02);
    }

    #[test]
    fn downsample_keeps_extremes_and_shrinks() {
        let points: Vec<(f64, f64)> = (0..10_000)
            .map(|i| (i as f64, (i as f64 / 50.0).sin() * 40.0))
            .map(|(i, v)| (i, if i == 7777.0 { 99.0 } else { v }))
            .collect();
        let out = downsample(&points, 1200);
        assert!(out.len() <= 1200);
        assert!(out.iter().any(|&(t, v)| t == 7777.0 && v == 99.0));
        assert!(
            out.windows(2).all(|w| w[0].0 <= w[1].0),
            "stays in time order"
        );
    }

    #[test]
    fn downsample_leaves_short_series_alone() {
        let points: Vec<(f64, f64)> = (0..100).map(|i| (i as f64, i as f64)).collect();
        assert_eq!(downsample(&points, 1200), points);
    }

    #[test]
    fn regions_merge_consecutive_flags() {
        let flags = [false, true, true, false, false, true];
        let xs = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(regions(&flags, &xs, 1.0), vec![(1.0, 3.0), (5.0, 6.0)]);
    }

    #[test]
    fn ticks_are_round_and_cover_range() {
        let t = ticks(13.0, 87.0, 4);
        assert_eq!(t, vec![20.0, 40.0, 60.0, 80.0]);
        assert!(ticks(0.0, 1.0, 4).len() >= 3);
    }

    #[test]
    fn verdict_levels() {
        assert_eq!(verdict_level(1, 0, 0, Some(70.0)), "bad");
        assert_eq!(verdict_level(0, 2, 0, Some(70.0)), "warn");
        assert_eq!(verdict_level(0, 0, 1, None), "warn");
        assert_eq!(verdict_level(0, 0, 0, Some(86.0)), "warn");
        assert_eq!(verdict_level(0, 0, 0, Some(70.0)), "good");
        assert_eq!(verdict_level(0, 0, 0, None), "good");
    }
}
