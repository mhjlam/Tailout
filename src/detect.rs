use crate::process::Cancel;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub const ANALYSIS_WIDTH: usize = 192;
pub const ANALYSIS_HEIGHT: usize = 108;
pub const SCENE: u8 = 1;
pub const SOLID: u8 = 2;
pub const FULLSCREEN: u8 = 4;
pub const SPINNER: u8 = 8;
pub const REFERENCE: u8 = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Criterion {
    #[default]
    Black,
    White,
    Color,
    // Accept reference criteria in older presets. The UI now has a separate checkbox.
    Reference,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DetectionSettings {
    pub scene_change: bool,
    pub solid_screen: bool,
    pub exit_fullscreen: bool,
    pub loading_spinner: bool,
    pub reference_enabled: bool,
    /// Protected interval after Begin. Never cut within the first two minutes.
    pub grace_seconds: f64,
    pub scene_threshold: f64,
    pub criterion: Criterion,
    pub color: [u8; 3],
    pub color_tolerance: u8,
    pub pixel_fraction: f64,
    pub black_level: u8,
    pub white_level: u8,
    pub similarity: f64,
    pub hold_seconds: f64,
    pub reference: Option<Histogram>,
}
impl Default for DetectionSettings {
    fn default() -> Self {
        Self {
            scene_change: true,
            solid_screen: true,
            exit_fullscreen: true,
            loading_spinner: true,
            reference_enabled: false,
            grace_seconds: 120.0,
            scene_threshold: 0.55,
            criterion: Criterion::Black,
            color: [255, 255, 255],
            color_tolerance: 24,
            pixel_fraction: 0.95,
            black_level: 16,
            white_level: 240,
            similarity: 0.92,
            hold_seconds: 2.0,
            reference: None,
        }
    }
}
impl DetectionSettings {
    pub fn uses_reference(&self) -> bool {
        self.reference_enabled || self.criterion == Criterion::Reference
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.scene_change
                || self.solid_screen
                || self.exit_fullscreen
                || self.loading_spinner
                || self.uses_reference(),
            "Select at least one end detector."
        );
        ensure!(
            self.grace_seconds.is_finite() && (120.0..=86400.0).contains(&self.grace_seconds),
            "Grace period must be at least 2 minutes and at most 24 hours."
        );
        ensure!(
            self.scene_threshold.is_finite() && (0.05..=1.0).contains(&self.scene_threshold),
            "Scene-change threshold must be 5–100%."
        );
        ensure!(
            self.pixel_fraction.is_finite() && (0.01..=1.0).contains(&self.pixel_fraction),
            "Pixel percentage must be 1–100%."
        );
        ensure!(
            self.similarity.is_finite() && (0.0..=1.0).contains(&self.similarity),
            "Similarity must be 0–100%."
        );
        ensure!(
            self.hold_seconds.is_finite() && (0.1..=3600.0).contains(&self.hold_seconds),
            "Down period must be 0.1–3600 seconds."
        );
        if self.uses_reference() {
            let histogram = self
                .reference
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Choose a reference frame or image."))?;
            ensure!(
                histogram.bins.len() == 512
                    && histogram.bins.iter().all(|v| v.is_finite() && *v >= 0.0)
                    && (histogram.bins.iter().sum::<f64>() - 1.0).abs() < 0.001,
                "Invalid reference histogram."
            );
        }
        Ok(())
    }
    pub fn solid_matches(&self, rgb: &[u8]) -> bool {
        if rgb.is_empty() || self.criterion == Criterion::Reference {
            return false;
        }
        let hits = rgb
            .as_chunks::<3>()
            .0
            .iter()
            .filter(|pixel| {
                let luma =
                    (54 * pixel[0] as u32 + 183 * pixel[1] as u32 + 19 * pixel[2] as u32) / 256;
                match self.criterion {
                    Criterion::Black => luma <= self.black_level as u32,
                    Criterion::White => luma >= self.white_level as u32,
                    Criterion::Color => pixel
                        .iter()
                        .zip(self.color)
                        .all(|(a, b)| a.abs_diff(b) <= self.color_tolerance),
                    Criterion::Reference => false,
                }
            })
            .count();
        hits as f64 / (rgb.len() / 3) as f64 >= self.pixel_fraction
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Histogram {
    pub bins: Vec<f64>,
}
impl Histogram {
    pub fn from_rgb(rgb: &[u8]) -> Self {
        let mut bins = vec![0.0; 512];
        let count = rgb.len() / 3;
        if count > 0 {
            for p in rgb.as_chunks::<3>().0 {
                bins[(p[0] as usize / 32) * 64 + (p[1] as usize / 32) * 8 + p[2] as usize / 32] +=
                    1.0 / count as f64;
            }
        }
        Self { bins }
    }
    pub fn similarity(&self, other: &Self) -> f64 {
        self.bins
            .iter()
            .zip(&other.bins)
            .map(|(a, b)| (a * b).sqrt())
            .sum::<f64>()
            .clamp(0.0, 1.0)
    }
}

/// Stateful visual evidence. Each bit is an independent detector; confirmation
/// never combines unrelated, short signals into a single sufficiently long run.
#[derive(Default)]
pub struct FrameAnalyzer {
    previous_histogram: Option<Histogram>,
    scene_baseline: Option<Histogram>,
    previous_luma: Vec<u8>,
    spinner_until: f64,
}
impl FrameAnalyzer {
    pub fn analyze(&mut self, time: f64, rgb: &[u8], settings: &DetectionSettings) -> u8 {
        let mut mask = 0;
        if settings.solid_screen && settings.solid_matches(rgb) {
            mask |= SOLID;
        }
        let histogram = Histogram::from_rgb(rgb);
        if settings.uses_reference()
            && settings
                .reference
                .as_ref()
                .is_some_and(|reference| reference.similarity(&histogram) >= settings.similarity)
        {
            mask |= REFERENCE;
        }
        if settings.scene_change {
            if let Some(baseline) = &self.scene_baseline
                && histogram.similarity(baseline) >= 1.0 - settings.scene_threshold * 0.5
            {
                self.scene_baseline = None;
            }
            if self.scene_baseline.is_none()
                && let Some(previous) = &self.previous_histogram
                && 1.0 - previous.similarity(&histogram) >= settings.scene_threshold
            {
                self.scene_baseline = Some(previous.clone());
            }
            if self.scene_baseline.is_some() {
                mask |= SCENE;
            }
        }
        self.previous_histogram = Some(histogram);
        if settings.exit_fullscreen || settings.loading_spinner {
            let luma: Vec<u8> = rgb
                .as_chunks::<3>()
                .0
                .iter()
                .map(|p| ((54 * p[0] as u32 + 183 * p[1] as u32 + 19 * p[2] as u32) / 256) as u8)
                .collect();
            if luma.len() == ANALYSIS_WIDTH * ANALYSIS_HEIGHT {
                if settings.exit_fullscreen && browser_chrome(&luma) {
                    mask |= FULLSCREEN;
                }
                if settings.loading_spinner {
                    if self.previous_luma.len() == luma.len()
                        && spinning_ring(&luma, &self.previous_luma)
                    {
                        self.spinner_until = time + 0.25;
                    }
                    if time < self.spinner_until {
                        mask |= SPINNER;
                    }
                }
            }
            self.previous_luma = luma;
        }
        mask
    }
}

/// Browser-like top chrome needs multiple horizontal separators AND text-like
/// short edges on otherwise flat rows. Plain letterboxing has no text evidence.
fn browser_chrome(luma: &[u8]) -> bool {
    let w = ANALYSIS_WIDTH;
    let h = ANALYSIS_HEIGHT;
    let mut separators = 0;
    let mut text_rows = 0;
    let mut flat_rows = 0;
    for y in 1..h / 4 {
        let row = &luma[y * w..(y + 1) * w];
        let above = &luma[(y - 1) * w..y * w];
        let edges = row.windows(2).filter(|p| p[0].abs_diff(p[1]) > 35).count();
        let boundary = row
            .iter()
            .zip(above)
            .filter(|(a, b)| a.abs_diff(**b) > 24)
            .count();
        separators += usize::from(boundary > w * 3 / 4);
        text_rows += usize::from(edges >= w / 12 && edges <= w / 2);
        flat_rows += usize::from(edges < w / 40);
    }
    separators >= 2 && text_rows >= 3 && flat_rows >= 2
}

/// A partial bright/dark annulus whose sectors move while the surrounding image
/// stays still. Search several likely player centres; do not classify a static
/// circle, a moving full-screen picture, or arbitrary central flicker as a spinner.
fn spinning_ring(current: &[u8], previous: &[u8]) -> bool {
    let w = ANALYSIS_WIDTH;
    let h = ANALYSIS_HEIGHT;
    let changed = current
        .iter()
        .zip(previous)
        .filter(|(a, b)| a.abs_diff(**b) > 20)
        .count();
    if changed == 0 || changed > w * h / 25 {
        return false;
    }
    for cy in [h / 2, h * 2 / 5, h * 3 / 5] {
        for cx in [w / 2, w / 3, w * 2 / 3] {
            for radius in [3.0_f64, 4.0, 5.0, 6.0, 8.0, 10.0] {
                let sample = |pixels: &[u8], angle: f64, r: f64| -> u8 {
                    let x = (cx as f64 + angle.cos() * r).round() as usize;
                    let y = (cy as f64 + angle.sin() * r).round() as usize;
                    pixels[y * w + x]
                };
                let background = (0..16)
                    .map(|i| {
                        let angle = i as f64 * std::f64::consts::TAU / 16.0;
                        sample(current, angle, radius * 1.6) as u32
                            + sample(current, angle, radius * 0.35) as u32
                    })
                    .sum::<u32>() as f64
                    / 32.0;
                let mut active = 0_u16;
                let mut old_active = 0_u16;
                for i in 0..16 {
                    let angle = i as f64 * std::f64::consts::TAU / 16.0;
                    if (sample(current, angle, radius) as f64 - background).abs() > 35.0 {
                        active |= 1 << i;
                    }
                    if (sample(previous, angle, radius) as f64 - background).abs() > 35.0 {
                        old_active |= 1 << i;
                    }
                }
                let count = active.count_ones();
                if !(4..=13).contains(&count) || (active ^ old_active).count_ones() < 2 {
                    continue;
                }
                let mut outer_changes = 0;
                let mut inner_changes = 0;
                let mut ring_changes = 0;
                let r2 = radius * radius;
                for y in 0..h {
                    for x in 0..w {
                        if current[y * w + x].abs_diff(previous[y * w + x]) <= 20 {
                            continue;
                        }
                        let distance =
                            (x as f64 - cx as f64).powi(2) + (y as f64 - cy as f64).powi(2);
                        if distance > r2 * 2.25 {
                            outer_changes += 1;
                        } else if distance < r2 * 0.36 {
                            inner_changes += 1;
                        } else {
                            ring_changes += 1;
                        }
                    }
                }
                if ring_changes >= 2 && outer_changes <= changed / 10 && inner_changes <= 1 {
                    return true;
                }
            }
        }
    }
    false
}

#[derive(Clone, Copy, Debug)]
pub struct FramePoint {
    pub time: f64,
    pub matches: u8,
}
pub trait FrameSource {
    /// Every decoded frame in [start, end), retaining its presentation timestamp.
    fn window(&mut self, start: f64, end: f64) -> Result<Vec<FramePoint>>;
}

/// Search chronologically, protecting the grace period. A reverse sparse search
/// cannot guarantee the earliest event. Bounded windows retain analyzer state
/// and each detector's run independently across process/window boundaries.
pub fn detect_end(
    source: &mut impl FrameSource,
    start: f64,
    end: f64,
    settings: &DetectionSettings,
    cancel: &Cancel,
    progress: &mut dyn FnMut(f64),
) -> Result<Option<f64>> {
    settings.validate()?;
    ensure!(end > start, "Empty analysis range.");
    cancel.check()?;
    progress(0.0);
    let cutoff = start + settings.grace_seconds;
    if cutoff + settings.hold_seconds > end + 0.000001 {
        cancel.check()?;
        progress(1.0);
        return Ok(None);
    }
    let mut runs = [None::<f64>; 5];
    let mut cursor = (cutoff - 0.5).max(start);
    let mut last_time = None;
    while cursor < end - 0.000001 {
        cancel.check()?;
        let to = (cursor + 4.0).min(end);
        for frame in source.window(cursor, to)? {
            cancel.check()?;
            if frame.time < cutoff - 0.000001
                || frame.time >= end
                || last_time.is_some_and(|last| frame.time <= last + 0.000001)
            {
                continue;
            }
            last_time = Some(frame.time);
            // The preceding frame remains presented until this timestamp. This
            // also confirms a run whose first nonmatching frame is at its limit.
            if let Some(found) = confirmed(&runs, frame.time, settings.hold_seconds) {
                cancel.check()?;
                progress(1.0);
                return Ok(Some(found));
            }
            for (index, run) in runs.iter_mut().enumerate() {
                if frame.matches & (1 << index) != 0 {
                    run.get_or_insert(frame.time);
                } else {
                    *run = None;
                }
            }
        }
        cursor = to;
        progress(((cursor - cutoff) / (end - cutoff)).clamp(0.0, 0.999));
    }
    cancel.check()?;
    let found = confirmed(&runs, end, settings.hold_seconds);
    progress(1.0);
    Ok(found)
}
fn confirmed(runs: &[Option<f64>; 5], time: f64, hold: f64) -> Option<f64> {
    runs.iter()
        .flatten()
        .copied()
        .filter(|start| time - start + 0.000001 >= hold)
        .min_by(f64::total_cmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Synthetic {
        frames: Vec<FramePoint>,
        windows: usize,
    }
    impl FrameSource for Synthetic {
        fn window(&mut self, from: f64, to: f64) -> Result<Vec<FramePoint>> {
            self.windows += 1;
            Ok(self
                .frames
                .iter()
                .filter(|f| f.time >= from - 0.000001 && f.time < to - 0.000001)
                .copied()
                .collect())
        }
    }
    fn source(end: f64, runs: &[(f64, f64, u8)]) -> Synthetic {
        Synthetic {
            frames: (0..(end * 10.0) as usize)
                .map(|i| {
                    let time = i as f64 / 10.0;
                    FramePoint {
                        time,
                        matches: runs
                            .iter()
                            .filter(|(a, b, _)| time >= *a && time < *b)
                            .fold(0, |mask, (_, _, flag)| mask | flag),
                    }
                })
                .collect(),
            windows: 0,
        }
    }
    fn find(end: f64, runs: &[(f64, f64, u8)]) -> Option<f64> {
        detect_end(
            &mut source(end, runs),
            0.0,
            end,
            // These edge-case fixtures deliberately use a long hold period.
            &DetectionSettings {
                hold_seconds: 15.0,
                ..Default::default()
            },
            &Cancel::default(),
            &mut |_| {},
        )
        .unwrap()
    }
    #[test]
    fn default_hold_accepts_two_seconds_but_rejects_a_shorter_event() {
        let settings = DetectionSettings::default();
        let found = detect_end(
            &mut source(150.0, &[(122.0, 123.9, SOLID), (130.2, 132.2, SOLID)]),
            0.0,
            150.0,
            &settings,
            &Cancel::default(),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(found, Some(130.2));
    }
    #[test]
    fn defaults_enable_four_independent_detectors_and_grace_follows_begin() {
        let settings = DetectionSettings::default();
        assert!(
            settings.scene_change
                && settings.solid_screen
                && settings.exit_fullscreen
                && settings.loading_spinner
        );
        assert!(!settings.reference_enabled);
        assert_eq!(settings.grace_seconds, 120.0);
        let result = detect_end(
            &mut source(240.0, &[(125.0, 150.0, SOLID), (190.0, 220.0, SCENE)]),
            60.0,
            240.0,
            &settings,
            &Cancel::default(),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(result, Some(190.0));
    }
    #[test]
    fn earliest_qualifying_event_wins_even_if_normal_video_returns() {
        assert_eq!(
            find(250.0, &[(122.3, 145.0, SOLID), (180.0, 250.0, REFERENCE)]),
            Some(122.3)
        );
    }
    #[test]
    fn grace_period_and_short_events_are_ignored() {
        assert_eq!(
            find(
                250.0,
                &[
                    (20.0, 90.0, SOLID),
                    (125.0, 135.0, SOLID),
                    (160.2, 200.0, SPINNER)
                ]
            ),
            Some(160.2)
        );
        assert_eq!(find(119.0, &[(0.0, 119.0, SOLID)]), None);
        let settings = DetectionSettings {
            grace_seconds: 119.0,
            ..Default::default()
        };
        assert!(settings.validate().is_err());
    }
    #[test]
    fn recoveries_and_different_detectors_cannot_be_stitched_together() {
        assert_eq!(
            find(200.0, &[(122.0, 130.0, SOLID), (130.0, 139.0, REFERENCE)]),
            None
        );
        assert_eq!(
            find(200.0, &[(122.0, 136.9, SOLID), (137.0, 150.0, SOLID)]),
            None
        );
    }
    #[test]
    fn runs_cross_windows_and_final_run_is_confirmed_at_video_end() {
        assert_eq!(find(180.0, &[(164.7, 180.0, FULLSCREEN)]), Some(164.7));
        assert_eq!(find(180.0, &[(165.1, 180.0, FULLSCREEN)]), None);
        assert_eq!(find(180.0, &[(120.0, 180.0, SOLID)]), Some(120.0));
    }
    #[test]
    fn progress_is_monotonic_and_cancellation_does_not_report_completion() {
        let mut updates = Vec::new();
        let found = detect_end(
            &mut source(220.0, &[(160.0, 190.0, SOLID)]),
            0.0,
            220.0,
            &DetectionSettings::default(),
            &Cancel::default(),
            &mut |v| updates.push(v),
        )
        .unwrap();
        assert_eq!(found, Some(160.0));
        assert_eq!(updates.first(), Some(&0.0));
        assert_eq!(updates.last(), Some(&1.0));
        assert!(updates.windows(2).all(|w| w[0] <= w[1]));
        let cancel = Cancel::default();
        let mut complete = false;
        let error = detect_end(
            &mut source(220.0, &[]),
            0.0,
            220.0,
            &DetectionSettings::default(),
            &cancel,
            &mut |v| {
                complete |= v == 1.0;
                if v > 0.1 {
                    cancel.cancel();
                }
            },
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "Cancelled");
        assert!(!complete);
    }
    #[test]
    fn no_match_finishes_and_short_videos_are_not_decoded() {
        let mut video = source(30.0, &[]);
        assert_eq!(
            detect_end(
                &mut video,
                0.0,
                30.0,
                &DetectionSettings::default(),
                &Cancel::default(),
                &mut |_| {}
            )
            .unwrap(),
            None
        );
        assert_eq!(video.windows, 0);
        assert_eq!(find(180.0, &[]), None);
    }
    #[test]
    fn color_reference_and_scene_signals_are_independent() {
        let black = vec![0; ANALYSIS_WIDTH * ANALYSIS_HEIGHT * 3];
        let white = vec![255; black.len()];
        let mut settings = DetectionSettings {
            exit_fullscreen: false,
            loading_spinner: false,
            ..Default::default()
        };
        let mut analyzer = FrameAnalyzer::default();
        assert_eq!(analyzer.analyze(120.0, &white, &settings), 0);
        assert_eq!(analyzer.analyze(121.0, &black, &settings), SCENE | SOLID);
        settings.solid_screen = false;
        settings.scene_change = false;
        settings.reference_enabled = true;
        settings.reference = Some(Histogram::from_rgb(&white));
        assert_eq!(analyzer.analyze(122.0, &white, &settings), REFERENCE);
        settings.criterion = Criterion::Color;
        settings.color = [220, 20, 30];
        assert!(settings.solid_matches(&[220, 20, 30].repeat(100)));
        assert!(!settings.solid_matches(&black));
        assert_eq!(
            Histogram::from_rgb(&black).similarity(&Histogram::from_rgb(&white)),
            0.0
        );
    }
    #[test]
    fn browser_chrome_requires_text_evidence_and_rejects_letterboxing() {
        let mut luma = vec![45; ANALYSIS_WIDTH * ANALYSIS_HEIGHT];
        assert!(!browser_chrome(&luma));
        for y in 3..20 {
            for x in 0..ANALYSIS_WIDTH {
                luma[y * ANALYSIS_WIDTH + x] = if (6..10).contains(&y) && x % 8 < 3 {
                    240
                } else {
                    180
                };
            }
        }
        assert!(browser_chrome(&luma));
        for y in 0..ANALYSIS_HEIGHT {
            for x in 0..ANALYSIS_WIDTH {
                luma[y * ANALYSIS_WIDTH + x] = if !(15..=90).contains(&y) { 0 } else { 90 };
            }
        }
        assert!(!browser_chrome(&luma));
    }
    fn ring(phase: f64) -> Vec<u8> {
        let mut image = vec![20; ANALYSIS_WIDTH * ANALYSIS_HEIGHT];
        for y in 0..ANALYSIS_HEIGHT {
            for x in 0..ANALYSIS_WIDTH {
                let dx = x as f64 - ANALYSIS_WIDTH as f64 / 2.0;
                let dy = y as f64 - ANALYSIS_HEIGHT as f64 / 2.0;
                let radius = (dx * dx + dy * dy).sqrt();
                let angle = (dy.atan2(dx) - phase).rem_euclid(std::f64::consts::TAU);
                if (4.3..=5.7).contains(&radius) && angle < 3.8 {
                    image[y * ANALYSIS_WIDTH + x] = 240;
                }
            }
        }
        image
    }
    #[test]
    fn spinner_needs_a_moving_ring_on_a_static_surrounding_image() {
        let a = ring(0.0);
        let b = ring(0.7);
        assert!(spinning_ring(&b, &a));
        assert!(!spinning_ring(&a, &a));
        assert!(!spinning_ring(&vec![240; a.len()], &a));
    }
}
