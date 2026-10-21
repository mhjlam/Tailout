use eframe::egui::{self, Color32, RichText, TextureHandle};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tailout::{
    detect::Criterion,
    editor::{
        BatchOptions, EndFeedback, Preset, ProgramOptions, QueueSort, Session, VideoEdit,
        VideoOptions, ViewMode, optional_time,
    },
    media::{self, Conversion, ConversionProgress, FrameImage, Input, MediaInfo, Tools},
    model::{Encoder, ResizeMode, parse_time, time_text},
    process::Cancel,
};

const GREEN: Color32 = Color32::from_rgb(104, 221, 154);
const RED: Color32 = Color32::from_rgb(244, 119, 122);

struct Item {
    id: u64,
    input: Input,
    edit: VideoEdit,
    info: Option<MediaInfo>,
    status: String,
    error: Option<String>,
    result: Option<Conversion>,
    probe_ticket: u64,
    probe_pending: bool,
    probe_cancel: Cancel,
    find_ticket: u64,
    find_cancel: Cancel,
    find_progress: f32,
    find_started: Option<Instant>,
    reference_ticket: u64,
}
impl Item {
    fn error_message(&self) -> Option<&str> {
        self.error
            .as_deref()
            .filter(|message| !message.trim().is_empty())
            .or_else(|| match &self.edit.feedback {
                EndFeedback::Failed(message) if !message.trim().is_empty() => {
                    Some(message.as_str())
                }
                _ => None,
            })
    }
    fn new(id: u64, input: Input, edit: VideoEdit) -> Self {
        Self {
            id,
            input,
            edit,
            info: None,
            status: String::new(),
            error: None,
            result: None,
            probe_ticket: 0,
            probe_pending: false,
            probe_cancel: Cancel::default(),
            find_ticket: 0,
            find_cancel: Cancel::default(),
            find_progress: 0.0,
            find_started: None,
            reference_ticket: 0,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct DeleteTarget {
    id: u64,
    path: PathBuf,
}
#[derive(Clone)]
enum DeleteState {
    Confirm(Vec<DeleteTarget>),
    Running(Vec<DeleteTarget>, usize),
}

enum Message {
    Added(Result<Vec<Input>, String>),
    Probed(u64, u64, Result<MediaInfo, String>),
    Preview(u64, u64, f64, Result<FrameImage, String>),
    Reference(u64, u64, Result<(FrameImage, String), String>),
    BatchReference(u64, Result<(FrameImage, String), String>),
    Found(u64, u64, Result<Option<f64>, String>),
    FindProgress(u64, u64, f64, Option<(usize, usize)>),
    Progress(u64, usize, usize, ConversionProgress),
    Converted(u64, Result<Conversion, String>),
    DeleteProgress(DeleteTarget),
    Deleted(Vec<DeleteTarget>, Vec<Result<(), String>>),
    AnalysisDone,
    Finished(Vec<u64>),
}
struct PreviewRequest {
    id: u64,
    epoch: u64,
    path: PathBuf,
    time: f64,
    cancel: Cancel,
}
#[derive(Clone, Copy)]
enum DragTarget {
    Playhead,
    Begin,
    End,
}
#[derive(Default)]
struct Actions {
    find: bool,
    cancel_find: bool,
    reference: bool,
    use_frame: bool,
}
#[derive(Clone, Copy)]
struct DetectionScope {
    batch: bool,
    can_find: bool,
}
#[derive(Clone, Copy)]
struct SearchActivity {
    progress: f32,
    elapsed: Option<Duration>,
    batch: bool,
}

struct ExportActivity {
    id: u64,
    event: ConversionProgress,
    started: Instant,
    last_advance: Instant,
    last_report: Option<Instant>,
    measured_seconds: f64,
    measured_frames: u64,
    measured_bytes: u64,
}
impl ExportActivity {
    fn new(id: u64, event: ConversionProgress, now: Instant) -> Self {
        Self {
            id,
            last_report: event.stats.as_ref().map(|_| now),
            measured_seconds: event
                .stats
                .as_ref()
                .and_then(|stats| stats.seconds)
                .unwrap_or(0.0),
            measured_frames: event
                .stats
                .as_ref()
                .and_then(|stats| stats.frame)
                .unwrap_or(0),
            measured_bytes: event
                .stats
                .as_ref()
                .and_then(|stats| stats.bytes)
                .unwrap_or(0),
            event,
            started: now,
            last_advance: now,
        }
    }
    fn update(&mut self, mut event: ConversionProgress, now: Instant) {
        if event.phase != self.event.phase {
            let started = self.started;
            event.overall = event.overall.max(self.event.overall);
            *self = Self::new(self.id, event, now);
            self.started = started;
            return;
        }
        let fraction_advanced = event
            .fraction
            .zip(self.event.fraction)
            .is_some_and(|(a, b)| a > b)
            || (self.event.fraction.is_none() && event.fraction.is_some_and(|value| value > 0.0));
        let seconds = event
            .stats
            .as_ref()
            .and_then(|stats| stats.seconds)
            .unwrap_or(0.0);
        let frames = event
            .stats
            .as_ref()
            .and_then(|stats| stats.frame)
            .unwrap_or(0);
        let bytes = event
            .stats
            .as_ref()
            .and_then(|stats| stats.bytes)
            .unwrap_or(0);
        let stats_advanced = seconds > self.measured_seconds
            || frames > self.measured_frames
            || bytes > self.measured_bytes;
        self.measured_seconds = self.measured_seconds.max(seconds);
        self.measured_frames = self.measured_frames.max(frames);
        self.measured_bytes = self.measured_bytes.max(bytes);
        if fraction_advanced || stats_advanced {
            self.last_advance = now;
        }
        if event.stats.is_some() {
            self.last_report = Some(now);
        }
        // Some encoders emit a report without a timestamp while buffering.
        event.fraction = match (event.fraction, self.event.fraction) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        event.overall = event.overall.max(self.event.overall);
        self.event = event;
    }
    fn phase_label(&self) -> String {
        let encoder = match self.event.encoder {
            Some("hevc_nvenc") => Some("NVIDIA HEVC"),
            Some("libx265") => Some("HEVC (x265)"),
            value => value,
        };
        match encoder {
            Some(encoder) => format!("{} · {encoder}", self.event.phase),
            None => self.event.phase.into(),
        }
    }
    fn idle(&self, now: Instant) -> Option<Duration> {
        let elapsed = now.saturating_duration_since(self.last_advance);
        (elapsed >= Duration::from_secs(30)).then_some(elapsed)
    }
    fn summary(&self, now: Instant) -> String {
        let mut text = format!(
            "Elapsed {}",
            compact_time(
                now.saturating_duration_since(self.started)
                    .as_secs_f64()
                    .floor()
            )
        );
        if self.event.phase == "Encoding"
            && self.event.fraction.is_none_or(|value| value == 0.0)
            && self.measured_frames > 0
        {
            text.push_str(&format!(" · {} frames", self.measured_frames));
        }
        if let Some(speed) = self.event.stats.as_ref().and_then(|stats| stats.speed) {
            text.push_str(&format!(" · {speed:.2}×"));
        }
        text
    }
    fn diagnostics(&self, now: Instant) -> String {
        let mut text = String::new();
        if let Some(stats) = &self.event.stats {
            if let Some(frame) = stats.frame {
                text.push_str(&format!("Frames: {frame}\n"));
            }
            if let Some(seconds) = stats.seconds {
                text.push_str(&format!("Processed time: {}\n", time_text(seconds)));
            }
            if let Some(bytes) = stats.bytes {
                text.push_str(&format!(
                    "Output written: {:.1} MiB\n",
                    bytes as f64 / 1048576.0
                ));
            }
        }
        if let Some(last) = self.last_report {
            text.push_str(&format!(
                "Last FFmpeg report: {} s ago",
                now.saturating_duration_since(last).as_secs()
            ));
        } else {
            text.push_str("Waiting for the first backend progress report.");
        }
        text
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
enum SettingsTab {
    #[default]
    Batch,
    Video,
}

pub struct App {
    tools: Tools,
    items: Vec<Item>,
    selected: Option<u64>,
    queue_selection: BTreeSet<u64>,
    selection_anchor: Option<u64>,
    next_id: u64,
    queue_sort_dirty: bool,
    delete_state: Option<DeleteState>,
    batch: BatchOptions,
    settings_tab: SettingsTab,
    program: ProgramOptions,
    texture: Option<TextureHandle>,
    frame: Option<FrameImage>,
    frame_time: Option<f64>,
    pan: egui::Vec2,
    drag: Option<DragTarget>,
    wheel: f32,
    preview_epoch: u64,
    preview_cancel: Cancel,
    preview_tx: Option<Sender<PreviewRequest>>,
    preview_worker: Option<JoinHandle<()>>,
    tx: Sender<Message>,
    rx: Receiver<Message>,
    threads: Vec<JoinHandle<()>>,
    cancel: Cancel,
    auxiliary_cancel: Cancel,
    analysis_cancel: Cancel,
    batch_reference_ticket: u64,
    busy: bool,
    collecting: bool,
    finding_batch: bool,
    queued_find: bool,
    status: String,
    error: Option<String>,
    progress: f32,
    export_activity: Option<ExportActivity>,
    state_path: Option<PathBuf>,
    save_at: Option<Instant>,
    #[cfg(debug_assertions)]
    smoke: Option<(PathBuf, Instant, bool)>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        let mut style = (*cc.egui_ctx.global_style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.interaction.selectable_labels = false;
        style.visuals.panel_fill = Color32::from_rgb(23, 26, 30);
        style.visuals.selection.bg_fill = Color32::from_rgb(39, 75, 59);
        style.visuals.selection.stroke.color = GREEN;
        cc.egui_ctx.set_global_style(style);
        #[cfg(not(test))]
        let state_path = session_path();
        #[cfg(test)]
        let state_path: Option<PathBuf> = None;
        let mut load_error = None;
        let session = load_preferences(state_path.as_deref()).unwrap_or_else(|error| {
            load_error = Some(format!("Could not restore preferences: {error}"));
            Session::default()
        });
        Self::with_session(session, state_path, load_error)
    }
    fn with_session(
        session: Session,
        state_path: Option<PathBuf>,
        load_error: Option<String>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let (preview_tx, preview_rx) = mpsc::channel::<PreviewRequest>();
        let sender = tx.clone();
        let preview_tools = Tools::default();
        // One decoder, latest queued request wins. In-flight frames can finish
        // while scrubbing, so constant mouse motion never starves the preview.
        let preview_worker = thread::spawn(move || {
            while let Ok(mut request) = preview_rx.recv() {
                for newer in preview_rx.try_iter() {
                    request = newer;
                }
                if request.cancel.is_cancelled() {
                    continue;
                }
                let result = preview_tools
                    .preview(&request.path, request.time, &request.cancel)
                    .map_err(|e| format!("{e:#}"));
                let _ = sender.send(Message::Preview(
                    request.id,
                    request.epoch,
                    request.time,
                    result,
                ));
            }
        });
        let mut app = Self {
            tools: Tools::default(),
            items: Vec::new(),
            selected: None,
            queue_selection: BTreeSet::new(),
            selection_anchor: None,
            next_id: 1,
            queue_sort_dirty: false,
            delete_state: None,
            batch: session.batch,
            settings_tab: SettingsTab::default(),
            program: session.program,
            texture: None,
            frame: None,
            frame_time: None,
            pan: egui::Vec2::ZERO,
            drag: None,
            wheel: 0.0,
            preview_epoch: 0,
            preview_cancel: Cancel::default(),
            preview_tx: Some(preview_tx),
            preview_worker: Some(preview_worker),
            tx,
            rx,
            threads: Vec::new(),
            cancel: Cancel::default(),
            auxiliary_cancel: Cancel::default(),
            analysis_cancel: Cancel::default(),
            batch_reference_ticket: 0,
            busy: false,
            collecting: false,
            finding_batch: false,
            queued_find: false,
            status: String::new(),
            error: load_error,
            progress: 0.0,
            export_activity: None,
            state_path,
            save_at: None,
            #[cfg(debug_assertions)]
            smoke: std::env::var_os("TAILOUT_UI_SMOKE").map(|p| (p.into(), Instant::now(), false)),
        };
        if !app.program.seek_step.is_finite() || !(0.01..=60.0).contains(&app.program.seek_step) {
            app.program.seek_step = 1.0;
        }
        // Restore preferences, never reopen a queue from a previous launch.
        app.add(
            std::env::args_os()
                .skip(1)
                .map(PathBuf::from)
                .filter(|p| media::supported_video(p))
                .map(Input::new)
                .collect(),
        );
        #[cfg(debug_assertions)]
        if app.smoke.is_some()
            && let Some(index) = app.selected_index()
        {
            app.items[index].edit.auto_end = true;
        }
        app
    }
    fn selected_index(&self) -> Option<usize> {
        let id = self.selected?;
        self.items.iter().position(|item| item.id == id)
    }
    fn index(&self, id: u64) -> Option<usize> {
        self.items.iter().position(|item| item.id == id)
    }
    fn locked(&self) -> bool {
        self.busy || self.finding_batch || self.delete_state.is_some()
    }
    fn searching(&self) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item.edit.feedback, EndFeedback::Searching))
    }
    fn spawn(&mut self, action: impl FnOnce(Sender<Message>) + Send + 'static) {
        self.threads.retain(|t| !t.is_finished());
        let tx = self.tx.clone();
        self.threads.push(thread::spawn(move || action(tx)));
    }
    fn dirty(&mut self) {
        self.save_at = Some(Instant::now() + Duration::from_millis(800));
    }
    fn session(&self) -> Session {
        Session {
            batch: self.batch.clone(),
            program: self.program.clone(),
            ..Default::default()
        }
    }
    fn save(&mut self) {
        self.save_at = None;
        let Some(path) = &self.state_path else {
            return;
        };
        let result = (|| -> anyhow::Result<()> {
            use std::io::Write;
            let directory = path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("Invalid session path"))?;
            std::fs::create_dir_all(directory)?;
            let mut file = tempfile::NamedTempFile::new_in(directory)?;
            file.write_all(&serde_json::to_vec_pretty(&self.session())?)?;
            file.as_file().sync_all()?;
            file.persist(path)?;
            Ok(())
        })();
        if let Err(e) = result {
            self.error = Some(format!("Could not save session: {e}"));
        }
    }
    fn add(&mut self, inputs: Vec<Input>) {
        for mut input in inputs {
            if let Ok(path) = std::fs::canonicalize(&input.path) {
                input.path = path;
            }
            if self.items.iter().any(|item| item.input.path == input.path) {
                continue;
            }
            let id = self.next_id;
            self.next_id += 1;
            let mut edit = VideoEdit::default();
            edit.inherit_detection(&self.batch);
            self.items.push(Item::new(id, input, edit));
        }
        self.queue_sort_dirty = true;
        self.sort_queue();
        if self.selected.is_none()
            && let Some(item) = self.items.first()
        {
            self.select(item.id);
        }
        self.probe_queue();
        self.dirty();
    }
    fn sort_queue(&mut self) {
        // Keep a running export/search in the order it started. Sorting resumes
        // once it finishes, including duration updates received in the meantime.
        if !self.queue_sort_dirty || self.locked() {
            return;
        }
        self.queue_sort_dirty = false;
        let order = self.program.queue_sort;
        match order {
            QueueSort::Alphabetical => self.items.sort_by_cached_key(|item| {
                (
                    item.input
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_lowercase(),
                    item.id,
                )
            }),
            QueueSort::Added => self.items.sort_by_key(|item| item.id),
            QueueSort::Shortest | QueueSort::Longest => {
                let duration = |item: &Item| {
                    item.info
                        .as_ref()
                        .map(|info| info.duration)
                        .filter(|duration| duration.is_finite() && *duration > 0.0)
                };
                self.items.sort_by(|a, b| {
                    let compare = match (duration(a), duration(b)) {
                        (Some(a), Some(b)) if order == QueueSort::Shortest => a.total_cmp(&b),
                        (Some(a), Some(b)) => b.total_cmp(&a),
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => std::cmp::Ordering::Equal,
                    };
                    compare.then_with(|| a.id.cmp(&b.id))
                });
            }
        }
    }

    fn select(&mut self, id: u64) {
        if self.index(id).is_none() {
            return;
        }
        self.queue_selection.clear();
        self.queue_selection.insert(id);
        self.selection_anchor = Some(id);
        if self.selected != Some(id) {
            self.activate_video(id);
        }
    }
    fn queue_click(&mut self, id: u64, modifiers: egui::Modifiers) {
        if self.delete_state.is_some() {
            return;
        }
        let Some(index) = self.index(id) else {
            return;
        };
        let additive = modifiers.ctrl || modifiers.command;
        if modifiers.shift {
            let anchor = self
                .selection_anchor
                .or(self.selected)
                .and_then(|anchor| self.index(anchor))
                .unwrap_or(index);
            if !additive {
                self.queue_selection.clear();
            }
            for item in &self.items[anchor.min(index)..=anchor.max(index)] {
                self.queue_selection.insert(item.id);
            }
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.items[anchor].id);
            }
            if self.selected != Some(id) {
                self.activate_video(id);
            }
        } else if additive {
            self.selection_anchor = Some(id);
            if self.queue_selection.remove(&id) {
                if self.selected == Some(id) {
                    let next = self
                        .items
                        .iter()
                        .find(|item| self.queue_selection.contains(&item.id))
                        .map(|item| item.id);
                    self.clear_active_video();
                    if let Some(next) = next {
                        self.activate_video(next);
                    }
                }
            } else {
                self.queue_selection.insert(id);
                if self.selected != Some(id) {
                    self.activate_video(id);
                }
            }
        } else {
            self.select(id);
        }
    }
    fn clear_active_video(&mut self) {
        self.selected = None;
        self.preview_cancel.cancel();
        self.preview_epoch += 1;
        self.texture = None;
        self.frame = None;
        self.frame_time = None;
        self.drag = None;
        self.pan = egui::Vec2::ZERO;
        self.wheel = 0.0;
    }
    fn activate_video(&mut self, id: u64) {
        let Some(index) = self.index(id) else {
            return;
        };
        self.selected = Some(id);
        self.preview_epoch += 1;
        self.preview_cancel.cancel();
        self.preview_cancel = Cancel::default();
        self.texture = None;
        self.frame = None;
        self.frame_time = None;
        self.pan = egui::Vec2::ZERO;
        self.drag = None;
        self.wheel = 0.0;
        self.error = None;
        if self.items[index].info.is_some() {
            self.request_preview();
        } else if !self.items[index].probe_pending {
            self.probe(id);
        }
        self.dirty();
    }
    fn probe(&mut self, id: u64) {
        let Some(index) = self.index(id) else {
            return;
        };
        let item = &mut self.items[index];
        item.probe_cancel.cancel();
        item.probe_cancel = Cancel::default();
        item.probe_ticket += 1;
        item.probe_pending = true;
        let ticket = item.probe_ticket;
        let cancel = item.probe_cancel.clone();
        let path = item.input.path.clone();
        let tools = self.tools.clone();
        self.spawn(move |tx| {
            let _ = tx.send(Message::Probed(
                id,
                ticket,
                tools.probe(&path, &cancel).map_err(|e| format!("{e:#}")),
            ));
        });
    }
    fn probe_queue(&mut self) {
        if matches!(self.delete_state, Some(DeleteState::Running(..))) {
            return;
        }
        // Read durations for every item without launching one FFprobe per file
        // at once when a large folder is added.
        let active = self.items.iter().filter(|item| item.probe_pending).count();
        let pending: Vec<_> = self
            .items
            .iter()
            .filter(|item| item.info.is_none() && item.error.is_none() && !item.probe_pending)
            .take(2_usize.saturating_sub(active))
            .map(|item| item.id)
            .collect();
        for id in pending {
            self.probe(id);
        }
    }
    fn request_preview(&mut self) {
        let Some(index) = self.selected_index() else {
            return;
        };
        let item = &self.items[index];
        if let Some(tx) = &self.preview_tx {
            let _ = tx.send(PreviewRequest {
                id: item.id,
                epoch: self.preview_epoch,
                path: item.input.path.clone(),
                time: item.edit.playhead,
                cancel: self.preview_cancel.clone(),
            });
        }
    }
    fn commit_edit(&mut self, index: usize, mut edit: VideoEdit) {
        let old = &self.items[index].edit;
        if *old == edit {
            return;
        }
        let seek = old.playhead != edit.playhead;
        let discarded = !edit.auto_end
            && edit.feedback == EndFeedback::NotSearched
            && old.feedback != edit.feedback;
        if !old.same_search(&edit) || discarded {
            let end_changed = old.end != edit.end;
            self.items[index].find_cancel.cancel();
            self.items[index].find_ticket += 1;
            self.items[index].find_started = None;
            self.items[index].find_progress = 0.0;
            edit.mark_stale(end_changed);
        }
        self.items[index].edit = edit;
        self.dirty();
        if seek {
            self.request_preview();
        }
    }
    fn sync_batch_detection(&mut self) {
        for index in 0..self.items.len() {
            if !self.items[index].edit.override_detection {
                let mut edit = self.items[index].edit.clone();
                edit.inherit_detection(&self.batch);
                self.items[index].reference_ticket += 1;
                self.commit_edit(index, edit);
            }
        }
    }
    fn find_selected(&mut self) {
        let Some(index) = self.selected_index() else {
            return;
        };
        let Some(info) = self.items[index].info.clone() else {
            return;
        };
        let (start, end) = match self.items[index].edit.search_range(info.duration) {
            Ok(range) => range,
            Err(e) => {
                self.items[index].edit.feedback = EndFeedback::Failed(e.to_string());
                return;
            }
        };
        let item = &mut self.items[index];
        item.find_cancel.cancel();
        item.find_cancel = Cancel::default();
        item.find_ticket += 1;
        item.edit.feedback = EndFeedback::Searching;
        item.find_progress = 0.0;
        item.find_started = Some(Instant::now());
        let id = item.id;
        let ticket = item.find_ticket;
        let cancel = item.find_cancel.clone();
        let path = item.input.path.clone();
        let detection = item.edit.detection.clone();
        let tools = self.tools.clone();
        self.spawn(move |tx| {
            let _ = tx.send(Message::Found(
                id,
                ticket,
                tools
                    .detect(
                        &path,
                        &info,
                        start,
                        end,
                        &detection,
                        &cancel,
                        &mut |value| {
                            let _ = tx.send(Message::FindProgress(id, ticket, value, None));
                        },
                    )
                    .map_err(|e| format!("{e:#}")),
            ));
        });
    }
    fn find_queue(&mut self) {
        if self.locked() || self.searching() {
            return;
        }
        self.queue_sort_dirty = true;
        self.sort_queue();
        self.analysis_cancel = Cancel::default();
        let cancel = self.analysis_cancel.clone();
        let mut jobs = Vec::new();
        for item in &mut self.items {
            if !item.edit.auto_end {
                continue;
            }
            item.find_cancel.cancel();
            item.find_ticket += 1;
            item.edit.feedback = EndFeedback::Searching;
            item.find_progress = 0.0;
            item.find_started = None;
            if item.edit.search_limit.is_none() {
                item.edit.search_limit = Some(item.edit.end.clone());
            }
            jobs.push((
                item.id,
                item.find_ticket,
                item.input.path.clone(),
                item.edit.clone(),
            ));
        }
        if jobs.is_empty() {
            return;
        }
        self.finding_batch = true;
        self.progress = 0.0;
        let tools = self.tools.clone();
        self.spawn(move |tx| {
            for (number, (id, ticket, path, mut edit)) in jobs.iter().cloned().enumerate() {
                if cancel.is_cancelled() {
                    break;
                }
                let mut report = |value| {
                    let _ = tx.send(Message::FindProgress(
                        id,
                        ticket,
                        value,
                        Some((number, jobs.len())),
                    ));
                };
                report(0.0);
                let result = (|| -> anyhow::Result<Option<f64>> {
                    cancel.check()?;
                    let info = tools.probe(&path, &cancel)?;
                    let (start, end) = edit.search_range(info.duration)?;
                    tools.detect(
                        &path,
                        &info,
                        start,
                        end,
                        &edit.detection,
                        &cancel,
                        &mut report,
                    )
                })()
                .map_err(|e| format!("{e:#}"));
                if !cancel.is_cancelled() {
                    report(1.0);
                }
                let _ = tx.send(Message::Found(id, ticket, result));
            }
            let _ = tx.send(Message::AnalysisDone);
        });
    }
    fn process_videos(&mut self, only: Option<u64>) {
        if self.locked() || self.collecting || self.searching() {
            return;
        }
        // Resolve the active sort at the moment processing starts, even if a
        // pending update has not yet marked the displayed queue for sorting.
        self.queue_sort_dirty = true;
        self.sort_queue();
        let mut jobs = Vec::new();
        for item in self
            .items
            .iter()
            .filter(|item| only.is_none_or(|id| item.id == id))
        {
            match item.edit.settings(&self.batch) {
                Ok(settings) => jobs.push((item.id, item.input.clone(), settings)),
                Err(e) => {
                    self.error = Some(format!(
                        "{}: {e}",
                        item.input
                            .path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                    ));
                    return;
                }
            }
        }
        if jobs.is_empty() {
            return;
        }
        for item in self
            .items
            .iter_mut()
            .filter(|item| only.is_none_or(|id| item.id == id))
        {
            item.status = "Queued".into();
            item.error = None;
            item.result = None;
        }
        self.cancel = Cancel::default();
        let cancel = self.cancel.clone();
        let tools = self.tools.clone();
        self.busy = true;
        self.error = None;
        self.progress = 0.0;
        self.export_activity = None;
        self.status = if only.is_some() {
            "Starting video…"
        } else {
            "Starting queue…"
        }
        .into();
        self.spawn(move |tx| {
            for (number, (id, input, settings)) in jobs.iter().enumerate() {
                let result = if cancel.is_cancelled() {
                    Err("Cancelled".into())
                } else {
                    tools
                        .convert_with_progress(input, settings, &cancel, &mut |event| {
                            let _ = tx.send(Message::Progress(*id, number, jobs.len(), event));
                        })
                        .map_err(|e| format!("{e:#}"))
                };
                let _ = tx.send(Message::Converted(*id, result));
            }
            let _ = tx.send(Message::Finished(
                jobs.iter().map(|(id, _, _)| *id).collect(),
            ));
        });
    }
    fn receive(&mut self, ctx: &egui::Context) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Message::Added(result) => {
                    self.collecting = false;
                    match result {
                        Ok(inputs) => self.add(inputs),
                        Err(e) => self.error = Some(e),
                    }
                }
                Message::Probed(id, ticket, result) => {
                    if let Some(index) = self.index(id)
                        && self.items[index].probe_ticket == ticket
                    {
                        self.items[index].probe_pending = false;
                        match result {
                            Ok(info) => {
                                self.items[index].edit.playhead =
                                    self.items[index].edit.playhead.clamp(0.0, seek_max(&info));
                                self.items[index].info = Some(info);
                                self.queue_sort_dirty = true;
                                if self.selected == Some(id) {
                                    self.request_preview();
                                }
                            }
                            Err(e) if e != "Cancelled" => self.items[index].error = Some(e),
                            _ => {}
                        }
                    }
                }
                Message::Preview(id, epoch, time, result)
                    if self.selected == Some(id) && epoch == self.preview_epoch =>
                {
                    match result {
                        Ok(frame) => {
                            self.texture = Some(ctx.load_texture(
                                "preview",
                                egui::ColorImage::from_rgb([frame.width, frame.height], &frame.rgb),
                                egui::TextureOptions::LINEAR,
                            ));
                            self.frame = Some(frame);
                            self.frame_time = Some(time);
                        }
                        Err(e) if e != "Cancelled" => self.error = Some(e),
                        _ => {}
                    }
                }
                Message::Reference(id, ticket, result) => {
                    if let Some(index) = self.index(id)
                        && self.items[index].reference_ticket == ticket
                    {
                        match result {
                            Ok((frame, name)) => {
                                let mut edit = self.items[index].edit.clone();
                                edit.detection.reference = Some(frame.histogram);
                                edit.reference_name = name;
                                edit.override_detection = true;
                                self.commit_edit(index, edit);
                            }
                            Err(e) => self.error = Some(e),
                        }
                    }
                }
                Message::BatchReference(ticket, result)
                    if ticket == self.batch_reference_ticket =>
                {
                    match result {
                        Ok((frame, name)) => {
                            self.batch.detection.reference = Some(frame.histogram);
                            self.batch.reference_name = name;
                            self.sync_batch_detection();
                            self.dirty();
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                Message::Found(id, ticket, result) => {
                    if let Some(index) = self.index(id)
                        && self.items[index].find_ticket == ticket
                    {
                        self.items[index].find_started = None;
                        self.items[index].find_progress = 1.0;
                        match result {
                            Ok(end) => {
                                self.items[index].edit.apply_match(end);
                                if let Some(info) = &self.items[index].info {
                                    self.items[index].edit.playhead =
                                        self.items[index].edit.playhead.min(seek_max(info));
                                }
                                if self.selected == Some(id) {
                                    self.request_preview();
                                }
                            }
                            Err(e) => {
                                self.items[index].edit.feedback = if e == "Cancelled" {
                                    EndFeedback::Cancelled
                                } else {
                                    EndFeedback::Failed(e)
                                }
                            }
                        }
                        self.dirty();
                    }
                }
                Message::FindProgress(id, ticket, value, batch) => {
                    if let Some(index) = self.index(id)
                        && self.items[index].find_ticket == ticket
                        && matches!(self.items[index].edit.feedback, EndFeedback::Searching)
                        && value.is_finite()
                    {
                        let item = &mut self.items[index];
                        item.find_started.get_or_insert_with(Instant::now);
                        item.find_progress = item.find_progress.max(value.clamp(0.0, 1.0) as f32);
                        if let Some((number, total)) = batch {
                            self.progress =
                                (number as f32 + item.find_progress) / total.max(1) as f32;
                            self.status = format!("Finding end {} / {total}", number + 1);
                        }
                    }
                }
                Message::Progress(id, index, total, mut event) if self.busy => {
                    event.fraction = event
                        .fraction
                        .filter(|value| value.is_finite())
                        .map(|value| value.clamp(0.0, 1.0));
                    if let Some(i) = self.index(id) {
                        event.overall = if event.overall.is_finite() {
                            event.overall.clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        self.items[i].status =
                            format!("{} {:.1}%", event.phase, event.overall * 100.0);
                        let now = Instant::now();
                        if let Some(activity) = &mut self.export_activity
                            && activity.id == id
                        {
                            activity.update(event, now);
                        } else {
                            self.export_activity = Some(ExportActivity::new(id, event, now));
                        }
                        let activity = self.export_activity.as_ref().unwrap();
                        self.status =
                            format!("{} / {total} · {}", index + 1, activity.phase_label());
                        self.progress =
                            ((index as f64 + activity.event.overall) / total.max(1) as f64) as f32;
                    }
                }
                Message::Converted(id, result) => {
                    if let Some(index) = self.index(id) {
                        match result {
                            Ok(result) => {
                                self.items[index].status = result.status.clone();
                                let replaced =
                                    self.batch.replace_original && result.output.is_some();
                                let moved = result.source_removed;
                                self.items[index].error = result.warning.clone();
                                if moved {
                                    let item = &mut self.items[index];
                                    item.input.path = result.output.clone().unwrap();
                                    // The queue now refers to the already-trimmed export.
                                    // Do not apply the old source offsets to it again.
                                    item.edit.start = "0".into();
                                    item.edit.end.clear();
                                    item.edit.playhead = 0.0;
                                    item.edit.discard_auto_end();
                                }
                                self.items[index].result = Some(result);
                                if replaced || moved {
                                    let item = &mut self.items[index];
                                    item.probe_cancel.cancel();
                                    item.probe_ticket += 1;
                                    item.probe_pending = false;
                                    item.info = None;
                                    self.queue_sort_dirty = true;
                                }
                                if moved && self.selected == Some(id) {
                                    self.activate_video(id);
                                }
                            }
                            Err(e) => {
                                self.items[index].status = if e == "Cancelled" {
                                    "Cancelled"
                                } else {
                                    "Failed"
                                }
                                .into();
                                self.items[index].error = Some(e);
                            }
                        }
                    }
                }
                Message::DeleteProgress(target) => {
                    if let Some(DeleteState::Running(targets, index)) = &mut self.delete_state
                        && let Some(current) = targets.iter().position(|item| *item == target)
                    {
                        *index = current;
                    }
                }
                Message::Deleted(targets, results) => {
                    if matches!(&self.delete_state, Some(DeleteState::Running(current, _)) if *current == targets)
                        && targets.len() == results.len()
                    {
                        self.delete_state = None;
                        let mut removed = BTreeSet::new();
                        let mut failed = Vec::new();
                        for (target, result) in targets.into_iter().zip(results) {
                            if let Some(index) = self
                                .index(target.id)
                                .filter(|&index| self.items[index].input.path == target.path)
                            {
                                match result {
                                    Ok(()) => {
                                        removed.insert(target.id);
                                    }
                                    Err(error) => {
                                        let message = format!(
                                            "Could not delete {}: {error}",
                                            display_path(&target.path)
                                        );
                                        self.items[index].status = "Delete failed".into();
                                        self.items[index].error = Some(message.clone());
                                        failed.push((target.id, message));
                                    }
                                }
                            }
                        }
                        self.remove_queue_items(&removed);
                        self.status = format!(
                            "Deleted {} file{}",
                            removed.len(),
                            if removed.len() == 1 { "" } else { "s" }
                        );
                        if !failed.is_empty() {
                            self.status.push_str(&format!(" · {} failed", failed.len()));
                            self.error = if failed.len() == 1 && self.selected == Some(failed[0].0)
                            {
                                None
                            } else {
                                Some(
                                    failed
                                        .iter()
                                        .map(|(_, message)| message.as_str())
                                        .collect::<Vec<_>>()
                                        .join("\n"),
                                )
                            };
                            if self
                                .selected
                                .is_some_and(|id| failed.iter().any(|(failed, _)| *failed == id))
                            {
                                self.preview_cancel = Cancel::default();
                                if self
                                    .selected_index()
                                    .is_some_and(|index| self.items[index].info.is_some())
                                {
                                    self.request_preview();
                                }
                            }
                        }
                    }
                }
                Message::AnalysisDone => {
                    self.finding_batch = false;
                    self.status = if self.analysis_cancel.is_cancelled() {
                        "End-marker search cancelled"
                    } else {
                        self.progress = 1.0;
                        "End-marker search complete"
                    }
                    .into();
                }
                Message::Finished(ids) => {
                    self.busy = false;
                    self.export_activity = None;
                    let done = self
                        .items
                        .iter()
                        .filter(|item| ids.contains(&item.id) && item.status == "Done")
                        .count();
                    let failed = self
                        .items
                        .iter()
                        .filter(|item| ids.contains(&item.id) && item.status == "Failed")
                        .count();
                    self.status = format!("{done} done · {failed} failed");
                    self.progress = 1.0;
                }
                _ => {}
            }
        }
        self.probe_queue();
        self.sort_queue();
    }
    fn clear_queue(&mut self) {
        for item in &self.items {
            item.find_cancel.cancel();
            item.probe_cancel.cancel();
        }
        self.items.clear();
        self.queue_selection.clear();
        self.selection_anchor = None;
        self.selected = None;
        self.texture = None;
        self.frame = None;
        self.frame_time = None;
        self.drag = None;
        self.pan = egui::Vec2::ZERO;
        self.preview_cancel.cancel();
        self.preview_epoch += 1;
        self.dirty();
    }
    fn toolbar(&mut self, root: &mut egui::Ui) {
        egui::Panel::top("toolbar").show(root, |ui| {
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Tailout").size(21.0).strong());
                ui.add_space(16.0);
                ui.add_enabled_ui(!self.locked() && !self.collecting, |ui| {
                    if ui.button("Add videos").on_hover_text("Add MP4, MKV or MOV files. Every video has its own settings and markers.").clicked()
                        && let Some(files) = rfd::FileDialog::new().add_filter("Videos", &["mp4", "mkv", "mov"]).pick_files() {
                        self.add(files.into_iter().map(Input::new).collect());
                    }
                    if ui.button("Add folder").on_hover_text("Add videos from a folder, including subfolders when checked.").clicked()
                        && let Some(root) = rfd::FileDialog::new().pick_folder() {
                        self.collecting = true;
                        let recurse = self.program.recurse;
                        let cancel = self.auxiliary_cancel.clone();
                        self.spawn(move |tx| {
                            let _ = tx.send(Message::Added(media::collect_folder(&root, recurse, &cancel).map_err(|e| e.to_string())));
                        });
                    }
                    ui.checkbox(&mut self.program.recurse, "Include subfolders").on_hover_text("Include nested folders when using Add folder. Does not change videos already queued.");
                });
            });
            ui.add_space(5.0);
        });
    }
    fn queue_ui(&mut self, root: &mut egui::Ui) {
        let ctx = &root.ctx().clone();
        let mut remove = None;
        let mut delete = None;
        let mut open_folder = None;
        let mut process_video = None;
        egui::Panel::left("queue_v2")
            .exact_size(320.0).resizable(false).show(root, |ui| {
                section(ui, &format!("VIDEOS  {}", self.items.len()));
                ui.add_enabled_ui(!self.locked(), |ui| {
                    ui.spacing_mut().interact_size.y = ui.text_style_height(&egui::TextStyle::Button)
                        .max(ui.spacing().icon_width) + 2.0 * ui.spacing().button_padding.y;
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!self.collecting && !self.items.is_empty(), egui::Button::new("Clear queue"))
                            .on_hover_text("Remove all queued videos and their edits from this session.").clicked() {
                            self.clear_queue();
                        }
                        ui.label("Sort");
                        let previous = self.program.queue_sort;
                        egui::ComboBox::from_id_salt("queue_sort").width(130.0)
                            .selected_text(self.program.queue_sort.label()).show_ui(ui, |ui| {
                                for order in QueueSort::ALL {
                                    ui.selectable_value(&mut self.program.queue_sort, order, order.label());
                                }
                            }).response.on_hover_text("Sort alphabetically by filename or by full video duration. Processing follows this order. Duration sorts put unknown lengths last.");
                        if self.program.queue_sort != previous {
                            self.queue_sort_dirty = true;
                            self.sort_queue();
                        }
                    });
                });
                let mut cards = Vec::new();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for item in &self.items {
                        let response = ui.push_id(("queue_card", item.id), |ui| {
                            let processing = self.busy
                                && self.export_activity.as_ref().is_some_and(|activity| activity.id == item.id)
                                && item.result.is_none()
                                && item.error.is_none();
                            queue_card(ui, item, self.selected == Some(item.id), self.queue_selection.contains(&item.id), processing)
                        }).inner;
                        cards.push((item.id, response));
                    }
                });
                for (id, response) in cards {
                    if response.clicked() { self.queue_click(id, ctx.input(|input| input.modifiers)); }
                    if response.secondary_clicked() && self.delete_state.is_none() && !self.queue_selection.contains(&id) {
                        self.select(id);
                    }
                    response.context_menu(|ui| {
                        if ui.add_enabled(!self.locked() && !self.collecting && !self.searching(), egui::Button::new("Process this video"))
                            .on_hover_text("Process only this video, even when multiple videos are selected. Uses its markers, inherited or overridden processing settings, and the batch output options.").clicked() {
                            process_video = Some(id);
                            ui.close();
                        }
                        if ui.button("Open containing folder").on_hover_text("Open the folder of this video, even when multiple videos are selected.").clicked() {
                            open_folder = Some(id);
                            ui.close();
                        }
                        if let Some(message) = self.index(id).and_then(|index| self.items[index].error_message())
                            && ui.button("Copy error message").on_hover_text("Copy this video's full error message, even when multiple videos are selected.").clicked() {
                            ui.ctx().copy_text(message.to_owned());
                            ui.close();
                        }
                        ui.separator();
                        let ids = if self.queue_selection.contains(&id) { self.queue_selection.clone() } else { BTreeSet::from([id]) };
                        let count = ids.len();
                        ui.add_enabled_ui(!self.locked() && !self.collecting, |ui| {
                            let label = if count == 1 { "Remove from queue".into() } else { format!("Remove selected ({count})") };
                            if ui.button(label).on_hover_text("Files stay on disk.").clicked() {
                                remove = Some(ids.clone());
                                ui.close();
                            }
                            let label = if count == 1 { "Delete from disk…".into() } else { format!("Delete {count} files from disk…") };
                            if ui.button(RichText::new(label).color(RED)).on_hover_text("Permanently delete the source files after confirmation.").clicked() {
                                delete = Some(ids);
                                ui.close();
                            }
                        });
                    });
                }
            });
        if let Some(id) = process_video {
            self.process_videos(Some(id));
        }
        if let Some(index) = open_folder.and_then(|id| self.index(id))
            && let Err(error) = open_containing_folder(&self.items[index].input.path)
        {
            self.error = Some(format!("Could not open containing folder: {error}"));
        }
        if let Some(ids) = remove {
            self.remove_queue_items(&ids);
        }
        if let Some(ids) = delete {
            self.request_delete(&ids);
        }
    }
    fn remove_queue_items(&mut self, ids: &BTreeSet<u64>) {
        let old_index = self.selected_index().unwrap_or(0);
        let removed_active = self.selected.is_some_and(|id| ids.contains(&id));
        let old_len = self.items.len();
        self.items.retain(|item| {
            if ids.contains(&item.id) {
                item.find_cancel.cancel();
                item.probe_cancel.cancel();
                false
            } else {
                true
            }
        });
        if self.items.len() == old_len {
            return;
        }
        self.queue_selection.retain(|id| !ids.contains(id));
        if removed_active {
            self.clear_active_video();
            let next = self
                .items
                .iter()
                .find(|item| self.queue_selection.contains(&item.id))
                .or_else(|| {
                    self.items
                        .get(old_index.min(self.items.len().saturating_sub(1)))
                })
                .map(|item| item.id);
            if let Some(id) = next {
                self.queue_selection.insert(id);
                self.activate_video(id);
            }
        }
        if self.selection_anchor.is_some_and(|id| ids.contains(&id)) {
            self.selection_anchor = self.selected;
        }
        self.dirty();
    }
    fn request_delete(&mut self, ids: &BTreeSet<u64>) {
        if self.locked() || self.collecting {
            return;
        }
        let targets: Vec<_> = self
            .items
            .iter()
            .filter(|item| ids.contains(&item.id))
            .map(|item| DeleteTarget {
                id: item.id,
                path: item.input.path.clone(),
            })
            .collect();
        if !targets.is_empty() {
            self.delete_state = Some(DeleteState::Confirm(targets));
        }
    }
    fn confirm_delete(&mut self, targets: Vec<DeleteTarget>) {
        if self.busy
            || self.finding_batch
            || self.collecting
            || !matches!(&self.delete_state, Some(DeleteState::Confirm(current)) if *current == targets)
        {
            return;
        }
        if !targets.iter().all(|target| {
            self.index(target.id)
                .is_some_and(|index| self.items[index].input.path == target.path)
        }) {
            self.delete_state = None;
            self.error = Some("The queue changed. Choose Delete from disk again.".into());
            return;
        }
        for target in &targets {
            let index = self.index(target.id).unwrap();
            let item = &mut self.items[index];
            cancel_find(item);
            item.probe_cancel.cancel();
            item.probe_ticket += 1;
            item.probe_pending = false;
            item.reference_ticket += 1;
            if self.selected == Some(target.id) {
                self.preview_cancel.cancel();
                self.preview_epoch += 1;
            }
        }
        self.error = None;
        self.delete_state = Some(DeleteState::Running(targets.clone(), 0));
        let cancel = self.auxiliary_cancel.clone();
        self.spawn(move |tx| {
            let mut results = Vec::with_capacity(targets.len());
            for target in &targets {
                let _ = tx.send(Message::DeleteProgress(target.clone()));
                results.push(
                    delete_video_file(&target.path, &cancel).map_err(|error| error.to_string()),
                );
            }
            let _ = tx.send(Message::Deleted(targets, results));
        });
    }
    fn delete_dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(state) = self.delete_state.clone() else {
            return;
        };
        match state {
            DeleteState::Confirm(targets) => {
                let mut confirm = false;
                let mut cancel = false;
                let count = targets.len();
                let response = egui::Modal::new(egui::Id::unique("delete_video_confirmation")).show(ctx, |ui| {
                    ui.set_width(460.0_f32.min((ctx.content_rect().width() - 40.0).max(200.0)));
                    ui.heading(if count == 1 { "Delete file permanently?".into() } else { format!("Delete {count} files permanently?") });
                    ui.label("This deletes the source files from disk and removes them from the queue. This cannot be undone.");
                    egui::ScrollArea::vertical().id_salt("delete_source_paths").max_height(240.0).show(ui, |ui| {
                        for target in &targets { ui.add(egui::Label::new(RichText::new(display_path(&target.path)).monospace()).wrap()); }
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        confirm = ui.button(RichText::new(if count == 1 { "Delete file".into() } else { format!("Delete {count} files") }).color(RED)).clicked();
                        let cancel_button = ui.button("Cancel");
                        if ui.memory(|memory| memory.focused().is_none()) { cancel_button.request_focus(); }
                        cancel = cancel_button.clicked();
                    });
                });
                if cancel || response.should_close() {
                    self.delete_state = None;
                } else if confirm {
                    self.confirm_delete(targets);
                }
            }
            DeleteState::Running(targets, index) => {
                egui::Modal::new(egui::Id::unique("deleting_video")).show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Deleting file {} of {}…", index + 1, targets.len()));
                    });
                    if let Some(target) = targets.get(index) {
                        ui.add(egui::Label::new(display_path(&target.path)).wrap());
                    }
                });
            }
        }
    }
    fn options_ui(
        &mut self,
        root: &mut egui::Ui,
        edit: &mut VideoEdit,
        info: Option<&MediaInfo>,
    ) -> Actions {
        let ctx = &root.ctx().clone();
        let mut load = false;
        let mut save = false;
        let mut actions = Actions::default();
        let enabled = !self.locked();
        let quality_summary = self
            .selected_index()
            .and_then(|index| self.items[index].result.as_ref())
            .filter(|result| !result.quality.is_empty())
            .map(|result| {
                result
                    .quality
                    .iter()
                    .enumerate()
                    .map(|(index, score)| {
                        format!(
                            "Video {}: SSIM {:.5} · PSNR {:.2} dB",
                            index + 1,
                            score.ssim,
                            score.psnr
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        let filename = self.selected_index().map(|index| {
            self.items[index]
                .input
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
        egui::Panel::right("options_v2").exact_size(315.0).resizable(false).show(root, |ui| {
            let old_tab = self.settings_tab;
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.settings_tab, SettingsTab::Batch, "Batch settings")
                    .on_hover_text("Shared processing defaults and destination for the queue.");
                ui.selectable_value(&mut self.settings_tab, SettingsTab::Video, "Per-video settings")
                    .on_hover_text("Processing overrides for the selected video. Editing a value creates an override automatically.");
            });
            if old_tab != self.settings_tab {
                clear_keyboard_focus(ui);
            }
            ui.separator();
            egui::Panel::bottom("preset_actions_v5").show(ui, |ui| {
                ui.separator();
                ui.add_enabled_ui(enabled && (self.settings_tab == SettingsTab::Batch || filename.is_some()), |ui| {
                    ui.horizontal(|ui| {
                        let batch = self.settings_tab == SettingsTab::Batch;
                        save = ui.button("Save preset").on_hover_text(if batch {
                            "Save all batch processing, Detect end, reference histogram and output settings. Per-video markers are excluded."
                        } else {
                            "Save this video's effective processing and Detect end settings, reference histogram, markers and playhead. Batch destination settings are excluded."
                        }).clicked();
                        load = ui.button("Load preset").on_hover_text(if batch {
                            "Load all batch defaults and output settings. A video preset imports processing and Detect end settings without copying its markers."
                        } else {
                            "Load all per-video settings and markers. A batch preset imports processing and Detect end settings while keeping this video's markers. Other videos are unchanged."
                        }).clicked();
                    });
                });
            });
            egui::ScrollArea::vertical().id_salt(("settings_scroll", self.settings_tab)).show(ui, |ui| {
                if self.settings_tab == SettingsTab::Batch {
                    ui.add_enabled_ui(enabled, |ui| {
                        let controls_id = settings_editor_id(ui.ctx(), SettingsTab::Batch, None, false);
                        ui.push_id(controls_id, |ui| {
                            video_options_ui(ui, &mut self.batch.video, info);
                        });
                        ui.separator();
                        ui.radio_value(&mut self.batch.replace_original, true, "Replace originals")
                            .on_hover_text("Replace each original with its validated conversion. No backup is created; choose Save to folder to write separate files.");
                        ui.indent("replacement_options", |ui| {
                            ui.add_enabled_ui(self.batch.replace_original, |ui| {
                                ui.checkbox(&mut self.batch.force_overwrite, "Force overwrite")
                                    .on_hover_text("Replace the original even when the converted file is the same size or larger. All validation and quality checks still apply.");
                            });
                        });
                        ui.radio_value(&mut self.batch.replace_original, false, "Save to folder")
                            .on_hover_text("Save separate files named filename.hevc.ext in the chosen folder. Existing output names get a unique numbered suffix. The option below controls whether originals are kept.");
                        let output_dir = self.batch.output_dir.clone();
                        ui.indent("new_file_options", |ui| {
                            ui.add_enabled_ui(!self.batch.replace_original, |ui| {
                                destination_row(ui, output_dir.as_path(), |path| self.batch.output_dir = path);
                                ui.checkbox(&mut self.batch.delete_original, "Delete originals after successful export")
                                    .on_hover_text("Enabled by default. Delete each source only after its converted output passes all validation and quality checks and is successfully saved. Uncheck to keep both files. If deletion fails, both files are retained and a warning is shown.");
                            });
                        });

                    });
                    ui.separator();
                    ui.label(RichText::new("All videos · local detection overrides respected").small().weak());
                    let mut detection = VideoEdit {
                        auto_end: self.batch.auto_end,
                        detection: self.batch.detection.clone(),
                        reference_name: self.batch.reference_name.clone(),
                        ..Default::default()
                    };
                    ui.add_enabled(enabled, egui::Checkbox::new(&mut detection.auto_end, "Detect end"))
                        .on_hover_text("Batch default for every video without a local detection override. Find ends in queue searches eligible videos; markers stay individual.");
                    let activity = self.finding_batch.then_some(SearchActivity { progress: self.progress, elapsed: None, batch: true });
                    let eligible = self.items.iter().filter(|item| if item.edit.override_detection { item.edit.auto_end } else { detection.auto_end }).count();
                    ui.label(RichText::new(format!("{eligible} of {} videos enabled", self.items.len())).small().weak());
                    detection_ui(ui, &mut detection, &mut actions, self.frame.is_some(), activity, enabled && !self.searching(), DetectionScope { batch: true, can_find: eligible > 0 });
                    self.batch.auto_end = detection.auto_end;
                    self.batch.detection = detection.detection;
                    self.batch.reference_name = detection.reference_name;
                } else if let Some(name) = &filename {
                    ui.add(egui::Label::new(RichText::new(name).small().weak()).truncate());
                    ui.add_enabled_ui(enabled, |ui| {
                        let mut reset = false;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(if edit.override_video || edit.override_detection { "Custom settings" } else { "Using batch settings" }).small().weak());
                            reset = ui.add_enabled(edit.override_video || edit.override_detection, egui::Button::new("Reset to batch settings"))
                                .on_hover_text("Restore batch processing and Detect end settings for this video. Its begin and end markers are kept.").clicked();
                        });
                        if reset {
                            edit.set_video_override(false, &self.batch);
                            edit.reset_detection(&self.batch);
                            if let Some(index) = self.selected_index() {
                                cancel_find(&mut self.items[index]);
                                self.items[index].reference_ticket += 1;
                            }
                            clear_keyboard_focus(ui);
                        }
                        let mut video = edit.effective_video(&self.batch).clone();
                        let initial = video.clone();
                        let controls_id = settings_editor_id(ui.ctx(), SettingsTab::Video, self.selected, reset);
                        ui.push_id(controls_id, |ui| {
                            video_options_ui(ui, &mut video, info);
                        });
                        if video != initial {
                            edit.video = video;
                            edit.override_video = true;
                        }
                        if let Some(summary) = &quality_summary {
                            ui.label(RichText::new(summary).small().color(GREEN));
                        }
                    });
                    ui.separator();
                    ui.label(RichText::new(if edit.override_detection { "Detect end · custom for this video" } else { "Detect end · using batch defaults" }).small().weak());
                    let initial = (edit.auto_end, edit.detection.clone(), edit.reference_name.clone());
                    if ui.add_enabled(enabled, egui::Checkbox::new(&mut edit.auto_end, "Detect end"))
                        .on_hover_text("Find an end marker for this video. Changing these settings creates a local detection override. Manual end edits disable detection for this video.").changed() && !edit.auto_end {
                        edit.discard_auto_end();
                    }
                    let activity = self.selected_index().and_then(|index| {
                        let item = &self.items[index];
                        matches!(item.edit.feedback, EndFeedback::Searching).then_some(SearchActivity {
                            progress: item.find_progress, elapsed: item.find_started.map(|started| started.elapsed()), batch: self.finding_batch,
                        })
                    });
                    detection_ui(ui, edit, &mut actions, self.frame.is_some(), activity, enabled, DetectionScope { batch: false, can_find: info.is_some() });
                    if initial != (edit.auto_end, edit.detection.clone(), edit.reference_name.clone()) { edit.override_detection = true; }
                } else {
                    ui.label(RichText::new("Select a video to edit its settings.").weak());
                }

            });
        });
        if self.settings_tab == SettingsTab::Batch && actions.find {
            self.queued_find = true;
            actions.find = false;
        }
        if save {
            self.save_preset(edit);
        }
        if load && let Some(preset) = self.load_preset() {
            self.apply_preset(ctx, edit, preset);
        }
        actions
    }
    fn apply_preset(&mut self, ctx: &egui::Context, edit: &mut VideoEdit, preset: Preset) {
        match self.settings_tab {
            SettingsTab::Batch => {
                let full_batch = matches!(preset, Preset::Batch(_));
                self.batch_reference_ticket += 1;
                preset.apply_to_batch(&mut self.batch);
                self.status = if full_batch {
                    "Loaded batch preset."
                } else {
                    "Loaded processing and Detect end settings into the batch."
                }
                .into();
            }
            SettingsTab::Video => {
                let Some(index) = self.selected_index() else {
                    return;
                };
                // Loading a preset invalidates pending analysis/reference replies.
                cancel_find(&mut self.items[index]);
                self.items[index].reference_ticket += 1;
                preset.apply_to_video(edit);
                self.status = "Loaded preset for the selected video.".into();
            }
        }
        let _ = settings_editor_id(ctx, self.settings_tab, self.selected, true);
        ctx.memory_mut(|memory| {
            if let Some(id) = memory.focused() {
                memory.surrender_focus(id);
            }
        });
    }
    fn save_preset(&mut self, edit: &VideoEdit) {
        let (preset, filename) = match self.settings_tab {
            SettingsTab::Batch => (Preset::Batch(self.batch.clone()), "batch-preset.json"),
            SettingsTab::Video => (Preset::Video(edit.preset(&self.batch)), "video-preset.json"),
        };
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Tailout preset", &["json"])
            .set_file_name(filename)
            .save_file()
        {
            let result = (|| -> anyhow::Result<()> {
                std::fs::write(path, serde_json::to_vec_pretty(&preset.to_value())?)?;
                Ok(())
            })();
            if let Err(e) = result {
                self.error = Some(e.to_string());
            }
        }
    }
    fn load_preset(&mut self) -> Option<Preset> {
        let path = rfd::FileDialog::new()
            .add_filter("Tailout preset", &["json"])
            .pick_file()?;
        let result = (|| -> anyhow::Result<Preset> {
            let value = serde_json::from_slice(&std::fs::read(path)?)?;
            Preset::from_value(value)
        })();
        match result {
            Ok(preset) => Some(preset),
            Err(e) => {
                self.error = Some(e.to_string());
                None
            }
        }
    }
    fn central_ui(
        &mut self,
        root: &mut egui::Ui,
        edit: &mut VideoEdit,
        info: Option<&MediaInfo>,
    ) -> Actions {
        let ctx = &root.ctx().clone();
        let actions = Actions::default();
        egui::CentralPanel::default().show(root,|ui| {
            let Some(info)=info else { ui.centered_and_justified(|ui|ui.label(if self.selected.is_some(){"Reading video…"}else if !self.items.is_empty(){"Select a video to preview"}else{"Add a video to begin"})); return; };
            let trim_width = ui.available_width().max(200.0);
            egui::Panel::bottom("trim_v2").show(ui,|ui| {
                ui.set_width(trim_width);
                ui.add_enabled_ui(!self.locked(),|ui| { self.timeline(ui,edit,info); });
                    ui.columns(2,|columns| {
                        columns[0].add_enabled_ui(!self.locked(),|ui| {
                            ui.label(RichText::new("BEGIN").color(GREEN).strong());
                            ui.horizontal(|ui| { ui.add(egui::TextEdit::singleline(&mut edit.start).desired_width(112.0)).on_hover_text("Inclusive start time: seconds, MM:SS.mmm or HH:MM:SS.mmm."); if ui.button("Here").on_hover_text("Set the green begin marker at the playhead. Keyboard: I.").clicked(){edit.start=time_text(edit.playhead);} });
                        });
                        columns[1].add_enabled_ui(!self.locked(),|ui| {
                            ui.label(RichText::new("END").color(RED).strong());
                            ui.horizontal(|ui| { if ui.add(egui::TextEdit::singleline(&mut edit.end).desired_width(112.0).hint_text("End of video")).on_hover_text("Exclusive end marker. Empty means the end of the video. Manual changes disable Detect end. Keyboard: O sets End at the playhead.").changed() { edit.discard_auto_end(); }
                                if ui.button("Here").on_hover_text("Set the red end marker at the playhead and discard Detect end.").clicked(){edit.set_manual_end(edit.playhead);} });
                        });
                    });
                    ui.add_space(5.0);
            });
            ui.label(RichText::new(format!("{} × {}  ·  {}",info.primary().width,info.primary().height,aspect_text(info))).small().weak()).on_hover_text("Source pixel dimensions and display aspect ratio, including rotation and pixel aspect ratio.");
            let size=ui.available_size().max(egui::vec2(1.0,1.0));
            let (rect,response)=ui.allocate_exact_size(size,egui::Sense::click_and_drag());
            if response.clicked() { clear_keyboard_focus(ui); }
            ui.painter().rect_filled(rect,5.0,Color32::from_rgb(11,13,16));
            if response.dragged() { self.pan+=response.drag_delta(); }
            if let Some(texture)=&self.texture {
                let primary=info.primary(); let height=if primary.rotated(){primary.width}else{primary.height} as f32;
                let aspect=info.aspect(); let [width,height]=self.program.view.size([rect.width(),rect.height()],aspect,[height*aspect,height]);
                self.pan.x=self.pan.x.clamp(-((width-rect.width())/2.0).max(0.0),((width-rect.width())/2.0).max(0.0));
                self.pan.y=self.pan.y.clamp(-((height-rect.height())/2.0).max(0.0),((height-rect.height())/2.0).max(0.0));
                let image_rect=egui::Rect::from_center_size(rect.center()+self.pan,egui::vec2(width,height));
                ui.painter().with_clip_rect(rect).image(texture.id(),image_rect,egui::Rect::from_min_max(egui::Pos2::ZERO,egui::pos2(1.0,1.0)),Color32::WHITE);
            } else { ui.painter().text(rect.center(),egui::Align2::CENTER_CENTER,"Loading frame…",egui::FontId::proportional(14.0),ui.visuals().weak_text_color()); }
            response.clone().on_hover_text("Scroll or ← / → to seek. Shift = 10×, Alt = 0.1×. I = begin, O = end. Drag to pan; hover at the top-right for view options.");
            if response.hovered() && !self.locked() { self.scroll_seek(ctx,edit,info); }
            if preview_view_control(ui, rect, &mut self.program.view) {
                self.pan = egui::Vec2::ZERO;
            }
        });
        actions
    }
    fn timeline(
        &mut self,
        ui: &mut egui::Ui,
        edit: &mut VideoEdit,
        info: &MediaInfo,
    ) -> egui::Rect {
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 64.0),
            egui::Sense::click_and_drag(),
        );
        if response.clicked() {
            clear_keyboard_focus(ui);
        }
        let track = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 8.0, rect.top() + 22.0),
            egui::pos2(rect.right() - 8.0, rect.top() + 34.0),
        );
        let x = |time: f64| {
            track.left() + ((time / info.duration).clamp(0.0, 1.0) as f32) * track.width()
        };
        let begin = parse_time(&edit.start)
            .unwrap_or(0.0)
            .clamp(0.0, info.duration);
        let end = optional_time(&edit.end)
            .ok()
            .flatten()
            .unwrap_or(info.duration)
            .clamp(0.0, info.duration);
        let painter = ui.painter();
        painter.rect_filled(track, 3.0, Color32::from_rgb(51, 56, 62));
        if end > begin {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x(begin), track.top()),
                    egui::pos2(x(end), track.bottom()),
                ),
                2.0,
                Color32::from_rgb(47, 87, 66),
            );
        }
        for (time, color) in [(begin, GREEN), (end, RED)] {
            let at = x(time);
            painter.line_segment(
                [
                    egui::pos2(at, track.top() - 10.0),
                    egui::pos2(at, track.bottom() + 8.0),
                ],
                egui::Stroke::new(2.5_f32, color),
            );
        }
        let at = x(edit.playhead);
        painter.line_segment(
            [
                egui::pos2(at, track.top() - 3.0),
                egui::pos2(at, track.bottom() + 5.0),
            ],
            egui::Stroke::new(1.5_f32, Color32::WHITE),
        );
        painter.text(
            egui::pos2(rect.left() + 8.0, rect.bottom()),
            egui::Align2::LEFT_BOTTOM,
            time_text(edit.playhead),
            egui::FontId::monospace(11.0),
            Color32::WHITE,
        );
        painter.text(
            egui::pos2(rect.right() - 8.0, rect.bottom()),
            egui::Align2::RIGHT_BOTTOM,
            time_text(info.duration),
            egui::FontId::monospace(11.0),
            ui.visuals().weak_text_color(),
        );
        if self.drag.is_none()
            && (response.is_pointer_button_down_on() || response.clicked())
            && let Some(position) = ui
                .input(|input| input.pointer.press_origin())
                .or(response.interact_pointer_pos())
        {
            self.drag = Some(if (position.x - x(begin)).abs() <= 9.0 {
                DragTarget::Begin
            } else if (position.x - x(end)).abs() <= 9.0 {
                DragTarget::End
            } else {
                DragTarget::Playhead
            });
        }
        if (response.is_pointer_button_down_on() || response.dragged() || response.clicked())
            && let Some(position) = response.interact_pointer_pos()
        {
            let time = ((position.x - track.left()) / track.width()).clamp(0.0, 1.0) as f64
                * info.duration;
            match self.drag.unwrap_or(DragTarget::Playhead) {
                DragTarget::Playhead => edit.playhead = time.min(seek_max(info)),
                DragTarget::Begin => {
                    let value = time.min((end - 0.05).max(0.0));
                    edit.start = time_text(value);
                    edit.playhead = value.min(seek_max(info));
                }
                DragTarget::End => {
                    let value = time.max((begin + 0.05).min(info.duration));
                    edit.set_manual_end(value);
                    edit.playhead = value.min(seek_max(info));
                }
            }
        }
        if response.drag_stopped() || !ui.input(|input| input.pointer.primary_down()) {
            self.drag = None;
        }
        if response.hovered() {
            self.scroll_seek(ui.ctx(), edit, info);
        }
        response.on_hover_text("Drag to scrub; the image updates while moving. Drag the green or red line to change markers. Scroll to seek.");
        rect
    }
    fn scroll_seek(&mut self, ctx: &egui::Context, edit: &mut VideoEdit, info: &MediaInfo) {
        let (delta, modifiers) = ctx.input(|input| {
            let mut delta = 0.0;
            for event in &input.events {
                if let egui::Event::MouseWheel {
                    unit,
                    delta: motion,
                    ..
                } = event
                {
                    let value = if motion.y.abs() > 0.0 {
                        motion.y
                    } else {
                        motion.x
                    };
                    delta += match unit {
                        egui::MouseWheelUnit::Line => value,
                        egui::MouseWheelUnit::Point => value / 50.0,
                        egui::MouseWheelUnit::Page => value,
                    };
                }
            }
            (delta, input.modifiers)
        });
        self.wheel += delta;
        let notches = self.wheel.trunc();
        self.wheel -= notches;
        if notches != 0.0 {
            edit.playhead = (edit.playhead
                + notches as f64 * self.program.seek_step * seek_multiplier(modifiers))
            .clamp(0.0, seek_max(info));
        }
    }
    fn keyboard_seek(&self, ctx: &egui::Context, edit: &mut VideoEdit, info: Option<&MediaInfo>) {
        let Some(info) = info else {
            return;
        };
        if ctx.egui_wants_keyboard_input() || self.locked() {
            return;
        }
        ctx.input(|input| {
            let direction = if input.key_pressed(egui::Key::ArrowRight) {
                1.0
            } else if input.key_pressed(egui::Key::ArrowLeft) {
                -1.0
            } else {
                0.0
            };
            if direction != 0.0 {
                edit.playhead = (edit.playhead
                    + direction * self.program.seek_step * seek_multiplier(input.modifiers))
                .clamp(0.0, seek_max(info));
            }
            if input.key_pressed(egui::Key::Home) {
                edit.playhead = 0.0;
            }
            if input.key_pressed(egui::Key::End) {
                edit.playhead = seek_max(info);
            }
            if input.key_pressed(egui::Key::I) {
                edit.start = time_text(edit.playhead);
            }
            if input.key_pressed(egui::Key::O) {
                edit.set_manual_end(edit.playhead);
            }
        });
    }
    fn reference(&mut self, use_frame: bool) {
        let batch = self.settings_tab == SettingsTab::Batch;
        let index = self.selected_index();
        if !batch && index.is_none() {
            return;
        }
        if use_frame {
            if let Some(frame) = &self.frame {
                if batch {
                    self.batch_reference_ticket += 1;
                    self.batch.detection.reference = Some(frame.histogram.clone());
                    self.batch.reference_name = time_text(self.frame_time.unwrap_or(0.0));
                    self.sync_batch_detection();
                    self.dirty();
                } else {
                    let index = index.unwrap();
                    let mut edit = self.items[index].edit.clone();
                    edit.detection.reference = Some(frame.histogram.clone());
                    edit.reference_name = time_text(self.frame_time.unwrap_or(edit.playhead));
                    edit.override_detection = true;
                    self.items[index].reference_ticket += 1;
                    self.commit_edit(index, edit);
                }
            }
        } else if let Some(path) = rfd::FileDialog::new()
            .add_filter("Image", &["png", "jpg", "jpeg", "bmp", "webp"])
            .pick_file()
        {
            let (id, ticket) = if batch {
                self.batch_reference_ticket += 1;
                (0, self.batch_reference_ticket)
            } else {
                let item = &mut self.items[index.unwrap()];
                // Choosing a per-video reference is already a local edit, even
                // while its histogram is still being decoded in the background.
                item.edit.override_detection = true;
                item.reference_ticket += 1;
                (item.id, item.reference_ticket)
            };
            let tools = self.tools.clone();
            let cancel = self.auxiliary_cancel.clone();
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            self.spawn(move |tx| {
                let result = tools
                    .preview(&path, 0.0, &cancel)
                    .map(|frame| (frame, name))
                    .map_err(|e| e.to_string());
                let _ = tx.send(if batch {
                    Message::BatchReference(ticket, result)
                } else {
                    Message::Reference(id, ticket, result)
                });
            });
        }
    }
    fn cancel_analysis(&mut self) {
        self.analysis_cancel.cancel();
        for item in &mut self.items {
            cancel_find(item);
        }
        self.status = "End-marker search cancelled".into();
        self.dirty();
    }
    fn cancel_selected_search(&mut self) {
        if self.finding_batch {
            self.cancel_analysis();
        } else if let Some(index) = self.selected_index() {
            cancel_find(&mut self.items[index]);
            self.dirty();
        }
    }
    fn footer(&mut self, root: &mut egui::Ui) {
        egui::Panel::bottom("actions_v2").show(root,|ui| {ui.add_space(4.0);ui.horizontal(|ui| {
            if self.busy {if ui.button("Cancel export").clicked(){self.cancel.cancel();}}
            else {if ui.add_enabled(!self.items.is_empty()&&!self.collecting&&!self.searching()&&!self.finding_batch,egui::Button::new("Process queue").fill(Color32::from_rgb(41,81,60))).on_hover_text("Export every queued video using its own markers and video settings, plus the batch output options. Does not run auto-end searches.").clicked(){self.process_videos(None);}
                if (self.searching()||self.finding_batch)&&ui.button("Cancel search").clicked(){self.cancel_analysis();}}
            if self.busy {
                let fraction = self.export_activity.as_ref().map(|activity| activity.event.overall).unwrap_or(0.0);
                ui.add(egui::ProgressBar::new(fraction as f32).desired_width(150.0).text(format!("{:.1}%", fraction * 100.0)).animate(true))
                    .on_hover_text("Progress for this video, including encoding, validation and any quality comparison. Resets only when the next video starts. Stage percentages are weighted.");
            } else if self.finding_batch {ui.add(egui::ProgressBar::new(self.progress).desired_width(150.0).animate(true));}
            else if let Some(item)=self.items.iter().find(|item|matches!(item.edit.feedback,EndFeedback::Searching)) {ui.spinner();ui.add(egui::ProgressBar::new(item.find_progress).desired_width(150.0).text("Finding end…").animate(true));}
            ui.label(&self.status);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(egui::DragValue::new(&mut self.program.seek_step).range(0.01..=60.0).speed(0.1).suffix(" s")).on_hover_text("Arrow keys and mouse wheel. Shift = 10×; Alt = 0.1×. Text fields keep normal arrow-key editing.");
                ui.label("Seek step");
            });
        });
            if self.busy && let Some(activity) = &self.export_activity {
                let now = Instant::now();
                ui.horizontal(|ui| {
                    ui.label(activity.summary(now)).on_hover_text(activity.diagnostics(now));
                    if let Some(index) = self.index(activity.id) {
                        ui.add(egui::Label::new(RichText::new(self.items[index].input.path.file_name().unwrap_or_default().to_string_lossy()).weak()).truncate())
                            .on_hover_text(display_path(&self.items[index].input.path));
                    }
                });
                if let Some(idle) = activity.idle(now) {
                    ui.colored_label(Color32::from_rgb(225,193,119), format!("No measurable progress for {}. Cancel export is available.", compact_time(idle.as_secs_f64().floor())))
                        .on_hover_text("Encoded time, frame count and output size have not advanced. This can happen during startup or finalization as well as a stalled backend. The elapsed timer alone does not prove FFmpeg is advancing.");
                }
            }
            if let Some(error)=&self.error{error_label(ui, error);}
            if let Some(index)=self.selected_index()&&let Some(error)=&self.items[index].error {error_label(ui, error);}
            ui.add_space(4.0);
        });
    }
    #[cfg(debug_assertions)]
    fn smoke(&mut self, ctx: &egui::Context) {
        let Some((path, started, requested)) = &mut self.smoke else {
            return;
        };
        if started.elapsed() > Duration::from_secs(3) && !*requested {
            let path = path.clone();
            let close = ctx.clone();
            ctx.request_screenshot(move |image| {
                use std::io::Write;
                let result = (|| -> std::io::Result<()> {
                    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
                    writeln!(file, "P6\n{} {}\n255", image.width(), image.height())?;
                    for pixel in &image.pixels {
                        file.write_all(&pixel.to_array()[..3])?;
                    }
                    file.flush()
                })();
                if let Err(error) = result {
                    eprintln!("Screenshot failed: {error}");
                }
                close.send_viewport_cmd(egui::ViewportCommand::Close);
            });
            *requested = true;
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = &root.ctx().clone();
        #[cfg(debug_assertions)]
        self.smoke(ctx);
        self.receive(ctx);
        let old_batch = self.batch.clone();
        let old_program = self.program.clone();
        self.toolbar(root);
        self.footer(root);
        self.queue_ui(root);
        let index = self.selected_index();
        let mut edit = index
            .map(|i| self.items[i].edit.clone())
            .unwrap_or_default();
        let info = index.and_then(|i| self.items[i].info.clone());
        let actions = self.options_ui(root, &mut edit, info.as_ref());
        self.central_ui(root, &mut edit, info.as_ref());
        self.keyboard_seek(ctx, &mut edit, info.as_ref());
        if let Some(index) = index {
            self.commit_edit(index, edit);
        }
        if old_batch.auto_end != self.batch.auto_end
            || old_batch.detection != self.batch.detection
            || old_batch.reference_name != self.batch.reference_name
        {
            self.sync_batch_detection();
        }
        if std::mem::take(&mut self.queued_find) {
            self.find_queue();
        }
        if old_batch != self.batch || old_program != self.program {
            self.dirty();
        }
        if actions.find {
            self.find_selected();
        }
        if actions.cancel_find {
            self.cancel_selected_search();
        }
        if actions.reference {
            self.reference(false);
        }
        if actions.use_frame {
            self.reference(true);
        }
        if !self.locked() {
            let dropped = ctx.input(|i| {
                i.raw
                    .dropped_files
                    .iter()
                    .map(|f| f.path().to_path_buf())
                    .filter(|p| media::supported_video(p))
                    .map(Input::new)
                    .collect::<Vec<_>>()
            });
            if !dropped.is_empty() {
                self.add(dropped);
            }
        }
        self.delete_dialog_ui(ctx);
        if self.save_at.is_some_and(|time| Instant::now() >= time) {
            self.save();
        }
        if self.busy
            || self.searching()
            || self.collecting
            || self.save_at.is_some()
            || self.threads.iter().any(|t| !t.is_finished())
        {
            ctx.request_repaint_after(Duration::from_millis(30));
        }
        // Preview replies arrive on a dedicated long-lived decoder thread.
        if self.selected.is_some() {
            ctx.request_repaint_after(Duration::from_millis(40));
        }
    }
    fn on_exit(&mut self, _: Option<&eframe::glow::Context>) {
        self.cancel.cancel();
        self.auxiliary_cancel.cancel();
        self.analysis_cancel.cancel();
        self.preview_cancel.cancel();
        for item in &self.items {
            item.find_cancel.cancel();
            item.probe_cancel.cancel();
        }
        self.preview_tx.take();
        if let Some(worker) = self.preview_worker.take() {
            let _ = worker.join();
        }
        for worker in self.threads.drain(..) {
            let _ = worker.join();
        }
        self.save();
    }
}

