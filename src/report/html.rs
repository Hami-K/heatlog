//! Self-contained HTML report: a plain-language verdict and charts up front,
//! full numbers in a collapsible technical section. One offline file; the
//! only script is a small inline hover tooltip.

use std::fmt::Write as _;

use anyhow::Result;

use super::charts::{self, ChartData, MAX_POINTS};
use super::{fmt_time, verdict, Series, Summary};
use crate::session::Session;

const CHART_W: f64 = 860.0;
const CHART_H: f64 = 220.0;
const MARGIN_L: f64 = 56.0;
const MARGIN_R: f64 = 12.0;
const MARGIN_T: f64 = 10.0;
const MARGIN_B: f64 = 26.0;

pub fn render(session: &Session, summary: &Summary) -> Result<String> {
    let duration = summary.duration_secs as f64;
    let mut charts_html = String::new();
    for chart in charts::build(session, summary) {
        write!(
            charts_html,
            "<figure><figcaption>{} ({})</figcaption>{}<p class=\"note\">{}</p></figure>",
            chart.title,
            chart.unit,
            chart_svg(&chart, duration),
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

fn chart_svg(c: &ChartData, duration: f64) -> String {
    let points = charts::downsample(&c.points, MAX_POINTS);
    let (lo, hi, hot_from) = c.y_domain();

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
    for &(start, end) in &c.bands {
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
--warn-bd:#b45309;--bad-bg:rgba(220,38,38,.12);--bad-bd:#dc2626;\
--na-bg:rgba(107,114,128,.10)}\
@media (prefers-color-scheme:dark){:root{--bg:#111827;--ink:#e5e7eb;--muted:#9ca3af;\
--grid:#374151;--line:#60a5fa;--band-gpu:rgba(248,113,113,.22);--band-cpu:rgba(251,191,36,.20);\
--band-hot:rgba(248,113,113,.10);\
--good-bg:rgba(74,222,128,.12);--good-bd:#4ade80;--warn-bg:rgba(251,191,36,.12);\
--warn-bd:#fbbf24;--bad-bg:rgba(248,113,113,.14);--bad-bd:#f87171;\
--na-bg:rgba(156,163,175,.12)}}\
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
.verdict.na{background:var(--na-bg);border-color:var(--muted)}\
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
    fn ticks_are_round_and_cover_range() {
        let t = ticks(13.0, 87.0, 4);
        assert_eq!(t, vec![20.0, 40.0, 60.0, 80.0]);
        assert!(ticks(0.0, 1.0, 4).len() >= 3);
    }
}
