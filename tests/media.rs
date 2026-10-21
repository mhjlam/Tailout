use std::{
    fs,
    io::Write,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};
use tailout::{
    detect::{Criterion, DetectionSettings},
    editor::{BatchOptions, Clip, VideoEdit},
    media::{Input, Tools},
    model::{Encoder, ResizeMode, Settings},
    process::{self, Cancel},
};

fn generate(path: &Path, arguments: &[&str]) {
    let tools = Tools::default();
    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-n"])
        .args(arguments)
        .arg(path);
    process::run(&mut cmd, &Cancel::default(), false, &mut |_| {}).unwrap();
}

fn fixture(path: &Path, duration: &str) {
    generate(
        path,
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size=320x180:rate=12:duration={duration}"),
            "-c:v",
            "libx264",
            "-crf",
            "12",
        ],
    );
}
fn settings(output: &Path) -> Settings {
    Settings {
        output_dir: output.into(),
        encoder: Encoder::Software,
        replace_original: false,
        delete_original: false,
        force_overwrite: true,
        ..Default::default()
    }
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn trims_resizes_compares_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("a [1] $(literal) ä.mp4");
    fixture(&input, "3");
    let bytes = fs::read(&input).unwrap();
    let output = dir.path().to_path_buf();
    let tools = Tools::default();
    let mut options = settings(&output);
    options.start = 0.5;
    options.end = Some(2.5);
    options.resize = true;
    options.width = 160;
    options.height = 90;
    options.compare = true;
    let mut events = Vec::new();
    let result = tools
        .convert_with_progress(
            &Input::new(input.clone()),
            &options,
            &Cancel::default(),
            &mut |event| events.push(event),
        )
        .unwrap();
    assert_eq!(result.status, "Done");
    assert_eq!(events.first().unwrap().overall, 0.0);
    assert_eq!(events.last().unwrap().overall, 1.0);
    assert!(
        events
            .windows(2)
            .all(|pair| pair[1].overall >= pair[0].overall)
    );
    for phase in ["Encoding", "Validating", "Comparing"] {
        assert!(events.iter().any(|event| {
            event.phase == phase
                && event
                    .stats
                    .as_ref()
                    .is_some_and(|stats| stats.frame.is_some_and(|frame| frame > 0))
        }));
    }
    assert!(
        events
            .iter()
            .any(|event| event.phase == "Encoding" && event.encoder == Some("libx265"))
    );

    assert_eq!(fs::read(&input).unwrap(), bytes);
    let info = tools
        .probe(result.output.as_ref().unwrap(), &Cancel::default())
        .unwrap();
    assert_eq!((info.primary().width, info.primary().height), (160, 90));
    assert!((info.duration - 2.0).abs() < 0.1);
    assert_eq!(info.primary().codec_name.as_deref(), Some("hevc"));
    assert!(result.quality[0].ssim > 0.8);
    let second = tools
        .convert(
            &Input::new(input.clone()),
            &options,
            &Cancel::default(),
            &mut |_, _| {},
        )
        .unwrap();
    assert_ne!(result.output, second.output);
    let first_path = result.output.as_ref().unwrap();
    let second_path = second.output.as_ref().unwrap();
    assert_eq!(
        first_path.file_name().unwrap(),
        "a [1] $(literal) ä.hevc.mp4"
    );
    assert_eq!(
        second_path.file_name().unwrap(),
        "a [1] $(literal) ä.hevc (1).mp4"
    );
    assert_eq!(
        fs::canonicalize(first_path.parent().unwrap()).unwrap(),
        fs::canonicalize(dir.path()).unwrap()
    );
    assert_eq!(fs::read(&input).unwrap(), bytes);
    assert!(result.output.unwrap().is_file());
    assert!(!fs::read_dir(output).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tailout-")
    }));
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn replaces_without_creating_backups_or_touching_existing_files() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mkv");
    fixture(&input, "1");
    let original = fs::read(&input).unwrap();
    fs::write(dir.path().join("source.mkv.bak"), "existing").unwrap();
    let mut options = settings(dir.path());
    options.replace_original = true;
    let tools = Tools::default();
    let result = tools
        .convert(
            &Input::new(input.clone()),
            &options,
            &Cancel::default(),
            &mut |_, _| {},
        )
        .unwrap();
    assert_eq!(result.output, Some(fs::canonicalize(&input).unwrap()));
    assert_ne!(fs::read(&input).unwrap(), original);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    assert_eq!(
        fs::read_to_string(dir.path().join("source.mkv.bak")).unwrap(),
        "existing"
    );
    assert_eq!(
        tools
            .probe(&input, &Cancel::default())
            .unwrap()
            .primary()
            .codec_name
            .as_deref(),
        Some("hevc")
    );
    let skip = tools
        .convert(
            &Input::new(input),
            &options,
            &Cancel::default(),
            &mut |_, _| {},
        )
        .unwrap();
    assert_eq!(skip.status, "Already HEVC");
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn black_tail_is_found_at_the_frame_boundary_and_used_for_conversion() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("tail.mp4");
    generate(
        &input,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=10:duration=122",
            "-f",
            "lavfi",
            "-i",
            "color=black:size=160x90:rate=10:duration=4",
            "-filter_complex",
            "[0:v][1:v]concat=n=2:v=1:a=0[v]",
            "-map",
            "[v]",
            "-c:v",
            "libx264",
        ],
    );
    let tools = Tools::default();
    let cancel = Cancel::default();
    let info = tools.probe(&input, &cancel).unwrap();
    let detection = DetectionSettings {
        hold_seconds: 1.5,
        scene_change: false,
        exit_fullscreen: false,
        still_picture: false,
        ..Default::default()
    };
    let end = tools
        .detect(
            &input,
            &info,
            0.0,
            info.duration,
            &detection,
            &cancel,
            &mut |_| {},
        )
        .unwrap()
        .unwrap();
    assert!((end - 122.0).abs() < 0.02, "boundary was {end}");
    let scene_only = DetectionSettings {
        scene_change: true,
        solid_screen: false,
        ..detection.clone()
    };
    let scene_end = tools
        .detect(
            &input,
            &info,
            0.0,
            info.duration,
            &scene_only,
            &cancel,
            &mut |_| {},
        )
        .unwrap()
        .unwrap();
    assert!((scene_end - 122.0).abs() < 0.02);
    let mut options = settings(&dir.path().join("out"));
    options.auto_trim = Some(detection);
    let result = tools
        .convert(&Input::new(input), &options, &cancel, &mut |_, _| {})
        .unwrap();
    let output = tools.probe(&result.output.unwrap(), &cancel).unwrap();
    assert!((output.duration - 122.0).abs() < 0.1);
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn first_sustained_event_is_found_even_if_video_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("recovery.mkv");
    generate(
        &input,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=10:duration=122",
            "-f",
            "lavfi",
            "-i",
            "color=black:size=160x90:rate=10:duration=2",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=10:duration=2",
            "-filter_complex",
            "[0:v][1:v][2:v]concat=n=3:v=1:a=0[v]",
            "-map",
            "[v]",
            "-c:v",
            "libx264",
        ],
    );
    let tools = Tools::default();
    let cancel = Cancel::default();
    let info = tools.probe(&input, &cancel).unwrap();
    let mut detection = DetectionSettings {
        hold_seconds: 1.5,
        scene_change: false,
        exit_fullscreen: false,
        still_picture: false,
        ..Default::default()
    };
    let end = tools
        .detect(
            &input,
            &info,
            0.0,
            info.duration,
            &detection,
            &cancel,
            &mut |_| {},
        )
        .unwrap()
        .unwrap();
    assert!((end - 122.0).abs() < 0.02, "boundary {end}");
    detection.hold_seconds = 2.5;
    assert!(
        tools
            .detect(
                &input,
                &info,
                0.0,
                info.duration,
                &detection,
                &cancel,
                &mut |_| {}
            )
            .unwrap()
            .is_none()
    );
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn white_percentage_and_reference_histogram_are_supported() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("white.mp4");
    generate(
        &input,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=10:duration=122",
            "-f",
            "lavfi",
            "-i",
            "color=white:size=160x90:rate=10:duration=3,drawbox=x=0:y=0:w=32:h=90:color=black:t=fill",
            "-filter_complex",
            "[0:v][1:v]concat=n=2:v=1:a=0[v]",
            "-map",
            "[v]",
            "-c:v",
            "libx264",
        ],
    );
    let tools = Tools::default();
    let cancel = Cancel::default();
    let info = tools.probe(&input, &cancel).unwrap();
    let mut detection = DetectionSettings {
        criterion: Criterion::White,
        pixel_fraction: 0.75,
        hold_seconds: 1.5,
        scene_change: false,
        exit_fullscreen: false,
        still_picture: false,
        ..Default::default()
    };
    assert!(
        (tools
            .detect(
                &input,
                &info,
                0.0,
                info.duration,
                &detection,
                &cancel,
                &mut |_| {}
            )
            .unwrap()
            .unwrap()
            - 122.0)
            .abs()
            < 0.02
    );
    detection.pixel_fraction = 0.85;
    assert!(
        tools
            .detect(
                &input,
                &info,
                0.0,
                info.duration,
                &detection,
                &cancel,
                &mut |_| {}
            )
            .unwrap()
            .is_none()
    );
    detection.criterion = Criterion::Color;
    detection.color = [255, 255, 255];
    detection.pixel_fraction = 0.75;
    let color_end = tools
        .detect(
            &input,
            &info,
            0.0,
            info.duration,
            &detection,
            &cancel,
            &mut |_| {},
        )
        .unwrap()
        .unwrap();
    assert!((color_end - 122.0).abs() < 0.02);
    let frame = tools.preview(&input, 123.0, &cancel).unwrap();
    detection.reference_enabled = true;
    // Keep the solid detector enabled but unmatched: reference adds an OR rule.
    detection.criterion = Criterion::Black;
    detection.reference = Some(frame.histogram);
    assert!(
        (tools
            .detect(
                &input,
                &info,
                0.0,
                info.duration,
                &detection,
                &cancel,
                &mut |_| {}
            )
            .unwrap()
            .unwrap()
            - 122.0)
            .abs()
            < 0.02
    );
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn quality_rejection_leaves_input_and_output_folder_clean() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("reject.mp4");
    fixture(&input, "1");
    let original = fs::read(&input).unwrap();
    let mut options = settings(&dir.path().join("out"));
    options.delete_original = true;
    options.min_ssim = Some(1.0);
    options.quality = 30;
    assert!(
        Tools::default()
            .convert(
                &Input::new(input.clone()),
                &options,
                &Cancel::default(),
                &mut |_, _| {}
            )
            .is_err()
    );
    assert_eq!(original, fs::read(input).unwrap());
    assert_eq!(fs::read_dir(options.output_dir).unwrap().count(), 0);
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn cancellation_terminates_backend_promptly() {
    let cancel = Cancel::default();
    let token = cancel.clone();
    let started = Instant::now();
    let worker = thread::spawn(move || {
        let mut cmd = Command::new(Tools::default().ffmpeg);
        cmd.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-re",
            "-f",
            "lavfi",
            "-i",
            "color=size=64x64:rate=10",
            "-t",
            "30",
            "-f",
            "null",
            "-",
        ]);
        process::run(&mut cmd, &token, false, &mut |_| {})
    });
    thread::sleep(Duration::from_millis(250));
    cancel.cancel();
    assert!(
        worker
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("Cancelled")
    );
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn trimming_preserves_all_tracks_and_rebases_chapters() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("tracks.mkv");
    let subtitle = dir.path().join("sub.srt");
    let metadata = dir.path().join("chapters.txt");
    let attachment = dir.path().join("attachment.txt");
    fs::write(&subtitle, "1\n00:00:00,000 --> 00:00:02,900\nHello\n").unwrap();
    fs::write(&metadata, ";FFMETADATA1\ntitle=Test video\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1500\ntitle=First\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=1500\nEND=3000\ntitle=Second\n").unwrap();
    fs::write(&attachment, "Keep this attachment").unwrap();
    generate(
        &input,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=12:duration=3",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=3",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=880:duration=3",
            "-i",
            subtitle.to_str().unwrap(),
            "-i",
            metadata.to_str().unwrap(),
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-map",
            "2:a",
            "-map",
            "3:s",
            "-map_metadata",
            "4",
            "-map_chapters",
            "4",
            "-c:v",
            "libx264",
            "-c:a",
            "aac",
            "-c:s",
            "srt",
            "-metadata:s:a:0",
            "language=eng",
            "-metadata:s:a:1",
            "language=deu",
            "-attach",
            attachment.to_str().unwrap(),
            "-metadata:s:t",
            "mimetype=text/plain",
        ],
    );
    let tools = Tools::default();
    let mut options = settings(&dir.path().join("out"));
    options.start = 0.5;
    options.end = Some(2.5);
    let result = tools
        .convert(
            &Input::new(input),
            &options,
            &Cancel::default(),
            &mut |_, _| {},
        )
        .unwrap();
    let info = tools
        .probe(&result.output.unwrap(), &Cancel::default())
        .unwrap();
    assert_eq!(info.streams.len(), 5);
    assert_eq!(
        info.streams
            .iter()
            .filter(|s| s.codec_type == "audio")
            .count(),
        2
    );
    assert_eq!(info.chapters.len(), 2);
    assert_eq!(info.chapters[0].start_time.parse::<f64>().unwrap(), 0.0);
    assert!((info.chapters[1].start_time.parse::<f64>().unwrap() - 1.0).abs() < 0.01);
    let audio: Vec<_> = info
        .streams
        .iter()
        .filter(|s| s.codec_type == "audio")
        .collect();
    assert_eq!(
        audio[0].tags.get("language").map(String::as_str),
        Some("eng")
    );
    assert_eq!(
        audio[1].tags.get("language").map(String::as_str),
        Some("deu")
    );
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn ten_bit_sdr_is_preserved_and_hdr_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let sdr = dir.path().join("sdr.mkv");
    let hdr = dir.path().join("hdr.mkv");
    generate(
        &sdr,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=10:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p10le",
        ],
    );
    generate(
        &hdr,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=10:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p10le",
            "-x264-params",
            "colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc",
        ],
    );
    let tools = Tools::default();
    let options = settings(&dir.path().join("out"));
    let result = tools
        .convert(
            &Input::new(sdr),
            &options,
            &Cancel::default(),
            &mut |_, _| {},
        )
        .unwrap();
    assert_eq!(
        tools
            .probe(&result.output.unwrap(), &Cancel::default())
            .unwrap()
            .primary()
            .depth(),
        10
    );
    let bytes = fs::read(&hdr).unwrap();
    let error = tools
        .convert(
            &Input::new(hdr.clone()),
            &options,
            &Cancel::default(),
            &mut |_, _| {},
        )
        .unwrap_err();
    assert!(error.to_string().contains("HDR"));
    assert_eq!(fs::read(hdr).unwrap(), bytes);
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn portrait_and_rotated_sources_obey_bounds_without_upscaling() {
    let dir = tempfile::tempdir().unwrap();
    let portrait = dir.path().join("portrait.mov");
    let base = dir.path().join("base.mp4");
    let rotated = dir.path().join("rotated.mp4");
    generate(
        &portrait,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=180x320:rate=10:duration=1",
            "-c:v",
            "libx264",
        ],
    );
    fixture(&base, "1");
    generate(
        &rotated,
        &[
            "-display_rotation:v:0",
            "90",
            "-i",
            base.to_str().unwrap(),
            "-c",
            "copy",
        ],
    );
    let tools = Tools::default();
    let mut options = settings(&dir.path().join("out"));
    options.resize = true;
    options.width = 160;
    options.height = 90;
    for path in [portrait, rotated] {
        let result = tools
            .convert(
                &Input::new(path),
                &options,
                &Cancel::default(),
                &mut |_, _| {},
            )
            .unwrap();
        let info = tools
            .probe(&result.output.unwrap(), &Cancel::default())
            .unwrap();
        let stream = info.primary();
        let (width, height) = if stream.rotated() {
            (stream.height, stream.width)
        } else {
            (stream.width, stream.height)
        };
        assert!(width <= 160 && height <= 90);
    }
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn an_audio_tail_is_not_mistaken_for_sustained_black_video() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("long-audio.mkv");
    generate(
        &input,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=10:duration=121",
            "-f",
            "lavfi",
            "-i",
            "color=black:size=160x90:rate=10:duration=0.2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=125",
            "-filter_complex",
            "[0:v][1:v]concat=n=2:v=1:a=0[v]",
            "-map",
            "[v]",
            "-map",
            "2:a",
            "-c:v",
            "libx264",
            "-c:a",
            "aac",
        ],
    );
    let tools = Tools::default();
    let cancel = Cancel::default();
    let info = tools.probe(&input, &cancel).unwrap();
    assert!(info.duration < 121.4);
    let detection = DetectionSettings {
        hold_seconds: 1.0,
        scene_change: false,
        exit_fullscreen: false,
        still_picture: false,
        ..Default::default()
    };
    assert!(
        tools
            .detect(
                &input,
                &info,
                0.0,
                info.duration,
                &detection,
                &cancel,
                &mut |_| {}
            )
            .unwrap()
            .is_none()
    );
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn resize_modes_export_the_predicted_pixel_dimensions() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("resize.mp4");
    fixture(&input, "1");
    let tools = Tools::default();
    for (mode, keep, width_percent, height_percent, expected) in [
        (ResizeMode::Height, true, 50.0, 50.0, (160, 90)),
        (ResizeMode::Height, false, 50.0, 50.0, (320, 90)),
        (ResizeMode::Percent, true, 25.0, 50.0, (80, 44)),
        (ResizeMode::Percent, false, 50.0, 25.0, (160, 44)),
        (ResizeMode::Bounds, true, 50.0, 50.0, (320, 180)),
    ] {
        let mut options = settings(&dir.path().join("out"));
        options.resize = true;
        options.resize_mode = mode;
        options.keep_aspect = keep;
        options.width = 640;
        options.height = if mode == ResizeMode::Bounds { 720 } else { 90 };
        options.percent = width_percent;
        options.height_percent = height_percent;
        let result = tools
            .convert(
                &Input::new(input.clone()),
                &options,
                &Cancel::default(),
                &mut |_, _| {},
            )
            .unwrap();
        let info = tools
            .probe(&result.output.unwrap(), &Cancel::default())
            .unwrap();
        let stream = info.primary();
        assert_eq!((stream.width, stream.height), expected);
        if !keep {
            assert_eq!(stream.sample_aspect_ratio.as_deref(), Some("1:1"));
        }
    }
    let preview = tools.preview(&input, 0.0, &Cancel::default()).unwrap();
    assert_eq!((preview.width, preview.height), (320, 180));
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH; exercises NVENC when available"]
fn encoder_selection_matches_a_real_hd_nvenc_encode() {
    let tools = Tools::default();
    let cancel = Cancel::default();
    // Establish hardware availability independently of the application's probe.
    // A tiny synthetic frame can fail even with a working GPU and driver.
    let mut command = Command::new(&tools.ffmpeg);
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-f",
        "lavfi",
        "-i",
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
        "21",
        "-b:v",
        "0",
        "-f",
        "null",
        "-",
    ]);
    if process::run(&mut command, &cancel, false, &mut |_| {}).is_err() {
        assert!(tools.select_encoder(Encoder::Nvenc, 21, &cancel).is_err());
        assert_eq!(
            tools.select_encoder(Encoder::Auto, 21, &cancel).unwrap(),
            "libx265"
        );
        return;
    }
    for selection in [Encoder::Nvenc, Encoder::Auto] {
        assert_eq!(
            tools.select_encoder(selection, 21, &cancel).unwrap(),
            "hevc_nvenc"
        );
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("nvenc.mp4");
    fixture(&input, "1");
    let original = fs::read(&input).unwrap();
    let mut options = settings(&directory.path().join("out"));
    options.encoder = Encoder::Nvenc;
    let result = tools
        .convert(
            &Input::new(input.clone()),
            &options,
            &cancel,
            &mut |_, _| {},
        )
        .unwrap();
    assert_eq!(result.encoder, "hevc_nvenc");
    assert_eq!(result.status, "Done");
    let info = tools.probe(&result.output.unwrap(), &cancel).unwrap();
    assert_eq!(info.primary().codec_name.as_deref(), Some("hevc"));
    assert_eq!(fs::read(input).unwrap(), original);
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn larger_output_exports_but_only_replaces_when_forced() {
    let directory = tempfile::tempdir().unwrap();
    let source_folder = directory.path().join("originals");
    fs::create_dir_all(&source_folder).unwrap();
    let source = source_folder.join("small.mp4");
    generate(
        &source,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=size=64x64:rate=1:duration=1",
            "-c:v",
            "libx264",
            "-crf",
            "40",
        ],
    );
    let original = fs::read(&source).unwrap();
    let tools = Tools::default();
    let cancel = Cancel::default();
    let mut input = Input::new(source.clone());
    input.relative_dir = "nested".into();
    let mut options = settings(&directory.path().join("exports"));
    options.quality = 15;
    options.force_overwrite = false;
    let exported = tools
        .convert(&input, &options, &cancel, &mut |_, _| {})
        .unwrap();
    assert_eq!(exported.status, "Done");
    assert!(
        exported.saved_bytes <= 0,
        "fixture must grow during encoding"
    );
    assert!(exported.output.unwrap().is_file());
    assert_eq!(fs::read(&source).unwrap(), original);
    options.replace_original = true;
    let rejected = tools
        .convert(&input, &options, &cancel, &mut |_, _| {})
        .unwrap();
    assert_eq!(rejected.status, "Not smaller — source retained");
    assert!(rejected.output.is_none());
    assert_eq!(fs::read(&source).unwrap(), original);
    options.force_overwrite = true;
    let replaced = tools
        .convert(&input, &options, &cancel, &mut |_, _| {})
        .unwrap();
    assert_eq!(replaced.status, "Done");
    assert_eq!(
        replaced.output.as_ref(),
        Some(&fs::canonicalize(&source).unwrap())
    );
    assert_ne!(fs::read(&source).unwrap(), original);
    assert_eq!(fs::read_dir(&source_folder).unwrap().count(), 1);
    assert_eq!(
        tools
            .probe(&source, &cancel)
            .unwrap()
            .primary()
            .codec_name
            .as_deref(),
        Some("hevc")
    );
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn browser_and_still_picture_with_small_animation_are_detected_independently_after_grace() {
    use tailout::detect::{ANALYSIS_HEIGHT as H, ANALYSIS_WIDTH as W};
    let dir = tempfile::tempdir().unwrap();
    let tools = Tools::default();
    let cancel = Cancel::default();
    for spinner in [false, true] {
        let raw = dir.path().join(if spinner {
            "spinner.rgb"
        } else {
            "browser.rgb"
        });
        let mut pixels = std::io::BufWriter::new(fs::File::create(&raw).unwrap());
        for frame in 0..160 {
            let phase = frame as f64 * 0.7;
            for y in 0..H {
                for x in 0..W {
                    let value = if spinner {
                        let dx = x as f64 - W as f64 / 2.0;
                        let dy = y as f64 - H as f64 / 2.0;
                        let radius = (dx * dx + dy * dy).sqrt();
                        let angle = (dy.atan2(dx) - phase).rem_euclid(std::f64::consts::TAU);
                        if (4.3..=5.7).contains(&radius) && angle < 3.8 {
                            240
                        } else {
                            20
                        }
                    } else if (3..20).contains(&y) {
                        if (6..10).contains(&y) && x % 8 < 3 {
                            240
                        } else {
                            180
                        }
                    } else {
                        45
                    };
                    pixels.write_all(&[value; 3]).unwrap();
                }
            }
        }
        pixels.flush().unwrap();
        drop(pixels);
        let video = dir.path().join(if spinner {
            "spinner.mkv"
        } else {
            "browser.mkv"
        });
        generate(
            &video,
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=192x108:rate=8:duration=122",
                "-f",
                "rawvideo",
                "-pixel_format",
                "rgb24",
                "-video_size",
                "192x108",
                "-framerate",
                "8",
                "-i",
                raw.to_str().unwrap(),
                "-filter_complex",
                "[0:v][1:v]concat=n=2:v=1:a=0[v]",
                "-map",
                "[v]",
                "-c:v",
                "libx264",
                "-crf",
                "12",
                "-r",
                "8",
            ],
        );
        let info = tools.probe(&video, &cancel).unwrap();
        let detection = DetectionSettings {
            scene_change: false,
            solid_screen: false,
            exit_fullscreen: !spinner,
            still_picture: spinner,
            ..Default::default()
        };
        let end = tools
            .detect(
                &video,
                &info,
                0.0,
                info.duration,
                &detection,
                &cancel,
                &mut |_| {},
            )
            .unwrap()
            .expect("visual event");
        assert!(
            (122.0..=122.3).contains(&end),
            "spinner={spinner}, found={end}"
        );
    }
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn batch_defaults_and_video_overrides_export_with_independent_markers() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.mp4");
    let second = dir.path().join("second.mp4");
    fixture(&first, "3");
    fs::copy(&first, &second).unwrap();
    let source = fs::read(&first).unwrap();
    let mut batch = BatchOptions {
        output_dir: dir.path().join("out"),
        replace_original: false,
        delete_original: false,
        ..Default::default()
    };
    batch.video.encoder = Encoder::Software;
    batch.video.resize.enabled = true;
    batch.video.resize.mode = ResizeMode::Percent;
    batch.video.resize.percent = 50.0;
    let a = VideoEdit {
        clips: vec![Clip {
            start: "0.5".into(),
            end: "2".into(),
            ..Clip::default()
        }],
        ..Default::default()
    };
    let mut b = VideoEdit {
        clips: vec![Clip {
            start: "1".into(),
            end: "3".into(),
            ..Clip::default()
        }],
        ..Default::default()
    };
    b.set_video_override(true, &batch);
    b.video.resize.percent = 25.0;
    b.video.quality = 18;
    batch.video.quality = 26;
    let tools = Tools::default();
    let cancel = Cancel::default();
    for (input, edit, dimensions, duration, quality) in [
        (first.clone(), a, (160, 90), 1.5, 26),
        (second.clone(), b, (80, 44), 2.0, 18),
    ] {
        let options = edit.settings(&batch).unwrap();
        assert_eq!(options.quality, quality);
        let output = tools
            .convert(&Input::new(input), &options, &cancel, &mut |_, _| {})
            .unwrap()
            .output
            .unwrap();
        let info = tools.probe(&output, &cancel).unwrap();
        assert_eq!((info.primary().width, info.primary().height), dimensions);
        assert!((info.duration - duration).abs() < 0.1);
    }
    assert_eq!(fs::read(first).unwrap(), source);
    assert_eq!(fs::read(second).unwrap(), source);
}

