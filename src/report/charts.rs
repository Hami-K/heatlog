//! Chart-ready time series shared by the HTML report and the GUI: friendly
//! units, throttle shading regions, and spike-preserving downsampling.

use super::{cpu_throttle_flags, episodes, Summary};
use crate::session::{Sample, Session};

/// More points than this per chart just bloats the output, so longer
/// sessions are decimated (keeping each bucket's min and max, which
/// preserves spikes).
pub const MAX_POINTS: usize = 1200;

pub enum YAxis {
    /// 0 to 100.
    Pct,
    /// 0 to a padded data maximum.
    FromZero,
    /// Padded data range, extended so the shaded "hot" zone is visible.
    Temp { hot_from: f64 },
}

pub struct ChartData {
    pub title: &'static str,
    pub unit: &'static str,
    pub explain: String,
    /// (seconds since session start, value). Millisecond precision:
    /// rounding to whole seconds made jittered neighbours share an x
    /// position, drawing vertical jumps.
    pub points: Vec<(f64, f64)>,
    /// Shaded (start, end) second ranges where throttling was active.
    pub bands: Vec<(f64, f64)>,
    /// "gpu" (driver-reported) or "cpu" (estimated).
    pub band_class: &'static str,
    pub y: YAxis,
}

/// Builds every chart that has data, in display order.
pub fn build(session: &Session, summary: &Summary) -> Vec<ChartData> {
    let samples = &session.samples;
    let t0 = samples.first().map(|s| s.t_ms).unwrap_or(0);
    let xs: Vec<f64> = samples.iter().map(|s| secs_f64(s.t_ms - t0)).collect();

    let gpu_flags: Vec<bool> = samples
        .iter()
        .map(|s| s.gpu.as_ref().is_some_and(|g| !g.throttle.is_empty()))
        .collect();
    let gpu_bands = regions(&gpu_flags, &xs, summary.interval_secs as f64);
    let cpu_bands = cpu_throttle_flags(samples)
        .map(|(_, flags)| regions(&flags, &xs, summary.interval_secs as f64))
        .unwrap_or_default();

    let pick = |f: &dyn Fn(&Sample) -> Option<f64>| -> Vec<(f64, f64)> {
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

    // Each chart owns its band list; the lists are a handful of episode
    // ranges at most, so the clones are trivial.
    let charts = vec![
        ChartData {
            title: "GPU temperature",
            unit: "C",
            explain: explain(
                "How hot the graphics chip is. Staying out of the shaded hot zone is good.",
                &gpu_bands,
                gpu_shaded,
            ),
            points: pick(&|s| s.gpu.as_ref().map(|g| f64::from(g.temp_c))),
            bands: gpu_bands.clone(),
            band_class: "gpu",
            y: YAxis::Temp { hot_from: 85.0 },
        },
        ChartData {
            title: "GPU speed",
            unit: "GHz",
            explain: explain(
                "How fast the GPU is running. A steady line under load is good; sharp dips can mean throttling.",
                &gpu_bands,
                gpu_shaded,
            ),
            points: pick(&|s| s.gpu.as_ref().map(|g| f64::from(g.core_mhz) / 1000.0)),
            bands: gpu_bands.clone(),
            band_class: "gpu",
            y: YAxis::FromZero,
        },
        ChartData {
            title: "GPU power",
            unit: "W",
            explain: explain(
                "Electricity the GPU draws. Sitting flat at the same ceiling usually means its power limit.",
                &gpu_bands,
                gpu_shaded,
            ),
            points: pick(&|s| s.gpu.as_ref().map(|g| g.power_w)),
            bands: gpu_bands,
            band_class: "gpu",
            y: YAxis::FromZero,
        },
        ChartData {
            title: "CPU usage",
            unit: "%",
            explain: explain(
                "How busy the processor is, from 0 to 100%.",
                &cpu_bands,
                cpu_shaded,
            ),
            points: pick(&|s| Some(f64::from(s.cpu.usage_pct))),
            bands: cpu_bands.clone(),
            band_class: "cpu",
            y: YAxis::Pct,
        },
        ChartData {
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
            bands: cpu_bands.clone(),
            band_class: "cpu",
            y: YAxis::FromZero,
        },
        ChartData {
            title: "CPU temperature",
            unit: "C",
            explain: explain(
                "Processor temperature. Staying out of the shaded hot zone is good.",
                &cpu_bands,
                cpu_shaded,
            ),
            points: pick(&|s| s.cpu_temp_c.map(f64::from)),
            bands: cpu_bands,
            band_class: "cpu",
            y: YAxis::Temp { hot_from: 90.0 },
        },
    ];

    charts
        .into_iter()
        .filter(|c| !c.points.is_empty())
        .collect()
}

fn secs_f64(ms: i64) -> f64 {
    ms.max(0) as f64 / 1000.0
}

/// Min/max decimation: each bucket contributes its lowest and highest value
/// in time order, so peaks survive however long the session is.
pub fn downsample(points: &[(f64, f64)], max_points: usize) -> Vec<(f64, f64)> {
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
}
