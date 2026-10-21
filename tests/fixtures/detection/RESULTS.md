# Detection results

## Accuracy

Both Exact and Adaptive pass all **48 cases / 35 videos / 7.57 MiB** and agree
within one original 30 fps frame. Cases cover every detector, additive signals,
thresholds, interruptions, grace/Begin/End boundaries, short down periods,
sampling-window boundaries and the earliest of two endings. Progress and
cancellation checks pass.

The earlier 30-case baseline passed 26 cases. It exposed a scene-return bug,
missing compact browser chrome, and two limitations in ring-shaped spinner
recognition. Scene-return and browser fixes remain; ring recognition has now
been replaced with a still-picture criterion. The original public spinner
animations remain useful tests of tolerated small motion. A stationary icon is
now correctly a positive still-picture case.

New controls check a frozen picture, brief freezes followed by recovery, an
interrupted freeze, slow color drift, small versus large animated areas, and
strict versus relaxed changing-pixel margins. An anchored RGB comparison
rejects gradual changes that adjacent-frame comparisons would accept.

## Performance

Illustrative local measurements: optimized Rust 1.98.1 build, FFmpeg 9.0.2,
software decoding. Search duration excludes the protected two-minute prefix;
timings include seeking, FFmpeg startup, scaling, pixel analysis and confirmation.
Probe/reference preparation and benchmark-video generation are excluded. The
720p input is upscaled from the public fixture and generated temporarily.

| Search region | Exact | Adaptive | Speedup | Analyzed frames | FFmpeg invocations |
| --- | --- | --- | --- | --- | --- |
| 120 s, solid detector, 384 × 216 | 4.09 s | 1.09 s | 3.7× | 3615 → 121 | 31 → 8 |
| 120 s, default detectors, 384 × 216 | 4.25 s | 1.20 s | 3.6× | 3615 → 241 | 31 → 8 |
| 120 s, all five detectors, 384 × 216 | 4.28 s | 1.12 s | 3.8× | 3615 → 241 | 31 → 8 |
| 60 s, default detectors, 1280 × 720 | 2.81 s | 0.86 s | 3.3× | 1815 → 121 | 16 → 4 |

These no-match cases force each strategy to examine the entire eligible range.
The default detectors are scene, solid screen, fullscreen and still picture;
all-five adds a nonmatching reference histogram. All enabled detectors share
exactly the same scout reads. For the all-five case, scene/still run 241 checks
each and solid/fullscreen/reference run 121 each. Shared features need only
241 histograms and 121 luminance images.

The overlapping-candidates case contains forty seconds of alternating one-second
black/white screens, none long enough to qualify. Three scouting windows create
one continuous confirmation stream: 1,275 dense frames, with no overlapping
analysis ranges requested again. Codec-level seek pre-roll is not counted as
analyzed frames. This deliberately difficult case takes 1.94 s versus Exact's
1.69 s; dense confirmation plus scouting adds overhead when candidates are
frequent. Early matches can also be slightly slower. Adaptive is not universally
faster, and wall-clock assertions are deliberately avoided. The suite asserts
accuracy and deterministic work counts instead.

JSON reports are written under `.cache/detection/`. `ffmpeg_calls` counts actual
read processes; `refinement_calls` counts connected confirmation streams, and
`merged_candidates` counts proposals after each scouting window's interval union.
Per-detector counters are ordered scene, solid, fullscreen, still, reference.

## Limits and next checks

The tested promise is the earliest qualifying visual event after the grace
period. Ordinary film cuts can satisfy scene change, and static slides or quiet
shots can satisfy still picture. Browser heuristics can miss unfamiliar layouts;
histograms can match unrelated pictures with similar colors. The still-picture
margin is evaluated on the downscaled analysis image, not original pixels.

Sparse sampling remains a heuristic. Motion can alias with the sample times,
and near-threshold similarity can depend on which picture becomes the anchor.
Full-frame confirmation checks continuity once a candidate is found; it cannot
recover a candidate missed by scouting. Exact remains the comparison oracle.

Future additions should include more players/themes, fades, native HD, variable
frame rates and overlays on moving backgrounds. Preserve the small-data budget
and independent annotations. Re-run the comparison before changing thresholds,
sampling intervals or confirmation windows.