#[test]
#[ignore = "requires FFmpeg on PATH"]
fn ffmpeg_progress_arrives_before_exit_and_cancellation_stops_the_process() {
    let tools = Tools::default();
    let mut command = Command::new(&tools.ffmpeg);
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-re",
        "-f",
        "lavfi",
        "-i",
        "color=size=64x64:rate=10:duration=60",
        "-progress",
        "pipe:1",
        "-stats_period",
        "0.1",
        "-nostats",
        "-f",
        "null",
        "-",
    ]);
    let cancel = Cancel::default();
    let started = Instant::now();
    let mut reports = Vec::new();
    let error = process::run_with_progress(&mut command, &cancel, true, &mut |record| {
        if record.seconds.is_some_and(|seconds| seconds >= 0.5) {
            cancel.cancel();
        }
        reports.push(record);
    })
    .unwrap_err();
    assert!(error.to_string().contains("Cancelled"));
    assert!(
        reports
            .iter()
            .any(|record| record.frame.is_some_and(|frame| frame > 0))
    );
    assert!(
        reports
            .iter()
            .any(|record| record.seconds.is_some_and(|seconds| seconds >= 0.5))
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "progress must be delivered while FFmpeg is still running"
    );
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn folder_exports_delete_sources_only_after_successful_publication() {
    let dir = tempfile::tempdir().unwrap();
    let fixture_path = dir.path().join("fixture.mp4");
    fixture(&fixture_path, "1");
    let bytes = fs::read(&fixture_path).unwrap();
    let tools = Tools::default();
    for same_folder in [false, true] {
        for delete in [false, true] {
            let root = dir
                .path()
                .join(format!("same-{same_folder}-delete-{delete}"));
            fs::create_dir(&root).unwrap();
            let input = root.join("source.mp4");
            fs::write(&input, &bytes).unwrap();
            let output_dir = if same_folder {
                root.clone()
            } else {
                root.join("exports")
            };
            fs::create_dir_all(&output_dir).unwrap();
            let existing = output_dir.join("source.hevc.mp4");
            fs::write(&existing, b"existing output must survive").unwrap();
            let mut options = Settings {
                replace_original: false,
                output_dir,
                encoder: Encoder::Software,
                ..Default::default()
            };
            assert!(options.delete_original);
            options.delete_original = delete;
            let result = tools
                .convert(
                    &Input::new(input.clone()),
                    &options,
                    &Cancel::default(),
                    &mut |_, _| {},
                )
                .unwrap();
            assert_eq!(result.status, "Done");
            assert_eq!(result.source_removed, delete);
            assert!(result.warning.is_none());
            assert_eq!(input.exists(), !delete);
            if !delete {
                assert_eq!(fs::read(&input).unwrap(), bytes);
            }
            assert_eq!(fs::read(existing).unwrap(), b"existing output must survive");
            let output = result.output.unwrap();
            assert_eq!(output.file_name().unwrap(), "source.hevc (1).mp4");
            assert_eq!(
                tools
                    .probe(&output, &Cancel::default())
                    .unwrap()
                    .primary()
                    .codec_name
                    .as_deref(),
                Some("hevc")
            );
            // A skipped HEVC export must not remove the input even with cleanup on.
            options.delete_original = true;
            let skipped = tools
                .convert(
                    &Input::new(output.clone()),
                    &options,
                    &Cancel::default(),
                    &mut |_, _| {},
                )
                .unwrap();
            assert_eq!(skipped.status, "Already HEVC");
            assert!(skipped.output.is_none());
            assert!(!skipped.source_removed);
            assert!(output.is_file());
        }
    }
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn cancelled_folder_export_keeps_the_source_even_at_publication() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mp4");
    fixture(&input, "1");
    let bytes = fs::read(&input).unwrap();
    let mut options = settings(&dir.path().join("exports"));
    options.delete_original = true;
    let cancel = Cancel::default();
    let result = Tools::default().convert_with_progress(
        &Input::new(input.clone()),
        &options,
        &cancel,
        &mut |event| {
            if event.phase == "Saving" {
                cancel.cancel();
            }
        },
    );
    assert!(result.unwrap_err().to_string().contains("Cancelled"));
    assert_eq!(fs::read(input).unwrap(), bytes);
    assert_eq!(fs::read_dir(options.output_dir).unwrap().count(), 0);
}