fn section(ui: &mut egui::Ui, label: &str) {
    ui.add_space(5.0);
    ui.label(RichText::new(label).small().strong().weak());
    ui.add_space(3.0);
}
fn seek_multiplier(modifiers: egui::Modifiers) -> f64 {
    if modifiers.shift {
        10.0
    } else if modifiers.alt {
        0.1
    } else {
        1.0
    }
}
fn seek_max(info: &MediaInfo) -> f64 {
    (info.duration - info.primary().frame_seconds().min(info.duration)).max(0.0)
}
fn destination_row(ui: &mut egui::Ui, path: &Path, mut choose: impl FnMut(PathBuf)) {
    let text = if path.as_os_str().is_empty() {
        "Choose a folder…".into()
    } else {
        display_path(path)
    };
    ui.horizontal(|ui| {
        let available = (ui.available_width() - 68.0).max(1.0);
        let font = egui::TextStyle::Body.resolve(ui.style());
        let text_width = ui.painter().layout_no_wrap(text.clone(), font, ui.visuals().text_color()).size().x;
        ui.add_sized(
            [text_width.min(available), 23.0],
            egui::Label::new(&text).truncate().halign(egui::Align::Min),
        ).on_hover_text(&text);
        if ui.button("Browse")
            .on_hover_text("Choose where to save new files. You can use the originals' folder; exports always get distinct filenames.")
            .on_disabled_hover_text("Select Save to folder to choose an export folder.")
            .clicked()
            && let Some(path) = rfd::FileDialog::new().pick_folder()
        {
            choose(path);
        }
    });
}
fn clear_keyboard_focus(ui: &mut egui::Ui) {
    ui.memory_mut(|memory| {
        if let Some(id) = memory.focused() {
            memory.surrender_focus(id);
        }
    });
}
fn preview_view_control(ui: &mut egui::Ui, preview: egui::Rect, view: &mut ViewMode) -> bool {
    let controls = egui::Rect::from_min_size(
        egui::pos2(preview.right() - 130.0, preview.top() + 8.0),
        egui::vec2(122.0, 30.0),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .id_salt("preview_controls")
            .max_rect(controls),
        |ui| {
            ui.set_clip_rect(preview);
            let id = ui.make_persistent_id("preview_mode");
            let visible = ui
                .ctx()
                .pointer_hover_pos()
                .is_some_and(|pos| controls.contains(pos))
                || egui::ComboBox::is_open(ui.ctx(), id)
                || ui.memory(|memory| memory.has_focus(id));
            ui.set_opacity(if visible { 1.0 } else { 0.45 });
            egui::ComboBox::from_id_salt("preview_mode")
                .width(110.0)
                .selected_text(view.label())
                .show_ui(ui, |ui| {
                    let mut picked = false;
                    for mode in ViewMode::ALL {
                        if ui.selectable_value(view, mode, mode.label()).clicked() {
                            picked = true;
                            ui.close();
                        }
                    }
                    picked
                })
                .inner
                .unwrap_or(false)
        },
    )
    .inner
}
fn queue_label(
    ui: &mut egui::Ui,
    text: impl Into<egui::WidgetText>,
    height: f32,
    sense: egui::Sense,
) -> egui::Response {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| ui.add(egui::Label::new(text).truncate().sense(sense)),
    )
    .inner
}
fn compact_time(seconds: f64) -> String {
    let millis = (seconds.max(0.0) * 1000.0).round() as u64;
    let seconds = millis / 1000;
    let mut text = if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    };
    if !millis.is_multiple_of(1000) {
        text.push_str(format!(".{:03}", millis % 1000).trim_end_matches('0'));
    }
    text
}
fn queue_timing(duration: Option<f64>, edit: &VideoEdit) -> String {
    let Some(duration) = duration.filter(|duration| duration.is_finite() && *duration > 0.0) else {
        return "Duration unavailable".into();
    };
    let total = compact_time(duration);
    let Ok((start, end)) = parse_time(&edit.start).and_then(|start| {
        optional_time(&edit.end).map(|end| (start, end.unwrap_or(duration).min(duration)))
    }) else {
        return format!("{total} · Invalid trim");
    };
    if end - start < 0.05 {
        return format!("{total} · Invalid trim");
    }
    if start > 0.0 || end < duration {
        format!(
            "{total} · Trim {}–{} ({})",
            compact_time(start),
            compact_time(end),
            compact_time(end - start)
        )
    } else {
        total
    }
}
fn queue_card(
    ui: &mut egui::Ui,
    item: &Item,
    active: bool,
    selected: bool,
    processing: bool,
) -> egui::Response {
    let width = ui.available_width();
    ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
        egui::Frame::new().fill(if selected { Color32::from_rgb(37,66,53) } else { Color32::from_rgb(30,34,39) })
            .corner_radius(5.0).inner_margin(egui::Margin::symmetric(6,4)).show(ui, |ui| {
                ui.set_width((width - 12.0).max(1.0));
                let name = item.input.path.file_name().unwrap_or_default().to_string_lossy();
                queue_label(ui, RichText::new(name).color(if processing { Color32::WHITE } else if active { GREEN } else { ui.visuals().text_color() }), 18.0, egui::Sense::hover())
                    .on_hover_text(format!("{}\nCtrl+Click: toggle selection · Shift+Click: select a range\nRight-click for queue and file actions.", display_path(&item.input.path)));
                let timing = if item.info.is_none() && item.error.is_none() { "Reading duration…".into() }
                    else { queue_timing(item.info.as_ref().map(|info| info.duration), &item.edit) };
                let status = if !item.status.is_empty() { item.status.as_str() }
                    else if matches!(item.edit.feedback, EndFeedback::Searching) { "Finding end…" } else { "" };
                let neutral = egui::TextFormat { font_id: egui::FontId::proportional(11.0), color: if processing { Color32::WHITE } else { ui.visuals().weak_text_color() }, ..Default::default() };
                let mut details = egui::text::LayoutJob::default();
                details.append(&timing, 0.0, neutral.clone());
                if !status.is_empty() {
                    details.append(" · ", 0.0, neutral.clone());
                    let color = if status == "Done" { GREEN }
                        else if status == "Failed" || item.error.is_some() { RED }
                        else { neutral.color };
                    details.append(status, 0.0, egui::TextFormat { color, ..neutral });
                }
                let tooltip = item.error_message().map(error_summary).unwrap_or_else(|| details.text.clone());
                queue_label(ui, details, 16.0, egui::Sense::hover())
                    .on_hover_text(tooltip);
            });
    }).response
}
fn error_summary(error: &str) -> String {
    fn specific(message: &str) -> bool {
        [
            "permission denied",
            "access is denied",
            "no space left",
            "disk full",
            "no such file",
            "not found",
            "cannot load",
            "unsupported",
            "not supported",
            "not currently supported",
            "unknown encoder",
            "no capable devices",
            "driver does not support",
            "invalid data found",
            "not divisible",
        ]
        .iter()
        .any(|term| message.contains(term))
    }
    let mut best = "";
    let mut best_score = 0;
    for raw in error.split(['\r', '\n']) {
        let mut line = raw.trim();
        // FFmpeg prefixes contain a component name and an unstable address.
        while let Some((prefix, rest)) =
            line.strip_prefix('[').and_then(|line| line.split_once(']'))
        {
            if !prefix.contains(" @ ") && !["error", "fatal", "warning"].contains(&prefix) {
                break;
            }
            line = rest.trim();
        }
        if let Some((number, rest)) = line.split_once(':')
            && !number.is_empty()
            && number.chars().all(|ch| ch.is_ascii_digit())
        {
            if rest.contains("::") {
                continue;
            }
            line = rest.trim();
        }
        let lower = line.to_lowercase();
        if line.is_empty()
            || lower == "caused by:"
            || lower.contains("stack backtrace")
            || lower.starts_with("at ")
        {
            continue;
        }
        let wrapper = lower.starts_with("backend failed")
            || lower.ends_with(':')
            || [
                "conversion failed",
                "task finished with error code",
                "terminating thread",
                "error sending frames",
                "error while filtering",
                "error while processing",
                "error opening output files",
                "error opening input files",
                "could not open encoder before eof",
                "nothing was written",
            ]
            .iter()
            .any(|term| lower.contains(term));
        let score = if wrapper {
            1
        } else if specific(&lower) {
            3
        } else if [
            "error",
            "failed",
            "cannot",
            "could not",
            "unable",
            "invalid",
        ]
        .iter()
        .any(|term| lower.contains(term))
        {
            2
        } else {
            0
        };
        if best.is_empty() || score > best_score {
            best = line;
            best_score = score;
        }
    }
    // An anyhow context chain can also be formatted entirely on one line.
    let cause = best
        .rsplit(": ")
        .find(|part| specific(&part.to_lowercase()))
        .unwrap_or(best);
    let summary = if cause.is_empty() {
        "Unknown error.".into()
    } else if error.starts_with("Output saved to ") && error.contains("Original retained:") {
        format!("Output saved; original retained: {cause}")
    } else {
        cause.to_owned()
    };
    let mut text: String = summary.chars().take(240).collect();
    if summary.chars().count() > 240 {
        text.push('…');
    }
    text
}
fn error_label(ui: &mut egui::Ui, error: &str) -> egui::Response {
    let summary = error_summary(error);
    ui.add(egui::Label::new(RichText::new(&summary).color(RED)).truncate())
        .on_hover_text(summary)
}

fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    #[cfg(windows)]
    {
        // Canonical paths use verbatim prefixes internally; show familiar paths
        // in the UI while retaining the leading backslashes of network shares.
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{rest}");
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return rest.to_owned();
        }
    }
    text.into_owned()
}

fn open_containing_folder(video: &Path) -> anyhow::Result<()> {
    let video = std::path::absolute(video)?;
    let folder = video
        .parent()
        .ok_or_else(|| anyhow::anyhow!("No parent folder"))?;
    anyhow::ensure!(
        folder.is_dir(),
        "Folder does not exist: {}",
        folder.display()
    );
    open_folder_in_file_manager(folder)
}

#[cfg(windows)]
fn shell_folder_path(folder: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let path: Vec<_> = folder.as_os_str().encode_wide().collect();
    // Queue paths are canonicalized, but Explorer does not accept verbatim prefixes.
    let unc: Vec<_> = r"\\?\UNC\".encode_utf16().collect();
    let verbatim: Vec<_> = r"\\?\".encode_utf16().collect();
    let mut path = if let Some(rest) = path.strip_prefix(unc.as_slice()) {
        [r"\\".encode_utf16().collect::<Vec<_>>(), rest.to_vec()].concat()
    } else {
        path.strip_prefix(verbatim.as_slice())
            .unwrap_or(&path)
            .to_vec()
    };
    path.push(0);
    path
}

