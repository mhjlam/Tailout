use crate::{
    detect::{self, DetectionSettings, FramePoint, FrameSource, Histogram},
    model::{Encoder, Settings},
    process::{self, Cancel},
};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}
impl Default for Tools {
    fn default() -> Self {
        fn locate(name: &str) -> PathBuf {
            let filename = if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.to_owned()
            };
            if let Ok(exe) = std::env::current_exe() {
                let adjacent = exe.with_file_name(&filename);
                if adjacent.is_file() {
                    return adjacent;
                }
            }
            filename.into()
        }
        Self {
            ffmpeg: locate("ffmpeg"),
            ffprobe: locate("ffprobe"),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Stream {
    pub index: usize,
    #[serde(default)]
    pub codec_type: String,
    pub codec_name: Option<String>,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    pub pix_fmt: Option<String>,
    pub bits_per_raw_sample: Option<String>,
    pub duration: Option<String>,
    pub avg_frame_rate: Option<String>,
    pub sample_aspect_ratio: Option<String>,
    pub color_transfer: Option<String>,
    #[serde(default)]
    pub disposition: BTreeMap<String, i32>,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
    #[serde(default)]
    pub side_data_list: Vec<Value>,
}
impl Stream {
    pub fn frame_seconds(&self) -> f64 {
        self.avg_frame_rate
            .as_deref()
            .and_then(|rate| rate.split_once('/'))
            .and_then(|(a, b)| Some(b.parse::<f64>().ok()? / a.parse::<f64>().ok()?))
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(1.0 / 30.0)
    }
    pub fn ordinary_video(&self) -> bool {
        self.codec_type == "video" && self.disposition.get("attached_pic") != Some(&1)
    }
    pub fn rotated(&self) -> bool {
        self.side_data_list
            .iter()
            .filter_map(|v| v.get("rotation").and_then(Value::as_f64))
            .any(|angle| (angle.abs() % 180.0 - 90.0).abs() < 0.01)
    }
    pub fn depth(&self) -> u32 {
        if let Some(depth) = self
            .bits_per_raw_sample
            .as_ref()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|n| *n > 0)
        {
            return depth;
        }
        let format = self.pix_fmt.as_deref().unwrap_or("");
        for depth in [32, 16, 14, 12, 10, 9] {
            if format.contains(&format!("p{depth}"))
                || format.contains(&format!("p0{depth}"))
                || format.contains(&format!("gray{depth}"))
                || format.contains(&format!("f{depth}"))
            {
                return depth;
            }
        }
        if ["rgb48", "bgr48", "rgba64", "bgra64"]
            .iter()
            .any(|p| format.starts_with(p))
        {
            return 16;
        }
        if format.starts_with("x2") {
            return 10;
        }
        8
    }
    fn supported_sdr(&self) -> Result<()> {
        ensure!(
            !matches!(
                self.color_transfer.as_deref(),
                Some("smpte2084" | "arib-std-b67")
            ) && !["DOVI", "Dolby Vision", "Mastering display", "HDR Dynamic"]
                .iter()
                .any(|tag| self
                    .side_data_list
                    .iter()
                    .any(|v| v.to_string().contains(tag))),
            "HDR/Dolby Vision needs a metadata-preserving workflow. This source was retained."
        );
        ensure!(
            self.tags.get("alpha_mode").map(String::as_str) != Some("1")
                && !["yuva", "gbrap", "rgba", "bgra", "argb", "abgr"]
                    .iter()
                    .any(|prefix| self.pix_fmt.as_deref().unwrap_or("").starts_with(prefix)),
            "HEVC encoding here does not preserve transparency."
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Chapter {
    pub start_time: String,
    pub end_time: String,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
}
#[derive(Default, Deserialize)]
struct Format {
    duration: Option<String>,
}
#[derive(Default, Deserialize)]
struct Probe {
    #[serde(default)]
    streams: Vec<Stream>,
    #[serde(default)]
    chapters: Vec<Chapter>,
    #[serde(default)]
    format: Format,
}

#[derive(Clone, Debug)]
pub struct MediaInfo {
    pub streams: Vec<Stream>,
    pub chapters: Vec<Chapter>,
    pub duration: f64,
}
impl MediaInfo {
    pub fn videos(&self) -> impl Iterator<Item = &Stream> {
        self.streams.iter().filter(|s| s.ordinary_video())
    }
    pub fn primary(&self) -> &Stream {
        self.videos().next().expect("probe enforces a video")
    }
    pub fn aspect(&self) -> f32 {
        let video = self.primary();
        let sar = video
            .sample_aspect_ratio
            .as_deref()
            .and_then(|s| s.split_once(':'))
            .and_then(|(a, b)| Some(a.parse::<f32>().ok()? / b.parse::<f32>().ok()?))
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(1.0);
        let aspect = video.width as f32 * sar / video.height.max(1) as f32;
        if video.rotated() {
            1.0 / aspect
        } else {
            aspect
        }
    }
}

#[derive(Clone, Debug)]
pub struct Input {
    pub path: PathBuf,
    pub relative_dir: PathBuf,
}
impl Input {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            relative_dir: PathBuf::new(),
        }
    }
}
#[derive(Clone, Debug)]
pub struct FrameImage {
    pub rgb: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub histogram: Histogram,
}
#[derive(Clone, Debug)]
pub struct QualityScore {
    pub ssim: f64,
    pub psnr: f64,
}
#[derive(Clone, Debug)]
pub struct ConversionProgress {
    pub phase: &'static str,
    /// None for stages without a meaningful percentage (probing, publishing).
    pub fraction: Option<f64>,
    /// Monotonic completion of this file, including post-encode checks.
    pub overall: f64,
    pub encoder: Option<&'static str>,
    pub stats: Option<process::FfmpegProgress>,
    pub clip: Option<(usize, usize)>,
}
impl ConversionProgress {
    pub fn stage(phase: &'static str, encoder: Option<&'static str>) -> Self {
        Self {
            phase,
            fraction: None,
            overall: 0.0,
            encoder,
            stats: None,
            clip: None,
        }
    }
    fn measured(
        phase: &'static str,
        encoder: &'static str,
        duration: f64,
        stats: process::FfmpegProgress,
    ) -> Self {
        Self {
            phase,
            fraction: stats
                .seconds
                .map(|seconds| (seconds / duration).clamp(0.0, 1.0)),
            overall: 0.0,
            encoder: Some(encoder),
            stats: Some(stats),
            clip: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Conversion {
    /// Individual validated exports for a multi-clip source.
    pub clips: Vec<Conversion>,
    pub status: String,
    pub output: Option<PathBuf>,
    pub source_removed: bool,
    pub warning: Option<String>,
    pub start: f64,
    pub end: f64,
    pub encoder: String,
    pub saved_bytes: i128,
    pub quality: Vec<QualityScore>,
}

fn add(command: &mut Command, values: &[&str]) {
    command.args(values);
}
fn number(value: f64) -> String {
    format!("{value:.6}")
}
fn ffmpeg(tools: &Tools, level: &str) -> Command {
    let mut cmd = Command::new(&tools.ffmpeg);
    add(&mut cmd, &["-hide_banner", "-loglevel", level, "-nostdin"]);
    cmd
}

impl Tools {
    pub fn probe(&self, path: &Path, cancel: &Cancel) -> Result<MediaInfo> {
        let mut cmd = Command::new(&self.ffprobe);
        add(
            &mut cmd,
            &[
                "-v",
                "error",
                "-show_streams",
                "-show_format",
                "-show_chapters",
                "-of",
                "json",
            ],
        );
        cmd.arg(path);
        let output = process::run(&mut cmd, cancel, false, &mut |_| {})?;
        let data: Probe = serde_json::from_slice(&output.stdout).context("Invalid FFprobe JSON")?;
        let video = data
            .streams
            .iter()
            .find(|s| s.ordinary_video())
            .context("No video stream found")?;
        let duration = video
            .duration
            .as_ref()
            .and_then(|s| s.parse::<f64>().ok())
            // Matroska often omits stream.duration but writes a DURATION tag.
            // Container duration may include a much longer audio/subtitle tail.
            .or_else(|| {
                video
                    .tags
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case("duration"))
                    .and_then(|(_, value)| crate::model::parse_time(value).ok())
            })
            .or_else(|| {
                data.format
                    .duration
                    .as_ref()
                    .and_then(|s| s.parse::<f64>().ok())
            })
            .filter(|v| v.is_finite() && *v > 0.0)
            .context("Cannot determine video duration")?;
        Ok(MediaInfo {
            streams: data.streams,
            chapters: data.chapters,
            duration,
        })
    }

    pub fn preview(&self, path: &Path, time: f64, cancel: &Cancel) -> Result<FrameImage> {
        let mut cmd = ffmpeg(self, "error");
        add(&mut cmd, &["-ss", &number(time.max(0.0)), "-i"]);
        cmd.arg(path);
        add(
            &mut cmd,
            &[
                "-map",
                "0:V:0",
                "-frames:v",
                "1",
                "-vf",
                "scale=w='min(iw,1920)':h='min(ih,1080)':force_original_aspect_ratio=decrease,format=rgb24",
                "-f",
                "image2pipe",
                "-c:v",
                "ppm",
                "pipe:1",
            ],
        );
        let data = process::run(&mut cmd, cancel, false, &mut |_| {})?.stdout;
        let (width, height, rgb) = read_ppm(&data)?;
        let histogram = Histogram::from_rgb(&rgb);
        Ok(FrameImage {
            rgb,
            width,
            height,
            histogram,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn detect(
        &self,
        path: &Path,
        media: &MediaInfo,
        start: f64,
        end: f64,
        settings: &DetectionSettings,
        cancel: &Cancel,
        progress: &mut dyn FnMut(f64),
    ) -> Result<Option<f64>> {
        Ok(self
            .detect_profiled(
                path,
                media,
                start,
                end,
                settings,
                detect::SearchMode::Adaptive,
                cancel,
                progress,
            )?
            .end)
    }

    /// Detect an end marker and expose work counters for reproducible benchmarks.
    #[allow(clippy::too_many_arguments)]
    pub fn detect_profiled(
        &self,
        path: &Path,
        media: &MediaInfo,
        start: f64,
        end: f64,
        settings: &DetectionSettings,
        mode: detect::SearchMode,
        cancel: &Cancel,
        progress: &mut dyn FnMut(f64),
    ) -> Result<detect::DetectionResult> {
        media.primary().supported_sdr()?;
        let mut source = VideoFrames {
            tools: self,
            path,
            video_index: media.primary().index,
            settings,
            cancel,
            analyzer: detect::FrameAnalyzer::default(),
            stats: detect::DetectionStats::default(),
        };
        let end = match mode {
            detect::SearchMode::Exact => {
                detect::detect_end(&mut source, start, end, settings, cancel, progress)?
            }
            detect::SearchMode::Adaptive => source.adaptive(start, end, progress)?,
        };
        Ok(detect::DetectionResult {
            end,
            stats: source.stats,
        })
    }

    pub fn select_encoder(
        &self,
        selection: Encoder,
        quality: u8,
        cancel: &Cancel,
    ) -> Result<&'static str> {
        if selection != Encoder::Software {
            let mut cmd = ffmpeg(self, "error");
            add(
                &mut cmd,
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    // Tiny frames can fall below NVENC's supported dimensions
                    // even when the GPU and driver work. Probe with normal HD.
                    "color=size=1920x1080:rate=1",
                    "-frames:v",
                    "1",
                    "-c:v",
                    "hevc_nvenc",
                    "-preset",
                    "p5",
                    "-rc",
                    "vbr",
                    "-cq",
                    &quality.to_string(),
                    "-b:v",
                    "0",
                    "-f",
                    "null",
                    "-",
                ],
            );
            match process::run(&mut cmd, cancel, false, &mut |_| {}) {
                Ok(_) => return Ok("hevc_nvenc"),
                Err(error) if selection == Encoder::Nvenc => {
                    return Err(error.context("NVIDIA encoder is unavailable"));
                }
                Err(_) => cancel.check()?,
            }
        }
        let mut cmd = ffmpeg(self, "error");
        add(
            &mut cmd,
            &[
                "-f",
                "lavfi",
                "-i",
                "color=size=64x64:rate=1",
                "-frames:v",
                "1",
                "-c:v",
                "libx265",
                "-crf",
                &quality.to_string(),
                "-x265-params",
                "log-level=error",
                "-f",
                "null",
                "-",
            ],
        );
        process::run(&mut cmd, cancel, false, &mut |_| {})?;
        Ok("libx265")
    }

    pub fn convert(
        &self,
        input: &Input,
        settings: &Settings,
        cancel: &Cancel,
        progress: &mut dyn FnMut(&str, f64),
    ) -> Result<Conversion> {
        self.convert_with_progress(input, settings, cancel, &mut |event| {
            progress(event.phase, event.fraction.unwrap_or(0.0));
        })
    }

    /// Export kept intervals sequentially while retaining the source until
    /// every interval has been encoded, validated and published successfully.
    pub fn convert_clips(
        &self,
        input: &Input,
        settings: &[Settings],
        cancel: &Cancel,
        on_progress: &mut dyn FnMut(ConversionProgress),
    ) -> Result<Conversion> {
        ensure!(!settings.is_empty(), "No clips to export.");
        if settings.len() == 1 {
            return self.convert_with_progress(input, &settings[0], cancel, on_progress);
        }
        for value in settings {
            value.validate()?;
        }
        on_progress(ConversionProgress::stage("Reading", None));
        let source = fs::canonicalize(&input.path)?;
        let source_lock = open_source(&source)?;
        let original = source_lock.metadata()?;
        let info = self.probe(&source, cancel)?;
        let mut previous = 0.0;
        let mut durations = Vec::new();
        for value in settings {
            let (begin, end) = value.range(info.duration, None)?;
            ensure!(
                begin >= previous - 0.00001,
                "Clips overlap or are out of order."
            );
            previous = end;
            durations.push(end - begin);
        }
        let total: f64 = durations.iter().sum();
        let mut elapsed = 0.0;
        let mut clips = Vec::new();
        for (index, value) in settings.iter().enumerate() {
            let current = fs::metadata(&source)?;
            ensure!(
                current.len() == original.len() && current.modified()? == original.modified()?,
                "Source changed during split export. Original retained."
            );
            let mut options = value.clone();
            let mut clip_input = input.clone();
            if options.replace_original {
                options.output_dir = source
                    .parent()
                    .context("Missing source folder")?
                    .to_path_buf();
                clip_input.relative_dir.clear();
            }
            options.replace_original = false;
            options.delete_original = false;
            options.force_overwrite = false;
            options.clip_number = Some(index + 1);
            let result =
                self.convert_with_progress(&clip_input, &options, cancel, &mut |mut event| {
                    event.overall = 0.99 * (elapsed + event.overall * durations[index]) / total;
                    event.clip = Some((index + 1, settings.len()));
                    if event.phase == "Done" {
                        event.phase = "Saving";
                    }
                    on_progress(event);
                });
            match result {
                Ok(result) => {
                    ensure!(
                        result.output.is_some(),
                        "Clip {} was not exported. Original retained.",
                        index + 1
                    );
                    clips.push(result);
                }
                Err(_error) if cancel.is_cancelled() => bail!(
                    "Cancelled. {} clip files saved; original retained.",
                    clips.len()
                ),
                Err(error) => {
                    return Err(error.context(format!(
                        "Clip {} of {} failed; {} clip files saved. Original retained",
                        index + 1,
                        settings.len(),
                        clips.len()
                    )));
                }
            }
            elapsed += durations[index];
        }
        cancel.check()?;
        let bytes: u64 = clips
            .iter()
            .map(|clip| fs::metadata(clip.output.as_ref().unwrap()).map(|m| m.len()))
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .sum();
        let mut result = clips[0].clone();
        result.saved_bytes = original.len() as i128 - bytes as i128;
        result.end = clips.last().unwrap().end;
        result.quality = clips.iter().flat_map(|clip| clip.quality.clone()).collect();
        result.clips = clips;
        if settings[0].replace_original || settings[0].delete_original {
            drop(source_lock);
            match remove_exported_source(&source, &original, cancel) {
                Ok(()) => result.source_removed = true,
                Err(error) => {
                    result.warning = Some(format!(
                        "All clip files saved. Original retained: {error:#}"
                    ))
                }
            }
        }
        let mut finished = ConversionProgress::stage("Done", None);
        finished.overall = 1.0;
        finished.fraction = Some(1.0);
        on_progress(finished);
        Ok(result)
    }

    pub fn convert_with_progress(
        &self,
        input: &Input,
        settings: &Settings,
        cancel: &Cancel,
        on_progress: &mut dyn FnMut(ConversionProgress),
    ) -> Result<Conversion> {
        settings.validate()?;
        let comparing = settings.compare || settings.min_ssim.is_some();
        let encode_start = if settings.auto_trim.is_some() {
            0.05
        } else {
            0.0
        };
        let encode_end = if comparing { 0.85 } else { 0.90 };
        let validate_end = if comparing { 0.92 } else { 0.99 };
        let mut overall = 0.0_f64;
        let mut progress = |mut event: ConversionProgress| {
            let fraction = event.fraction.unwrap_or(0.0);
            let value = match event.phase {
                "Reading" => 0.0,
                "Finding end" => fraction * encode_start,
                "Checking encoder" => encode_start,
                "Encoding" => encode_start + fraction * (encode_end - encode_start),
                "Reading output" => encode_end,
                "Validating" => encode_end + fraction * (validate_end - encode_end),
                "Comparing" => validate_end + fraction * (0.99 - validate_end),
                "Saving" => 0.99,
                "Done" => 1.0,
                _ => overall,
            };
            overall = overall.max(value).clamp(0.0, 1.0);
            event.overall = overall;
            on_progress(event);
        };
        progress(ConversionProgress::stage("Reading", None));
        let source_path = fs::canonicalize(&input.path)?;
        let source_lock = open_source(&source_path)?;
        let original_metadata = source_lock.metadata()?;
        let media = self.probe(&source_path, cancel)?;
        let (_, limit) = settings.range(media.duration, None)?;
        let detected = if let Some(detection) = &settings.auto_trim {
            progress(ConversionProgress::stage("Finding end", None));
            self.detect(
                &source_path,
                &media,
                settings.start,
                limit,
                detection,
                cancel,
                &mut |v| {
                    progress(ConversionProgress {
                        fraction: Some(v),
                        ..ConversionProgress::stage("Finding end", None)
                    })
                },
            )?
        } else {
            None
        };
        let (start, end) = settings.range(media.duration, detected)?;
        let trimmed = start > 0.00001 || end < media.duration - 0.00001;
        let mut result = Conversion {
            clips: Vec::new(),
            status: "Already HEVC".into(),
            output: None,
            source_removed: false,
            warning: None,
            start,
            end,
            encoder: String::new(),
            saved_bytes: 0,
            quality: Vec::new(),
        };
        if !settings.force
            && !settings.resize
            && !trimmed
            && media
                .videos()
                .all(|s| s.codec_name.as_deref() == Some("hevc"))
        {
            progress(ConversionProgress {
                fraction: Some(1.0),
                ..ConversionProgress::stage("Done", None)
            });
            return Ok(result);
        }
        for stream in media.videos() {
            stream.supported_sdr()?;
        }
        progress(ConversionProgress::stage("Checking encoder", None));
        let encoder = self.select_encoder(settings.encoder, settings.quality, cancel)?;
        result.encoder = encoder.into();
        let directory = if settings.replace_original {
            source_path
                .parent()
                .context("Missing source directory")?
                .to_path_buf()
        } else {
            settings.output_dir.join(&input.relative_dir)
        };
        fs::create_dir_all(&directory)?;
        let extension = source_path
            .extension()
            .context("Missing extension")?
            .to_string_lossy()
            .to_lowercase();
        ensure!(
            ["mp4", "mkv", "mov"].contains(&extension.as_str()),
            "Supported video extensions: mp4, mkv, mov."
        );
        let staged = tempfile::Builder::new()
            .prefix(".tailout-")
            .suffix(&format!(".{extension}"))
            .tempfile_in(&directory)?
            .into_temp_path();
        let mut cmd = ffmpeg(self, "warning");
        add(
            &mut cmd,
            &["-y", "-ss", &number(start), "-noautorotate", "-i"],
        );
        cmd.arg(&source_path);
        // Chapter metadata is explicitly clipped and rebased when trimming.
        let mut chapter_file = None;
        if trimmed {
            let mut file = tempfile::NamedTempFile::new()?;
            write_chapters(&mut file, &media.chapters, start, end)?;
            add(&mut cmd, &["-f", "ffmetadata", "-i"]);
            cmd.arg(file.path());
            chapter_file = Some(file);
        }
        if trimmed {
            add(&mut cmd, &["-t", &number(end - start)]);
        }
        add(
            &mut cmd,
            &[
                "-map",
                "0",
                "-map_metadata",
                "0",
                "-map_chapters",
                if trimmed { "1" } else { "0" },
                "-c",
                "copy",
            ],
        );
        if trimmed {
            add(&mut cmd, &["-c:a", "aac", "-b:a", "192k"]);
            if media.streams.iter().any(|s| s.codec_type == "subtitle") {
                // A copied cue can span both trim boundaries. Clamp its packet
                // timestamp AND duration so it cannot extend the new container.
                cmd.arg("-bsf:s").arg(format!(
                    "setts=pts=max(PTS\\,0):dts=max(DTS\\,0):duration=max(0\\,min(PTS+DURATION\\,{}/TB)-max(PTS\\,0))",
                    number(end-start)
                ));
            }
        }
        let mut video_ordinal = 0;
        for stream in &media.streams {
            let flags: Vec<_> = stream
                .disposition
                .iter()
                .filter(|(_, v)| **v == 1)
                .map(|(key, _)| key.as_str())
                .collect();
            cmd.arg(format!("-disposition:{}", stream.index))
                .arg(if flags.is_empty() {
                    "0".into()
                } else {
                    flags.join("+")
                });
            if stream.codec_type != "video" {
                continue;
            }
            if stream.ordinary_video() {
                let specifier = format!("v:{video_ordinal}");
                cmd.arg(format!("-c:{specifier}")).arg(encoder);
                cmd.arg(format!("-fps_mode:{specifier}")).arg("passthrough");
                if encoder == "libx265" {
                    cmd.arg(format!("-crf:{specifier}"))
                        .arg(settings.quality.to_string());
                    cmd.arg(format!("-preset:{specifier}")).arg("medium");
                    cmd.arg(format!("-x265-params:{specifier}"))
                        .arg("log-level=error");
                } else {
                    for (option, value) in [
                        ("preset", "p5".to_owned()),
                        ("rc", "vbr".to_owned()),
                        ("cq", settings.quality.to_string()),
                        ("b", "0".to_owned()),
                    ] {
                        cmd.arg(format!("-{option}:{specifier}")).arg(value);
                    }
                }
                if settings.resize {
                    let (width, height) = resize_dimensions(settings, stream);
                    let filter = format!(
                        "scale={width}:{height}{}",
                        if settings.keep_aspect {
                            ""
                        } else {
                            ",setsar=1"
                        }
                    );
                    cmd.arg(format!("-filter:{specifier}")).arg(filter);
                }
                if extension != "mkv" {
                    cmd.arg(format!("-tag:{specifier}")).arg("hvc1");
                }
            }
            video_ordinal += 1;
        }
        if extension != "mkv" {
            add(&mut cmd, &["-movflags", "+faststart"]);
        }
        add(
            &mut cmd,
            &["-progress", "pipe:1", "-stats_period", "0.5", "-nostats"],
        );
        cmd.arg(&staged);
        progress(ConversionProgress::stage("Encoding", Some(encoder)));
        process::run_with_progress(&mut cmd, cancel, true, &mut |stats| {
            progress(ConversionProgress::measured(
                "Encoding",
                encoder,
                end - start,
                stats,
            ));
        })?;
        drop(chapter_file);
        progress(ConversionProgress::stage("Reading output", Some(encoder)));
        let output = self.probe(&staged, cancel)?;
        validate_structure(&media, &output, settings, trimmed, start, end)?;
        progress(ConversionProgress::stage("Validating", Some(encoder)));
        let mut cmd = ffmpeg(self, "error");
        add(
            &mut cmd,
            &["-xerror", "-err_detect", "explode", "-noautorotate", "-i"],
        );
        cmd.arg(&staged);
        add(
            &mut cmd,
            &[
                "-map",
                "0:V",
                "-map",
                "0:a?",
                "-progress",
                "pipe:1",
                "-nostats",
                "-f",
                "null",
                "-",
            ],
        );
        process::run_with_progress(&mut cmd, cancel, true, &mut |stats| {
            progress(ConversionProgress::measured(
                "Validating",
                encoder,
                end - start,
                stats,
            ));
        })?;
        if settings.compare || settings.min_ssim.is_some() {
            progress(ConversionProgress::stage("Comparing", Some(encoder)));
            result.quality = self.compare(
                &source_path,
                &staged,
                &media,
                &output,
                start,
                end - start,
                cancel,
                &mut |fraction, stats| {
                    progress(ConversionProgress {
                        phase: "Comparing",
                        clip: None,
                        overall: 0.0,
                        fraction,
                        stats: Some(stats),
                        encoder: Some(encoder),
                    })
                },
            )?;
            if let Some(minimum) = settings.min_ssim {
                ensure!(
                    result.quality.iter().all(|score| score.ssim >= minimum),
                    "SSIM is below the required {minimum:.4}. Source retained."
                );
            }
        }
        let output_metadata = fs::metadata(&staged)?;
        result.saved_bytes = original_metadata.len() as i128 - output_metadata.len() as i128;
        if settings.replace_original && !settings.force_overwrite && result.saved_bytes <= 0 {
            result.status = "Not smaller — source retained".into();
            progress(ConversionProgress {
                fraction: Some(1.0),
                ..ConversionProgress::stage("Done", Some(encoder))
            });
            return Ok(result);
        }
        cancel.check()?;
        progress(ConversionProgress::stage("Saving", Some(encoder)));
        let file = OpenOptions::new().write(true).open(&staged)?;
        file.set_modified(original_metadata.modified()?)?;
        file.sync_all()?;
        drop(file);
        if settings.replace_original {
            cancel.check()?;
            drop(source_lock);
            atomic_replace(&staged, &source_path)?;
            result.output = Some(source_path);
        } else {
            let stem = source_path
                .file_stem()
                .context("Missing filename")?
                .to_string_lossy();
            let name = OsString::from(match settings.clip_number {
                Some(number) => format!("{stem}-{number}.hevc.{extension}"),
                None => format!("{stem}.hevc.{extension}"),
            });
            cancel.check()?;
            result.output = Some(publish_unique(staged, &directory, &name)?);
            if settings.delete_original {
                drop(source_lock);
                // Once the validated output has been published, a cleanup failure
                // must not hide that output or attempt to undo the successful save.
                match remove_exported_source(&source_path, &original_metadata, cancel) {
                    Ok(()) => result.source_removed = true,
                    Err(error) => {
                        result.warning = Some(format!(
                            "Output saved to {}. Original retained: {error:#}",
                            result.output.as_ref().unwrap().display()
                        ))
                    }
                }
            }
        }
        result.status = "Done".into();
        progress(ConversionProgress {
            fraction: Some(1.0),
            ..ConversionProgress::stage("Done", Some(encoder))
        });
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    fn compare(
        &self,
        source: &Path,
        output: &Path,
        original: &MediaInfo,
        encoded: &MediaInfo,
        start: f64,
        duration: f64,
        cancel: &Cancel,
        progress: &mut dyn FnMut(Option<f64>, process::FfmpegProgress),
    ) -> Result<Vec<QualityScore>> {
        let mut scores = Vec::new();
        let streams = original.videos().count();
        for (index, (before, after)) in original.videos().zip(encoded.videos()).enumerate() {
            let pixel = if before.depth() > 8 {
                "yuv444p16le"
            } else {
                "yuv444p"
            };
            let normalize = format!(
                "scale={}:{}:flags=bicubic,format={pixel},settb=AVTB,setpts=PTS-STARTPTS",
                after.width, after.height
            );
            let graph = format!(
                "[0:{}]{normalize},split=2[a][b];[1:{}]{normalize},split=2[c][d];[a][c]ssim=shortest=1:repeatlast=0[s];[b][d]psnr=shortest=1:repeatlast=0[p]",
                before.index, after.index
            );
            let mut cmd = ffmpeg(self, "info");
            add(&mut cmd, &["-ss", &number(start), "-noautorotate", "-i"]);
            cmd.arg(source);
            add(&mut cmd, &["-noautorotate", "-i"]);
            cmd.arg(output);
            add(
                &mut cmd,
                &[
                    "-t",
                    &number(duration),
                    "-filter_complex",
                    &graph,
                    "-map",
                    "[s]",
                    "-map",
                    "[p]",
                    "-an",
                    "-f",
                    "null",
                    "-",
                ],
            );
            add(
                &mut cmd,
                &["-progress", "pipe:1", "-stats_period", "0.5", "-nostats"],
            );
            let log = process::run_with_progress(&mut cmd, cancel, true, &mut |stats| {
                let fraction = stats.seconds.map(|seconds| {
                    (index as f64 + (seconds / duration).clamp(0.0, 1.0)) / streams as f64
                });
                progress(fraction, stats);
            })?
            .stderr;
            let ssim = metric(&log, "All:").context("SSIM missing")?;
            let psnr = metric(&log, "average:").context("PSNR missing")?;
            ensure!(ssim.is_finite(), "Invalid SSIM");
            scores.push(QualityScore { ssim, psnr });
        }
        Ok(scores)
    }
}

fn metric(log: &str, marker: &str) -> Option<f64> {
    log.rsplit_once(marker)?
        .1
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn validate_structure(
    original: &MediaInfo,
    output: &MediaInfo,
    settings: &Settings,
    trimmed: bool,
    start: f64,
    end: f64,
) -> Result<()> {
    ensure!(
        original.streams.len() == output.streams.len(),
        "Output lost or added media streams."
    );
    let expected_chapters = if trimmed {
        original
            .chapters
            .iter()
            .filter(|chapter| {
                chapter.end_time.parse::<f64>().unwrap_or(0.0) > start
                    && chapter.start_time.parse::<f64>().unwrap_or(f64::INFINITY) < end
            })
            .count()
    } else {
        original.chapters.len()
    };
    ensure!(
        output.chapters.len() == expected_chapters,
        "Output chapter count changed unexpectedly."
    );
    for (before, after) in original.streams.iter().zip(&output.streams) {
        ensure!(
            before.codec_type == after.codec_type,
            "Stream type changed."
        );
        for (flag, value) in &before.disposition {
            ensure!(
                after.disposition.get(flag).unwrap_or(&0) == value,
                "Stream disposition changed: {flag}"
            );
        }
        if before.ordinary_video() {
            ensure!(
                after.codec_name.as_deref() == Some("hevc"),
                "Output is not HEVC."
            );
            ensure!(
                after.depth() >= before.depth(),
                "Output lost video bit depth."
            );
            if settings.resize {
                let (width, height) = resize_dimensions(settings, before);
                ensure!(
                    after.width == width
                        && after.height == height
                        && after.width <= before.width
                        && after.height <= before.height,
                    "Output resize dimensions are incorrect."
                );
            } else {
                ensure!(
                    after.width == before.width && after.height == before.height,
                    "Output dimensions changed."
                );
            }
            ensure!(
                before.rotated() == after.rotated(),
                "Rotation metadata was lost."
            );
        } else if before.codec_type == "audio" && trimmed {
            ensure!(
                after.codec_name.as_deref() == Some("aac"),
                "Trimmed audio codec is unexpected."
            );
        } else {
            ensure!(
                before.codec_name == after.codec_name,
                "Copied stream changed codec."
            );
        }
    }
    ensure!(
        (output.duration - (end - start)).abs() <= (end - start).mul_add(0.001, 0.5),
        "Output duration {:.3}s differs from the requested {:.3}s trim range.",
        output.duration,
        end - start
    );
    Ok(())
}

fn resize_dimensions(settings: &Settings, stream: &Stream) -> (u32, u32) {
    let resize = settings.resize_options();
    if stream.rotated() {
        let (width, height) = resize.dimensions(stream.height, stream.width);
        (height, width)
    } else {
        resize.dimensions(stream.width, stream.height)
    }
}

fn read_ppm(data: &[u8]) -> Result<(usize, usize, Vec<u8>)> {
    let mut at = 0;
    let mut tokens = Vec::new();
    while tokens.len() < 4 {
        while at < data.len() && data[at].is_ascii_whitespace() {
            at += 1;
        }
        if data.get(at) == Some(&b'#') {
            while at < data.len() && data[at] != b'\n' {
                at += 1;
            }
            continue;
        }
        let from = at;
        while at < data.len() && !data[at].is_ascii_whitespace() {
            at += 1;
        }
        ensure!(at > from, "No frame at this timestamp. Seek earlier.");
        tokens.push(std::str::from_utf8(&data[from..at])?);
    }
    ensure!(
        tokens[0] == "P6" && tokens[3] == "255",
        "Unsupported preview image."
    );
    let width: usize = tokens[1].parse()?;
    let height: usize = tokens[2].parse()?;
    if data.get(at) == Some(&b'\r') && data.get(at + 1) == Some(&b'\n') {
        at += 2;
    } else {
        at += 1;
    }
    let size = width
        .checked_mul(height)
        .and_then(|v| v.checked_mul(3))
        .context("Invalid preview dimensions")?;
    ensure!(
        width > 0 && height > 0 && data.len().checked_sub(at) == Some(size),
        "Invalid preview pixel data."
    );
    Ok((width, height, data[at..].to_vec()))
}

fn escape_metadata(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('=', "\\=")
        .replace(';', "\\;")
        .replace('#', "\\#")
        .replace('\n', "\\\n")
        .replace('\r', "")
}
fn write_chapters(
    output: &mut impl Write,
    chapters: &[Chapter],
    start: f64,
    end: f64,
) -> Result<()> {
    writeln!(output, ";FFMETADATA1")?;
    for chapter in chapters {
        let left = chapter.start_time.parse::<f64>()?.max(start);
        let right = chapter.end_time.parse::<f64>()?.min(end);
        if right <= left {
            continue;
        }
        writeln!(
            output,
            "[CHAPTER]\nTIMEBASE=1/1000000\nSTART={}\nEND={}",
            ((left - start) * 1e6).round() as u64,
            ((right - start) * 1e6).round() as u64
        )?;
        for (key, value) in &chapter.tags {
            writeln!(
                output,
                "{}={}",
                escape_metadata(key),
                escape_metadata(value)
            )?;
        }
    }
    output.flush()?;
    Ok(())
}

fn remove_exported_source(path: &Path, original: &fs::Metadata, cancel: &Cancel) -> Result<()> {
    let started = std::time::Instant::now();
    loop {
        cancel.check()?;
        let current = fs::metadata(path)?;
        ensure!(
            current.is_file()
                && current.len() == original.len()
                && current.modified()? == original.modified()?,
            "Source changed during conversion"
        );
        match fs::remove_file(path) {
            Ok(()) => return Ok(()),
            // Preview/probe readers can briefly retain Windows file handles.
            Err(error)
                if cfg!(windows)
                    && matches!(error.raw_os_error(), Some(32 | 33))
                    && started.elapsed() < std::time::Duration::from_secs(2) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn open_source(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    Ok(options.open(path)?)
}

fn publish_unique(
    mut staged: tempfile::TempPath,
    directory: &Path,
    name: &std::ffi::OsStr,
) -> Result<PathBuf> {
    let name = Path::new(name);
    for i in 0..10000 {
        let path = if i == 0 {
            directory.join(name)
        } else {
            let stem = name.file_stem().unwrap_or_default().to_string_lossy();
            let extension = name.extension().unwrap_or_default().to_string_lossy();
            directory.join(format!("{stem} ({i}).{extension}"))
        };
        match staged.persist_noclobber(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                staged = error.path;
            }
            Err(error) => return Err(error.error.into()),
        }
    }
    bail!("Too many files with the same output name.")
}

#[cfg(windows)]
fn atomic_replace(staged: &Path, source: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let staged: Vec<u16> = staged.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: terminated UTF-16 path buffers live for the call. There is no
    // delete/move fallback if the filesystem cannot replace atomically.
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::ReplaceFileW(
            source.as_ptr(),
            staged.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
#[cfg(not(windows))]
fn atomic_replace(staged: &Path, source: &Path) -> Result<()> {
    Ok(fs::rename(staged, source)?)
}

struct VideoFrames<'a> {
    tools: &'a Tools,
    path: &'a Path,
    video_index: usize,
    settings: &'a DetectionSettings,
    cancel: &'a Cancel,
    analyzer: detect::FrameAnalyzer,
    stats: detect::DetectionStats,
}
impl VideoFrames<'_> {
    fn adaptive(
        &mut self,
        start: f64,
        end: f64,
        progress: &mut dyn FnMut(f64),
    ) -> Result<Option<f64>> {
        let settings = self.settings;
        let cancel = self.cancel;
        settings.validate()?;
        ensure!(end > start, "Empty analysis range.");
        // Very short down periods leave little room for sparse sampling.
        if settings.hold_seconds < 0.5 {
            return detect::detect_end(self, start, end, settings, cancel, progress);
        }
        cancel.check()?;
        progress(0.0);
        let cutoff = start + settings.grace_seconds;
        if cutoff + settings.hold_seconds > end + 0.000001 {
            cancel.check()?;
            progress(1.0);
            return Ok(None);
        }
        let mut schedule = detect::ScoutSchedule::new(settings);
        let lookbehind = schedule.lookbehind();
        let mut cursor = (cutoff - 0.5).max(start);
        let mut dense_analyzer = detect::FrameAnalyzer::default();
        let mut tracker = detect::RunTracker::default();
        let mut dense_until = f64::NEG_INFINITY;
        let mut dense_cutoff = cutoff;
        while cursor < end - 0.000001 {
            cancel.check()?;
            let to = (cursor + 16.0).min(end);
            let frames = self.read_sampled(cursor, to, Some(&mut schedule))?;
            let windows =
                detect::candidate_windows(&frames, cutoff, end, settings.hold_seconds, lookbehind);
            self.stats.merged_candidates += windows.len() as u64;
            for (from, until) in windows {
                cancel.check()?;
                if until <= dense_until + 0.000001 {
                    continue;
                }
                // Overlapping candidates extend the same confirmation stream.
                // Its analyzer and run onsets cache everything needed from the
                // previous read, so no dense section is decoded a second time.
                let read_from = (from - 0.5).max(start);
                if read_from > dense_until + 0.000001 {
                    dense_analyzer = detect::FrameAnalyzer::default();
                    tracker = detect::RunTracker::default();
                    dense_until = read_from;
                    dense_cutoff = from;
                    self.stats.refinement_calls += 1;
                }
                std::mem::swap(&mut self.analyzer, &mut dense_analyzer);
                let result = (|| -> Result<Option<f64>> {
                    while dense_until < until - 0.000001 {
                        cancel.check()?;
                        let next = (dense_until + 4.0).min(until);
                        for frame in self.read(dense_until, next)? {
                            if let Some(found) =
                                tracker.push(frame, dense_cutoff, settings.hold_seconds)
                            {
                                return Ok(Some(found));
                            }
                        }
                        dense_until = next;
                    }
                    Ok(tracker.finish(until, settings.hold_seconds))
                })();
                std::mem::swap(&mut self.analyzer, &mut dense_analyzer);
                if let Some(found) = result? {
                    cancel.check()?;
                    progress(1.0);
                    return Ok(Some(found));
                }
            }
            cursor = to;
            progress(((cursor - cutoff) / (end - cutoff)).clamp(0.0, 0.999));
        }
        cancel.check()?;
        progress(1.0);
        Ok(None)
    }
    fn read(&mut self, start: f64, end: f64) -> Result<Vec<FramePoint>> {
        self.read_sampled(start, end, None)
    }
    fn read_sampled(
        &mut self,
        start: f64,
        end: f64,
        mut schedule: Option<&mut detect::ScoutSchedule>,
    ) -> Result<Vec<FramePoint>> {
        self.stats.ffmpeg_calls += 1;
        let mut cmd = ffmpeg(self.tools, "info");
        add(&mut cmd, &["-ss", &number(start), "-noautorotate", "-i"]);
        cmd.arg(self.path);
        cmd.args(["-t", &number((end - start).max(0.00001))]);
        let scout = schedule.is_some();
        if scout {
            self.stats.scout_calls += 1;
        }
        let selection = schedule
            .as_ref()
            .map(|schedule| {
                format!(
                    "select='isnan(prev_selected_t)+gte(t-prev_selected_t,{})',",
                    number(schedule.spacing)
                )
            })
            .unwrap_or_default();
        cmd.args([
            "-map",
            &format!("0:{}", self.video_index),
            "-an",
            "-sn",
            "-dn",
            "-vf",
            &format!(
                "{selection}scale={}:{},format=rgb24,showinfo",
                detect::ANALYSIS_WIDTH,
                detect::ANALYSIS_HEIGHT
            ),
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "pipe:1",
        ]);
        let output = process::run(&mut cmd, self.cancel, false, &mut |_| {})?;
        let times: Vec<f64> = output
            .stderr
            .lines()
            .filter_map(|line| {
                line.split_once("pts_time:")?
                    .1
                    .split_whitespace()
                    .next()?
                    .parse::<f64>()
                    .ok()
            })
            .collect();
        let frame_size = detect::ANALYSIS_WIDTH * detect::ANALYSIS_HEIGHT * 3;
        ensure!(
            output.stdout.len() % frame_size == 0
                && times.len() >= output.stdout.len() / frame_size,
            "Could not align frame data and timestamps."
        );
        let mut frames = Vec::new();
        for (rgb, relative_time) in output.stdout.chunks_exact(frame_size).zip(times) {
            self.cancel.check()?;
            let time = start + relative_time;
            if time < end - 0.000001 {
                self.stats.frames_analyzed += 1;
                if scout {
                    self.stats.scout_frames += 1;
                } else {
                    self.stats.dense_frames += 1;
                }
                let requested = schedule.as_mut().map_or_else(
                    || self.settings.enabled_mask(),
                    |schedule| schedule.requested(time),
                );
                let matches = self.analyzer.analyze_requested(
                    time,
                    rgb,
                    self.settings,
                    requested,
                    &mut self.stats,
                );
                frames.push(FramePoint {
                    time,
                    matches,
                    still_since: self.analyzer.still_since,
                });
            }
        }
        Ok(frames)
    }
}
impl FrameSource for VideoFrames<'_> {
    fn window(&mut self, start: f64, end: f64) -> Result<Vec<FramePoint>> {
        self.read(start, end)
    }
}

pub fn collect_folder(root: &Path, recurse: bool, cancel: &Cancel) -> Result<Vec<Input>> {
    fn visit(
        root: &Path,
        directory: &Path,
        recurse: bool,
        output: &mut Vec<Input>,
        cancel: &Cancel,
    ) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            cancel.check()?;
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() && recurse {
                visit(root, &path, recurse, output, cancel)?;
            } else if kind.is_file()
                && supported_video(&path)
                && !path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .starts_with(".tailout-")
                && !path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .ends_with(".hevc")
            {
                output.push(Input {
                    relative_dir: directory.strip_prefix(root)?.to_path_buf(),
                    path,
                });
            }
        }
        Ok(())
    }
    let mut output = Vec::new();
    visit(root, root, recurse, &mut output, cancel)?;
    output.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(output)
}
pub fn supported_video(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        ["mp4", "mkv", "mov"]
            .iter()
            .any(|v| extension.eq_ignore_ascii_case(v))
    })
}

#[cfg(test)]
mod preview_tests {
    use super::*;
    #[test]
    fn ppm_keeps_whitespace_valued_first_pixel_and_real_dimensions() {
        let mut data = b"P6\n2 1\n255\n".to_vec();
        data.extend_from_slice(&[10, 32, 13, 0, 255, 128]);
        let (width, height, rgb) = read_ppm(&data).unwrap();
        assert_eq!((width, height), (2, 1));
        assert_eq!(rgb, vec![10, 32, 13, 0, 255, 128]);
        assert!(read_ppm(b"P6\n2 1\n255\nabc").is_err());
    }
}
