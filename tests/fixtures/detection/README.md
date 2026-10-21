# End-detection test corpus

Small, offline regression videos for Tailout's scene, solid screen, browser chrome,
still-picture and reference histogram detectors. The corpus includes positive
and negative cases, with independently specified event times in `manifest.json`.
There are 48 cases backed by 35 silent videos totaling 7.57 MiB.

## Run

FFmpeg/FFprobe must be on PATH. No downloads are made during tests.

```powershell
python tests/fixtures/detection/prepare.py --verify
cargo test --release --locked --test detection -- --ignored --nocapture --test-threads=1
```

Use a release build for performance measurements. Reports go to the ignored
`.cache/detection/` directory. Each row records the expected/actual marker, wall
clock time, frames analyzed in Rust, scouting/confirmation reads, per-detector
evaluations and shared feature builds. Wall clock timings are informational,
not CI assertions.

## Coverage and labels

| Cases | What they check |
| --- | --- |
| Black, white, blue | Exact onset, minimum down period, selected color and pixel-percentage thresholds |
| Flash/recovery, interrupted black | A short interruption cannot be merged into a sustained match |
| Natural film credits and noisy VHS | Dark credits at 90% versus 95%; real analog noise before a sustained black interval |
| Natural and controlled scene cuts | Actual camera cut, persistent scene change, return before the down period |
| Browser recording, letterboxing, cropped page | Actual browser chrome and a cropped web editor; reject black bars alone |
| Still pictures and two published spinner animations | Frozen pictures with small moving, stationary, off-center and differently sized icons |
| Still-picture margins | Small versus large animation, strict versus relaxed margins, slow drift, brief freezes and recovery |
| Shared coordination | All five detectors share scout reads; overlapping false candidates extend one dense scan without rereads |
| Reference match/reject | Same reference histogram versus unrelated footage |
| Additive detectors | Earliest qualifying detector; short signals from different detectors cannot be combined |
| Grace and search boundaries | Begin offsets, a fully protected interval, matches at the grace boundary and across sampling windows |
| Long negative and two separated endings | Work when nothing matches; the first sustained match wins over a later black tail |

A frozen frame occupies the protected first 120 seconds; the short test event
then occurs at its annotated time. This exercises the real grace period and
seeking while keeping the stored files small. Footage and animations are
transcoded to silent 384 × 216 H.264 at 30 fps. Browser recordings retain their
original letterboxing. Most event transitions are controlled edits using the
public assets below; they are not unedited recordings of player failures.
Solid-color screens and interrupted intervals are generated controls.

The natural camera cut was independently located with FFmpeg's scene filter
at 6.433 s into the excerpt and visually checked. Natural credits were checked
with RGB pixel counts at the analysis resolution, rather than using Tailout to
label its own answers. VHS reaches a sustained 95% at 125.933 s in the constructed
case, rather than at its first noisy dark frame. `expected: null` means no
qualifying event is expected.

A camera cut is evidence of a scene change, not proof that a recording ended.
Visible browser chrome is likewise a heuristic; a cropped webpage can be
recognizable from its editor controls.
Passing these cases does not establish accuracy on every player or recording.
Still-picture detection intentionally accepts static slides and other quiet images.
It replaces ring-shape recognition: a stationary icon is now a positive case too.
Histogram matching compares colors, so different pictures with similar colors can match.

## Search strategies and performance

The app uses one shared adaptive scouting pass in chronological, 16-second
windows. At the default two-second down period, scene and still-picture checks
run about twice per second; solid-color, fullscreen and reference checks run
about once per second. Each detector has its own schedule and history, but
FFmpeg supplies the fastest required frame stream once. Histograms and luminance
images are built lazily, at most once per shared frame. Shorter down periods
reduce the intervals; periods below 0.5 seconds use the exact scan.

All detectors' candidate ranges are padded for onset uncertainty and merged.
Confirmation checks every original frame with all enabled detectors, using
bounded four-second reads. Overlapping ranges extend the existing analyzer and
run state instead of rereading frames. Separate detectors cannot combine short
signals to satisfy the down period. Only confirmed evidence moves the marker.