#[cfg(windows)]
#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn failed_source_cleanup_retains_both_files_and_reports_the_saved_output() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mp4");
    fixture(&input, "1");
    let bytes = fs::read(&input).unwrap();
    // Allow readers/encoding, but forbid deletion for the whole operation.
    let _lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&input)
        .unwrap();
    let mut options = settings(&dir.path().join("exports"));
    options.delete_original = true;
    let result = Tools::default()
        .convert(
            &Input::new(input.clone()),
            &options,
            &Cancel::default(),
            &mut |_, _| {},
        )
        .unwrap();
    assert_eq!(result.status, "Done");
    assert!(!result.source_removed);
    assert!(result.output.as_ref().unwrap().is_file());
    assert!(
        result
            .warning
            .as_ref()
            .unwrap()
            .contains("Original retained")
    );
    assert!(
        result
            .warning
            .unwrap()
            .contains(&result.output.unwrap().display().to_string())
    );
    assert_eq!(fs::read(input).unwrap(), bytes);
}

#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn split_exports_numbered_independent_ranges_with_continuous_per_video_progress() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("split sample.mp4");
    fixture(&input, "6");
    let bytes = fs::read(&input).unwrap();
    let tools = Tools::default();
    let mut first = settings(dir.path());
    first.start = 0.5;
    first.end = Some(1.5);
    first.compare = true;
    let mut second = first.clone();
    second.start = 4.0;
    second.end = Some(5.0);
    let mut progress = Vec::new();
    let result = tools
        .convert_clips(
            &Input::new(input.clone()),
            &[first, second],
            &Cancel::default(),
            &mut |event| progress.push(event),
        )
        .unwrap();
    assert_eq!(result.clips.len(), 2);
    assert!(!result.source_removed);
    assert_eq!(fs::read(input).unwrap(), bytes);
    for (index, clip) in result.clips.iter().enumerate() {
        let output = clip.output.as_ref().unwrap();
        assert_eq!(
            output.file_name().unwrap(),
            format!("split sample-{}.hevc.mp4", index + 1).as_str()
        );
        let info = tools.probe(output, &Cancel::default()).unwrap();
        assert!((info.duration - 1.0).abs() < 0.15);
        assert!(!clip.quality.is_empty());
        assert!(clip.quality.iter().all(|score| score.ssim > 0.90));
    }
    assert!(
        progress
            .windows(2)
            .all(|pair| pair[0].overall <= pair[1].overall)
    );
    assert_eq!(progress.last().unwrap().overall, 1.0);
    assert!(progress.iter().any(|event| event.clip == Some((1, 2))));
    assert!(progress.iter().any(|event| event.clip == Some((2, 2))));
    assert!(
        progress
            .iter()
            .filter(|event| event.clip.is_some())
            .all(|event| event.overall < 1.0)
    );
}
#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn split_replacement_removes_original_only_after_all_clips_and_never_overwrites_collisions() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.mp4");
    fixture(&input, "5");
    let existing = dir.path().join("source-1.hevc.mp4");
    fs::write(&existing, b"Keep this existing file").unwrap();
    let mut first = settings(dir.path());
    first.replace_original = true;
    first.start = 0.5;
    first.end = Some(1.5);
    let mut second = first.clone();
    second.start = 3.0;
    second.end = Some(4.0);
    let result = Tools::default()
        .convert_clips(
            &Input::new(input.clone()),
            &[first, second],
            &Cancel::default(),
            &mut |_| {},
        )
        .unwrap();
    assert!(result.source_removed);
    assert!(!input.exists());
    assert_eq!(fs::read(existing).unwrap(), b"Keep this existing file");
    assert_eq!(result.clips.len(), 2);
    assert!(
        result
            .clips
            .iter()
            .all(|clip| clip.output.as_ref().unwrap().is_file())
    );
    assert!(
        result.clips[0]
            .output
            .as_ref()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("(1)")
    );
}
#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn split_failure_after_a_successful_clip_retains_original_and_completed_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("partial.mp4");
    fixture(&input, "5");
    let bytes = fs::read(&input).unwrap();
    let mut first = settings(dir.path());
    first.replace_original = true;
    first.start = 0.5;
    first.end = Some(1.5);
    let mut second = first.clone();
    second.start = 3.0;
    second.end = Some(4.0);
    second.compare = true;
    second.min_ssim = Some(1.0);
    let error = Tools::default()
        .convert_clips(
            &Input::new(input.clone()),
            &[first, second],
            &Cancel::default(),
            &mut |_| {},
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("Clip 2 of 2 failed"));
    assert_eq!(fs::read(input).unwrap(), bytes);
    assert!(dir.path().join("partial-1.hevc.mp4").is_file());
    assert!(!dir.path().join("partial-2.hevc.mp4").exists());
}
#[test]
#[ignore = "requires FFmpeg and FFprobe on PATH"]
fn split_cancellation_between_clips_keeps_original_and_the_first_export() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("cancel split.mp4");
    fixture(&input, "5");
    let bytes = fs::read(&input).unwrap();
    let mut first = settings(dir.path());
    first.replace_original = true;
    first.start = 0.5;
    first.end = Some(1.5);
    let mut second = first.clone();
    second.start = 3.0;
    second.end = Some(4.0);
    let cancel = Cancel::default();
    let stop = cancel.clone();
    let error = Tools::default()
        .convert_clips(
            &Input::new(input.clone()),
            &[first, second],
            &cancel,
            &mut |event| {
                if event.clip == Some((1, 2))
                    && event.phase == "Saving"
                    && event.fraction == Some(1.0)
                {
                    stop.cancel();
                }
            },
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("Cancelled"));
    assert_eq!(fs::read(input).unwrap(), bytes);
    assert!(dir.path().join("cancel split-1.hevc.mp4").is_file());
    assert!(!dir.path().join("cancel split-2.hevc.mp4").exists());
}
