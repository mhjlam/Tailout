//! Offline end-to-end regression corpus. FFmpeg/FFprobe are the only runtime prerequisites.
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, time::Instant};
use tailout::{
    detect::{Criterion, DetectionSettings, SearchMode},
    media::Tools,
    process::Cancel,
};

#[derive(Clone, Deserialize)]
struct Case {
    id: String,
    detector: String,
    file: String,
    expected: Option<f64>,
    tolerance: f64,
    bytes: u64,
    pixel_fraction: Option<f64>,
    color: Option<[u8; 3]>,
    reference_file: Option<String>,
    reference_time: Option<f64>,
    search_end: Option<f64>,
    search_start: Option<f64>,
    hold_seconds: Option<f64>,
    still_changed_fraction: Option<f64>,
}
#[derive(Deserialize)]
struct Corpus {
    cases: Vec<Case>,
}
#[derive(Serialize)]
struct Measurement {
    id: String,
    mode: SearchMode,
    expected: Option<f64>,
    actual: Option<f64>,
    passed: bool,
    elapsed_ms: f64,
    stats: tailout::detect::DetectionStats,
}
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/detection")
}
fn corpus() -> Corpus {
    serde_json::from_str(include_str!("fixtures/detection/manifest.json")).unwrap()
}
fn settings(case: &Case, tools: &Tools) -> DetectionSettings {
    let mut options = DetectionSettings {
        scene_change: false,
        solid_screen: false,
        exit_fullscreen: false,
        still_picture: false,
        reference_enabled: false,
        ..Default::default()
    };
    match case.detector.as_str() {
        "defaults" => options = DetectionSettings::default(),
        "all" => {
            options = DetectionSettings {
                reference_enabled: true,
                ..Default::default()
            }
        }
        "black" => options.solid_screen = true,
        "white" => {
            options.solid_screen = true;
            options.criterion = Criterion::White;
        }
        "color" => {
            options.solid_screen = true;
            options.criterion = Criterion::Color;
        }
        "scene" => options.scene_change = true,
        "fullscreen" => options.exit_fullscreen = true,
        "still" => options.still_picture = true,
        "reference" => options.reference_enabled = true,
        "additive" => {
            options.solid_screen = true;
            options.exit_fullscreen = true;
        }
        other => panic!("Unknown detector {other}"),
    }
    if let Some(fraction) = case.still_changed_fraction {
        options.still_changed_fraction = fraction;
    }
    if let Some(hold) = case.hold_seconds {
        options.hold_seconds = hold;
    }
    if let Some(fraction) = case.pixel_fraction {
        options.pixel_fraction = fraction;
    }
    if let Some(color) = case.color {
        options.color = color;
    }
    if options.reference_enabled {
        options.reference = Some(
            tools
                .preview(
                    &root().join(case.reference_file.as_ref().unwrap_or(&case.file)),
                    case.reference_time.unwrap(),
                    &Cancel::default(),
                )
                .unwrap()
                .histogram,
        );
    }
    options
}
fn measure(case: &Case, mode: SearchMode) -> Measurement {
    let tools = Tools::default();
    let path = root().join(&case.file);
    assert_eq!(
        fs::metadata(&path).unwrap().len(),
        case.bytes,
        "{} changed; rebuild the manifest",
        case.id
    );
    let cancel = Cancel::default();
    let info = tools.probe(&path, &cancel).unwrap();
    let options = settings(case, &tools);
    let mut progress = Vec::new();
    let clock = Instant::now();
    let result = tools
        .detect_profiled(
            &path,
            &info,
            case.search_start.unwrap_or(0.0),
            case.search_end.unwrap_or(info.duration),
            &options,
            mode,
            &cancel,
            &mut |value| progress.push(value),
        )
        .unwrap();
    let elapsed_ms = clock.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(progress.first(), Some(&0.0), "{} initial progress", case.id);
    assert_eq!(
        progress.last(),
        Some(&1.0),
        "{} completion progress",
        case.id
    );
    assert!(
        progress.windows(2).all(|pair| pair[0] <= pair[1]),
        "{} nonmonotonic progress",
        case.id
    );
    let passed = match (case.expected, result.end) {
        (Some(expected), Some(actual)) => (actual - expected).abs() <= case.tolerance,
        (None, None) => true,
        _ => false,
    };
    let measurement = Measurement {
        id: case.id.clone(),
        mode,
        expected: case.expected,
        actual: result.end,
        passed,
        elapsed_ms,
        stats: result.stats,
    };
    print_measurement(&measurement);
    measurement
}
fn print_measurement(m: &Measurement) {
    println!(
        "{:?} {:28} {:4} expected={:?} actual={:?} {:7.1}ms {:5} frames {:3} FFmpeg calls",
        m.mode,
        m.id,
        if m.passed { "PASS" } else { "FAIL" },
        m.expected,
        m.actual,
        m.elapsed_ms,
        m.stats.frames_analyzed,
        m.stats.ffmpeg_calls
    );
}
fn report(name: &str, measurements: &[Measurement]) {
    let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".cache/detection");
    fs::create_dir_all(&folder).unwrap();
    fs::write(
        folder.join(name),
        serde_json::to_vec_pretty(measurements).unwrap(),
    )
    .unwrap();
    println!("Wrote {name} ({} measurements)", measurements.len());
}
#[test]
#[ignore = "requires FFmpeg/FFprobe; run with --release --ignored --nocapture"]
fn adaptive_matches_exact_and_annotated_events() {
    let mut measurements = Vec::new();
    let mut failed = Vec::new();
    for case in &corpus().cases {
        let exact = measure(case, SearchMode::Exact);
        let adaptive = measure(case, SearchMode::Adaptive);
        let agrees = match (exact.actual, adaptive.actual) {
            (Some(a), Some(b)) => (a - b).abs() <= 1.0 / 30.0 + 0.00001,
            (None, None) => true,
            _ => false,
        };
        if !exact.passed || !adaptive.passed || !agrees {
            failed.push(case.id.clone());
        }
        if case.id == "long-no-match" {
            assert!(adaptive.stats.frames_analyzed * 3 < exact.stats.frames_analyzed);
            assert!(adaptive.stats.ffmpeg_calls * 2 < exact.stats.ffmpeg_calls);
        }
        if matches!(
            case.id.as_str(),
            "long-default-detectors" | "long-all-detectors"
        ) {
            assert_eq!(
                adaptive.stats.scout_calls, 8,
                "one shared scouting pass regardless of detector count"
            );
            assert!(
                adaptive.stats.detector_evaluations[0] > adaptive.stats.detector_evaluations[1]
            );
            assert!(adaptive.stats.luma_frames < adaptive.stats.frames_analyzed);
        }
        if case.id == "long-all-detectors" {
            assert!(
                adaptive.stats.histograms
                    < adaptive.stats.detector_evaluations[0]
                        + adaptive.stats.detector_evaluations[4],
                "reuse histogram for scene and reference"
            );
        }
        if case.id == "overlapping-candidates" {
            assert_eq!(
                adaptive.stats.refinement_calls, 1,
                "overlaps extend one confirmation stream"
            );
            assert!(
                adaptive.stats.merged_candidates >= 2,
                "exercise multiple scouting windows"
            );
            assert!(
                adaptive.stats.dense_frames <= 1_336,
                "never re-read overlapping dense sections"
            );
        }
        measurements.push(exact);
        measurements.push(adaptive);
    }
    report("comparison.json", &measurements);
    assert!(failed.is_empty(), "Accuracy disagreements: {failed:?}");
}
#[test]
#[ignore = "requires FFmpeg/FFprobe"]
fn search_cancellation_stops_without_reporting_completion() {
    let corpus = corpus();
    let case = corpus
        .cases
        .iter()
        .find(|c| c.id == "long-no-match")
        .unwrap();
    let tools = Tools::default();
    let path = root().join(&case.file);
    let info = tools.probe(&path, &Cancel::default()).unwrap();
    for mode in [SearchMode::Exact, SearchMode::Adaptive] {
        let cancel = Cancel::default();
        let mut completed = false;
        let error = tools
            .detect_profiled(
                &path,
                &info,
                0.0,
                info.duration,
                &settings(case, &tools),
                mode,
                &cancel,
                &mut |value| {
                    completed |= value == 1.0;
                    if value > 0.0 {
                        cancel.cancel();
                    }
                },
            )
            .unwrap_err();
        assert_eq!(error.to_string(), "Cancelled");
        assert!(!completed);
    }
}

