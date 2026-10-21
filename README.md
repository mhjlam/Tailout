# Tailout

A small Rust desktop application for trimming, shrinking and converting videos
to HEVC. FFmpeg and FFprobe do the media work.

## Windows installer

Run `Tailout-1.0.0-windows-x64-setup.exe` from `dist/` to install for your
Windows account. No administrator access is needed. Setup creates a Start menu
shortcut and offers an optional desktop shortcut. Windows 10/11 on x64, or
Windows 11 on ARM with x64 emulation, is required.

FFmpeg and FFprobe are separate prerequisites; they are not bundled. Install
FFmpeg 6 or newer with `libx265` and put both executables on `PATH`, or beside
the installed `tailout.exe`. Windows builds are available through
[FFmpeg's download page](https://ffmpeg.org/download.html).

The default install folder is `%LOCALAPPDATA%\Programs\Tailout`. Close Tailout
before running a newer installer. Upgrades reuse the same installation;
Windows Settings → Apps can uninstall it. Upgrades and uninstall preserve
preferences in `%APPDATA%\Tailout`, user presets, and video files.
The installer is unsigned, so Windows may show a publisher/reputation warning.

To build the installer, install [Inno Setup 6.3 or newer](https://jrsoftware.org/isinfo.php)
and run this from PowerShell 7:

```powershell
./installer/build-installer.ps1
# Use cached Rust dependencies only:
./installer/build-installer.ps1 -Offline
```

The script builds the release executable, reads its version from `Cargo.toml`,
checks the executable's x64 architecture, and produces the setup executable and
its SHA-256 checksum in `dist/`. Pass `-IsccPath <path>` if ISCC is not on PATH,
or `-SkipBuild` to package an already-built release executable. No backend
executables are downloaded or added to PATH by the installer.

`./installer/Test-Installer.ps1` verifies installation into an isolated `.cache/`
folder, Start menu shortcuts, repeat installation and uninstall. It checks that
user-created files and existing preferences survive, and leaves logs for review.
The check refuses to run if Tailout is already installed or has a Start menu
shortcut, so it cannot alter an existing installation.

## Build and run

Requirements: Rust 1.95 or newer, and FFmpeg/FFprobe 6 or newer with `libx265`.
Windows is the tested desktop platform. NVIDIA encoding additionally requires a
compatible GPU and driver. FFmpeg executables can be on `PATH` or beside
`tailout.exe`; they are not bundled.

```powershell
cargo run --release
```

Or build once and launch `target/release/tailout.exe`:

```powershell
cargo build --release
```

You can also pass video paths to the executable or drag videos onto its window.

The Windows executable and application window use the reel-and-scissors icon in
[`assets/tailout.ico`](assets/tailout.ico). It includes 16, 20, 24, 32, 40, 48,
64, 96, 128 and 256 px variants, plus individual PNGs and a 512 px PNG. Artwork,
generation prompts and rebuild instructions are in [`assets/README.md`](assets/README.md).
Windows builds need a resource compiler: the MSVC Windows SDK is discovered
automatically, with `llvm-rc` as a fallback; GNU builds use `windres`.


## Use

1. **Add videos** or **Add folder**. Folder scanning optionally includes
   subdirectories. Select a queue item to inspect it.
2. Scrub the timeline; frames update while dragging, without waiting for release.
   Drag the green begin or red end line to move a trim marker. Enter **Begin**
   and **End** as seconds, `MM:SS.mmm`, or `HH:MM:SS.mmm`; **Here** uses the
   playhead. An empty End means this video's end. The preview shows frames,
   without audio playback.
3. In **Batch settings**, choose the encoder and quality. Lower numbers mean higher quality and usually
   larger files. Quality 21 is the default; the supported range is 15–30.
   **HEVC (x265)** uses CRF; NVIDIA uses NVENC CQ. These scales are not equivalent.
   Automatic tests NVENC first and falls back to software if unavailable.
4. Optionally enable **Resize**. Choose pixel bounds, height only, or percentage.
   **Keep original aspect ratio** calculates the other dimension as appropriate.
   With it off, height-only changes height while leaving width unchanged;
   percentage mode provides independent width and height percentages. The
   predicted pixel dimensions are displayed. Every mode is shrink-only, including
   for rotated portrait videos; resize dimensions are rounded down to even pixels.
5. To customize one video, select it and open the **Per-video settings** tab. It repeats
   the processing controls and initially shows the inherited batch values. Changing
   a value automatically creates an override for the active video. **Reset to
   batch settings** removes that override and follows the latest batch values again.
6. In **Batch settings**, **Replace originals** is selected by default. Validated
   conversions replace the source files without creating backups. Choose **Save
   to folder** and select an export folder, which may be the same folder as the
   source. **Delete originals after successful export** is enabled by default;
   uncheck it to keep both files. Exports use `filename.hevc.ext`; existing names get
   numbered suffixes, so no existing file is overwritten. Each mode's settings
   remain visible and are grayed out when inactive. Switching modes retains the
   selected export folder and deletion preference. Each original is deleted only
   after its output passes validation and quality checks and is saved successfully.
   Skipped, failed or cancelled exports retain their originals. A deletion failure
   keeps the saved output and reports that the original was retained.
   Click **Process queue** to export sequentially, or right-click a video and
   choose **Process this video** to export only that item with its own settings.
   Errors on one file do not stop the remaining queue. Cancel stops the active
   operation and remaining items.

### Settings scopes and saved preferences

- The **Batch settings** tab sets encoding, quality, resize, HEVC re-encoding
  and quality checks for every video without an override. Edits take
  effect automatically for existing and newly added videos. These defaults can be
  configured before adding files. The destination, **Force overwrite**, and
  **Delete originals after successful export** also apply to the
  whole queue. Destination and deletion preferences are saved between launches
  and included in batch presets.
- The **Per-video settings** tab repeats the processing controls for the active
  queue item. Changing one automatically creates a local override initialized
  from the current batch values. Switching tabs or selecting a different video
  alone does not create an override. **Using batch settings** or **Custom settings**
  shows its status. Later batch changes leave the entire processing override unchanged.
  **Reset to batch settings** removes it and resumes inheritance immediately,
  including future batch changes. Reset does not change other videos.
- **Detect end** and all detector options now live in the right panel. Batch
  defaults apply to existing and newly added videos without a local detection
  override. **Find ends in queue** sits with those controls and shows how many
  queued videos have detection enabled; it processes them sequentially using
  each video's effective settings. Queue selection does not limit that search.
- The per-video tab repeats the detector controls. Editing them creates a local
  detection override, independently of any encoding override. **Reset to batch
  settings** restores both groups to the latest batch defaults without changing
  begin/end markers. Manual End changes disable detection locally, so later batch
  edits cannot undo a manual end marker. Re-enable Detect end or Reset to resume.
- Begin/end markers, playhead and search results always remain individual. Batch
  changes never copy a found end marker between videos. Changing settings marks
  previous matches stale; another explicit search is required.
- Program preferences sit beside the actions they affect: **Include subfolders**
  next to **Add folder**, **Seek step** at the bottom-right, and the preview dropdown
  at the top-right of the video. **Clear queue** sits above the video list.
  The queue sidebar has a fixed width of 320 pixels; the settings panel stays at
  its minimum width of 315 pixels. Neither divider can be dragged. Each compact card has a
  single filename line and a second line with the full duration and any trim
  range plus resulting length. Hover to read truncated names or timing details.
  **Sort** above the queue defaults to **Alphabetical**, comparing filenames
  without case sensitivity. **Added order**, **Shortest first** and **Longest
  first** are also available. Duration sorts use the full source duration rather
  than trimmed length; unknown durations stay last and move into place as they
  load. Equal names or durations keep their added order. Saved sort choices are
  retained between launches. Sorting preserves the selected video, its markers and settings;
  processing applies the active sort when it starts and captures that order for
  the entire run. Later duration updates do not reshuffle in-flight exports or
  queue-wide detection. The sort preference is saved automatically.
  Sorting is disabled during export or a queue-wide end-marker search.
  Click a video to select it alone; **Ctrl+Click** toggles individual selections,
  **Shift+Click** selects a range in the displayed order, and **Ctrl+Shift+Click**
  adds a range to the selection. Selected cards are highlighted; the active
  video's filename is green and its preview, markers and per-video controls stay
  individual. Sorting preserves the selection. **Process queue** still processes
  the whole queue, including unselected videos. **Process this video** in the
  right-click menu processes only the clicked video, even in a group selection.
  It uses that video's markers and effective processing settings plus the batch
  output options, including any enabled source deletion. Other queue results are
  preserved, and progress and the final count cover only the requested item.
  Single-video processing is disabled during export, folder scanning, deletion
  or an end-marker search; it does not start an auto-end search itself.
  Right-click a video for **Open containing folder**, **Remove from queue** or
  **Delete from disk…**. Opening the folder always targets the right-clicked video,
  including in a group selection, and stays available while processing. For a
  group selection these become **Remove selected (N)** and **Delete N files from
  disk…**. Right-clicking a selected video preserves the group; right-clicking an
  unselected video selects it alone. Queue removal leaves files on disk.
  Videos with an error also offer **Copy error message**, which copies the full
  diagnostic text for the right-clicked video, including multiline backend or
  end-detection errors. It remains available while other videos are processing.
  Error labels and hover text show only a short primary cause, without backend
  log prefixes, follow-on failures or stack traces. Full diagnostics are retained
  exclusively for **Copy error message**.
  Disk deletion requires confirmation showing every full source path in a
  scrollable list; **Cancel**, Escape or clicking outside leaves files untouched.
  Confirmed deletion removes only those source files and their queue items,
  preserving separate exported files. Any file that fails to delete remains
  queued with its error. Remove and delete actions are disabled during export, queue-wide end-marker
  search or folder scanning. A deletion dialog shows which file is being deleted.
  The window opens centered on the main monitor.

Every launch starts with an empty queue, unless videos are explicitly supplied on
the command line. Batch processing defaults, destination settings and program
preferences, including shared Detect end defaults and its reference histogram,
are saved automatically. Per-video overrides and markers belong to the current
queue; save a video preset to reuse them later.
On Windows the preferences file is `%APPDATA%\Tailout\session.json`; on Linux it
is `$XDG_CONFIG_HOME/tailout/session.json` (or `~/.config/tailout/session.json`).
Removing a video or clearing the queue discards that video's edits and override,
while batch defaults are retained. `TAILOUT_STATE_PATH` selects an alternative
preferences file, useful for isolated testing.

**Save preset / Load preset** follows the active settings tab. The buttons stay
at the bottom of the panel while its options scroll.

- **Batch settings:** save/load encoder, quality, every resize option, HEVC
  re-encoding, comparison/minimum SSIM, Detect end enablement, all detector
  thresholds/flags, grace/down periods, reference histogram and label, output
  folder, replacement mode, Force overwrite and source-deletion preference.
  Existing local overrides are kept. Per-video markers are never copied.
- **Per-video settings:** save/load effective encoding and detector settings,
  including inherited values and reference histogram, plus begin/end markers and
  playhead. Loaded settings become local overrides. Batch output options stay
  batch-wide and are not changed by loading into the per-video tab.
- Loading across tabs imports both processing and detection configuration. A
  video preset loaded into Batch settings leaves the batch destination unchanged;
  a batch preset loaded into Per-video settings keeps that video's markers.

New presets use version 5. Earlier batch/video presets remain supported. Running
jobs, transient search feedback and search ceilings are not restored; loading
never starts analysis. Program preferences such as Seek step and preview view
are saved separately and do not belong to either preset scope.

### Seeking and preview views

Scroll over the image or timeline, or press **Left/Right**, to seek by the program
seek step (default one second). **Shift** multiplies the step by ten; **Alt** uses
one tenth. **Home/End** jump to the first/last seekable frame. **I/O** place the
begin/end markers at the playhead. Text fields retain normal text-editing keys.

The preview defaults to **Fit**, showing the whole image while preserving its
display aspect ratio. Hover at the top-right of the video to reveal the dropdown:
choose **Fit**, **Original**, or **Stretch**. The dropdown stays translucent
when idle and becomes fully visible on hover or while its menu is open. Drag to pan an oversized
Original preview; choosing a mode centers it again. Stretch may distort the
preview; it never changes export settings. Source dimensions and
display aspect ratio are shown above the image. Preview decoding retains the
frame's shape and uses a proxy bounded by 1920×1080; Original refers to the
source's display extent, not extra detail beyond that proxy. The preview decoder
coalesces queued seeks and shows completed frames continuously while dragging.

Outputs retain the input container (`.mp4`, `.mkv`, or `.mov`) and are named
`movie.hevc.mp4`, for example. Name collisions create numbered siblings; existing
files are never overwritten in output-folder mode. Folder inputs preserve
relative subdirectories. Separate file selections use a flat output directory.
**Replace originals** is the default. **Save to folder** deletes sources after
successful export by default; uncheck **Delete originals after successful export**
to retain them. After deleting a source, its queue item points to the saved output;
its markers reset to the full exported video so source offsets are not applied again.

Begin is an absolute offset within that video. End beyond the video's duration
is clamped to its duration. Begin beyond its end produces an error for that item.
Processing the queue again starts every item again with its current settings.

The export bar covers **one video's complete job, from 0 to 100%**, and resets
only for the next video. The counter beside it shows the video's position in the
queue. Measured FFmpeg progress is mapped across encoding and post-encode checks:
encoding uses 0–90%, validation 90–99%, and saving completes the last percent.
With quality comparison enabled, encoding uses 0–85%, validation 85–92%, and
comparison 92–99%. These stage weights are not estimates of their elapsed-time
shares. The optional backend auto-end pass reserves the first 5% when used.

The footer shows the actual encoder (**NVIDIA HEVC** or **HEVC (x265)**), elapsed
file time and FFmpeg's reported speed. Automatic can fall back to software, which
can be much slower on large videos. Hover the timing line for
processed frames, encoded time, output bytes and the age of the last FFmpeg report.
Reading input/output, checking the encoder, validation, comparison and saving
have separate status labels, so a post-encoding check is visible too.

After 30 seconds without increasing encoded time, frames or output bytes, the
footer shows **No measurable progress**. Startup/buffering or container finalization
can also cause this notice; it does not prove the process has hung. **Cancel
export** remains available. The UI timer and spinner indicate that Tailout is
responsive; only the measured counters indicate backend advancement.

## Automatic end marker

In the right panel, enable **Detect end** and select one or more detectors.
In **Batch settings**, click **Find ends in queue** to analyze every enabled
video with its effective settings. In **Per-video settings**, **Find end**
searches only the selected video. Search moves forward and selects the **earliest qualifying event after
Begin + Grace period**. Grace defaults to **2 minutes** and cannot be set lower.
A video shorter than the protected interval plus the Down period has no match.
Increasing Grace protects a longer part of the recording.

The detectors are additive: **any one** can supply the end marker. The first four
are enabled by default; the reference histogram is optional.

| Detector | What it checks | Controls |
| --- | --- | --- |
| Scene change | Abrupt change in adjacent-frame color histograms which stays different from the preceding scene | Change threshold, default 55% |
| Black / solid color | A predominantly black, white or chosen-color image | Pixels, default 95%; black Level 16, white Level 240; custom RGB tolerance |
| Exit full screen | Browser-like toolbar bands containing short text edges above the picture | Enable/disable |
| Spinning load circle | A changing partial ring near likely player centers, with a still surrounding picture | Enable/disable |
| Reference histogram | Similarity to the color distribution of a selected image | Similarity, default 92%; Current frame or Image… |

**Down period** defaults to **2 seconds**. A single detector must remain true for
that duration; the marker is placed at the beginning of that event. Brief
interruptions reset that detector's timer. Different short signals cannot be
combined to satisfy the period. Later recovery does not invalidate an earlier
qualifying event, and the event does not need to continue to the end of the file.
For example, after the two-minute grace, three seconds of black followed by normal
video selects the first black frame; a one-second black flash is ignored.

A match moves End and the playhead and displays **Match**. No match leaves End
unchanged. Changing detection settings requires another explicit **Find end**;
export always uses the visible markers and never silently searches again. Use
**Find ends in queue** to analyze queued videos with their own settings.

**Here** beside End, the **O** key, editing End, or dragging its red marker discards
the automatic result, unchecks Detect end, cancels an active per-video search and
ignores any late result. This also works when the manual value equals the found
value. Detection settings remain available if Detect end is enabled again. Begin's
Here button only changes Begin. A manual End becomes the ceiling for a later
explicit search; repeating searches retains that ceiling rather than using the
last automatic result as a new limit.

During search, progress and **Cancel search** appear with the detector settings.
Single-video searches also show elapsed time.
Progress measures the forward coverage after Grace; it is not an ETA. Cancellation
stops the active FFmpeg process and keeps the marker. During a batch search,
Cancel stops the current and waiting searches; completed matches are kept.

### Detection behavior and limits

FFmpeg decodes low-resolution **192×108 RGB** frames in bounded four-second
windows, preserving source presentation timestamps. The analyzer and the five
independent confirmation timers retain state across windows. A short warm-up
before the grace boundary provides scene-change context without allowing a cut
inside the grace period. Every decoded frame after Grace is analyzed, so a
reverse/coarse seek cannot skip an earlier qualifying event. The scan stops as
soon as the earliest event is confirmed. This requires decoding the intervening
video; longer recordings can take longer to search. Memory and backend capture
are bounded, and cancellation remains available.

These are configurable visual heuristics, not recognition of a recording's
meaning. A normal scene cut which stays different can satisfy Scene change.
Browser chrome varies, and unusual toolbars, embedded players, spinner positions,
spinner sizes or animation styles may not be recognized. Letterboxing alone is
not considered a fullscreen exit; a static ring alone is not a spinner. Review
the found marker and disable a detector that does not suit a recording.

Scene and reference matching use normalized 8×8×8 RGB histograms and the
Bhattacharyya coefficient. Histograms compare color distributions, not objects
or image layout; unrelated pictures with similar colors can match. Presets store
the reference histogram, so the original reference file is not needed later.

## Options

- **Re-encode HEVC:** enabled by default for new videos. Convert already-HEVC
  video again. When unchecked, existing HEVC is skipped unless trimming or
  resizing requires encoding. Re-encoding is lossy.
- **Force overwrite:** enabled for **Replace originals**. Allow replacement even
  if the converted file is the same size or larger; validation and quality checks
  still apply. When unchecked, a replacement that does not reduce file size is
  discarded and the source is retained. **Save to folder** exports are always saved
  regardless of size and never overwrite existing files, even in the source folder.
- **Compare quality:** compute SSIM and PSNR for every ordinary video stream.
  Comparison failures prevent publication. **Minimum SSIM** additionally rejects
  output below that score. For a resized output, the reference is resized too;
  these metrics do not measure lost resolution or guarantee perceptual quality.
- **Replace originals / Save to folder:** replacement is the default and writes
  validated results back to the source locations without creating backups.
  Save to folder creates separate `filename.hevc.ext` exports in the selected
  folder, including when it is the source folder. Filename collisions receive
  numbered suffixes. Unsupported atomic-replacement filesystems fail without a
  delete-then-move fallback.
- **Delete originals after successful export:** enabled by default for Save to
  folder; disabled under Replace originals. Uncheck it to retain the sources.
  Deletion runs separately for each video, only after validation, any requested
  quality comparison, and successful output publication. Skipped or unsuccessful
  exports do not delete sources. If source cleanup fails or is cancelled after
  publication, the output stays saved and a warning reports that the source was
  retained; cleanup is never forced.
- **Save preset / Load preset:** save or load JSON for the active settings tab.
  Batch presets contain all shared processing, detection and output settings;
  video presets contain effective processing/detection settings, markers and
  playhead. Cross-scope loading imports processing and detection configuration
  while preserving destination or markers as appropriate.

## Media behavior and safeguards

All ordinary video streams are encoded to HEVC. Audio, subtitles, cover art and
attachments are retained, with chapters, metadata and track dispositions.
Trimming re-encodes every audio stream to AAC at 192 kbit/s for accurate audio
boundaries; untrimmed audio is copied. Subtitle packet timestamps/durations are
clipped to the selected interval, and chapters are clipped and rebased. Subtitle
payload behavior still depends on the codec and player, particularly for bitmap
subtitles. Unsupported container/stream combinations fail instead of silently
dropping a track.

Variable frame timing and rotation metadata are retained. There is no forced
8-bit conversion: output losing bit depth is rejected. Recognized HDR/Dolby Vision
and alpha video are refused because this application does not implement their
required preservation workflows. Interlacing is not automatically removed.

Every output is created in a unique temporary file, probed for expected streams,
codecs, dispositions, chapters, bit depth, dimensions and duration, and fully
decoded before publication. The source modification time is preserved. Backend
arguments are passed directly, so brackets, spaces, Unicode and `$()` in names
remain literal. Disk space is required for the temporary output.

The UI remains responsive during probing, analysis, encoding and validation.
Closing or cancelling terminates backend processes; Windows Job Objects also
cover forced host termination. A forcibly terminated application can leave a
`.tailout-*` temporary file, ignored by folder discovery. Remove these only after
confirming no conversion is active. Folder discovery skips generated `*.hevc.*`
outputs and symbolic links; generated videos may still be selected explicitly.

## Development and verification

The UI uses official egui/eframe/emath 0.37 releases from crates.io. The
numeric-drag crash previously encountered with Seek step is fixed upstream;
no vendored library or Cargo patch override is required. Regression tests cover
decimal-carry rounding and dragging Seek step across decimal boundaries.
Clipboard support is explicitly enabled for queue error-message copying.

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
# Generated-media integration tests require FFmpeg/FFprobe:
cargo test --test media -- --ignored --test-threads=1
```

Unit tests cover time parsing, inherited processing settings, per-video overrides,
presets/session state, explicit end-marker
searches, resize planning, preview geometry, live scrubbing, draggable markers,
keyboard/wheel seeking, additive visual detectors, grace periods, earliest-event
selection, manual overrides, independent confirmation timers and cancellation. Integration tests generate
disposable videos and exercise trimming, sequential export safety, collision
handling, replacement without backups, stream/chapter preservation, rotation, 10-bit SDR, rejection
paths, resize modes, solid-color/reference/scene detection, browser and spinner
fixtures, preview dimensions and cancellation.
Encoder selection is checked against an independent HD NVENC encode: on compatible
hardware the test exports with NVENC; otherwise it checks software fallback.

The PowerShell converter remains separately usable:

```powershell
cd powershell
pwsh -NoProfile -File ./tests/Invoke-Tests.ps1
```

Backend details: [FFmpeg](https://ffmpeg.org/ffmpeg.html),
[filters](https://ffmpeg.org/ffmpeg-filters.html),
[packet timestamps](https://ffmpeg.org/ffmpeg-bitstream-filters.html#setts).