#[cfg(windows)]
fn open_folder_in_file_manager(folder: &Path) -> anyhow::Result<()> {
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
    let folder = shell_folder_path(folder);
    // SAFETY: the folder is null-terminated and valid throughout the call; all
    // optional pointer arguments are null. The shell owns the launched window.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            folder.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    anyhow::ensure!(
        result > 32,
        "Windows could not open the folder (shell error {result})"
    );
    Ok(())
}

#[cfg(not(windows))]
fn open_folder_in_file_manager(folder: &Path) -> anyhow::Result<()> {
    let launcher = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut child = std::process::Command::new(launcher).arg(folder).spawn()?;
    // The file manager may stay open; reap its launcher without blocking the UI.
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn delete_video_file(path: &Path, cancel: &Cancel) -> anyhow::Result<()> {
    let started = Instant::now();
    loop {
        cancel.check()?;
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            // Give our cancelled FFmpeg/FFprobe processes time to release Windows
            // handles, without blocking the UI or retrying unrelated failures.
            Err(error)
                if cfg!(windows)
                    && matches!(error.raw_os_error(), Some(32 | 33))
                    && started.elapsed() < Duration::from_secs(2) =>
            {
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error.into()),
        }
    }
}
fn settings_editor_id(
    ctx: &egui::Context,
    tab: SettingsTab,
    video: Option<u64>,
    reset: bool,
) -> egui::Id {
    let video = (tab == SettingsTab::Video).then_some(video).flatten();
    let id = egui::Id::unique(("settings_controls", tab, video));
    let revision = ctx.data_mut(|data| {
        let mut revision = data.get_temp::<u64>(id).unwrap_or(0);
        if reset {
            revision = revision.wrapping_add(1);
            data.insert_temp(id, revision);
        }
        revision
    });
    id.with(revision)
}
fn video_options_ui(ui: &mut egui::Ui, video: &mut VideoOptions, info: Option<&MediaInfo>) {
    ui.horizontal(|ui| {
        ui.label("Encoder");
        egui::ComboBox::from_id_salt("encoder").selected_text(video.encoder.label()).show_ui(ui, |ui| {
            for value in [Encoder::Auto, Encoder::Software, Encoder::Nvenc] {
                ui.selectable_value(&mut video.encoder, value, value.label());
            }
        }).response.on_hover_text("Automatic tries NVENC and falls back to x265. Explicit NVIDIA requires a compatible GPU.");
    });
    ui.horizontal(|ui| {
        ui.label("Quality");
        ui.add(egui::Slider::new(&mut video.quality, 15..=30)).on_hover_text("Lower = higher quality and usually larger files. HEVC (x265) uses CRF; NVIDIA uses CQ.");
    });
    ui.separator();
    resize_ui(ui, video, info);
    ui.separator();
    ui.checkbox(&mut video.force, "Re-encode HEVC").on_hover_text("Enabled by default. Re-encode already-HEVC videos. Uncheck to skip them unless trimming or resizing requires encoding. Re-encoding is lossy.");
    ui.checkbox(&mut video.compare, "Compare quality").on_hover_text("Compute SSIM and PSNR before publication. Failed comparisons block output. Adds another decoding pass.");
    ui.horizontal(|ui| {
        ui.add_enabled_ui(video.compare, |ui| {
            ui.checkbox(&mut video.require_ssim, "Minimum SSIM")
                .on_hover_text("Reject output if any video stream scores below this value.");
            ui.add_enabled_ui(video.require_ssim, |ui| {
                ui.add(
                    egui::DragValue::new(&mut video.min_ssim)
                        .speed(0.001)
                        .range(0.0..=1.0)
                        .max_decimals(4),
                );
            });
        });
    });
}
fn resize_ui(ui: &mut egui::Ui, edit: &mut VideoOptions, info: Option<&MediaInfo>) {
    ui.checkbox(&mut edit.resize.enabled,"Resize").on_hover_text("Shrink only. The dimensions below are capped at the source's size; videos are never enlarged.");
    ui.add_enabled_ui(edit.resize.enabled,|ui| {
        ui.horizontal(|ui|{ui.label("Mode");egui::ComboBox::from_id_salt("resize_mode").selected_text(match edit.resize.mode{ResizeMode::Bounds=>"Pixel bounds",ResizeMode::Height=>"Height only",ResizeMode::Percent=>"Percentage"}).show_ui(ui,|ui|{ui.selectable_value(&mut edit.resize.mode,ResizeMode::Bounds,"Pixel bounds");ui.selectable_value(&mut edit.resize.mode,ResizeMode::Height,"Height only");ui.selectable_value(&mut edit.resize.mode,ResizeMode::Percent,"Percentage");});});
        ui.checkbox(&mut edit.resize.keep_aspect,"Keep original aspect ratio").on_hover_text("Height only: compute width from height. Percentage: use one scale for both axes. Off allows independent dimensions and may distort the image.");
        ui.horizontal(|ui|{match edit.resize.mode {
            ResizeMode::Bounds=>{ui.label("W");ui.add(egui::DragValue::new(&mut edit.resize.width).range(2..=16384).suffix(" px"));ui.label("H");ui.add(egui::DragValue::new(&mut edit.resize.height).range(2..=16384).suffix(" px"));},
            ResizeMode::Height=>{ui.label("Height");ui.add(egui::DragValue::new(&mut edit.resize.height).range(2..=16384).suffix(" px"));},
            ResizeMode::Percent=>{if !edit.resize.keep_aspect{ui.label("W");}ui.add(egui::DragValue::new(&mut edit.resize.percent).range(0.1..=100.0).speed(0.5).suffix("%"));if !edit.resize.keep_aspect{ui.label("H");ui.add(egui::DragValue::new(&mut edit.resize.height_percent).range(0.1..=100.0).speed(0.5).suffix("%"));}},
        }
            if edit.resize.mode != ResizeMode::Bounds && let Some(info)=info { output_size_label(ui,edit,info); }
        });
    });
    if edit.resize.mode == ResizeMode::Bounds
        && let Some(info) = info
    {
        output_size_label(ui, edit, info);
    }
}
fn output_size_label(ui: &mut egui::Ui, edit: &VideoOptions, info: &MediaInfo) {
    let stream = info.primary();
    let (w, h) = if stream.rotated() {
        (stream.height, stream.width)
    } else {
        (stream.width, stream.height)
    };
    let (w, h) = edit.resize.dimensions(w, h);
    ui.label(RichText::new(format!("{w} × {h} px")).small().color(GREEN)).on_hover_text("Predicted display-oriented pixel dimensions. Resizing rounds down to even pixels and never enlarges.");
}
fn cancel_find(item: &mut Item) {
    if matches!(item.edit.feedback, EndFeedback::Searching) {
        item.find_cancel.cancel();
        // Invalidate even an already-queued success so Cancel cannot move End.
        item.find_ticket += 1;
        item.find_started = None;
        item.edit.feedback = EndFeedback::Cancelled;
    }
}
fn detection_ui(
    ui: &mut egui::Ui,
    edit: &mut VideoEdit,
    actions: &mut Actions,
    has_frame: bool,
    activity: Option<SearchActivity>,
    enabled: bool,
    scope: DetectionScope,
) {
    let DetectionScope { batch, can_find } = scope;
    if let Some(activity) = activity {
        ui.horizontal(|ui| {
            actions.cancel_find = ui.button("Cancel search").on_hover_text(if activity.batch {
                "Stop this batch search, including queued videos. Existing end markers are kept."
            } else {
                "Stop this search and its FFmpeg process. The end marker stays unchanged."
            }).clicked();
            if let Some(elapsed) = activity.elapsed {
                ui.label(format!("{} s", elapsed.as_secs()));
            } else {
                ui.label("Waiting…");
            }
        });
        ui.add(egui::ProgressBar::new(activity.progress.min(0.999))
            .desired_width(ui.available_width())
            .text(format!("{:.0}% scanned", activity.progress.min(0.99) * 100.0))
            .animate(true))
            .on_hover_text("Shows how much video after the grace period has been checked, moving forward. The first confirmed event ends the search; elapsed time is not an estimated time remaining.");
    } else {
        actions.find=ui.add_enabled(enabled && can_find && (batch || edit.auto_end),egui::Button::new(if batch { "Find ends in queue" } else { "Find end" })).on_hover_text("Search now. A match moves the red end marker automatically. Later settings changes require another explicit search.").clicked();
    }
    ui.add_enabled_ui(enabled && edit.auto_end, |ui| {
        if edit.detection.criterion == Criterion::Reference {
            edit.detection.reference_enabled = true;
            edit.detection.solid_screen = false;
            edit.detection.criterion = Criterion::Black;
        }
        ui.horizontal(|ui| {
            ui.label("Grace period");
            let mut minutes = edit.detection.grace_seconds / 60.0;
            ui.add(egui::DragValue::new(&mut minutes).range(2.0..=1440.0).speed(0.1).suffix(" min"))
                .on_hover_text("Ignore events within this interval after Begin. Minimum two minutes; default two minutes.");
            edit.detection.grace_seconds = minutes * 60.0;
        });
        ui.horizontal(|ui| {
            ui.label("Down period");
            ui.add(egui::DragValue::new(&mut edit.detection.hold_seconds).range(0.1..=3600.0).speed(0.5).suffix(" s"))
                .on_hover_text("One detector must remain true for this duration. The marker is placed at its first frame, not after the wait. Brief events and recoveries are ignored.");
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut edit.detection.scene_change, "Scene change")
                .on_hover_text("A sharp histogram change that stays different for the Down period. A normal scene cut can also match; raise Change or disable this detector if needed.");
            ui.add_enabled_ui(edit.detection.scene_change, |ui| {
                ui.label("Change");
                let mut percent = edit.detection.scene_threshold * 100.0;
                ui.add(egui::DragValue::new(&mut percent).range(5.0..=100.0).suffix("%"))
                    .on_hover_text("Minimum change in adjacent-frame color distributions. Higher values require a more abrupt change.");
                edit.detection.scene_threshold = percent / 100.0;
            });
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut edit.detection.solid_screen, "Black / solid color")
                .on_hover_text("Detect a predominantly black, white or chosen-color image for the Down period.");
            ui.add_enabled_ui(edit.detection.solid_screen, |ui| {
                egui::ComboBox::from_id_salt("screen_color").width(68.0)
                    .selected_text(match edit.detection.criterion { Criterion::White => "White", Criterion::Color => "Color", _ => "Black" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut edit.detection.criterion, Criterion::Black, "Black");
                        ui.selectable_value(&mut edit.detection.criterion, Criterion::White, "White");
                        ui.selectable_value(&mut edit.detection.criterion, Criterion::Color, "Color");
                    });
            });
        });
        ui.add_enabled_ui(edit.detection.solid_screen, |ui| {
            ui.horizontal(|ui| {
                ui.label("Pixels");
                let mut percent = edit.detection.pixel_fraction * 100.0;
                ui.add(egui::DragValue::new(&mut percent).range(1.0..=100.0).suffix("%"))
                    .on_hover_text("Minimum matching percentage of the image.");
                edit.detection.pixel_fraction = percent / 100.0;
                match edit.detection.criterion {
                    Criterion::Color => {
                        ui.color_edit_button_srgb(&mut edit.detection.color).on_hover_text("Choose the dominant screen color.");
                        ui.label("±");
                        ui.add(egui::DragValue::new(&mut edit.detection.color_tolerance).range(0..=255))
                            .on_hover_text("Allowed difference in each RGB channel.");
                    }
                    _ => {
                        ui.label("Level");
                        let level = if edit.detection.criterion == Criterion::White { &mut edit.detection.white_level } else { &mut edit.detection.black_level };
                        ui.add(egui::DragValue::new(level).range(0..=255)).on_hover_text("Black: luminance at or below this value. White: at or above it. Range 0–255.");
                    }
                }
            });
        });
        ui.checkbox(&mut edit.detection.exit_fullscreen, "Exit full screen")
            .on_hover_text("Look for persistent browser-style toolbar bands and text edges. This visual heuristic may miss some applications or match interface-like scenes.");
        ui.checkbox(&mut edit.detection.loading_spinner, "Spinning load circle")
            .on_hover_text("Look for a small rotating ring near likely player centers while the surrounding image stays still. Not all spinner designs or positions are recognized.");
        ui.horizontal(|ui| {
            ui.checkbox(&mut edit.detection.reference_enabled, "Reference histogram")
                .on_hover_text("Also match an image's color distribution. This adds another way to end the video; it does not constrain the other selected detectors.");
            ui.add_enabled_ui(edit.detection.reference_enabled, |ui| {
                let mut percent = edit.detection.similarity * 100.0;
                ui.add(egui::DragValue::new(&mut percent).range(0.0..=100.0).suffix("%"))
                    .on_hover_text("Required histogram similarity. Different images with similar colors can match.");
                edit.detection.similarity = percent / 100.0;
            });
        });
        ui.add_enabled_ui(edit.detection.reference_enabled, |ui| {
            ui.horizontal(|ui| {
                actions.use_frame = ui.add_enabled(has_frame, egui::Button::new("Current frame"))
                    .on_hover_text("Use the displayed frame as the histogram reference.").clicked();
                actions.reference = ui.button("Image…").on_hover_text("Choose a reference image.").clicked();
            });
            if edit.detection.reference.is_some() {
                ui.label(RichText::new(if edit.reference_name.is_empty() { "Reference loaded" } else { &edit.reference_name }).small().weak());
            }
        });
    });
    if batch {
        return;
    }
    let (text, color) = match &edit.feedback {
        EndFeedback::NotSearched => ("Not searched".into(), ui.visuals().weak_text_color()),
        EndFeedback::Stale => (
            "Settings changed · find again".into(),
            Color32::from_rgb(225, 193, 119),
        ),
        EndFeedback::Searching => ("Finding end…".into(), GREEN),
        EndFeedback::Cancelled => (
            "Cancelled · end unchanged".into(),
            ui.visuals().weak_text_color(),
        ),
        EndFeedback::Found(time) => (format!("Match · {}", time_text(*time)), GREEN),
        EndFeedback::NoMatch => (
            "No match · end unchanged".into(),
            Color32::from_rgb(225, 193, 119),
        ),
        EndFeedback::Failed(error) => (format!("Search failed: {}", error_summary(error)), RED),
    };
    ui.add(egui::Label::new(RichText::new(&text).small().color(color)).truncate())
        .on_hover_text(text);
}
fn aspect_text(info: &MediaInfo) -> String {
    fn gcd(mut a: u64, mut b: u64) -> u64 {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a.max(1)
    }
    let stream = info.primary();
    let (n, d) = stream
        .sample_aspect_ratio
        .as_deref()
        .and_then(|s| s.split_once(':'))
        .and_then(|(a, b)| Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok()?)))
        .filter(|(a, b)| *a > 0 && *b > 0)
        .unwrap_or((1, 1));
    let (mut a, mut b) = (stream.width as u64 * n, stream.height as u64 * d);
    if stream.rotated() {
        (a, b) = (b, a);
    }
    let divisor = gcd(a, b);
    format!("{}:{}  ({:.3})", a / divisor, b / divisor, info.aspect())
}
fn load_preferences(current: Option<&Path>) -> anyhow::Result<Session> {
    let Some(path) = current.filter(|path| path.is_file()) else {
        return Ok(Session::default());
    };
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}
#[cfg(not(test))]
fn session_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("TAILOUT_STATE_PATH") {
        return Some(path.into());
    }
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("TAILOUT_UI_SMOKE") {
        return Some(PathBuf::from(path).with_extension("session.json"));
    }
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|root| PathBuf::from(root).join("Tailout/session.json"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|root| PathBuf::from(root).join(".config")))
            .map(|root| root.join("tailout/session.json"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use eframe::App as _;
    struct Harness {
        app: App,
        ctx: egui::Context,
        previews: Receiver<PreviewRequest>,
    }
    impl Harness {
        fn new() -> Self {
            let ctx = egui::Context::default();
            let mut app = App::new(&eframe::CreationContext::_new_kittest(ctx.clone()));
            app.preview_tx.take();
            app.preview_worker.take().unwrap().join().unwrap();
            let (tx, previews) = mpsc::channel();
            app.preview_tx = Some(tx);
            app.items.clear();
            for id in 1..=2 {
                let mut item = Item::new(
                    id,
                    Input::new(format!("test-{id}.mp4").into()),
                    VideoEdit::default(),
                );
                item.info = Some(MediaInfo {
                    streams: vec![media::Stream {
                        width: 1920,
                        height: 1080,
                        codec_type: "video".into(),
                        codec_name: Some("h264".into()),
                        avg_frame_rate: Some("30/1".into()),
                        ..Default::default()
                    }],
                    chapters: Vec::new(),
                    duration: 30.0,
                });
                app.items.push(item);
            }
            app.selected = Some(1);
            app.queue_selection = BTreeSet::from([1]);
            app.selection_anchor = Some(1);
            app.next_id = 3;
            app.save_at = None;
            Self { app, ctx, previews }
        }
        fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            self.frame_with_modifiers(events, egui::Modifiers::default())
        }
        fn frame_with_modifiers(
            &mut self,
            events: Vec<egui::Event>,
            modifiers: egui::Modifiers,
        ) -> egui::FullOutput {
            let mut frame = eframe::Frame::_new_kittest();
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1360.0, 920.0),
                )),
                events,
                ..Default::default()
            };
            input
                .events
                .insert(0, egui::Event::ModifiersChanged(modifiers));
            let mut output = self.ctx.run_ui(input, |ui| self.app.ui(ui, &mut frame));
            // Headless tests inspect shapes without uploading textures to a GPU.
            output.textures_delta.clear();
            output
        }
        fn timeline(&mut self, events: Vec<egui::Event>) -> egui::Rect {
            let mut rect = egui::Rect::NOTHING;
            let info = self.app.items[0].info.clone().unwrap();
            let mut edit = self.app.items[0].edit.clone();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 200.0),
                )),
                events,
                ..Default::default()
            };
            let mut output = self.ctx.run_ui(input, |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    rect = self.app.timeline(ui, &mut edit, &info);
                });
            });
            output.textures_delta.clear();
            self.app.commit_edit(0, edit);
            rect
        }
    }
    impl Drop for Harness {
        fn drop(&mut self) {
            self.app.on_exit(None);
        }
    }
    fn pointer(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }
    fn key(key: egui::Key, pressed: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Default::default(),
        }
    }
    fn text_centers(output: &egui::FullOutput, needle: &str) -> Vec<egui::Pos2> {
        fn visit(shape: &egui::Shape, needle: &str, positions: &mut Vec<egui::Pos2>) {
            match shape {
                egui::Shape::Text(text) if text.galley.job.text == needle => {
                    positions.push(text.pos + text.galley.size() * 0.5);
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        visit(shape, needle, positions);
                    }
                }
                _ => {}
            }
        }
        let mut positions = Vec::new();
        for shape in &output.shapes {
            visit(&shape.shape, needle, &mut positions);
        }
        positions
    }

    #[test]
    fn export_bar_covers_one_whole_video_and_resets_only_for_the_next_file() {
        let mut h = Harness::new();
        h.app.busy = true;
        for (phase, stage, overall, shown) in [
            ("Encoding", Some(0.5), 0.45, "45.0%"),
            ("Reading output", None, 0.9, "90.0%"),
            ("Validating", Some(0.0), 0.9, "90.0%"),
            ("Validating", Some(1.0), 0.99, "99.0%"),
            ("Saving", None, 0.99, "99.0%"),
            ("Done", Some(1.0), 1.0, "100.0%"),
        ] {
            h.app
                .tx
                .send(Message::Progress(
                    1,
                    0,
                    160,
                    ConversionProgress {
                        fraction: stage,
                        overall,
                        ..ConversionProgress::stage(phase, Some("libx265"))
                    },
                ))
                .unwrap();
            h.frame(Vec::new());
            let output = h.frame(Vec::new());
            assert!(!text_centers(&output, shown).is_empty());
            assert_eq!(
                h.app.export_activity.as_ref().unwrap().event.overall,
                overall
            );
            assert!(
                h.app.progress < 0.01,
                "a 160-video queue must not shrink the visible file progress"
            );
        }
        h.app
            .tx
            .send(Message::Progress(
                2,
                1,
                160,
                ConversionProgress::stage("Reading", None),
            ))
            .unwrap();
        h.frame(Vec::new());
        let output = h.frame(Vec::new());
        assert!(!text_centers(&output, "0.0%").is_empty());
        assert_eq!(h.app.export_activity.as_ref().unwrap().id, 2);
        assert_eq!(h.app.export_activity.as_ref().unwrap().event.overall, 0.0);
        assert!(h.app.status.starts_with("2 / 160"));
        h.app.tx.send(Message::Finished(vec![1, 2])).unwrap();
        h.app.receive(&h.ctx);
        assert!(h.app.export_activity.is_none());
    }
    #[test]
    fn unchanged_ffmpeg_reports_do_not_hide_a_stall_and_stage_changes_keep_elapsed_time() {
        let now = Instant::now();
        let event = ConversionProgress {
            fraction: Some(0.25),
            overall: 0.225,
            stats: Some(tailout::process::FfmpegProgress {
                seconds: Some(10.0),
                frame: Some(300),
                bytes: Some(4096),
                speed: Some(0.5),
                ..Default::default()
            }),
            ..ConversionProgress::stage("Encoding", Some("libx265"))
        };
        let mut activity = ExportActivity::new(1, event.clone(), now - Duration::from_secs(120));
        activity.last_advance = now - Duration::from_secs(40);
        activity.update(event.clone(), now);
        assert_eq!(activity.idle(now), Some(Duration::from_secs(40)));
        assert!(!activity.summary(now).contains("remaining"));
        activity.update(
            ConversionProgress {
                overall: 0.225,
                stats: Some(Default::default()),
                ..ConversionProgress::stage("Encoding", Some("libx265"))
            },
            now,
        );
        activity.update(event.clone(), now);
        assert_eq!(
            activity.idle(now),
            Some(Duration::from_secs(40)),
            "an N/A report must not reset measured counters"
        );
        assert!(activity.phase_label().contains("HEVC (x265)"));
        let mut advancing = event;
        advancing.fraction = Some(0.3);
        advancing.overall = 0.27;
        advancing.stats.as_mut().unwrap().frame = Some(301);
        activity.update(advancing, now);
        assert!(activity.idle(now).is_none());
        assert_eq!(activity.summary(now), "Elapsed 2:00 · 0.50×");
        let before = activity.started;
        activity.update(
            ConversionProgress {
                overall: 0.9,
                ..ConversionProgress::stage("Validating", Some("libx265"))
            },
            now,
        );
        assert_eq!(activity.started, before);
        assert_eq!(activity.event.overall, 0.9);
        assert!(activity.event.fraction.is_none());
        assert!(!activity.summary(now).contains("remaining"));
    }
    #[test]
    fn search_progress_is_per_video_and_rejects_stale_updates() {
        let mut h = Harness::new();
        for item in &mut h.app.items {
            item.edit.feedback = EndFeedback::Searching;
            item.find_ticket = 7;
        }
        h.app
            .tx
            .send(Message::FindProgress(2, 7, 0.4, None))
            .unwrap();
        h.app
            .tx
            .send(Message::FindProgress(2, 6, 0.9, None))
            .unwrap();
        h.app
            .tx
            .send(Message::FindProgress(2, 7, f64::NAN, None))
            .unwrap();
        h.app
            .tx
            .send(Message::FindProgress(2, 7, 0.2, None))
            .unwrap();
        h.app.receive(&h.ctx);
        assert_eq!(h.app.items[0].find_progress, 0.0);
        assert_eq!(h.app.items[1].find_progress, 0.4);
        assert!(h.app.items[1].find_started.is_some());
        h.app.cancel_selected_search();
        assert_eq!(h.app.items[0].edit.feedback, EndFeedback::Cancelled);
        assert_eq!(h.app.items[1].edit.feedback, EndFeedback::Searching);
        assert!(!h.app.items[1].find_cancel.is_cancelled());
    }

    #[test]
    fn inline_cancel_works_during_single_and_batch_searches_and_ignores_late_matches() {
        for batch in [false, true] {
            let mut h = Harness::new();
            h.app.finding_batch = batch;
            h.app.settings_tab = if batch {
                SettingsTab::Batch
            } else {
                SettingsTab::Video
            };
            h.app.progress = 0.35;
            for item in &mut h.app.items {
                item.edit.auto_end = true;
                item.edit.end = "20".into();
                item.edit.feedback = EndFeedback::Searching;
                item.find_ticket = 7;
                item.find_started = Some(Instant::now());
                item.find_progress = 0.35;
            }
            h.frame(Vec::new());
            let output = h.frame(Vec::new());
            assert!(!text_centers(&output, "35% scanned").is_empty());
            let pos = text_centers(&output, "Cancel search")
                .into_iter()
                .find(|pos| pos.x > 350.0)
                .expect("Cancel in the detector section, outside disabled controls");
            h.frame(vec![egui::Event::PointerMoved(pos)]);
            h.frame(vec![pointer(pos, true)]);
            h.frame(vec![pointer(pos, false)]);
            assert_eq!(h.app.items[0].edit.feedback, EndFeedback::Cancelled);
            assert!(h.app.items[0].find_cancel.is_cancelled());
            assert_eq!(
                h.app.items[1].edit.feedback,
                if batch {
                    EndFeedback::Cancelled
                } else {
                    EndFeedback::Searching
                }
            );
            assert_eq!(h.app.analysis_cancel.is_cancelled(), batch);
            h.app.tx.send(Message::Found(1, 7, Ok(Some(12.0)))).unwrap();
            h.app
                .tx
                .send(Message::FindProgress(1, 7, 0.9, None))
                .unwrap();
            h.app.receive(&h.ctx);
            assert_eq!(h.app.items[0].edit.end, "20");
            assert_eq!(h.app.items[0].edit.feedback, EndFeedback::Cancelled);
            if batch {
                h.app.tx.send(Message::AnalysisDone).unwrap();
                h.app.receive(&h.ctx);
                assert!(!h.app.finding_batch);
                assert!(h.app.status.contains("cancelled"));
            }
        }
    }

    #[test]
    fn settings_tabs_create_local_overrides_and_reset_resumes_inheritance() {
        fn click_at(h: &mut Harness, at: egui::Pos2) {
            h.frame(vec![egui::Event::PointerMoved(at)]);
            h.frame(vec![pointer(at, true)]);
            h.frame(vec![pointer(at, false)]);
        }
        fn click(h: &mut Harness, label: &str) {
            let output = h.frame(Vec::new());
            click_at(h, text_centers(&output, label)[0]);
        }
        fn local_quality(h: &mut Harness, text: &str, submit: bool) {
            let output = h.frame(Vec::new());
            let at = text_centers(&output, "21")
                .into_iter()
                .max_by(|a, b| a.y.total_cmp(&b.y))
                .unwrap();
            click_at(h, at);
            h.frame(Vec::new());
            h.frame(vec![egui::Event::Text(text.into())]);
            if submit {
                h.frame(vec![key(egui::Key::Enter, true)]);
                h.frame(vec![key(egui::Key::Enter, false)]);
            }
        }
        let mut h = Harness::new();
        h.app.program.queue_sort = QueueSort::Added;
        h.app.items[0].edit.start = "4".into();
        h.app.items[0].edit.end = "24".into();
        h.app.items[0].edit.auto_end = true;
        h.app.items[0].edit.detection.hold_seconds = 3.5;
        h.app.items[1].edit.start = "8".into();
        h.app.items[1].edit.end = "28".into();
        h.app.items[1].edit.detection.hold_seconds = 7.0;
        let marker_state: Vec<_> = h
            .app
            .items
            .iter()
            .map(|item| {
                (
                    item.edit.start.clone(),
                    item.edit.end.clone(),
                    item.edit.detection.clone(),
                    item.edit.auto_end,
                )
            })
            .collect();
        h.frame(Vec::new());
        let output = h.frame(Vec::new());
        assert!(text_centers(&output, "Batch defaults").is_empty());
        assert!(text_centers(&output, "This video").is_empty());
        assert!(text_centers(&output, "Override settings for this video").is_empty());
        assert!(text_centers(&output, "Reset to batch settings").is_empty());
        click(&mut h, "Re-encode HEVC");
        assert!(!h.app.batch.video.force);
        assert!(
            h.app
                .items
                .iter()
                .all(|item| !item.edit.settings(&h.app.batch).unwrap().force)
        );
        click(&mut h, "Per-video settings");
        h.frame(Vec::new());
        assert!(
            !h.app.items[0].edit.override_video,
            "switching tabs does not create an override"
        );
        local_quality(&mut h, "18", false);
        assert!(h.app.items[0].edit.override_video);
        assert_eq!(
            h.app.items[0].edit.settings(&h.app.batch).unwrap().quality,
            18
        );
        assert_eq!(
            h.app.items[1].edit.settings(&h.app.batch).unwrap().quality,
            21
        );
        click(&mut h, "Reset to batch settings");
        for _ in 0..3 {
            h.frame(Vec::new());
        }
        assert!(
            !h.app.items[0].edit.override_video,
            "reset while editing a number must stay reset"
        );
        assert_eq!(
            h.app.items[0].edit.settings(&h.app.batch).unwrap().quality,
            21
        );
        local_quality(&mut h, "18", true);
        assert!(h.app.items[0].edit.override_video);
        click(&mut h, "Batch settings");
        click(&mut h, "Resize");
        assert!(h.app.batch.video.resize.enabled);
        assert!(!h.app.items[0].edit.settings(&h.app.batch).unwrap().resize);
        assert!(h.app.items[1].edit.settings(&h.app.batch).unwrap().resize);
        h.app.add(vec![Input::new("added-later.mp4".into())]);
        assert!(h.app.items[2].edit.settings(&h.app.batch).unwrap().resize);
        assert!(!h.app.items[2].edit.override_video);
        click(&mut h, "Per-video settings");
        click(&mut h, "Reset to batch settings");
        assert!(!h.app.items[0].edit.override_video);
        assert!(h.app.items[0].edit.settings(&h.app.batch).unwrap().resize);
        for (index, mut expected) in marker_state.into_iter().enumerate() {
            if index == 0 {
                expected.2 = h.app.batch.detection.clone();
                expected.3 = h.app.batch.auto_end;
            }
            let edit = &h.app.items[index].edit;
            assert_eq!(
                (
                    edit.start.clone(),
                    edit.end.clone(),
                    edit.detection.clone(),
                    edit.auto_end
                ),
                expected
            );
        }
    }
    #[test]
    fn batch_detector_defaults_update_inheritors_and_manual_markers_reject_late_searches() {
        let mut h = Harness::new();
        h.app.batch.auto_end = true;
        h.app.batch.detection.hold_seconds = 4.0;
        h.app.sync_batch_detection();
        assert!(h.app.items.iter().all(|item| item.edit.auto_end));
        let mut edit = h.app.items[0].edit.clone();
        edit.feedback = EndFeedback::Searching;
        h.app.items[0].edit = edit.clone();
        h.app.items[0].find_ticket = 9;
        edit.set_manual_end(20.0);
        h.app.commit_edit(0, edit);
        h.app.batch.detection.hold_seconds = 5.0;
        h.app.sync_batch_detection();
        h.app.tx.send(Message::Found(1, 9, Ok(Some(12.0)))).unwrap();
        h.app.receive(&h.ctx);
        assert_eq!(h.app.items[0].edit.end, time_text(20.0));
        assert!(!h.app.items[0].edit.auto_end);
        assert_eq!(h.app.items[1].edit.detection.hold_seconds, 5.0);
        let mut added = VideoEdit::default();
        added.inherit_detection(&h.app.batch);
        assert!(added.auto_end);
        assert_eq!(added.detection.hold_seconds, 5.0);
    }

    #[test]
    fn detector_controls_apply_batch_defaults_and_queue_search_skips_manual_overrides() {
        let mut h = Harness::new();
        h.app.items[0].edit.end = "20".into();
        h.app.items[1].edit.end = "25".into();
        click_text(&mut h, "Detect end", 0);
        assert!(h.app.batch.auto_end);
        assert!(h.app.items.iter().all(|item| item.edit.auto_end));
        assert!(
            !h.app.searching(),
            "enabling detection never runs analysis implicitly"
        );
        click_text(&mut h, "Per-video settings", 0);
        click_text(&mut h, "Detect end", 0);
        assert!(h.app.items[0].edit.override_detection);
        assert!(!h.app.items[0].edit.override_video);
        assert!(!h.app.items[0].edit.auto_end);
        assert!(h.app.items[1].edit.auto_end);
        click_text(&mut h, "Batch settings", 0);
        let output = h.frame(Vec::new());
        assert!(!text_centers(&output, "1 of 2 videos enabled").is_empty());
        click_text(&mut h, "Find ends in queue", 0);
        assert!(h.app.finding_batch);
        assert_eq!(h.app.items[0].edit.feedback, EndFeedback::NotSearched);
        assert_eq!(h.app.items[1].edit.feedback, EndFeedback::Searching);
        for worker in h.app.threads.drain(..) {
            worker.join().unwrap();
        }
        h.app.receive(&h.ctx);
        assert!(!h.app.finding_batch);
        assert_eq!(h.app.items[0].edit.end, "20");
        assert_eq!(h.app.items[1].edit.end, "25");
        click_text(&mut h, "Per-video settings", 0);
        click_text(&mut h, "Reset to batch settings", 0);
        assert!(h.app.items[0].edit.auto_end);
        assert!(!h.app.items[0].edit.override_detection);
        assert_eq!(h.app.items[0].edit.end, "20");
    }

    #[test]
    fn reference_results_respect_batch_scope_overrides_and_replaced_presets() {
        let mut h = Harness::new();
        h.app.items[1].edit.override_detection = true;
        h.app.batch_reference_ticket = 5;
        let frame = FrameImage {
            rgb: vec![0, 0, 0],
            width: 1,
            height: 1,
            histogram: tailout::detect::Histogram::from_rgb(&[0, 0, 0]),
        };
        h.app
            .tx
            .send(Message::BatchReference(
                5,
                Ok((frame.clone(), "shared.png".into())),
            ))
            .unwrap();
        h.app.receive(&h.ctx);
        assert_eq!(h.app.batch.reference_name, "shared.png");
        assert_eq!(h.app.items[0].edit.reference_name, "shared.png");
        assert!(h.app.items[1].edit.reference_name.is_empty());
        let mut edit = h.app.items[0].edit.clone();
        h.app
            .apply_preset(&h.ctx, &mut edit, Preset::Batch(BatchOptions::default()));
        h.app
            .tx
            .send(Message::BatchReference(5, Ok((frame, "stale.png".into()))))
            .unwrap();
        h.app.receive(&h.ctx);
        assert!(h.app.batch.reference_name.is_empty());
    }

    #[test]
    fn preset_application_follows_the_active_tab_and_invalidates_replaced_video_jobs() {
        let mut h = Harness::new();
        let mut edit = h.app.items[0].edit.clone();
        edit.start = "4".into();
        edit.end = "24".into();
        edit.auto_end = true;
        let mut other = h.app.items[1].edit.clone();
        other.set_video_override(true, &h.app.batch);
        other.video.quality = 28;
        h.app.items[1].edit = other.clone();
        let mut saved = edit.preset(&h.app.batch);
        saved.video.quality = 17;
        let preset = Preset::from_value(Preset::Video(saved).to_value()).unwrap();
        h.app.apply_preset(&h.ctx, &mut edit, preset.clone());
        assert_eq!(h.app.batch.video.quality, 17);
        assert!(!edit.override_video);
        assert_eq!(edit.start, "4");
        assert_eq!(h.app.items[1].edit, other);
        h.app.settings_tab = SettingsTab::Video;
        h.app.items[0].edit = edit.clone();
        h.app.items[0].edit.feedback = EndFeedback::Searching;
        h.app.items[0].find_ticket = 7;
        h.app.items[0].reference_ticket = 3;
        h.app.apply_preset(&h.ctx, &mut edit, preset);
        h.app.commit_edit(0, edit);
        assert!(h.app.items[0].edit.override_video);
        assert!(h.app.items[0].find_cancel.is_cancelled());
        assert_eq!(h.app.items[0].reference_ticket, 4);
        h.app.tx.send(Message::Found(1, 7, Ok(Some(12.0)))).unwrap();
        h.app.receive(&h.ctx);
        assert_eq!(h.app.items[0].edit.end, "24");
        assert_eq!(h.app.items[0].edit.feedback, EndFeedback::NotSearched);
        assert_eq!(h.app.items[1].edit, other);
        assert_eq!(h.app.batch.video.quality, 17);
    }
    #[test]
    fn tabs_presets_and_video_selection_keep_their_scope_without_a_queue() {
        let mut h = Harness::new();
        h.app.settings_tab = SettingsTab::Video;
        h.app.select(2);
        assert!(h.app.settings_tab == SettingsTab::Video);
        h.frame(Vec::new());
        let output = h.frame(Vec::new());
        assert_eq!(text_centers(&output, "Re-encode HEVC").len(), 1);
        assert!(!text_centers(&output, "test-2.mp4").is_empty());
        assert!(text_centers(&output, "Replace originals").is_empty());
        assert_eq!(text_centers(&output, "Save preset").len(), 1);
        h.app.clear_queue();
        h.frame(Vec::new());
        let output = h.frame(Vec::new());
        assert!(!text_centers(&output, "Select a video to edit its settings.").is_empty());
        assert!(text_centers(&output, "Re-encode HEVC").is_empty());
        let before = h.app.batch.clone();
        let mut edit = VideoEdit::default();
        h.app
            .apply_preset(&h.ctx, &mut edit, Preset::Batch(BatchOptions::default()));
        assert_eq!(edit, VideoEdit::default());
        assert_eq!(h.app.batch, before);
        h.app.settings_tab = SettingsTab::Batch;
        let mut saved = BatchOptions {
            output_dir: "exports".into(),
            replace_original: false,
            ..Default::default()
        };
        saved.video.quality = 20;
        h.app
            .apply_preset(&h.ctx, &mut edit, Preset::Batch(saved.clone()));
        assert_eq!(h.app.batch, saved);
        assert_eq!(edit, VideoEdit::default());
    }
    #[test]
    fn batch_processing_can_be_configured_before_adding_videos() {
        let mut h = Harness::new();
        h.app.clear_queue();
        h.frame(Vec::new());
        let output = h.frame(Vec::new());
        let at = text_centers(&output, "Re-encode HEVC")[0];
        h.frame(vec![egui::Event::PointerMoved(at)]);
        h.frame(vec![pointer(at, true)]);
        h.frame(vec![pointer(at, false)]);
        assert!(!h.app.batch.video.force);
        assert!(h.app.items.is_empty());
    }
    #[test]
    fn switching_videos_retains_independent_edits() {
        let mut h = Harness::new();
        let mut edit = h.app.items[0].edit.clone();
        edit.start = "5".into();
        edit.end = "20".into();
        edit.override_video = true;
        edit.video.quality = 18;
        h.app.commit_edit(0, edit);
        h.app.select(2);
        assert_eq!(h.app.items[1].edit, VideoEdit::default());
        let mut edit = h.app.items[1].edit.clone();
        edit.override_video = true;
        edit.video.quality = 27;
        h.app.commit_edit(1, edit);
        h.app.select(1);
        assert_eq!(h.app.items[0].edit.start, "5");
        assert_eq!(h.app.items[0].edit.end, "20");
        assert_eq!(h.app.items[0].edit.video.quality, 18);
        assert_eq!(h.app.items[1].edit.video.quality, 27);
    }
    #[test]
    fn alphabetical_sort_is_default_and_uses_case_insensitive_filenames_with_stable_ties() {
        let mut h = Harness::new();
        assert_eq!(h.app.program.queue_sort, QueueSort::Alphabetical);
        h.app.items.clear();
        for (id, path) in [
            (1, "a-folder/Zebra.mp4"),
            (2, "z-folder/apple.mp4"),
            (3, "z-folder/Banana.mp4"),
            (4, "a-folder/APPLE.mp4"),
            (5, "éclair.mp4"),
            (6, "a-folder/banana.mp4"),
        ] {
            let mut item = Item::new(id, Input::new(path.into()), VideoEdit::default());
            item.error = Some("No duration available".into());
            h.app.items.push(item);
        }
        h.app.items[0].edit.end = "20".into();
        h.app.queue_selection = BTreeSet::from([1, 4]);
        h.app.queue_sort_dirty = true;
        h.app.sort_queue();
        let ids = |app: &App| app.items.iter().map(|item| item.id).collect::<Vec<_>>();
        assert_eq!(ids(&h.app), [2, 4, 3, 6, 1, 5]);
        assert_eq!(h.app.selected, Some(1));
        assert_eq!(h.app.queue_selection, BTreeSet::from([1, 4]));
        assert_eq!(h.app.items[h.app.index(1).unwrap()].edit.end, "20");
        h.app.next_id = 7;
        h.app.add(vec![Input::new("aardvark.mp4".into())]);
        assert_eq!(ids(&h.app), [7, 2, 4, 3, 6, 1, 5]);
        let restored: Session =
            serde_json::from_value(serde_json::to_value(h.app.session()).unwrap()).unwrap();
        assert_eq!(restored.program.queue_sort, QueueSort::Alphabetical);
        let explicit: ProgramOptions =
            serde_json::from_value(serde_json::json!({"queue_sort":"Added"})).unwrap();
        assert_eq!(
            explicit.queue_sort,
            QueueSort::Added,
            "saved choices remain respected"
        );
    }

    #[test]
    fn queue_duration_sort_preserves_selection_edits_and_added_order_as_metadata_arrives() {
        let mut h = Harness::new();
        h.app.items[0].info.as_mut().unwrap().duration = 60.0;
        h.app.items[0].edit.end = "1".into(); // Sort full duration, not retained length.
        h.app.items[0].edit.override_video = true;
        h.app.items[0].edit.video.quality = 18;
        let selected_edit = h.app.items[0].edit.clone();
        let base_info = h.app.items[1].info.clone().unwrap();
        for (id, duration) in [(3, Some(30.0)), (4, Some(10.0)), (5, None), (6, None)] {
            let mut item = Item::new(
                id,
                Input::new(format!("sort-{id}.mp4").into()),
                VideoEdit::default(),
            );
            item.info = duration.map(|duration| MediaInfo {
                duration,
                ..base_info.clone()
            });
            item.probe_pending = id == 5;
            if id == 6 {
                item.error = Some("Cannot read duration".into());
            }
            h.app.items.push(item);
        }
        let ids = |app: &App| app.items.iter().map(|item| item.id).collect::<Vec<_>>();
        h.app.program.queue_sort = QueueSort::Shortest;
        h.app.queue_sort_dirty = true;
        h.app.sort_queue();
        assert_eq!(ids(&h.app), [4, 2, 3, 1, 5, 6]);
        assert_eq!(h.app.selected, Some(1));
        assert_eq!(
            h.app.items[h.app.selected_index().unwrap()].edit,
            selected_edit
        );
        h.app
            .tx
            .send(Message::Probed(
                5,
                0,
                Ok(MediaInfo {
                    duration: 5.0,
                    ..base_info
                }),
            ))
            .unwrap();
        h.app.receive(&h.ctx);
        assert_eq!(ids(&h.app), [5, 4, 2, 3, 1, 6]);
        h.app.program.queue_sort = QueueSort::Longest;
        h.app.queue_sort_dirty = true;
        h.app.sort_queue();
        assert_eq!(ids(&h.app), [1, 2, 3, 4, 5, 6]);
        h.app.program.queue_sort = QueueSort::Added;
        h.app.queue_sort_dirty = true;
        h.app.sort_queue();
        assert_eq!(ids(&h.app), [1, 2, 3, 4, 5, 6]);
        assert_eq!(h.app.items[0].edit, selected_edit);
        let restored: Session =
            serde_json::from_value(serde_json::to_value(h.app.session()).unwrap()).unwrap();
        assert!(restored.videos.is_empty());
        assert_eq!(restored.program.queue_sort, QueueSort::Added);
        let old: ProgramOptions =
            serde_json::from_value(serde_json::json!({"seek_step":0.5})).unwrap();
        assert_eq!(old.queue_sort, QueueSort::Alphabetical);
    }
    #[test]
    fn queue_sort_control_changes_order_and_waits_for_running_work_to_finish() {
        let mut h = Harness::new();
        h.app.items[0].info.as_mut().unwrap().duration = 60.0;
        h.frame(Vec::new());
        let output = h.frame(Vec::new());
        let at = text_centers(&output, "Alphabetical")[0];
        h.frame(vec![egui::Event::PointerMoved(at)]);
        h.frame(vec![pointer(at, true)]);
        h.frame(vec![pointer(at, false)]);
        let output = h.frame(Vec::new());
        let at = text_centers(&output, "Shortest first")[0];
        h.frame(vec![egui::Event::PointerMoved(at)]);
        h.frame(vec![pointer(at, true)]);
        h.frame(vec![pointer(at, false)]);
        assert_eq!(h.app.program.queue_sort, QueueSort::Shortest);
        assert_eq!(h.app.items[0].id, 2);
        assert_eq!(h.app.selected, Some(1));
        assert!(h.app.save_at.is_some());
        for batch_search in [false, true] {
            h.app.busy = !batch_search;
            h.app.finding_batch = batch_search;
            h.app.items[0].info.as_mut().unwrap().duration = 120.0;
            h.app.queue_sort_dirty = true;
            h.app.sort_queue();
            assert_eq!(h.app.items[0].id, 2, "running work keeps its order");
            h.app
                .tx
                .send(if batch_search {
                    Message::AnalysisDone
                } else {
                    Message::Finished(vec![1, 2])
                })
                .unwrap();
            h.app.receive(&h.ctx);
            assert_eq!(h.app.items[0].id, 1);
            assert!(!h.app.queue_sort_dirty);
            // Restore the shorter second item for the next case.
            h.app.items[1].info.as_mut().unwrap().duration = 30.0;
            h.app.queue_sort_dirty = true;
            h.app.sort_queue();
            assert_eq!(h.app.items[0].id, 2);
        }
        let restored: Session =
            serde_json::from_value(serde_json::to_value(h.app.session()).unwrap()).unwrap();
        assert_eq!(restored.program.queue_sort, QueueSort::Shortest);
    }
    fn extend_test_queue(h: &mut Harness) {
        let info = h.app.items[0].info.clone();
        for id in 3..=6 {
            let mut item = Item::new(
                id,
                Input::new(format!("test-{id}.mp4").into()),
                VideoEdit::default(),
            );
            item.info = info.clone();
            h.app.items.push(item);
        }
        h.app.next_id = 7;
    }
    fn modified_queue_click(h: &mut Harness, id: u64, modifiers: egui::Modifiers) {
        let output = h.frame(Vec::new());
        let name = h.app.items[h.app.index(id).unwrap()]
            .input
            .path
            .file_name()
            .unwrap()
            .to_string_lossy();
        let at = text_centers(&output, &name)
            .into_iter()
            .find(|pos| pos.x < 320.0)
            .unwrap();
        h.frame_with_modifiers(vec![egui::Event::PointerMoved(at)], modifiers);
        for pressed in [true, false] {
            h.frame_with_modifiers(
                vec![egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers,
                }],
                modifiers,
            );
        }
    }
    #[test]
    fn queue_multiselect_ctrl_shift_and_plain_click_keep_individual_edits() {
        let mut h = Harness::new();
        extend_test_queue(&mut h);
        let edits: Vec<_> = h.app.items.iter().map(|item| item.edit.clone()).collect();
        let ctrl = egui::Modifiers {
            ctrl: true,
            ..Default::default()
        };
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let ctrl_shift = egui::Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        };
        for (id, modifiers, expected, active) in [
            (3, ctrl, vec![1, 3], Some(3)),
            (5, shift, vec![3, 4, 5], Some(5)),
            (2, shift, vec![2, 3], Some(2)),
            (6, ctrl_shift, vec![2, 3, 4, 5, 6], Some(6)),
            (6, egui::Modifiers::default(), vec![6], Some(6)),
            (6, ctrl, vec![], None),
            (4, shift, vec![4, 5, 6], Some(4)),
            (5, ctrl, vec![4, 6], Some(4)),
            (4, ctrl, vec![6], Some(6)),
            (6, ctrl, vec![], None),
        ] {
            modified_queue_click(&mut h, id, modifiers);
            assert_eq!(
                h.app.queue_selection.iter().copied().collect::<Vec<_>>(),
                expected
            );
            assert_eq!(h.app.selected, active);
        }
        let output = h.frame(Vec::new());
        assert!(!text_centers(&output, "Select a video to preview").is_empty());
        assert_eq!(
            h.app
                .items
                .iter()
                .map(|item| item.edit.clone())
                .collect::<Vec<_>>(),
            edits
        );
        modified_queue_click(&mut h, 3, egui::Modifiers::default());
        assert_eq!(h.app.queue_selection, BTreeSet::from([3]));
        modified_queue_click(&mut h, 5, ctrl);
        open_queue_context(&mut h, 3);
        assert_eq!(h.app.queue_selection, BTreeSet::from([3, 5]));
        assert_eq!(h.app.selected, Some(5));
        close_queue_context(&mut h);
        open_queue_context(&mut h, 4);
        assert_eq!(h.app.queue_selection, BTreeSet::from([4]));
        assert_eq!(h.app.selected, Some(4));
        close_queue_context(&mut h);
    }
    #[test]
    fn sorted_multiselection_bulk_removal_cancels_work_and_never_deletes_files() {
        let directory = tempfile::tempdir().unwrap();
        let mut h = Harness::new();
        extend_test_queue(&mut h);
        let mut files = Vec::new();
        for (item, duration) in h
            .app
            .items
            .iter_mut()
            .zip([60.0, 10.0, 50.0, 20.0, 40.0, 30.0])
        {
            let path = directory.path().join(format!("test-{}.mp4", item.id));
            std::fs::write(&path, b"source").unwrap();
            files.push(path.clone());
            item.input.path = path;
            item.info.as_mut().unwrap().duration = duration;
        }
        modified_queue_click(&mut h, 4, egui::Modifiers::default());
        modified_queue_click(
            &mut h,
            3,
            egui::Modifiers {
                ctrl: true,
                ..Default::default()
            },
        );
        h.app.program.queue_sort = QueueSort::Longest;
        h.app.queue_sort_dirty = true;
        h.app.sort_queue();
        assert_eq!(h.app.queue_selection, BTreeSet::from([3, 4]));
        assert_eq!(h.app.selection_anchor, Some(3));
        assert_eq!(h.app.selected, Some(3));
        modified_queue_click(
            &mut h,
            4,
            egui::Modifiers {
                shift: true,
                ..Default::default()
            },
        );
        assert_eq!(h.app.queue_selection, BTreeSet::from([3, 4, 5, 6]));
        let cancellations: Vec<_> = h
            .app
            .items
            .iter()
            .map(|item| (item.id, item.find_cancel.clone(), item.probe_cancel.clone()))
            .collect();
        for mode in 0..3 {
            h.app.busy = mode == 0;
            h.app.finding_batch = mode == 1;
            h.app.collecting = mode == 2;
            open_queue_context(&mut h, 3);
            click_text(&mut h, "Remove selected (4)", 0);
            assert_eq!(h.app.items.len(), 6);
            close_queue_context(&mut h);
        }
        h.app.busy = false;
        h.app.finding_batch = false;
        h.app.collecting = false;
        open_queue_context(&mut h, 3);
        click_text(&mut h, "Remove selected (4)", 0);
        assert_eq!(
            h.app.items.iter().map(|item| item.id).collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(h.app.queue_selection.contains(&h.app.selected.unwrap()));
        assert_eq!(h.app.queue_selection.len(), 1);
        assert!(
            h.app
                .items
                .iter()
                .any(|item| Some(item.id) == h.app.selection_anchor)
        );
        for (id, find, probe) in cancellations {
            assert_eq!(find.is_cancelled(), id >= 3);
            assert_eq!(probe.is_cancelled(), id >= 3);
        }
        assert!(files.iter().all(|path| path.exists()));
        h.app.clear_queue();
        assert!(h.app.queue_selection.is_empty() && h.app.selection_anchor.is_none());
        assert!(files.iter().all(|path| path.exists()));
    }
    fn open_queue_context(h: &mut Harness, id: u64) {
        let output = h.frame(Vec::new());
        let name = h.app.items[h.app.index(id).unwrap()]
            .input
            .path
            .file_name()
            .unwrap()
            .to_string_lossy();
        let at = text_centers(&output, &name)
            .into_iter()
            .find(|pos| pos.x < 320.0)
            .unwrap();
        right_click_at(h, at);
    }
    fn right_click_at(h: &mut Harness, at: egui::Pos2) {
        h.frame(vec![egui::Event::PointerMoved(at)]);
        for pressed in [true, false] {
            h.frame(vec![egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Secondary,
                pressed,
                modifiers: Default::default(),
            }]);
        }
        h.frame(Vec::new());
    }
    fn close_queue_context(h: &mut Harness) {
        h.frame(vec![key(egui::Key::Escape, true)]);
        h.frame(vec![key(egui::Key::Escape, false)]);
    }
    fn click_text(h: &mut Harness, label: &str, index: usize) {
        let output = h.frame(Vec::new());
        let at = text_centers(&output, label)[index];
        h.frame(vec![egui::Event::PointerMoved(at)]);
        h.frame(vec![pointer(at, true)]);
        h.frame(vec![pointer(at, false)]);
    }
    #[test]
    fn disk_delete_requires_confirmation_and_all_dismissals_keep_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("video with spaces ü.mp4");
        std::fs::write(&path, b"source").unwrap();
        let mut h = Harness::new();
        h.app.items[0].input.path = std::fs::canonicalize(&path).unwrap();
        let target = DeleteTarget {
            id: 1,
            path: h.app.items[0].input.path.clone(),
        };
        h.app.confirm_delete(vec![target]);
        assert!(h.app.delete_state.is_none());
        assert!(path.exists());
        for mode in ["Cancel", "Escape", "Outside"] {
            h.frame(Vec::new());
            open_queue_context(&mut h, 1);
            click_text(&mut h, "Delete from disk…", 0);
            let output = h.frame(Vec::new());
            assert!(matches!(h.app.delete_state, Some(DeleteState::Confirm(_))));
            assert!(!text_centers(&output, &path.display().to_string()).is_empty());
            assert!(!text_centers(&output, "Delete file permanently?").is_empty());
            assert!(
                text_centers(&output, "Delete file")[0].x < text_centers(&output, "Cancel")[0].x
            );
            assert!(path.exists());
            let playhead = h.app.items[0].edit.playhead;
            h.frame(vec![key(egui::Key::ArrowRight, true)]);
            h.frame(vec![key(egui::Key::ArrowRight, false)]);
            assert_eq!(h.app.items[0].edit.playhead, playhead);
            match mode {
                "Cancel" => click_text(&mut h, "Cancel", 0),
                "Escape" => {
                    h.frame(vec![key(egui::Key::Escape, true)]);
                    h.frame(vec![key(egui::Key::Escape, false)]);
                }
                _ => {
                    let at = egui::pos2(20.0, 250.0);
                    h.frame(vec![egui::Event::PointerMoved(at)]);
                    h.frame(vec![pointer(at, true)]);
                    h.frame(vec![pointer(at, false)]);
                }
            }
            assert!(h.app.delete_state.is_none());
            assert_eq!(h.app.items.len(), 2);
            assert_eq!(std::fs::read(&path).unwrap(), b"source");
        }
        open_queue_context(&mut h, 1);
        click_text(&mut h, "Remove from queue", 0);
        assert_eq!(h.app.items.len(), 1);
        assert!(path.exists(), "queue removal never deletes files");
    }
    #[test]
    fn confirmed_delete_removes_only_the_target_and_rejects_its_late_work() {
        for selected in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let first = directory.path().join("first.mp4");
            let second = directory.path().join("second.mp4");
            let exported = directory.path().join("first.hevc.mp4");
            for path in [&first, &second, &exported] {
                std::fs::write(path, b"keep").unwrap();
            }
            let mut h = Harness::new();
            h.app.items[0].input.path = first.clone();
            h.app.items[1].input.path = second.clone();
            h.app.items[1].edit.end = "20".into();
            let index = if selected { 0 } else { 1 };
            let id = h.app.items[index].id;
            let path = h.app.items[index].input.path.clone();
            h.app.items[index].edit.feedback = EndFeedback::Searching;
            h.app.items[index].find_ticket = 7;
            let find_cancel = h.app.items[index].find_cancel.clone();
            let probe_cancel = h.app.items[index].probe_cancel.clone();
            open_queue_context(&mut h, id);
            click_text(&mut h, "Delete from disk…", 0);
            h.frame(Vec::new());
            assert!(path.exists());
            click_text(&mut h, "Delete file", 0);
            for worker in h.app.threads.drain(..) {
                worker.join().unwrap();
            }
            h.app.receive(&h.ctx);
            assert!(h.app.delete_state.is_none());
            assert!(!path.exists());
            assert_eq!(h.app.items.len(), 1);
            assert_eq!(h.app.selected, Some(if selected { 2 } else { 1 }));
            assert!(h.app.items[0].input.path.exists());
            assert!(
                exported.exists(),
                "separate converted outputs are never deleted"
            );
            assert!(find_cancel.is_cancelled() && probe_cancel.is_cancelled());
            h.app
                .tx
                .send(Message::Found(id, 7, Ok(Some(12.0))))
                .unwrap();
            h.app.receive(&h.ctx);
            assert_eq!(h.app.items[0].edit.end, if selected { "20" } else { "" });
        }
    }
    #[test]
    fn failed_delete_keeps_the_queue_item_and_deletion_is_blocked_during_batch_work() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("directory.mp4");
        std::fs::create_dir(&path).unwrap();
        let sentinel = path.join("keep.txt");
        std::fs::write(&sentinel, b"keep").unwrap();
        let mut h = Harness::new();
        h.app.items[0].input.path = path.clone();
        for mode in 0..3 {
            h.app.busy = mode == 0;
            h.app.finding_batch = mode == 1;
            h.app.collecting = mode == 2;
            h.app.request_delete(&BTreeSet::from([1]));
            assert!(h.app.delete_state.is_none());
        }
        h.app.busy = false;
        h.app.finding_batch = false;
        h.app.collecting = false;
        h.app.request_delete(&BTreeSet::from([1]));
        h.app.confirm_delete(vec![DeleteTarget {
            id: 1,
            path: path.clone(),
        }]);
        for worker in h.app.threads.drain(..) {
            worker.join().unwrap();
        }
        h.app.receive(&h.ctx);
        assert!(h.app.delete_state.is_none());
        assert_eq!(h.app.items.len(), 2);
        assert_eq!(h.app.selected, Some(1));
        assert!(
            h.app.items[0]
                .error
                .as_ref()
                .unwrap()
                .contains("Could not delete")
        );
        assert_eq!(h.app.items[0].status, "Delete failed");
        assert!(
            h.app.error.is_none(),
            "selected file error is shown once via its queue item"
        );
        assert_eq!(std::fs::read(sentinel).unwrap(), b"keep");
    }
    #[test]
    fn queue_times_show_the_effective_range_and_retained_length() {
        let mut edit = VideoEdit::default();
        assert_eq!(queue_timing(Some(303.0), &edit), "5:03");
        edit.start = "10".into();
        edit.end = "299".into();
        assert_eq!(
            queue_timing(Some(303.0), &edit),
            "5:03 · Trim 0:10–4:59 (4:49)"
        );
        edit.start = "1:00:00.125".into();
        edit.end.clear();
        assert_eq!(
            queue_timing(Some(3661.25), &edit),
            "1:01:01.25 · Trim 1:00:00.125–1:01:01.25 (1:01.125)"
        );
        edit.start = "5".into();
        edit.end = "9999".into();
        assert_eq!(
            queue_timing(Some(30.0), &edit),
            "0:30 · Trim 0:05–0:30 (0:25)"
        );
        edit.end = "2".into();
        assert_eq!(queue_timing(Some(30.0), &edit), "0:30 · Invalid trim");
        edit.end = "typing".into();
        assert_eq!(queue_timing(Some(30.0), &edit), "0:30 · Invalid trim");
        assert_eq!(queue_timing(None, &edit), "Duration unavailable");
    }
    #[test]
    fn long_queue_names_stay_on_one_line_and_cards_stay_compact() {
        let mut h = Harness::new();
        let name = format!("{}.mp4", "long filename ".repeat(20));
        h.app.items[0].input.path = PathBuf::from(&name);
        h.app.items[0].edit.start = "4".into();
        h.app.items[0].edit.end = "24".into();
        h.frame(Vec::new());
        let output = h.frame(Vec::new());
        let first = text_centers(&output, &name)[0];
        let second = text_centers(&output, "test-2.mp4")[0];
        assert!(
            second.y - first.y <= 48.0,
            "two-line cards must remain compact"
        );
        let time = text_centers(&output, "0:30 · Trim 0:04–0:24 (0:20)")[0];
        assert!(time.y > first.y && time.y - first.y <= 20.0);
        assert_eq!(
            text_centers(&output, "0:30").len(),
            1,
            "other video has its own duration without a trim"
        );
        fn check_name(shape: &egui::Shape, name: &str) {
            match shape {
                egui::Shape::Text(text) if text.galley.job.text == name => {
                    assert_eq!(text.galley.rows.len(), 1);
                    assert!(text.galley.size().x <= 292.0);
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        check_name(shape, name);
                    }
                }
                _ => {}
            }
        }
        for shape in &output.shapes {
            check_name(&shape.shape, &name);
        }
    }
    #[test]
    fn queue_actions_are_only_in_context_menus_and_work_from_the_timing_line() {
        let mut h = Harness::new();
        h.frame(Vec::new());
        let output = h.frame(Vec::new());
        for label in [
            "×",
            "…",
            "Remove selected (1)",
            "Open containing folder",
            "Process this video",
            "Remove from queue",
            "Delete from disk…",
        ] {
            assert!(text_centers(&output, label).is_empty());
        }
        let name = text_centers(&output, "test-1.mp4")[0];
        right_click_at(&mut h, name + egui::vec2(0.0, 20.0));
        let output = h.frame(Vec::new());
        assert!(!text_centers(&output, "Remove from queue").is_empty());
        assert!(!text_centers(&output, "Delete from disk…").is_empty());
        assert!(!text_centers(&output, "Open containing folder").is_empty());
        assert!(!text_centers(&output, "Process this video").is_empty());
        close_queue_context(&mut h);
        right_click_at(&mut h, egui::pos2(300.0, name.y));
        let output = h.frame(Vec::new());
        assert!(
            !text_centers(&output, "Remove from queue").is_empty(),
            "the full card width accepts right-clicks"
        );
    }
    #[cfg(windows)]
    #[test]
    fn folder_shell_paths_preserve_unicode_and_normalize_local_and_unc_prefixes() {
        for (input, expected) in [
            (r"C:\Videos\été, sample", r"C:\Videos\été, sample"),
            (r"\\?\C:\Videos\été, sample", r"C:\Videos\été, sample"),
            (r"\\?\UNC\server\share\影片", r"\\server\share\影片"),
        ] {
            assert_eq!(display_path(Path::new(input)), expected);
            let path = shell_folder_path(Path::new(input));
            assert_eq!(path.last(), Some(&0));
            assert_eq!(
                String::from_utf16(&path[..path.len() - 1]).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn context_menu_processes_only_the_clicked_video_and_preserves_other_results() {
        for target in [1, 2] {
            let directory = tempfile::tempdir().unwrap();
            let mut h = Harness::new();
            h.app.queue_selection = BTreeSet::from([1, 2]);
            let other = if target == 1 { 2 } else { 1 };
            for item in &mut h.app.items {
                item.input.path = directory.path().join(format!("test-{}.mp4", item.id));
                item.status = "Done".into();
            }
            let index = h.app.index(target).unwrap();
            h.app.items[index].edit.start = "5".into();
            h.app.items[index].edit.end = "20".into();
            let other_index = h.app.index(other).unwrap();
            h.app.items[other_index].edit.start = "invalid unrelated marker".into();
            let other_edit = h.app.items[other_index].edit.clone();
            open_queue_context(&mut h, target);
            click_text(&mut h, "Process this video", 0);
            assert!(h.app.busy);
            assert_eq!(h.app.items[index].status, "Queued");
            assert_eq!(h.app.items[other_index].status, "Done");
            assert!(
                h.app.error.is_none(),
                "unrelated invalid settings do not block single-video processing"
            );
            for worker in h.app.threads.drain(..) {
                worker.join().unwrap();
            }
            h.app.receive(&h.ctx);
            // Missing test inputs fail immediately without requiring FFmpeg.
            assert_eq!(h.app.items[index].status, "Failed");
            assert_eq!(h.app.items[other_index].status, "Done");
            assert_eq!(h.app.items[other_index].edit, other_edit);
            assert_eq!(h.app.status, "0 done · 1 failed");
            assert!(!h.app.busy);
            assert_eq!(
                h.app.selected,
                Some(1),
                "a selected group's active preview is preserved"
            );
        }
    }

    #[test]
    fn processing_captures_the_active_sort_at_start_and_keeps_worker_order_fixed() {
        for (order, expected) in [
            (QueueSort::Alphabetical, vec![2, 4, 3, 1]),
            (QueueSort::Added, vec![1, 2, 3, 4]),
            (QueueSort::Shortest, vec![3, 1, 2, 4]),
            (QueueSort::Longest, vec![2, 1, 3, 4]),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let mut h = Harness::new();
            let base_info = h.app.items[0].info.clone().unwrap();
            h.app.items.clear();
            for (id, name, duration) in [
                (1, "zulu.mp4", Some(20.0)),
                (2, "alpha.mp4", Some(40.0)),
                (3, "echo.mp4", Some(10.0)),
                (4, "ALPHA.mp4", None),
            ] {
                let mut item = Item::new(
                    id,
                    Input::new(directory.path().join(name)),
                    VideoEdit::default(),
                );
                item.info = duration.map(|duration| MediaInfo {
                    duration,
                    ..base_info.clone()
                });
                h.app.items.push(item);
            }
            // Deliberately stale list: startup must apply the chosen sort itself.
            h.app.items.reverse();
            h.app.program.queue_sort = order;
            h.app.queue_sort_dirty = false;
            h.app.process_videos(None);
            assert_eq!(
                h.app.items.iter().map(|item| item.id).collect::<Vec<_>>(),
                expected
            );
            let changed = h.app.index(2).unwrap();
            h.app.items[changed].info.as_mut().unwrap().duration = 1.0;
            h.app.queue_sort_dirty = true;
            h.app.sort_queue();
            assert_eq!(
                h.app.items.iter().map(|item| item.id).collect::<Vec<_>>(),
                expected,
                "metadata arriving during processing cannot reorder this run"
            );
            for worker in h.app.threads.drain(..) {
                worker.join().unwrap();
            }
            let messages = h.app.rx.try_iter().collect::<Vec<_>>();
            let processed = messages
                .iter()
                .filter_map(|message| match message {
                    Message::Converted(id, _) => Some(*id),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                processed, expected,
                "worker processing follows the captured sort"
            );
            assert!(
                messages
                    .iter()
                    .any(|message| matches!(message, Message::Finished(ids) if *ids == expected))
            );
            for message in messages {
                h.app.tx.send(message).unwrap();
            }
            h.app.receive(&h.ctx);
            assert!(!h.app.busy);
        }
    }

    #[test]
    fn processing_the_whole_queue_still_resets_and_runs_every_item() {
        let directory = tempfile::tempdir().unwrap();
        let mut h = Harness::new();
        for item in &mut h.app.items {
            item.input.path = directory.path().join(format!("test-{}.mp4", item.id));
            item.status = "Done".into();
        }
        h.app.process_videos(Some(99));
        assert!(!h.app.busy, "an absent target does not start an empty job");
        h.app.process_videos(None);
        assert!(h.app.items.iter().all(|item| item.status == "Queued"));
        for worker in h.app.threads.drain(..) {
            worker.join().unwrap();
        }
        h.app.receive(&h.ctx);
        assert!(h.app.items.iter().all(|item| item.status == "Failed"));
        assert_eq!(h.app.status, "0 done · 2 failed");
    }

    #[test]
    fn error_summaries_pick_the_cause_and_omit_backend_cascades_and_stack_frames() {
        let error = "Backend failed (exit code: 1):\n[hevc_nvenc @ 00000123] Cannot load nvcuda.dll\n[vost#0:0/hevc_nvenc @ 00000456] Error while opening encoder - maybe incorrect parameters\n[vf#0:0 @ 00000789] Error sending frames to consumers: Operation not permitted\nConversion failed!";
        assert_eq!(error_summary(error), "Cannot load nvcuda.dll");
        assert_eq!(
            error_summary(
                "Could not start backend C:\\tools\\ffmpeg.exe: Permission denied (os error 13)"
            ),
            "Permission denied (os error 13)"
        );
        assert_eq!(
            error_summary(
                "Could not convert video:\nCaused by:\n  0: No such file or directory\nStack backtrace:\n  0: tailout::error::convert\n at src/app.rs:123"
            ),
            "No such file or directory"
        );
        assert_eq!(
            error_summary(
                "Output duration 301.163s differs from the requested 302.425s trim range."
            ),
            "Output duration 301.163s differs from the requested 302.425s trim range."
        );
        assert_eq!(
            error_summary(
                "Output saved to C:\\export.hevc.mp4. Original retained: Access is denied (os error 5)"
            ),
            "Output saved; original retained: Access is denied (os error 5)"
        );
        assert_eq!(error_summary(" \n\r"), "Unknown error.");
        let long = "é".repeat(300);
        assert_eq!(error_summary(&long), format!("{}…", "é".repeat(240)));
    }

    #[test]
    fn context_menu_copies_the_clicked_videos_full_error_even_while_processing() {
        for detection_failure in [false, true] {
            let mut h = Harness::new();
            open_queue_context(&mut h, 2);
            let output = h.frame(Vec::new());
            assert!(text_centers(&output, "Copy error message").is_empty());
            close_queue_context(&mut h);
            let message = "Conversion failed for café.mp4\nFFmpeg details: invalid stream [0:1] — source retained.\nFinal diagnostic line.";
            h.app.items[0].error = Some("A different video's error".into());
            if detection_failure {
                h.app.items[1].edit.feedback = EndFeedback::Failed(message.into());
            } else {
                h.app.items[1].status = "Failed".into();
                h.app.items[1].error = Some(message.into());
            }
            h.app.select(1);
            h.app.queue_selection = BTreeSet::from([1, 2]);
            h.app.busy = true;
            open_queue_context(&mut h, 2);
            let output = h.frame(Vec::new());
            let at = text_centers(&output, "Copy error message")[0];
            h.frame(vec![egui::Event::PointerMoved(at)]);
            h.frame(vec![pointer(at, true)]);
            let output = h.frame(vec![pointer(at, false)]);
            assert!(
                output
                    .platform_output
                    .commands
                    .contains(&egui::OutputCommand::CopyText(message.into()))
            );
            assert_eq!(h.app.selected, Some(1));
            assert_eq!(h.app.queue_selection, BTreeSet::from([1, 2]));
            assert_eq!(h.app.items.len(), 2);
            assert!(h.app.busy);
        }
    }

    #[test]
    fn context_menu_removes_single_selected_or_unselected_videos() {
        for id in [1, 2] {
            let mut h = Harness::new();
            h.app.items[0].edit.video.quality = 18;
            h.app.items[1].edit.video.quality = 27;
            open_queue_context(&mut h, id);
            click_text(&mut h, "Remove from queue", 0);
            assert_eq!(h.app.items.len(), 1);
            let remaining = if id == 1 { 2 } else { 1 };
            assert_eq!(h.app.selected, Some(remaining));
            assert_eq!(
                h.app.items[0].edit.video.quality,
                if remaining == 1 { 18 } else { 27 }
            );
            open_queue_context(&mut h, remaining);
            click_text(&mut h, "Remove from queue", 0);
            assert!(h.app.items.is_empty() && h.app.queue_selection.is_empty());
        }
    }
    #[test]
    fn context_menu_bulk_disk_deletion_confirms_all_paths_and_retains_failed_files() {
        for partial_failure in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let first = directory.path().join("first.mp4");
            let second = directory.path().join("second.mp4");
            let export = directory.path().join("first.hevc.mp4");
            std::fs::write(&first, b"source").unwrap();
            std::fs::write(&export, b"keep export").unwrap();
            if partial_failure {
                std::fs::create_dir(&second).unwrap();
            } else {
                std::fs::write(&second, b"source").unwrap();
            }
            let mut h = Harness::new();
            h.app.items[0].input.path = first.clone();
            h.app.items[1].input.path = second.clone();
            modified_queue_click(
                &mut h,
                2,
                egui::Modifiers {
                    ctrl: true,
                    ..Default::default()
                },
            );
            open_queue_context(&mut h, 1);
            assert_eq!(h.app.queue_selection, BTreeSet::from([1, 2]));
            click_text(&mut h, "Delete 2 files from disk…", 0);
            let output = h.frame(Vec::new());
            assert!(!text_centers(&output, "Delete 2 files permanently?").is_empty());
            for path in [&first, &second] {
                assert!(path.exists());
                assert!(!text_centers(&output, &path.display().to_string()).is_empty());
            }
            click_text(&mut h, "Cancel", 0);
            assert!(first.exists() && second.exists());
            open_queue_context(&mut h, 1);
            click_text(&mut h, "Delete 2 files from disk…", 0);
            click_text(&mut h, "Delete 2 files", 0);
            for worker in h.app.threads.drain(..) {
                worker.join().unwrap();
            }
            h.app.receive(&h.ctx);
            assert!(!first.exists());
            assert!(export.exists());
            assert!(h.app.delete_state.is_none());
            if partial_failure {
                assert!(second.exists());
                assert_eq!(h.app.items.len(), 1);
                assert_eq!(h.app.items[0].id, 2);
                assert_eq!(h.app.items[0].status, "Delete failed");
                assert_eq!(h.app.selected, Some(2));
                assert_eq!(h.app.queue_selection, BTreeSet::from([2]));
            } else {
                assert!(!second.exists());
                assert!(h.app.items.is_empty() && h.app.queue_selection.is_empty());
                assert!(h.app.selected.is_none());
            }
        }
    }
    #[test]
    fn arrow_keys_and_scroll_seek_without_shared_playheads() {
        let mut h = Harness::new();
        h.frame(Vec::new());
        h.frame(vec![key(egui::Key::ArrowRight, true)]);
        assert_eq!(h.app.items[0].edit.playhead, 1.0);
        assert_eq!(h.app.items[1].edit.playhead, 0.0);
        let mut edit = h.app.items[0].edit.clone();
        let info = h.app.items[0].info.clone().unwrap();
        let input = egui::RawInput {
            events: vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                source: egui::MouseWheelSource::Wheel,
                phase: egui::TouchPhase::Move,
                delta: egui::vec2(0.0, 2.0),
                modifiers: Default::default(),
            }],
            ..Default::default()
        };
        let mut output = h
            .ctx
            .run_pass(input, |ctx| h.app.scroll_seek(ctx, &mut edit, &info));
        output.textures_delta.clear();
        assert_eq!(edit.playhead, 3.0);
    }
    #[test]
    fn scrubbing_requests_frames_while_pointer_is_still_held() {
        let mut h = Harness::new();
        let rect = h.timeline(Vec::new());
        let first = egui::pos2(
            rect.left() + 8.0 + (rect.width() - 16.0) * 0.25,
            rect.top() + 28.0,
        );
        let second = egui::pos2(
            rect.left() + 8.0 + (rect.width() - 16.0) * 0.75,
            rect.top() + 28.0,
        );
        h.timeline(vec![egui::Event::PointerMoved(first), pointer(first, true)]);
        h.timeline(vec![egui::Event::PointerMoved(second)]);
        let requests: Vec<_> = h.previews.try_iter().collect();
        assert!(requests.len() >= 2);
        assert!((requests[0].time - 7.5).abs() < 0.05);
        assert!((requests.last().unwrap().time - 22.5).abs() < 0.05);
    }
    #[test]
    fn dragging_begin_line_keeps_the_original_drag_target() {
        let mut h = Harness::new();
        h.app.items[0].edit.start = "5".into();
        h.app.items[0].edit.end = "25".into();
        let rect = h.timeline(Vec::new());
        let at = egui::pos2(
            rect.left() + 8.0 + (rect.width() - 16.0) * 5.0 / 30.0,
            rect.top() + 28.0,
        );
        h.timeline(vec![egui::Event::PointerMoved(at), pointer(at, true)]);
        h.timeline(vec![egui::Event::PointerMoved(at + egui::vec2(70.0, 0.0))]);
        assert!(parse_time(&h.app.items[0].edit.start).unwrap() > 6.0);
        assert_eq!(h.app.items[0].edit.end, "25");
    }
    #[test]
    fn detection_results_target_the_right_video_and_stale_jobs_are_ignored() {
        let mut h = Harness::new();
        h.app.items[0].find_ticket = 3;
        h.app.select(2);
        h.app.tx.send(Message::Found(1, 3, Ok(Some(12.0)))).unwrap();
        h.app.receive(&h.ctx);
        assert_eq!(h.app.items[0].edit.end, time_text(12.0));
        assert!(h.app.items[1].edit.end.is_empty());
        let mut edit = h.app.items[0].edit.clone();
        edit.detection.hold_seconds = 20.0;
        h.app.commit_edit(0, edit);
        h.app.tx.send(Message::Found(1, 3, Ok(Some(8.0)))).unwrap();
        h.app.receive(&h.ctx);
        assert_eq!(h.app.items[0].edit.end, time_text(12.0));
    }
    #[test]
    fn manual_end_here_discards_search_even_at_the_same_timestamp_and_ignores_late_results() {
        let mut h = Harness::new();
        h.app.items[0].edit.auto_end = true;
        h.app.items[0].edit.end = time_text(20.0);
        h.app.items[0].edit.playhead = 20.0;
        h.app.items[0].edit.search_limit = Some("25".into());
        h.app.items[0].edit.feedback = EndFeedback::Searching;
        h.app.items[0].find_ticket = 17;
        let cancel = h.app.items[0].find_cancel.clone();
        h.frame(Vec::new());
        let output = h.frame(Vec::new());
        let end_here = text_centers(&output, "Here")
            .into_iter()
            .max_by(|a, b| a.x.total_cmp(&b.x))
            .unwrap();
        h.frame(vec![egui::Event::PointerMoved(end_here)]);
        h.frame(vec![pointer(end_here, true)]);
        h.frame(vec![pointer(end_here, false)]);
        let edit = &h.app.items[0].edit;
        assert!(!edit.auto_end);
        assert_eq!(edit.feedback, EndFeedback::NotSearched);
        assert_eq!(edit.search_limit, None);
        assert_eq!(edit.end, time_text(20.0));
        assert!(cancel.is_cancelled());
        h.app
            .tx
            .send(Message::Found(1, 17, Ok(Some(12.0))))
            .unwrap();
        h.app.receive(&h.ctx);
        assert_eq!(h.app.items[0].edit.end, time_text(20.0));
    }
    #[test]
    fn manual_end_keyboard_and_timeline_override_automatic_markers() {
        let mut h = Harness::new();
        h.app.items[0].edit.auto_end = true;
        h.app.items[0].edit.apply_match(Some(22.0));
        let info = h.app.items[0].info.clone();
        let mut edit = h.app.items[0].edit.clone();
        let mut output = h.ctx.run_pass(
            egui::RawInput {
                events: vec![key(egui::Key::O, true)],
                ..Default::default()
            },
            |ctx| {
                h.app.keyboard_seek(ctx, &mut edit, info.as_ref());
            },
        );
        output.textures_delta.clear();
        h.app.commit_edit(0, edit);
        assert!(!h.app.items[0].edit.auto_end);
        assert_eq!(h.app.items[0].edit.feedback, EndFeedback::NotSearched);
        h.app.items[0].edit.auto_end = true;
        let rect = h.timeline(Vec::new());
        let at = egui::pos2(
            rect.left() + 8.0 + (rect.width() - 16.0) * 22.0 / 30.0,
            rect.top() + 28.0,
        );
        h.timeline(vec![egui::Event::PointerMoved(at), pointer(at, true)]);
        h.timeline(vec![egui::Event::PointerMoved(at - egui::vec2(100.0, 0.0))]);
        assert!(!h.app.items[0].edit.auto_end);
        assert!(parse_time(&h.app.items[0].edit.end).unwrap() < 22.0);
    }
    #[test]
    fn session_save_is_atomic_and_keeps_preferences_without_the_queue() {
        let directory = tempfile::tempdir().unwrap();
        let mut h = Harness::new();
        h.app.state_path = Some(directory.path().join("session.json"));
        h.app.items[0].edit.video.quality = 18;
        h.app.items[1].edit.video.quality = 26;
        h.app.save();
        h.app.program.seek_step = 0.5;
        h.app.batch.video.quality = 19;
        h.app.batch.video.resize.enabled = true;
        h.app.save();
        let session: Session =
            serde_json::from_slice(&std::fs::read(h.app.state_path.as_ref().unwrap()).unwrap())
                .unwrap();
        assert!(session.videos.is_empty());
        assert_eq!(session.selected, None);
        assert_eq!(h.app.items[0].edit.video.quality, 18);
        assert_eq!(h.app.items[1].edit.video.quality, 26);
        assert_eq!(session.program.seek_step, 0.5);
        assert_eq!(session.batch.video.quality, 19);
        assert!(session.batch.video.resize.enabled);
        assert!(h.app.error.is_none());
    }
    #[test]
    fn startup_ignores_a_legacy_queue_but_restores_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("previous.mp4");
        std::fs::write(&path, b"previous queue entry").unwrap();
        let session = Session {
            videos: vec![tailout::editor::SavedVideo {
                path: path.clone(),
                relative_dir: PathBuf::new(),
                edit: VideoEdit::default(),
            }],
            selected: Some(path),
            program: ProgramOptions {
                seek_step: 0.5,
                view: ViewMode::Original,
                ..Default::default()
            },
            batch: BatchOptions {
                output_dir: PathBuf::from("exports"),
                ..Default::default()
            },
        };
        let mut app = App::with_session(session, None, None);
        assert!(app.items.is_empty());
        assert_eq!(app.selected, None);
        assert_eq!(app.program.seek_step, 0.5);
        assert_eq!(app.program.view, ViewMode::Original);
        assert_eq!(app.batch.output_dir, PathBuf::from("exports"));
        app.on_exit(None);
    }
    #[test]
    fn preferences_load_only_from_the_configured_path() {
        let directory = tempfile::tempdir().unwrap();
        let current = directory.path().join("Tailout/session.json");
        assert_eq!(
            load_preferences(Some(&current)).unwrap().program,
            ProgramOptions::default()
        );
        assert_eq!(
            load_preferences(None).unwrap().program,
            ProgramOptions::default()
        );
        let mut preferences = Session::default();
        preferences.program.seek_step = 0.5;
        preferences.batch.output_dir = PathBuf::from("existing exports");
        let mut app = App::with_session(preferences, Some(current.clone()), None);
        app.on_exit(None);
        let restored = load_preferences(Some(&current)).unwrap();
        assert_eq!(restored.program.seek_step, 0.5);
        assert_eq!(restored.batch.output_dir, PathBuf::from("existing exports"));
        assert!(restored.videos.is_empty());
        std::fs::write(&current, b"invalid JSON").unwrap();
        assert!(load_preferences(Some(&current)).is_err());
    }
    #[test]
    fn preview_dropdown_is_discoverable_and_can_select_original() {
        fn preview_rect(shape: &egui::Shape) -> Option<egui::Rect> {
            match shape {
                egui::Shape::Rect(rect) if rect.fill == Color32::from_rgb(11, 13, 16) => {
                    Some(rect.rect)
                }
                egui::Shape::Vec(shapes) => shapes.iter().find_map(preview_rect),
                _ => None,
            }
        }
        let mut h = Harness::new();
        h.frame(Vec::new());
        let output = h.frame(Vec::new());
        assert!(
            !text_centers(&output, "Fit").is_empty(),
            "idle dropdown stays discoverable"
        );
        let rect = output
            .shapes
            .iter()
            .find_map(|shape| preview_rect(&shape.shape))
            .unwrap();
        let pos = egui::pos2(rect.right() - 70.0, rect.top() + 20.0);
        let output = h.frame(vec![egui::Event::PointerMoved(pos)]);
        assert!(!text_centers(&output, "Fit").is_empty());
        h.frame(vec![pointer(pos, true)]);
        h.frame(vec![pointer(pos, false)]);
        let output = h.frame(Vec::new());
        let original = text_centers(&output, "Original")[0];
        assert!(!text_centers(&output, "Stretch").is_empty());
        assert!(text_centers(&output, "Fit height").is_empty());
        assert!(text_centers(&output, "Fit width").is_empty());
        h.frame(vec![egui::Event::PointerMoved(original)]);
        h.frame(vec![pointer(original, true)]);
        h.frame(vec![pointer(original, false)]);
        assert_eq!(h.app.program.view, ViewMode::Original);
        assert_eq!(h.app.pan, egui::Vec2::ZERO);
        assert!(h.app.save_at.is_some());
        let output = h.frame(vec![egui::Event::PointerMoved(rect.center())]);
        assert!(!text_centers(&output, "Original").is_empty());
    }
    #[test]
    fn destination_modes_are_stacked_stable_and_gate_their_settings() {
        fn click(h: &mut Harness, pos: egui::Pos2) {
            h.frame(vec![egui::Event::PointerMoved(pos)]);
            h.frame(vec![pointer(pos, true)]);
            h.frame(vec![pointer(pos, false)]);
        }
        let mut h = Harness::new();
        h.app.batch = BatchOptions::default();
        assert!(h.app.batch.replace_original, "replacement is the default");
        assert!(h.app.batch.delete_original);
        h.frame(Vec::new());
        let output = h.frame(Vec::new());
        assert!(!text_centers(&output, "Batch settings").is_empty());
        assert!(text_centers(&output, "Output").is_empty());
        assert!(text_centers(&output, "Back up originals").is_empty());
        let replace = text_centers(&output, "Replace originals")[0];
        let new_files = text_centers(&output, "Save to folder")[0];
        let browse = text_centers(&output, "Browse")[0];
        let folder = text_centers(&output, "Choose a folder…")[0];
        let force = text_centers(&output, "Force overwrite")[0];
        let delete = text_centers(&output, "Delete originals after successful export")[0];
        assert!(folder.y < delete.y);
        click(&mut h, delete);
        assert!(
            h.app.batch.delete_original,
            "inactive deletion setting cannot be changed"
        );
        assert!(replace.y < force.y && force.y < new_files.y && new_files.y < folder.y);
        assert!(
            browse.x - folder.x < 130.0,
            "Browse stays beside the left-aligned folder label"
        );
        click(&mut h, force);
        assert!(h.app.batch.force_overwrite);
        click(&mut h, new_files);
        assert!(!h.app.batch.replace_original);
        let output = h.frame(Vec::new());
        assert_eq!(text_centers(&output, "Browse")[0], browse);
        assert_eq!(text_centers(&output, "Force overwrite")[0], force);
        assert_eq!(text_centers(&output, "Choose a folder…")[0], folder);
        assert_eq!(
            text_centers(&output, "Delete originals after successful export")[0],
            delete
        );
        click(&mut h, delete);
        assert!(!h.app.batch.delete_original);
        click(&mut h, force);
        assert!(
            h.app.batch.force_overwrite,
            "disabled replacement option cannot be changed"
        );
        h.app.batch.output_dir = PathBuf::from("exports");
        assert!(
            !h.app.items[0]
                .edit
                .settings(&h.app.batch)
                .unwrap()
                .force_overwrite
        );
        click(&mut h, replace);
        assert!(h.app.batch.replace_original);
        assert_eq!(h.app.batch.output_dir, PathBuf::from("exports"));
        assert!(
            !h.app.batch.delete_original,
            "switching modes preserves the user's choice"
        );
        let output = h.frame(Vec::new());
        assert!(!text_centers(&output, "exports").is_empty());
        assert!(h.app.save_at.is_some());
    }
    #[test]
    fn moved_export_retargets_only_its_queue_item_and_cleanup_warnings_keep_the_source() {
        for moved in [false, true] {
            let mut h = Harness::new();
            h.app.batch.replace_original = false;
            let original = h.app.items[0].input.path.clone();
            let other = h.app.items[1].input.path.clone();
            h.app.items[0].edit.start = "5".into();
            h.app.items[0].edit.end = "20".into();
            h.app.items[0].edit.auto_end = true;
            // Assertions cover the immediate UI update, before the asynchronous reprobe.
            h.app.items[0].error = None;
            let warning =
                (!moved).then(|| "Output saved. Original retained: file is in use".to_owned());
            h.app
                .tx
                .send(Message::Converted(
                    1,
                    Ok(Conversion {
                        status: "Done".into(),
                        output: Some("exports/test-1.hevc.mp4".into()),
                        source_removed: moved,
                        warning: warning.clone(),
                        start: 5.0,
                        end: 20.0,
                        encoder: "libx265".into(),
                        saved_bytes: 100,
                        quality: Vec::new(),
                    }),
                ))
                .unwrap();
            h.app.receive(&h.ctx);
            let item = &h.app.items[0];
            assert_eq!(item.status, "Done");
            assert_eq!(item.error, warning);
            assert_eq!(h.app.items[1].input.path, other);
            assert_eq!(h.app.selected, Some(1));
            if moved {
                assert_eq!(item.input.path, PathBuf::from("exports/test-1.hevc.mp4"));
                assert_eq!(item.edit.start, "0");
                assert!(item.edit.end.is_empty());
                assert!(!item.edit.auto_end);
                assert!(item.probe_pending);
            } else {
                assert_eq!(item.input.path, original);
                assert_eq!(item.edit.start, "5");
                assert_eq!(item.edit.end, "20");
            }
        }
    }

    #[test]
    fn sidebars_keep_their_fixed_widths_when_the_dividers_are_dragged() {
        let mut h = Harness::new();
        h.frame(Vec::new());
        h.frame(Vec::new());
        for (id, expected, start, end) in [
            (
                "queue_v2",
                320.0,
                egui::pos2(320.0, 300.0),
                egui::pos2(180.0, 300.0),
            ),
            (
                "options_v2",
                315.0,
                egui::pos2(1045.0, 300.0),
                egui::pos2(850.0, 300.0),
            ),
        ] {
            let width = |ctx: &egui::Context| {
                egui::containers::panel::PanelState::load(
                    ctx,
                    ctx.viewport_id().root_ui_id().with(id),
                )
                .unwrap()
                .outer_rect
                .width()
            };
            assert!(
                (width(&h.ctx) - expected).abs() < 0.1,
                "{id} starts at its fixed width"
            );
            h.frame(vec![egui::Event::PointerMoved(start)]);
            h.frame(vec![pointer(start, true)]);
            h.frame(vec![egui::Event::PointerMoved(end)]);
            h.frame(vec![pointer(end, false)]);
            assert!(
                (width(&h.ctx) - expected).abs() < 0.1,
                "{id} cannot be resized"
            );
        }
    }
    #[test]
    fn seek_step_dragging_across_decimal_boundaries_remains_finite_and_bounded() {
        let mut h = Harness::new();
        h.app.program.seek_step = 1.0;
        h.frame(Vec::new());
        let output = h.frame(Vec::new());
        // DragValue now paints its numeric value and suffix as separate atoms.
        let at = text_centers(&output, "1.0")
            .into_iter()
            .find(|pos| pos.y > 850.0)
            .unwrap();
        h.frame(vec![egui::Event::PointerMoved(at), pointer(at, true)]);
        let mut changed = false;
        for dx in [
            10.0, 7.0, 6.0, 4.0, 3.0, 2.0, 1.0, 0.0, -1.0, -2.0, 20.0, 19.0, 18.0, 17.0, 11.0, 0.0,
            -30.0, 600.0, -600.0, 0.0,
        ] {
            h.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))]);
            let step = h.app.program.seek_step;
            assert!(
                step.is_finite() && (0.01..=60.0).contains(&step),
                "invalid seek step: {step}"
            );
            changed |= step != 1.0;
        }
        h.frame(vec![pointer(at, false)]);
        assert!(changed, "the numeric control was actually dragged");
        assert!(h.app.save_at.is_some());
    }
    #[test]
    fn numeric_drag_rounding_handles_a_decimal_carry_without_panicking() {
        let min = 0.7999999999999999;
        let max = 0.9999999999999999;
        let rounded = egui::emath::smart_aim::best_in_range_f64(min, max);
        assert!(rounded.is_finite() && (min..=max).contains(&rounded));
    }
    #[test]
    fn repeated_layout_keeps_controls_next_to_their_actions() {
        let mut h = Harness::new();
        h.app.items[0].edit.auto_end = true;
        let mut output = None;
        for _ in 0..80 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1360.0, 920.0),
                )),
                ..Default::default()
            };
            let mut frame = eframe::Frame::_new_kittest();
            let mut rendered = h.ctx.run_ui(input, |ui| h.app.ui(ui, &mut frame));
            rendered.textures_delta.clear();
            output = Some(rendered);
        }
        fn visit(shape: &egui::Shape, texts: &mut Vec<(String, egui::Pos2)>) {
            match shape {
                egui::Shape::Text(text) => texts.push((text.galley.job.text.clone(), text.pos)),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        visit(shape, texts);
                    }
                }
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for shape in &output.unwrap().shapes {
            visit(&shape.shape, &mut texts);
        }
        let name = texts
            .iter()
            .find(|(text, _)| text == "test-1.mp4")
            .expect("queue filename");
        assert!(
            name.1.x < 250.0 && name.1.y < 150.0,
            "queue card expanded: {:?}",
            name.1
        );
        let seek = texts
            .iter()
            .find(|(text, _)| text == "Seek step")
            .expect("seek step in bottom bar");
        assert!(seek.1.y > 850.0);
        let subfolders = texts
            .iter()
            .find(|(text, _)| text == "Include subfolders")
            .unwrap();
        let add_folder = texts.iter().find(|(text, _)| text == "Add folder").unwrap();
        assert!(subfolders.1.x > add_folder.1.x && (subfolders.1.y - add_folder.1.y).abs() < 5.0);
        let clear = texts
            .iter()
            .find(|(text, _)| text == "Clear queue")
            .unwrap();
        assert!(clear.1.x < 205.0 && clear.1.y < name.1.y && clear.1.y > 50.0);
        assert!(!texts.iter().any(|(text, _)| text == "PROGRAM"));
        assert!(texts.iter().any(|(text, _)| text == "Detect end"));
        assert!(seek.1.x > 1100.0, "seek controls are right-aligned");
        assert!(
            !texts
                .iter()
                .any(|(text, _)| text == "IN" || text == "OUT" || text == "Auto end")
        );
    }
}