#[test]
#[ignore = "creates a temporary 720p fixture; run with --release --ignored --nocapture"]
fn benchmark_hd_default_detectors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("public-hd-benchmark.mp4");
    let mut command = std::process::Command::new("ffmpeg");
    command
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(root().join("long-no-match.mp4"))
        .args([
            "-t",
            "180",
            "-vf",
            "scale=1280:720",
            "-an",
            "-map_metadata",
            "-1",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-crf",
            "28",
            "-g",
            "60",
            "-threads",
            "2",
        ])
        .arg(&path);
    tailout::process::run(&mut command, &Cancel::default(), false, &mut |_| {}).unwrap();
    let case = Case {
        id: "720p-default-detectors".into(),
        detector: "defaults".into(),
        file: path.to_string_lossy().into_owned(),
        bytes: fs::metadata(&path).unwrap().len(),
        expected: None,
        tolerance: 0.15,
        pixel_fraction: None,
        color: None,
        reference_file: None,
        reference_time: None,
        search_end: None,
        search_start: None,
        hold_seconds: None,
        still_changed_fraction: None,
    };
    let exact = measure(&case, SearchMode::Exact);
    let adaptive = measure(&case, SearchMode::Adaptive);
    assert!(exact.passed && adaptive.passed);
    assert!(adaptive.stats.frames_analyzed < exact.stats.frames_analyzed);
    report("benchmark-hd.json", &[exact, adaptive]);
}
