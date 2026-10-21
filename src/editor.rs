//! Batch processing defaults with explicit per-video overrides and independent markers.
use crate::{
    detect::DetectionSettings,
    model::{Encoder, ResizeOptions, Settings, parse_time, time_text},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum EndFeedback {
    #[default]
    NotSearched,
    Stale,
    Searching,
    Cancelled,
    Found(f64),
    NoMatch,
    Failed(String),
}

/// Encoding settings which may be inherited from the batch. Markers and
/// search state live on VideoEdit instead.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoOptions {
    pub encoder: Encoder,
    pub quality: u8,
    pub resize: ResizeOptions,
    pub force: bool,
    pub compare: bool,
    pub require_ssim: bool,
    pub min_ssim: f64,
}
impl Default for VideoOptions {
    fn default() -> Self {
        Self {
            encoder: Encoder::Auto,
            quality: 21,
            resize: ResizeOptions::default(),
            force: true,
            compare: false,
            require_ssim: false,
            min_ssim: 0.95,
        }
    }
}

/// A kept interval. IDs remain stable when clips are selected or reordered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Clip {
    pub id: u64,
    pub start: String,
    pub end: String,
    pub auto_end: bool,
    /// Manual end edits and explicit per-clip enable/disable choices survive
    /// later changes to shared detection settings.
    pub override_auto_end: bool,
    pub feedback: EndFeedback,
    pub search_limit: Option<String>,
}
impl Default for Clip {
    fn default() -> Self {
        Self {
            id: 1,
            start: time_text(0.0),
            end: String::new(),
            auto_end: false,
            override_auto_end: false,
            feedback: EndFeedback::NotSearched,
            search_limit: None,
        }
    }
}
impl Clip {
    pub fn range(&self, duration: f64) -> Result<(f64, f64)> {
        let begin = parse_time(&self.start)?;
        let end = optional_time(&self.end)?.unwrap_or(duration).min(duration);
        ensure!(
            end - begin >= 0.05,
            "A clip must contain at least 0.05 seconds."
        );
        Ok((begin, end))
    }
    pub fn discard_auto_end(&mut self) {
        self.auto_end = false;
        self.override_auto_end = true;
        self.search_limit = None;
        self.feedback = EndFeedback::NotSearched;
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "StoredVideoEdit")]
pub struct VideoEdit {
    pub clips: Vec<Clip>,
    pub selected_clip: usize,
    pub playhead: f64,
    #[serde(flatten)]
    pub video: VideoOptions,
    pub override_video: bool,
    pub override_detection: bool,
    pub detection: DetectionSettings,
    pub reference_name: String,
    /// Scratch editor values when every clip has been removed; never exported.
    #[serde(skip)]
    pub empty_clip: Clip,
}
impl Default for VideoEdit {
    fn default() -> Self {
        StoredVideoEdit::default().into()
    }
}
impl From<StoredVideoEdit> for VideoEdit {
    fn from(value: StoredVideoEdit) -> Self {
        let mut clips = value.clips.unwrap_or_else(|| {
            vec![Clip {
                start: value.start,
                end: value.end,
                auto_end: value.auto_end,
                override_auto_end: value.override_detection,
                feedback: value.feedback,
                search_limit: value.search_limit,
                ..Clip::default()
            }]
        });
        // Presets from older versions have no IDs. Normalize duplicates too.
        let mut used = std::collections::BTreeSet::new();
        for clip in &mut clips {
            if clip.id == 0 || !used.insert(clip.id) {
                clip.id = (1..).find(|id| !used.contains(id)).unwrap();
                used.insert(clip.id);
            }
        }
        let selected_clip = value.selected_clip.min(clips.len().saturating_sub(1));
        Self {
            clips,
            selected_clip,
            playhead: value.playhead,
            video: value.video,
            override_video: value.override_video,
            override_detection: value.override_detection,
            detection: value.detection,
            reference_name: value.reference_name,
            empty_clip: Clip::default(),
        }
    }
}
// Marker editing always targets the selected interval; canonical ranges live
// only in `clips`. An empty list has a disabled scratch editor, not an export.
impl std::ops::Deref for VideoEdit {
    type Target = Clip;
    fn deref(&self) -> &Clip {
        self.clips
            .get(self.selected_clip)
            .unwrap_or(&self.empty_clip)
    }
}
impl std::ops::DerefMut for VideoEdit {
    fn deref_mut(&mut self) -> &mut Clip {
        self.clips
            .get_mut(self.selected_clip)
            .unwrap_or(&mut self.empty_clip)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct StoredVideoEdit {
    pub clips: Option<Vec<Clip>>,
    pub selected_clip: usize,
    pub start: String,
    pub end: String,
    pub playhead: f64,
    #[serde(flatten)]
    pub video: VideoOptions,
    pub override_video: bool,
    pub auto_end: bool,
    pub override_detection: bool,
    pub detection: DetectionSettings,
    pub reference_name: String,
    pub feedback: EndFeedback,
    /// Retain the original search ceiling after moving End to a found boundary.
    pub search_limit: Option<String>,
}
impl Default for StoredVideoEdit {
    fn default() -> Self {
        Self {
            clips: None,
            selected_clip: 0,
            start: time_text(0.0),
            end: String::new(),
            playhead: 0.0,
            video: VideoOptions::default(),
            override_video: false,
            auto_end: false,
            override_detection: false,
            detection: DetectionSettings::default(),
            reference_name: String::new(),
            feedback: EndFeedback::default(),
            search_limit: None,
        }
    }
}
impl VideoEdit {
    pub fn effective_video<'a>(&'a self, batch: &'a BatchOptions) -> &'a VideoOptions {
        if self.override_video {
            &self.video
        } else {
            &batch.video
        }
    }
    pub fn set_video_override(&mut self, enabled: bool, batch: &BatchOptions) {
        if enabled && !self.override_video {
            self.video = batch.video.clone();
        }
        self.override_video = enabled;
    }
    pub fn inherit_detection(&mut self, batch: &BatchOptions) {
        if !self.override_detection {
            self.detection = batch.detection.clone();
            self.reference_name = batch.reference_name.clone();
        }
        let final_index = self.clips.len().saturating_sub(1);
        for (index, clip) in self.clips.iter_mut().enumerate() {
            if !clip.override_auto_end {
                clip.auto_end = index == final_index && batch.auto_end;
            }
        }
    }
    pub fn reset_detection(&mut self, batch: &BatchOptions) {
        self.override_detection = false;
        for clip in &mut self.clips {
            clip.override_auto_end = false;
            clip.feedback = EndFeedback::NotSearched;
            clip.search_limit = None;
        }
        self.inherit_detection(batch);
        self.feedback = EndFeedback::NotSearched;
        self.search_limit = None;
    }
    pub fn preset(&self, batch: &BatchOptions) -> Self {
        let mut preset = self.clone();
        preset.video = self.effective_video(batch).clone();
        preset.override_video = true;
        preset.inherit_detection(batch);
        preset.override_detection = true;
        for clip in &mut preset.clips {
            clip.feedback = EndFeedback::NotSearched;
            clip.search_limit = None;
            clip.override_auto_end = true;
        }
        preset
    }
    pub fn from_preset(value: serde_json::Value) -> Result<Self> {
        let mut edit: Self = if let Some(video) = value.get("video") {
            serde_json::from_value(video.clone())?
        } else {
            Self::from_legacy(serde_json::from_value(value)?)
        };
        edit.override_video = true;
        edit.override_detection = true;
        for clip in &mut edit.clips {
            clip.feedback = EndFeedback::NotSearched;
            clip.search_limit = None;
            clip.override_auto_end = true;
        }
        // An empty selection is a valid editor/preset state: the queue skips it.
        // Export still requires at least one kept interval.
        if !edit.clips.is_empty() {
            edit.validate_clips(None)?;
        }
        Ok(edit)
    }
    pub fn settings(&self, batch: &BatchOptions) -> Result<Settings> {
        let video = self.effective_video(batch);
        let settings = Settings {
            start: parse_time(&self.start)?,
            end: optional_time(&self.end)?,
            encoder: video.encoder,
            quality: video.quality,
            resize: video.resize.enabled,
            resize_mode: video.resize.mode,
            width: video.resize.width,
            height: video.resize.height,
            keep_aspect: video.resize.keep_aspect,
            percent: video.resize.percent,
            height_percent: video.resize.height_percent,
            force: video.force,
            compare: video.compare,
            min_ssim: (video.compare && video.require_ssim).then_some(video.min_ssim),
            output_dir: batch.output_dir.clone(),
            replace_original: batch.replace_original,
            delete_original: !batch.replace_original && batch.delete_original,
            force_overwrite: batch.replace_original && batch.force_overwrite,
            // GUI searches are deliberate. Export uses the marker already shown.
            auto_trim: None,
            clip_number: None,
        };
        settings.validate()?;
        Ok(settings)
    }
    pub fn search_range(&mut self, duration: f64) -> Result<(f64, f64)> {
        self.detection.validate()?;
        let start = parse_time(&self.start)?;
        let current_end = self.end.clone();
        let limit = self.search_limit.get_or_insert(current_end);
        let end = optional_time(limit)?.unwrap_or(duration).min(duration);
        let next = self
            .clips
            .get(self.selected_clip + 1)
            .map(|clip| parse_time(&clip.start))
            .transpose()?
            .unwrap_or(duration);
        let end = end.min(next);
        ensure!(end > start, "The search range is empty.");
        Ok((start, end))
    }
    pub fn apply_match(&mut self, end: Option<f64>) {
        match end {
            Some(time) => {
                self.end = time_text(time);
                self.playhead = time;
                self.feedback = EndFeedback::Found(time);
            }
            None => self.feedback = EndFeedback::NoMatch,
        }
    }
    pub fn discard_auto_end(&mut self) {
        std::ops::DerefMut::deref_mut(self).discard_auto_end();
    }
    pub fn set_manual_end(&mut self, time: f64) {
        self.end = time_text(time);
        self.discard_auto_end();
    }
    pub fn mark_stale(&mut self, manual_end_changed: bool) {
        if manual_end_changed {
            self.search_limit = None;
        }
        self.feedback = if matches!(self.feedback, EndFeedback::NotSearched) {
            EndFeedback::NotSearched
        } else {
            EndFeedback::Stale
        };
    }
    pub fn same_search(&self, other: &Self) -> bool {
        self.detection == other.detection
            && self.clips.len() == other.clips.len()
            && self.clips.iter().zip(&other.clips).all(|(a, b)| {
                a.id == b.id && a.start == b.start && a.end == b.end && a.auto_end == b.auto_end
            })
    }
    pub fn validate_clips(&self, duration: Option<f64>) -> Result<()> {
        ensure!(
            !self.clips.is_empty(),
            "Add at least one clip to export this video."
        );
        let mut previous_end = 0.0;
        for (index, clip) in self.clips.iter().enumerate() {
            let begin = parse_time(&clip.start)?;
            let end = optional_time(&clip.end)?
                .unwrap_or(duration.unwrap_or(f64::INFINITY))
                .min(duration.unwrap_or(f64::INFINITY));
            ensure!(
                end - begin >= 0.05,
                "Clip {} is empty or outside the video.",
                index + 1
            );
            ensure!(
                begin >= previous_end - 0.00001,
                "Clip {} overlaps the previous clip.",
                index + 1
            );
            previous_end = end;
        }
        Ok(())
    }
    pub fn export_settings(
        &self,
        batch: &BatchOptions,
        duration: Option<f64>,
    ) -> Result<Vec<Settings>> {
        self.validate_clips(duration)?;
        (0..self.clips.len())
            .map(|index| {
                let mut edit = self.clone();
                edit.selected_clip = index;
                edit.settings(batch)
            })
            .collect()
    }
    pub fn select_clip(&mut self, index: usize) {
        if index < self.clips.len() {
            self.selected_clip = index;
        }
    }
    pub fn constrain_selected(&mut self, duration: f64) {
        if self.clips.is_empty() {
            return;
        }
        let index = self.selected_clip;
        let left = index
            .checked_sub(1)
            .and_then(|i| optional_time(&self.clips[i].end).ok().flatten())
            .unwrap_or(0.0);
        let right = self
            .clips
            .get(index + 1)
            .and_then(|clip| parse_time(&clip.start).ok())
            .unwrap_or(duration)
            .min(duration);
        let Ok(begin) = parse_time(&self.start) else {
            return;
        };
        let Ok(end) = optional_time(&self.end) else {
            return;
        };
        let begin = begin.clamp(left, (right - 0.05).max(left));
        let bounded_end = end
            .unwrap_or(duration)
            .clamp((begin + 0.05).min(right), right);
        if parse_time(&self.start).ok() != Some(begin) {
            self.start = time_text(begin);
        }
        if end.is_some_and(|end| end != bounded_end) || (end.is_none() && right < duration) {
            self.end = time_text(bounded_end);
        }
    }
    pub fn split_here(&mut self, duration: f64) -> Result<()> {
        ensure!(!self.clips.is_empty(), "Select a clip to split.");
        let (begin, end) = self.range(duration)?;
        let at = self.playhead;
        ensure!(
            at - begin >= 0.05 && end - at >= 0.05,
            "Place the playhead inside a clip, away from its markers."
        );
        let mut trailing = self.clips[self.selected_clip].clone();
        trailing.id = (1..)
            .find(|id| self.clips.iter().all(|clip| clip.id != *id))
            .unwrap();
        trailing.start = time_text(at);
        trailing.feedback = EndFeedback::NotSearched;
        trailing.search_limit = None;
        self.end = time_text(at);
        self.discard_auto_end();
        self.clips.insert(self.selected_clip + 1, trailing);
        self.selected_clip += 1;
        Ok(())
    }
    pub fn add_clip(&mut self, duration: f64) -> Result<()> {
        let at = self.playhead.clamp(0.0, duration);
        for clip in &self.clips {
            let (begin, end) = clip.range(duration)?;
            ensure!(
                at < begin || at >= end,
                "The playhead is already in a kept clip. Use Split here."
            );
        }
        let index = self
            .clips
            .iter()
            .position(|clip| parse_time(&clip.start).is_ok_and(|begin| begin > at))
            .unwrap_or(self.clips.len());
        let end = self
            .clips
            .get(index)
            .map(|clip| clip.start.clone())
            .unwrap_or_default();
        let limit = optional_time(&end)?.unwrap_or(duration);
        ensure!(limit - at >= 0.05, "There is no room for a clip here.");
        let id = (1..)
            .find(|id| self.clips.iter().all(|clip| clip.id != *id))
            .unwrap();
        self.clips.insert(
            index,
            Clip {
                id,
                start: time_text(at),
                end,
                ..Clip::default()
            },
        );
        self.selected_clip = index;
        Ok(())
    }
    pub fn remove_clip(&mut self) {
        if self.selected_clip < self.clips.len() {
            self.clips.remove(self.selected_clip);
        }
        self.selected_clip = self.selected_clip.min(self.clips.len().saturating_sub(1));
    }
    pub fn from_legacy(settings: Settings) -> Self {
        let video = VideoOptions {
            encoder: settings.encoder,
            quality: settings.quality,
            resize: settings.resize_options(),
            force: settings.force,
            compare: settings.compare || settings.min_ssim.is_some(),
            require_ssim: settings.min_ssim.is_some(),
            min_ssim: settings.min_ssim.unwrap_or(0.95),
        };
        Self {
            clips: vec![Clip {
                start: time_text(settings.start),
                end: settings.end.map(time_text).unwrap_or_default(),
                auto_end: settings.auto_trim.is_some(),
                override_auto_end: true,
                ..Clip::default()
            }],
            video,
            override_video: true,
            override_detection: true,
            detection: settings.auto_trim.unwrap_or_default(),
            ..Self::default()
        }
    }
}

pub fn optional_time(value: &str) -> Result<Option<f64>> {
    if value.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(parse_time(value)?))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BatchOptions {
    pub video: VideoOptions,
    pub auto_end: bool,
    pub detection: DetectionSettings,
    pub reference_name: String,
    pub output_dir: PathBuf,
    pub replace_original: bool,
    pub delete_original: bool,
    #[serde(alias = "keep_larger")]
    pub force_overwrite: bool,
}
impl Default for BatchOptions {
    fn default() -> Self {
        Self {
            video: VideoOptions::default(),
            auto_end: false,
            detection: DetectionSettings::default(),
            reference_name: String::new(),
            output_dir: PathBuf::new(),
            replace_original: true,
            delete_original: true,
            force_overwrite: false,
        }
    }
}

/// A preset can be applied to either settings tab. Only video presets carry
/// per-video markers; shared detector configuration is available in both scopes.
#[derive(Clone, Debug, PartialEq)]
pub enum Preset {
    Batch(BatchOptions),
    Video(VideoEdit),
}
impl Preset {
    pub fn from_value(value: serde_json::Value) -> Result<Self> {
        let object = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Expected a Tailout preset."))?;
        ensure!(
            !(object.contains_key("batch") && object.contains_key("video")),
            "Ambiguous preset scope."
        );
        let preset = if let Some(batch) = object.get("batch") {
            ensure!(
                object.get("scope").is_none_or(|scope| scope == "batch"),
                "Invalid preset scope."
            );
            Self::Batch(serde_json::from_value(batch.clone())?)
        } else {
            ensure!(
                object.get("scope").is_none_or(|scope| scope == "video"),
                "Invalid preset scope."
            );
            ensure!(
                object.contains_key("video")
                    || ["quality", "encoder", "start", "resize"]
                        .iter()
                        .any(|key| object.contains_key(*key)),
                "Expected a Tailout preset."
            );
            Self::Video(VideoEdit::from_preset(value)?)
        };
        // Validate processing independently of a destination which may still be
        // unchosen, or video markers that will not be imported into the batch.
        VideoEdit {
            video: preset.processing().clone(),
            override_video: true,
            ..Default::default()
        }
        .settings(&BatchOptions::default())?;
        match &preset {
            Self::Batch(value) if value.auto_end => value.detection.validate()?,
            Self::Video(value) if value.clips.iter().any(|clip| clip.auto_end) => {
                value.detection.validate()?
            }
            _ => {}
        }
        Ok(preset)
    }
    pub fn to_value(&self) -> serde_json::Value {
        match self {
            Self::Batch(batch) => serde_json::json!({"version":6,"scope":"batch","batch":batch}),
            Self::Video(video) => serde_json::json!({"version":6,"scope":"video","video":video}),
        }
    }
    fn processing(&self) -> &VideoOptions {
        match self {
            Self::Batch(batch) => &batch.video,
            Self::Video(video) => &video.video,
        }
    }
    pub fn apply_to_batch(self, batch: &mut BatchOptions) {
        match self {
            Self::Batch(value) => *batch = value,
            Self::Video(value) => {
                batch.auto_end = value.clips.last().is_some_and(|clip| clip.auto_end);
                batch.video = value.video;
                batch.detection = value.detection;
                batch.reference_name = value.reference_name;
            }
        }
    }
    pub fn apply_to_video(self, edit: &mut VideoEdit) {
        match self {
            Self::Batch(value) => {
                edit.video = value.video;
                edit.override_video = true;
                let final_index = edit.clips.len().saturating_sub(1);
                for (index, clip) in edit.clips.iter_mut().enumerate() {
                    clip.auto_end = index == final_index && value.auto_end;
                    clip.override_auto_end = true;
                    clip.feedback = EndFeedback::NotSearched;
                    clip.search_limit = None;
                }
                edit.detection = value.detection;
                edit.reference_name = value.reference_name;
                edit.override_detection = true;
                edit.feedback = EndFeedback::NotSearched;
                edit.search_limit = None;
            }
            Self::Video(value) => *edit = value,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewMode {
    #[default]
    #[serde(alias = "FitHeight", alias = "FitWidth")]
    Fit,
    Original,
    Stretch,
}
impl ViewMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Fit => "Fit",
            Self::Original => "Original",
            Self::Stretch => "Stretch",
        }
    }
    pub const ALL: [Self; 3] = [Self::Fit, Self::Original, Self::Stretch];
    pub fn size(self, viewport: [f32; 2], aspect: f32, original: [f32; 2]) -> [f32; 2] {
        let [w, h] = viewport;
        match self {
            Self::Fit => {
                let scale = (w / aspect).min(h);
                [scale * aspect, scale]
            }
            Self::Original => original,
            Self::Stretch => viewport,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueueSort {
    #[default]
    Alphabetical,
    Added,
    Shortest,
    Longest,
}
impl QueueSort {
    pub const ALL: [Self; 4] = [
        Self::Alphabetical,
        Self::Added,
        Self::Shortest,
        Self::Longest,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Alphabetical => "Alphabetical",
            Self::Added => "Added order",
            Self::Shortest => "Shortest first",
            Self::Longest => "Longest first",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgramOptions {
    pub recurse: bool,
    pub seek_step: f64,
    pub view: ViewMode,
    pub queue_sort: QueueSort,
    pub find_all_clips: bool,
}
impl Default for ProgramOptions {
    fn default() -> Self {
        Self {
            recurse: true,
            seek_step: 1.0,
            view: ViewMode::default(),
            queue_sort: QueueSort::default(),
            find_all_clips: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedVideo {
    pub path: PathBuf,
    pub relative_dir: PathBuf,
    pub edit: VideoEdit,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub videos: Vec<SavedVideo>,
    pub selected: Option<PathBuf>,
    pub batch: BatchOptions,
    pub program: ProgramOptions,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn batch_processing_changes_apply_to_existing_and_new_videos_without_changing_marker_state() {
        let mut batch = BatchOptions::default();
        let mut a = VideoEdit {
            clips: vec![Clip {
                start: "5".into(),
                end: "30".into(),
                auto_end: true,
                ..Clip::default()
            }],
            playhead: 12.0,
            ..Default::default()
        };
        a.detection.hold_seconds = 4.0;
        a.feedback = EndFeedback::Found(30.0);
        a.search_limit = Some("60".into());
        let mut b = VideoEdit {
            clips: vec![Clip {
                start: "10".into(),
                end: "40".into(),
                ..Clip::default()
            }],
            playhead: 15.0,
            ..Default::default()
        };
        b.detection.hold_seconds = 8.0;
        let originals = (a.clone(), b.clone());
        batch.video.quality = 18;
        batch.video.encoder = Encoder::Software;
        batch.video.resize.enabled = true;
        batch.video.resize.height = 480;
        batch.video.compare = true;
        batch.video.require_ssim = true;
        batch.video.min_ssim = 0.97;
        for edit in [&a, &b, &VideoEdit::default()] {
            let settings = edit.settings(&batch).unwrap();
            assert_eq!(settings.quality, 18);
            assert_eq!(settings.encoder, Encoder::Software);
            assert!(settings.resize && settings.compare);
            assert_eq!(settings.height, 480);
            assert_eq!(settings.min_ssim, Some(0.97));
        }
        assert_eq!(a.settings(&batch).unwrap().start, 5.0);
        assert_eq!(b.settings(&batch).unwrap().end, Some(40.0));
        assert_eq!(
            (a, b),
            originals,
            "markers, playheads, auto-end criteria and results are never copied"
        );
    }
    #[test]
    fn video_override_snapshots_defaults_and_can_resume_inheritance() {
        let mut batch = BatchOptions::default();
        batch.video.quality = 19;
        let mut edit = VideoEdit {
            clips: vec![Clip {
                start: "7".into(),
                end: "28".into(),
                auto_end: true,
                ..Clip::default()
            }],
            ..Default::default()
        };
        let detection = edit.detection.clone();
        edit.set_video_override(true, &batch);
        assert_eq!(edit.video.quality, 19);
        edit.video.quality = 16;
        edit.video.resize.enabled = true;
        batch.video.quality = 27;
        assert_eq!(edit.settings(&batch).unwrap().quality, 16);
        assert!(edit.settings(&batch).unwrap().resize);
        assert_eq!(VideoEdit::default().settings(&batch).unwrap().quality, 27);
        edit.set_video_override(false, &batch);
        assert_eq!(edit.settings(&batch).unwrap().quality, 27);
        assert!(!edit.settings(&batch).unwrap().resize);
        assert_eq!(
            (&edit.start, &edit.end),
            (&"7".to_owned(), &"28".to_owned())
        );
        assert!(edit.auto_end);
        assert_eq!(edit.detection, detection);
        edit.set_video_override(true, &batch);
        assert_eq!(
            edit.video.quality, 27,
            "new override starts from current batch settings"
        );
    }
    #[test]
    fn export_cleanup_defaults_persist_and_apply_only_to_folder_exports() {
        let mut batch = BatchOptions::default();
        let video = VideoEdit::default();
        assert!(batch.delete_original);
        assert!(!video.settings(&batch).unwrap().delete_original);
        batch.replace_original = false;
        batch.output_dir = "exports".into();
        assert!(video.settings(&batch).unwrap().delete_original);
        batch.delete_original = false;
        assert!(!video.settings(&batch).unwrap().delete_original);
        assert_eq!(
            Preset::from_value(Preset::Batch(batch.clone()).to_value()).unwrap(),
            Preset::Batch(batch.clone())
        );
        let loaded: BatchOptions =
            serde_json::from_value(serde_json::json!({"replace_original":false})).unwrap();
        assert!(
            loaded.delete_original,
            "older batch settings receive the requested default"
        );
        let saved = Session {
            batch,
            ..Default::default()
        };
        let restored: Session =
            serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
        assert!(
            !restored.batch.delete_original,
            "an explicit keep-originals choice survives restart"
        );
    }

    #[test]
    fn scoped_presets_roundtrip_and_keep_transient_search_state_out_of_loaded_videos() {
        let mut batch = BatchOptions {
            output_dir: "exports".into(),
            replace_original: false,
            force_overwrite: true,
            ..Default::default()
        };
        batch.video.quality = 18;
        let saved = Preset::Batch(batch.clone());
        assert_eq!(Preset::from_value(saved.to_value()).unwrap(), saved);
        let edit = VideoEdit {
            clips: vec![Clip {
                start: "5".into(),
                end: "25".into(),
                auto_end: true,
                feedback: EndFeedback::Searching,
                override_auto_end: true,
                search_limit: Some("30".into()),
                ..Clip::default()
            }],
            override_detection: true,
            ..Default::default()
        };
        let Preset::Video(loaded) =
            Preset::from_value(Preset::Video(edit.preset(&batch)).to_value()).unwrap()
        else {
            panic!("video preset");
        };
        assert!(loaded.override_video && loaded.auto_end);
        assert_eq!(loaded.video, batch.video);
        assert_eq!((&loaded.start, &loaded.end), (&edit.start, &edit.end));
        assert_eq!(loaded.detection, edit.detection);
        assert_eq!(loaded.feedback, EndFeedback::NotSearched);
        assert_eq!(loaded.search_limit, None);
        let legacy = Preset::from_value(
            serde_json::json!({"version":2,"video":{"quality":19,"start":"3","end":"20"}}),
        )
        .unwrap();
        assert!(
            matches!(legacy, Preset::Video(video) if video.video.quality == 19 && video.override_video)
        );
        assert!(matches!(
            Preset::from_value(serde_json::to_value(Settings::default()).unwrap()).unwrap(),
            Preset::Video(_)
        ));
    }
    #[test]
    fn cross_scope_presets_import_shared_settings_and_preserve_individual_markers() {
        let mut batch = BatchOptions {
            output_dir: "keep-folder".into(),
            ..Default::default()
        };
        let mut video = VideoEdit {
            clips: vec![Clip {
                start: "7".into(),
                end: "27".into(),
                auto_end: true,
                feedback: EndFeedback::Searching,
                search_limit: Some("40".into()),
                ..Clip::default()
            }],
            playhead: 13.0,
            override_detection: true,
            ..Default::default()
        };
        video.detection.hold_seconds = 5.0;
        let mut saved_video = video.preset(&batch);
        saved_video.video.quality = 16;
        Preset::Video(saved_video).apply_to_batch(&mut batch);
        assert_eq!(batch.video.quality, 16);
        assert_eq!(batch.output_dir, PathBuf::from("keep-folder"));
        assert!(batch.replace_original);
        let expected = video.clone();
        let batch_before = batch.clone();
        Preset::Batch(batch.clone()).apply_to_video(&mut video);
        let mut expected = expected;
        expected.video = batch.video.clone();
        expected.override_video = true;
        expected.override_detection = true;
        expected.auto_end = batch.auto_end;
        expected.override_auto_end = true;
        expected.detection = batch.detection.clone();
        expected.reference_name = batch.reference_name.clone();
        expected.feedback = EndFeedback::NotSearched;
        expected.search_limit = None;
        assert_eq!(video, expected);
        assert_eq!(batch, batch_before);
        batch.video.quality = 29;
        assert_eq!(video.effective_video(&batch).quality, 16);
        video.set_video_override(false, &batch);
        assert_eq!(video.effective_video(&batch).quality, 29);
    }
    #[test]
    fn batch_detection_inherits_without_copying_markers_and_manual_end_stays_local() {
        let mut batch = BatchOptions {
            auto_end: true,
            ..Default::default()
        };
        batch.detection.hold_seconds = 4.0;
        let mut first = VideoEdit {
            clips: vec![Clip {
                start: "5".into(),
                end: "95".into(),
                ..Clip::default()
            }],
            ..Default::default()
        };
        let mut second = VideoEdit {
            clips: vec![Clip {
                start: "8".into(),
                end: "98".into(),
                ..Clip::default()
            }],
            ..Default::default()
        };
        first.inherit_detection(&batch);
        second.inherit_detection(&batch);
        assert!(first.auto_end && second.auto_end);
        first.override_detection = true;
        first.detection.hold_seconds = 7.0;
        batch.detection.hold_seconds = 6.0;
        first.inherit_detection(&batch);
        second.inherit_detection(&batch);
        assert_eq!(first.detection.hold_seconds, 7.0);
        assert_eq!(second.detection.hold_seconds, 6.0);
        second.set_manual_end(90.0);
        second.inherit_detection(&batch);
        assert!(!second.auto_end);
        assert!(second.override_auto_end && !second.override_detection && !second.override_video);
        second.reset_detection(&batch);
        assert!(second.auto_end && !second.override_detection);
        assert_eq!(second.end, time_text(90.0));
        assert_eq!(first.start, "5");
        assert_eq!(first.end, "95");
    }

    #[test]
    fn presets_roundtrip_all_scoped_settings_including_shared_reference_histograms() {
        let mut batch = BatchOptions {
            auto_end: true,
            output_dir: "exports".into(),
            replace_original: false,
            delete_original: false,
            force_overwrite: true,
            ..Default::default()
        };
        batch.video.encoder = Encoder::Software;
        batch.video.quality = 18;
        batch.video.resize.enabled = true;
        batch.video.resize.keep_aspect = false;
        batch.video.compare = true;
        batch.video.require_ssim = true;
        batch.video.min_ssim = 0.9;
        batch.detection.reference_enabled = true;
        batch.detection.reference = Some(crate::detect::Histogram::from_rgb(&[64, 128, 192]));
        batch.detection.hold_seconds = 3.5;
        batch.detection.still_changed_fraction = 0.035;
        batch.detection.still_pixel_tolerance = 22;
        batch.reference_name = "reference.png".into();
        let Preset::Batch(loaded) =
            Preset::from_value(Preset::Batch(batch.clone()).to_value()).unwrap()
        else {
            panic!()
        };
        assert_eq!(loaded, batch);
        let edit = VideoEdit {
            clips: vec![Clip {
                start: "3".into(),
                end: "23".into(),
                ..Clip::default()
            }],
            playhead: 8.0,
            ..Default::default()
        };
        let expected = edit.preset(&batch);
        let Preset::Video(loaded) =
            Preset::from_value(Preset::Video(expected.clone()).to_value()).unwrap()
        else {
            panic!()
        };
        assert_eq!(loaded, expected);
        assert!(loaded.override_video && loaded.override_detection);
        assert_eq!(loaded.detection.reference, batch.detection.reference);
        assert!(
            Preset::from_value(
                serde_json::json!({"batch":{"auto_end":true,"detection":{"hold_seconds":0}}})
            )
            .is_err()
        );
    }

    #[test]
    fn preset_scope_and_processing_errors_are_rejected_before_application() {
        for value in [
            serde_json::json!({}),
            serde_json::json!([]),
            serde_json::json!({"scope":"batch","video":{}}),
            serde_json::json!({"scope":"video","batch":{}}),
            serde_json::json!({"batch":{},"video":{}}),
            serde_json::json!({"scope":"batch","batch":{"video":{"quality":99}}}),
            serde_json::json!({"video":{"quality":0}}),
        ] {
            assert!(Preset::from_value(value).is_err());
        }
        // A destination can still be unchosen when saving/loading batch defaults.
        assert!(
            Preset::from_value(serde_json::json!({"batch":{"replace_original":false}})).is_ok()
        );
    }
    #[test]
    fn presets_capture_effective_settings_and_older_presets_load_as_overrides() {
        let mut batch = BatchOptions::default();
        batch.video.quality = 17;
        let edit = VideoEdit {
            clips: vec![Clip {
                start: "6".into(),
                end: "26".into(),
                ..Clip::default()
            }],
            ..Default::default()
        };
        let encoded = serde_json::json!({"version":3,"video":edit.preset(&batch)});
        let loaded = VideoEdit::from_preset(encoded).unwrap();
        batch.video.quality = 29;
        assert!(loaded.override_video);
        assert_eq!(loaded.settings(&batch).unwrap().quality, 17);
        assert_eq!(loaded.start, "6");
        assert_eq!(loaded.end, "26");
        let old=VideoEdit::from_preset(serde_json::json!({"version":2,"video":{"quality":18,"start":"3","end":"20","force":false}})).unwrap();
        assert_eq!(old.settings(&batch).unwrap().quality, 18);
        assert!(!old.settings(&batch).unwrap().force);
        assert_eq!(old.start, "3");
        let legacy = VideoEdit::from_preset(
            serde_json::to_value(Settings {
                quality: 24,
                start: 2.0,
                end: Some(22.0),
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        assert!(legacy.override_video);
        assert_eq!(legacy.settings(&batch).unwrap().quality, 24);
        assert_eq!(legacy.settings(&batch).unwrap().end, Some(22.0));
    }
    #[test]
    fn videos_export_independent_settings_and_markers() {
        let batch = BatchOptions {
            output_dir: "out".into(),
            ..Default::default()
        };
        let a = VideoEdit {
            clips: vec![Clip {
                start: "5".into(),
                end: "20".into(),
                ..Clip::default()
            }],
            video: VideoOptions {
                quality: 18,
                ..Default::default()
            },
            override_video: true,
            ..Default::default()
        };
        let b = VideoEdit {
            clips: vec![Clip {
                start: "2".into(),
                end: "10".into(),
                ..Clip::default()
            }],
            video: VideoOptions {
                quality: 26,
                ..Default::default()
            },
            override_video: true,
            ..Default::default()
        };
        assert_eq!(
            (
                a.settings(&batch).unwrap().start,
                a.settings(&batch).unwrap().quality
            ),
            (5.0, 18)
        );
        assert_eq!(
            (
                b.settings(&batch).unwrap().end,
                b.settings(&batch).unwrap().quality
            ),
            (Some(10.0), 26)
        );
    }
    #[test]
    fn found_marker_is_used_and_search_is_not_silently_repeated() {
        let mut edit = VideoEdit {
            clips: vec![Clip {
                auto_end: true,
                ..Clip::default()
            }],
            ..Default::default()
        };
        assert_eq!(edit.search_range(120.0).unwrap(), (0.0, 120.0));
        edit.apply_match(Some(90.0));
        assert_eq!(edit.end, time_text(90.0));
        assert_eq!(edit.search_range(120.0).unwrap(), (0.0, 120.0));
        edit.detection.hold_seconds = 20.0;
        edit.mark_stale(false);
        assert_eq!(edit.end, time_text(90.0));
        let batch = BatchOptions {
            output_dir: "out".into(),
            ..Default::default()
        };
        assert!(edit.settings(&batch).unwrap().auto_trim.is_none());
        assert_eq!(edit.settings(&batch).unwrap().end, Some(90.0));
    }
    #[test]
    fn no_match_keeps_end_and_manual_edits_reset_search_ceiling() {
        let mut edit = VideoEdit {
            clips: vec![Clip {
                end: "80".into(),
                ..Clip::default()
            }],
            ..Default::default()
        };
        edit.search_range(100.0).unwrap();
        edit.apply_match(Some(60.0));
        edit.apply_match(None);
        assert_eq!(edit.end, time_text(60.0));
        assert_eq!(edit.feedback, EndFeedback::NoMatch);
        edit.end = "70".into();
        edit.mark_stale(true);
        assert_eq!(edit.search_range(100.0).unwrap().1, 70.0);
    }
    #[test]
    fn session_roundtrip_keeps_per_video_and_program_options() {
        let session = Session {
            videos: vec![
                SavedVideo {
                    path: "a.mp4".into(),
                    relative_dir: PathBuf::new(),
                    edit: VideoEdit {
                        video: VideoOptions {
                            quality: 18,
                            ..Default::default()
                        },
                        override_video: true,
                        ..Default::default()
                    },
                },
                SavedVideo {
                    path: "b.mp4".into(),
                    relative_dir: PathBuf::new(),
                    edit: VideoEdit {
                        video: VideoOptions {
                            quality: 27,
                            ..Default::default()
                        },
                        override_video: true,
                        ..Default::default()
                    },
                },
            ],
            ..Default::default()
        };
        let copy: Session = serde_json::from_slice(&serde_json::to_vec(&session).unwrap()).unwrap();
        assert_eq!(copy.videos[0].edit.video.quality, 18);
        assert_eq!(copy.videos[1].edit.video.quality, 27);
    }
    #[test]
    fn preview_fit_preserves_aspect_and_accepts_legacy_modes() {
        assert_eq!(
            ViewMode::Fit.size([300.0, 400.0], 2.0, [1920.0, 960.0]),
            [300.0, 150.0]
        );
        assert_eq!(
            ViewMode::Stretch.size([300.0, 400.0], 2.0, [1920.0, 960.0]),
            [300.0, 400.0]
        );
        for legacy in ["FitWidth", "FitHeight"] {
            let mode: ViewMode = serde_json::from_str(&format!("\"{legacy}\"")).unwrap();
            assert_eq!(mode, ViewMode::Fit);
        }
    }
    #[test]
    fn clips_split_add_remove_and_allow_an_empty_selection() {
        let mut edit = VideoEdit {
            playhead: 10.0,
            ..Default::default()
        };
        edit.split_here(30.0).unwrap();
        assert_eq!(edit.clips.len(), 2);
        assert_eq!(edit.selected_clip, 1);
        assert_eq!(edit.clips[0].range(30.0).unwrap(), (0.0, 10.0));
        assert_eq!(edit.clips[1].range(30.0).unwrap(), (10.0, 30.0));
        edit.start = "20".into();
        edit.validate_clips(Some(30.0)).unwrap();
        edit.playhead = 15.0;
        edit.add_clip(30.0).unwrap();
        assert_eq!(edit.selected_clip, 1);
        assert_eq!(edit.range(30.0).unwrap(), (15.0, 20.0));
        assert_ne!(edit.clips[1].id, edit.clips[2].id);
        edit.remove_clip();
        assert_eq!(edit.clips.len(), 2);
        edit.remove_clip();
        edit.remove_clip();
        assert!(edit.clips.is_empty());
        assert!(
            edit.export_settings(&BatchOptions::default(), Some(30.0))
                .is_err()
        );
        edit.playhead = 3.0;
        edit.add_clip(30.0).unwrap();
        assert_eq!(edit.range(30.0).unwrap(), (3.0, 30.0));
    }
    #[test]
    fn clip_boundaries_cannot_cross_neighbors_and_exports_reject_overlap() {
        let mut edit = VideoEdit {
            playhead: 10.0,
            ..Default::default()
        };
        edit.split_here(30.0).unwrap();
        edit.start = "20".into();
        edit.selected_clip = 0;
        edit.end = "25".into();
        assert!(edit.validate_clips(Some(30.0)).is_err());
        edit.constrain_selected(30.0);
        assert_eq!(edit.end, time_text(20.0));
        edit.selected_clip = 1;
        edit.start = "5".into();
        edit.constrain_selected(30.0);
        assert_eq!(edit.start, time_text(20.0));
        edit.validate_clips(Some(30.0)).unwrap();
        assert!(edit.add_clip(30.0).is_err());
    }
    #[test]
    fn clip_detection_inherits_on_final_clip_and_manual_overrides_are_local() {
        let mut batch = BatchOptions {
            auto_end: true,
            ..Default::default()
        };
        let mut edit = VideoEdit::default();
        edit.inherit_detection(&batch);
        edit.playhead = 150.0;
        edit.split_here(400.0).unwrap();
        assert!(!edit.clips[0].auto_end && edit.clips[1].auto_end);
        edit.set_manual_end(350.0);
        assert!(!edit.auto_end && edit.override_auto_end);
        assert!(!edit.override_detection);
        batch.detection.hold_seconds = 5.0;
        edit.inherit_detection(&batch);
        assert!(!edit.auto_end);
        assert_eq!(edit.detection.hold_seconds, 5.0);
        edit.reset_detection(&batch);
        assert!(edit.auto_end);
        assert_eq!(edit.end, time_text(350.0));
    }
    #[test]
    fn clip_presets_migrate_legacy_and_roundtrip_every_range_without_search_state() {
        let legacy = serde_json::json!({"version":5,"scope":"video","video":{"start":"2","end":"20","auto_end":true,"override_detection":true}});
        let Preset::Video(mut edit) = Preset::from_value(legacy).unwrap() else {
            panic!()
        };
        assert_eq!(edit.clips.len(), 1);
        assert!(edit.auto_end && edit.override_auto_end);
        edit.playhead = 10.0;
        edit.split_here(30.0).unwrap();
        edit.feedback = EndFeedback::Searching;
        let preset = Preset::Video(edit.preset(&BatchOptions::default()));
        let value = preset.to_value();
        assert_eq!(value["version"], 6);
        let Preset::Video(restored) = Preset::from_value(value).unwrap() else {
            panic!()
        };
        assert_eq!(restored.clips.len(), 2);
        assert_eq!(restored.selected_clip, 1);
        assert!(
            restored.clips.iter().all(
                |clip| clip.feedback == EndFeedback::NotSearched && clip.search_limit.is_none()
            )
        );
        assert_eq!(restored.clips[0].end, time_text(10.0));
        assert_eq!(restored.clips[1].start, time_text(10.0));
        edit.clips.clear();
        let empty = Preset::Video(edit.preset(&BatchOptions::default()));
        let Preset::Video(restored) = Preset::from_value(empty.to_value()).unwrap() else {
            panic!()
        };
        assert!(restored.clips.is_empty());
        assert!(
            restored
                .export_settings(&BatchOptions::default(), Some(30.0))
                .is_err()
        );
    }
    #[test]
    fn clip_search_ceiling_stops_before_the_next_kept_range() {
        let mut edit = VideoEdit {
            playhead: 150.0,
            ..Default::default()
        };
        edit.split_here(400.0).unwrap();
        edit.selected_clip = 0;
        edit.end = "140".into();
        edit.search_limit = Some("400".into());
        assert_eq!(edit.search_range(400.0).unwrap(), (0.0, 150.0));
        edit.apply_match(None);
        assert_eq!(edit.end, "140");
    }
    #[test]
    fn clip_exports_share_encoding_but_keep_independent_markers_and_preset_scope() {
        let mut batch = BatchOptions::default();
        batch.video.quality = 19;
        let mut edit = VideoEdit {
            playhead: 10.0,
            ..Default::default()
        };
        edit.split_here(30.0).unwrap();
        edit.start = "20".into();
        let settings = edit.export_settings(&batch, Some(30.0)).unwrap();
        assert_eq!(settings.len(), 2);
        assert!(settings.iter().all(|value| value.quality == 19));
        assert_eq!(settings[0].range(30.0, None).unwrap(), (0.0, 10.0));
        assert_eq!(settings[1].range(30.0, None).unwrap(), (20.0, 30.0));
        let before = edit
            .clips
            .iter()
            .map(|clip| (clip.start.clone(), clip.end.clone()))
            .collect::<Vec<_>>();
        batch.auto_end = true;
        Preset::Batch(batch).apply_to_video(&mut edit);
        assert_eq!(
            before,
            edit.clips
                .iter()
                .map(|clip| (clip.start.clone(), clip.end.clone()))
                .collect::<Vec<_>>()
        );
        assert!(!edit.clips[0].auto_end && edit.clips[1].auto_end);
    }
}