**Still picture** compares RGB pixels to a fixed anchor picture. By default,
up to 2% of analysis pixels may change by more than 16 in any RGB channel.
This ignores small animations and minor compression noise while rejecting slow
drift that an adjacent-frame comparison could miss. Both tolerances are exposed
in the UI and saved in batch/video presets. Older loading-spinner preferences
migrate to this criterion. A short anchored sequence is required to propose a
candidate; the full down period is still confirmed at the original frame rate.
Black/white/color coverage and reference-histogram similarity remain independent
settings and already tolerate small visual differences.

`SearchMode::Exact` retains the full-frame oracle. The regression test compares
both strategies against the annotations and against each other within one frame.
It also checks progress and work counts; cancellation is tested separately.
A temporary, upscaled 720p benchmark exercises decoding at a larger resolution
without storing an HD copy in the repository. Its preparation time is excluded
from the measured search time.

Sampling reduces scaling, pixel analysis, transfers and process launches. It
usually cannot skip decoding interdependent compressed frames (see
[FFmpeg seeking](https://ffmpeg.org/ffmpeg.html#Main-options)); performance
therefore depends on codec, keyframe spacing, storage and hardware. Bounded
reads can also re-decode a keyframe prefix internally when seeking; the work
counters measure frames transferred for analysis, not all codec decode work. Sparse
search remains a heuristic: behavior near tolerance boundaries and motion that
aliases with sample times can escape scouting. Exact confirmation prevents
sampled gaps from being mistaken for a continuous match; it cannot recover a
candidate that sampling never notices.
See [RESULTS.md](RESULTS.md) for the measured results and the defects found.

## Sources and attribution

| Asset | Author and source | License | Changes |
| --- | --- | --- | --- |
| Old VHS tape ending black screen static | Glarph, [source recording](https://commons.wikimedia.org/wiki/File:Old_VHS_tape_ending_black_screen_static.webm) | [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/) | Short excerpt, scaled, muted and inserted after footage |
| Big Buck Bunny | © copyright Blender Foundation, [www.bigbuckbunny.org](https://www.bigbuckbunny.org/); [source file](https://commons.wikimedia.org/wiki/File:Big_Buck_Bunny_4K.webm) | [CC BY 3.0](https://creativecommons.org/licenses/by/3.0/) | Short excerpts, scaled, muted, held frames and controlled edits |
| Fullscreen and double column layout | Jaindeepali, [source recording](https://commons.wikimedia.org/wiki/File:Fullscreen_and_double_column_layout.ogv) | [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/) | Excerpt, scaled, muted and inserted after footage |
| CodeMirror brackets in discussion on Firefox | Nux, [source recording](https://commons.wikimedia.org/wiki/File:CodeMirror_brackets_in_discussion_on_Firefox.webm) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) | Excerpt, scaled and muted |
| YouTube loading symbol 3 (transparent) | Pixelmaniac pictures, [source animation](https://commons.wikimedia.org/wiki/File:YouTube_loading_symbol_3_(transparent).gif) | [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/) | Scaled, composited on gray, positioned and looped |
| Spinning wheel throbber | Jakub Steiner and Tango Desktop Project authors, [source animation](https://commons.wikimedia.org/wiki/File:Spinning_wheel_throbber.gif) | [CC BY-SA 2.5](https://creativecommons.org/licenses/by-sa/2.5/) | Scaled, composited, looped or frozen |

Derived videos containing the browser recording are distributed under
CC BY-SA 4.0; those containing the Tango animation under CC BY-SA 2.5.
Other Blender-derived video excerpts use CC BY 3.0. These asset licenses do not
change the license of the Rust test code.

## Rebuild

```powershell
python tests/fixtures/detection/prepare.py
```

Python's standard library and FFmpeg with libx264 are sufficient. Source downloads
are checksum-pinned and cached under `.cache/detection/sources/` (about 35 MB).
Each download is capped at 40 MiB. Use `--offline` to rebuild from the cache.
The complete checked-in corpus must stay below 8 MiB. Only the explicitly
allowlisted fixture filenames bypass the repository's private-media ignore rules.
Re-encoding with a different FFmpeg version can change file hashes; review the
manifest and run the entire suite after rebuilding.
