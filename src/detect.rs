use crate::process::Cancel;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub const ANALYSIS_WIDTH: usize = 192;
pub const ANALYSIS_HEIGHT: usize = 108;
pub const SCENE: u8 = 1;
pub const SOLID: u8 = 2;
pub const FULLSCREEN: u8 = 4;
pub const STILL: u8 = 8;
pub const REFERENCE: u8 = 16;

/// Keep an exact scan available as the accuracy oracle for faster strategies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchMode {
    Exact,
    Adaptive,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DetectionStats {
    pub ffmpeg_calls: u64,
    pub frames_analyzed: u64,
    pub refinement_calls: u64,
    pub scout_calls: u64,
    pub scout_frames: u64,
    pub dense_frames: u64,
    /// Scene, solid color, fullscreen, still picture, reference.
    pub detector_evaluations: [u64; 5],
    pub histograms: u64,
    pub luma_frames: u64,
    pub merged_candidates: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DetectionResult {
    pub end: Option<f64>,
    pub stats: DetectionStats,
}

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
    #[serde(alias = "loading_spinner")]
    pub still_picture: bool,
    pub still_pixel_tolerance: u8,
    pub still_changed_fraction: f64,
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
            still_picture: true,
            still_pixel_tolerance: 16,
            still_changed_fraction: 0.02,
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
                || self.still_picture
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
        ensure!(
            self.still_changed_fraction.is_finite()
                && (0.0..=0.25).contains(&self.still_changed_fraction),
            "Still-picture changing pixels must be 0–25%."
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

/// Each frame's expensive features are built once, only when a due detector needs them.
struct Features<'a> {
    rgb: &'a [u8],
    histogram: Option<Histogram>,
    luma: Option<Vec<u8>>,
}
impl<'a> Features<'a> {
    fn new(rgb: &'a [u8]) -> Self {
        Self {
            rgb,
            histogram: None,
            luma: None,
        }
    }
    fn histogram(&mut self, stats: &mut DetectionStats) -> &Histogram {
        self.histogram.get_or_insert_with(|| {
            stats.histograms += 1;
            Histogram::from_rgb(self.rgb)
        })
    }
    fn luma(&mut self, stats: &mut DetectionStats) -> &[u8] {
        self.luma.get_or_insert_with(|| {
            stats.luma_frames += 1;
            self.rgb
                .as_chunks::<3>()
                .0
                .iter()
                .map(|p| ((54 * p[0] as u32 + 183 * p[1] as u32 + 19 * p[2] as u32) / 256) as u8)
                .collect()
        })
    }
}
impl DetectionSettings {
    pub(crate) fn enabled_mask(&self) -> u8 {
        (u8::from(self.scene_change) * SCENE)
            | (u8::from(self.solid_screen) * SOLID)
            | (u8::from(self.exit_fullscreen) * FULLSCREEN)
            | (u8::from(self.still_picture) * STILL)
            | (u8::from(self.uses_reference()) * REFERENCE)
    }
}

/// All scouts share the fastest required frame stream. Slower detectors are
/// dispatched independently; temporal detectors keep their own history.
pub(crate) struct ScoutSchedule {
    intervals: [f64; 5],
    due: [f64; 5],
    enabled: u8,
    pub spacing: f64,
}
impl ScoutSchedule {
    pub fn new(settings: &DetectionSettings) -> Self {
        let fast = (settings.hold_seconds / 4.0).min(0.5);
        let slow = fast * 2.0;
        let enabled = settings.enabled_mask();
        let intervals = [fast, slow, slow, fast, slow];
        let spacing = intervals
            .iter()
            .enumerate()
            .filter(|(i, _)| enabled & (1 << i) != 0)
            .map(|(_, interval)| *interval)
            .min_by(f64::total_cmp)
            .unwrap_or(slow);
        Self {
            intervals,
            due: [f64::NEG_INFINITY; 5],
            enabled,
            spacing,
        }
    }
    pub fn requested(&mut self, time: f64) -> u8 {
        let mut mask = 0;
        for i in 0..5 {
            if self.enabled & (1 << i) != 0 && time + 0.000001 >= self.due[i] {
                mask |= 1 << i;
                self.due[i] = time + self.intervals[i];
            }
        }
        mask
    }
    pub fn lookbehind(&self) -> f64 {
        self.intervals
            .iter()
            .enumerate()
            .filter(|(i, _)| self.enabled & (1 << i) != 0)
            .map(|(_, interval)| *interval)
            .fold(0.0, f64::max)
            * 2.0
            + 0.5
    }
}

/// Shared stateful visual evidence. A still run is measured against an anchored
/// RGB picture, never just adjacent frames (which would accept slow drift).
#[derive(Default)]
pub struct FrameAnalyzer {
    previous_histogram: Option<Histogram>,
    scene_baseline: Option<Histogram>,
    still_anchor: Vec<u8>,
    still_start: Option<f64>,
    pub(crate) still_since: Option<f64>,
}
impl FrameAnalyzer {
    pub fn analyze(&mut self, time: f64, rgb: &[u8], settings: &DetectionSettings) -> u8 {
        self.analyze_requested(
            time,
            rgb,
            settings,
            settings.enabled_mask(),
            &mut DetectionStats::default(),
        )
    }
    pub(crate) fn analyze_requested(
        &mut self,
        time: f64,
        rgb: &[u8],
        settings: &DetectionSettings,
        requested: u8,
        stats: &mut DetectionStats,
    ) -> u8 {
        let requested = requested & settings.enabled_mask();
        for (index, count) in stats.detector_evaluations.iter_mut().enumerate() {
            *count += u64::from(requested & (1 << index) != 0);
        }
        let mut features = Features::new(rgb);
        let mut mask = 0;
        self.still_since = None;
        if requested & SOLID != 0 {
            let matches = match settings.criterion {
                Criterion::Black | Criterion::White => {
                    let luma = features.luma(stats);
                    !luma.is_empty()
                        && luma
                            .iter()
                            .filter(|&&value| match settings.criterion {
                                Criterion::Black => value <= settings.black_level,
                                _ => value >= settings.white_level,
                            })
                            .count() as f64
                            / luma.len() as f64
                            >= settings.pixel_fraction
                }
                _ => settings.solid_matches(rgb),
            };
            if matches {
                mask |= SOLID;
            }
        }
        if requested & REFERENCE != 0
            && settings.reference.as_ref().is_some_and(|reference| {
                reference.similarity(features.histogram(stats)) >= settings.similarity
            })
        {
            mask |= REFERENCE;
        }
        if requested & SCENE != 0 {
            let histogram = features.histogram(stats);
            let returned = self.scene_baseline.as_ref().is_some_and(|baseline| {
                histogram.similarity(baseline) >= 1.0 - settings.scene_threshold * 0.5
            });
            if returned {
                self.scene_baseline = None;
            }
            if !returned
                && self.scene_baseline.is_none()
                && let Some(previous) = &self.previous_histogram
                && 1.0 - previous.similarity(histogram) >= settings.scene_threshold
            {
                self.scene_baseline = Some(previous.clone());
            }
            if self.scene_baseline.is_some() {
                mask |= SCENE;
            }
            self.previous_histogram = Some(histogram.clone());
        }
        if requested & FULLSCREEN != 0 {
            let luma = features.luma(stats);
            if luma.len() == ANALYSIS_WIDTH * ANALYSIS_HEIGHT && browser_chrome(luma) {
                mask |= FULLSCREEN;
            }
        }
        if requested & STILL != 0 {
            if !rgb.is_empty()
                && rgb.len() == self.still_anchor.len()
                && still_matches(rgb, &self.still_anchor, settings)
            {
                self.still_since = self.still_start;
                mask |= STILL;
            } else {
                self.still_anchor.clear();
                self.still_anchor.extend_from_slice(rgb);
                self.still_start = Some(time);
            }
        }
        mask
    }
}
fn still_matches(rgb: &[u8], anchor: &[u8], settings: &DetectionSettings) -> bool {
    let allowed = ((rgb.len() / 3) as f64 * settings.still_changed_fraction).floor() as usize;
    let mut changed = 0;
    for (pixel, reference) in rgb.as_chunks::<3>().0.iter().zip(anchor.as_chunks::<3>().0) {
        if pixel
            .iter()
            .zip(reference)
            .any(|(a, b)| a.abs_diff(*b) > settings.still_pixel_tolerance)
        {
            changed += 1;
            if changed > allowed {
                return false;
            }
        }
    }
    true
}

/// Browser-like top chrome needs multiple horizontal separators AND text-like
/// short edges on otherwise flat rows. Plain letterboxing has no text evidence.
fn browser_chrome(luma: &[u8]) -> bool {
    let w = ANALYSIS_WIDTH;
    let h = ANALYSIS_HEIGHT;
    let mut separators = 0;
    let mut text_rows = 0;
    let mut flat_rows = 0;
    let mut previous_separator = None;
    let mut first_text = None;
    let mut compact_chrome = false;
    for y in 1..h / 4 {
        let row = &luma[y * w..(y + 1) * w];
        let above = &luma[(y - 1) * w..y * w];
        let edges = row.windows(2).filter(|p| p[0].abs_diff(p[1]) > 35).count();
        let boundary = row
            .iter()
            .zip(above)
            .filter(|(a, b)| a.abs_diff(**b) > 24)
            .count();
        if edges >= w / 12 && edges <= w / 2 {
            text_rows += 1;
            first_text.get_or_insert(y);
        }
        // A tab bar above an address bar becomes a thin strip at this scale.
        // Its second boundary need not span the full width of the first one.
        if boundary > w / 2 {
            compact_chrome |= previous_separator.is_some_and(|previous| {
                y - previous <= 4 && first_text.is_some_and(|text| text < previous)
            });
        }
        if boundary > w * 3 / 4 {
            separators += 1;
            previous_separator = Some(y);
        }
        flat_rows += usize::from(edges < w / 40);
    }
    flat_rows >= 2 && ((separators >= 2 && text_rows >= 3) || compact_chrome)
}

#[derive(Clone, Copy, Debug)]
pub struct FramePoint {
    pub time: f64,
    pub matches: u8,
    /// Original anchor timestamp for a matching still run.
    pub still_since: Option<f64>,
}
/// Merge all enabled detectors' overlapping proposals before full-rate reads.
/// Padding covers uncertainty in the onset plus temporal detector warm-up.
pub(crate) fn candidate_windows(
    frames: &[FramePoint],
    cutoff: f64,
    end: f64,
    hold: f64,
    lookbehind: f64,
) -> Vec<(f64, f64)> {
    let mut windows: Vec<(f64, f64)> = Vec::new();
    for frame in frames {
        // A single similar pair is common in ordinary footage. Require a
        // short anchored sequence before proposing still-picture confirmation.
        let mut evidence = frame.matches;
        if frame
            .still_since
            .is_some_and(|since| frame.time - since + 0.000001 < (hold / 2.0).min(1.0))
        {
            evidence &= !STILL;
        }
        if frame.time < cutoff - 0.000001 || evidence == 0 {
            continue;
        }
        let from = (frame.time - lookbehind).max(cutoff);
        let until = (frame.time + hold + lookbehind).min(end);
        if let Some(last) = windows.last_mut()
            && from <= last.1 + 0.000001
        {
            last.1 = last.1.max(until);
        } else {
            windows.push((from, until));
        }
    }
    windows
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
    scan_after(
        source,
        start,
        cutoff,
        end,
        settings.hold_seconds,
        cancel,
        progress,
    )
}

/// Confirm continuous evidence using all frames, including short recoveries.
pub(crate) fn scan_after(
    source: &mut impl FrameSource,
    start: f64,
    cutoff: f64,
    end: f64,
    hold: f64,
    cancel: &Cancel,
    progress: &mut dyn FnMut(f64),
) -> Result<Option<f64>> {
    let mut tracker = RunTracker::default();
    let mut cursor = (cutoff - 0.5).max(start);
    while cursor < end - 0.000001 {
        cancel.check()?;
        let to = (cursor + 4.0).min(end);
        for frame in source.window(cursor, to)? {
            cancel.check()?;
            if frame.time < end
                && let Some(found) = tracker.push(frame, cutoff, hold)
            {
                cancel.check()?;
                progress(1.0);
                return Ok(Some(found));
            }
        }
        cursor = to;
        progress(((cursor - cutoff) / (end - cutoff)).clamp(0.0, 0.999));
    }
    cancel.check()?;
    let found = tracker.finish(end, hold);
    progress(1.0);
    Ok(found)
}

/// Compact confirmation cache: preserve run onsets and analyzer history while
/// extending overlapping windows; no need to retain or re-decode their pixels.
#[derive(Default)]
pub(crate) struct RunTracker {
    runs: [Option<f64>; 5],
    last_time: Option<f64>,
}
impl RunTracker {
    pub fn push(&mut self, frame: FramePoint, cutoff: f64, hold: f64) -> Option<f64> {
        if frame.time < cutoff - 0.000001
            || self
                .last_time
                .is_some_and(|last| frame.time <= last + 0.000001)
        {
            return None;
        }
        self.last_time = Some(frame.time);
        // The preceding frame remains presented until this timestamp.
        if let Some(found) = self.finish(frame.time, hold) {
            return Some(found);
        }
        for (index, run) in self.runs.iter_mut().enumerate() {
            if frame.matches & (1 << index) != 0 {
                if index == 3
                    && let Some(anchor) = frame.still_since
                {
                    *run = Some(anchor.max(cutoff));
                } else {
                    run.get_or_insert(frame.time);
                }
            } else {
                *run = None;
            }
        }
        self.finish(frame.time, hold)
    }
    pub fn finish(&self, time: f64, hold: f64) -> Option<f64> {
        confirmed(&self.runs, time, hold)
    }
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
                        still_since: None,
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
                && settings.still_picture
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
                    (160.2, 200.0, STILL)
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
            still_picture: false,
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
    #[test]
    fn still_picture_tolerates_small_animation_but_rejects_motion_and_slow_drift() {
        let settings = DetectionSettings {
            scene_change: false,
            solid_screen: false,
            exit_fullscreen: false,
            ..Default::default()
        };
        let frame = vec![90; ANALYSIS_WIDTH * ANALYSIS_HEIGHT * 3];
        let mut analyzer = FrameAnalyzer::default();
        assert_eq!(analyzer.analyze(120.0, &frame, &settings), 0);
        let mut changed = frame.clone();
        changed[..300].fill(240); // < 2% changed pixels, like a small spinner.
        assert_eq!(analyzer.analyze(120.5, &changed, &settings), STILL);
        assert_eq!(analyzer.still_since, Some(120.0));
        changed[..3000].fill(240);
        assert_eq!(analyzer.analyze(121.0, &changed, &settings), 0);
        let mut analyzer = FrameAnalyzer::default();
        for (index, value) in [90, 98, 106, 114].iter().enumerate() {
            let mask = analyzer.analyze(index as f64, &vec![*value; frame.len()], &settings);
            if index == 3 {
                assert_eq!(mask, 0, "anchored comparison must reject gradual drift");
            }
        }
    }
    #[test]
    fn scout_dispatch_shares_features_and_runs_slow_detectors_less_often() {
        let mut settings = DetectionSettings {
            reference_enabled: true,
            ..Default::default()
        };
        let rgb = vec![80; ANALYSIS_WIDTH * ANALYSIS_HEIGHT * 3];
        settings.reference = Some(Histogram::from_rgb(&rgb));
        let mut analyzer = FrameAnalyzer::default();
        let mut schedule = ScoutSchedule::new(&settings);
        let mut stats = DetectionStats::default();
        for i in 0..5 {
            let time = i as f64 * schedule.spacing;
            analyzer.analyze_requested(time, &rgb, &settings, schedule.requested(time), &mut stats);
        }
        assert_eq!(stats.detector_evaluations, [5, 3, 3, 5, 3]);
        assert_eq!(
            stats.histograms, 5,
            "scene and reference share one histogram"
        );
        assert_eq!(
            stats.luma_frames, 3,
            "solid and fullscreen share one luminance image"
        );
    }
    #[test]
    fn candidate_windows_merge_across_detectors_and_keep_disjoint_ranges_in_order() {
        let frames = [
            (121.0, SOLID),
            (122.0, FULLSCREEN),
            (123.0, REFERENCE),
            (140.0, STILL),
        ]
        .into_iter()
        .map(|(time, matches)| FramePoint {
            time,
            matches,
            still_since: None,
        })
        .collect::<Vec<_>>();
        assert_eq!(
            candidate_windows(&frames, 120.0, 150.0, 2.0, 1.0),
            vec![(120.0, 126.0), (139.0, 143.0)]
        );
    }
    #[test]
    fn scouting_waits_for_still_evidence_without_suppressing_other_detectors() {
        let mut frames = vec![FramePoint {
            time: 120.5,
            matches: STILL,
            still_since: Some(120.0),
        }];
        assert!(candidate_windows(&frames, 120.0, 130.0, 2.0, 2.5).is_empty());
        frames[0].matches |= SOLID;
        assert_eq!(candidate_windows(&frames, 120.0, 130.0, 2.0, 2.5).len(), 1);
        frames[0].matches = STILL;
        frames[0].time = 121.0;
        assert_eq!(candidate_windows(&frames, 120.0, 130.0, 2.0, 2.5).len(), 1);
    }
    #[test]
    fn still_onset_is_the_anchor_and_confirmation_resets_after_motion() {
        let mut tracker = RunTracker::default();
        let frame = |time, since| FramePoint {
            time,
            matches: STILL,
            still_since: Some(since),
        };
        assert_eq!(tracker.push(frame(120.5, 120.0), 120.0, 2.0), None);
        assert_eq!(
            tracker.push(
                FramePoint {
                    time: 121.0,
                    matches: 0,
                    still_since: None
                },
                120.0,
                2.0
            ),
            None
        );
        assert_eq!(tracker.push(frame(121.5, 121.0), 120.0, 2.0), None);
        assert_eq!(tracker.push(frame(123.0, 121.0), 120.0, 2.0), Some(121.0));
    }
    #[test]
    fn old_spinner_presets_migrate_and_still_tolerances_round_trip() {
        let settings: DetectionSettings =
            serde_json::from_str(r#"{"loading_spinner":false}"#).unwrap();
        assert!(!settings.still_picture);
        assert_eq!(settings.still_changed_fraction, 0.02);
        let json = serde_json::to_string(&settings).unwrap();
        assert!(!json.contains("loading_spinner"));
        assert_eq!(
            serde_json::from_str::<DetectionSettings>(&json).unwrap(),
            settings
        );
        assert!(
            DetectionSettings {
                still_changed_fraction: f64::NAN,
                ..settings
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn returning_to_the_original_scene_ends_the_scene_change_run() {
        let a = vec![0; ANALYSIS_WIDTH * ANALYSIS_HEIGHT * 3];
        let b = vec![255; a.len()];
        let settings = DetectionSettings {
            solid_screen: false,
            exit_fullscreen: false,
            still_picture: false,
            ..Default::default()
        };
        let mut analyzer = FrameAnalyzer::default();
        assert_eq!(analyzer.analyze(120.0, &a, &settings), 0);
        assert_eq!(analyzer.analyze(121.0, &b, &settings), SCENE);
        assert_eq!(analyzer.analyze(121.5, &a, &settings), 0);
        assert_eq!(analyzer.analyze(122.0, &a, &settings), 0);
    }
}
