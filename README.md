# Tailout

**Video Trimmer and HEVC Batch Converter**

> Maurits Lam<br/>
> 2026

<p align="center">
  <img src="media/tailout.png" alt="Tailout with a test video loaded, showing the queue, video preview, trim markers and batch settings" height="400"/>
</p>

A Rust desktop application for trimming, shrinking and converting videos to HEVC,
using FFmpeg and FFprobe.

## Features

- **Trimming:** Set begin/end markers with a live frame preview.
- **HEVC encoding:** Use NVIDIA NVENC or CPU encoding with x265.
- **Resizing:** Shrink by pixel dimensions or percentage, with optional aspect-ratio preservation.
- **Batch processing:** Queue videos, share settings, and override individual videos. Save and load presets.
- **End detection:** Find scene changes, black/solid-color screens, fullscreen exits, loading circles, or a reference image's color histogram.

## Installation

Windows 10/11, 64-bit. Run the Windows installer, or build from source below.
Installation is per user and does not require administrator access.

Install [FFmpeg](https://ffmpeg.org/download.html) 6 or newer with `libx265`.
Both `ffmpeg.exe` and `ffprobe.exe` must be on `PATH` or beside `tailout.exe`;
they are not bundled. NVIDIA encoding additionally requires a compatible GPU and driver.

## Usage

1. Click **Add videos** or **Add folder**, then select a video in the queue.
2. Set **Begin** and **End** by entering times, dragging the markers, or clicking **Here** at the playhead.
3. Configure encoding and resize options in **Batch settings**. Changes in **Per-video settings** override that video's defaults; **Reset to batch settings** restores them.
4. Optionally enable **Detect end** and click **Find end** or **Find ends in queue**. Detection uses a two-minute grace period and a two-second down period by default. Review the result; manual end-marker changes override detection.
5. Choose the output mode and click **Process queue**. Right-click a video to process it individually.

**Replace originals** is the default and creates no backups. **Save to folder**
also deletes originals after successful export by default; uncheck **Delete originals
after successful export** to keep them.

Use **Left/Right** or the mouse wheel to seek; **I/O** set the begin/end markers.
The preview shows frames without audio playback.

## Building

Requires Rust 1.95 or newer. On Windows, install the MSVC C++ Build Tools and Windows SDK.

```powershell
cargo run --release
```

To create the Windows installer, install [Inno Setup](https://jrsoftware.org/isinfo.php)
6.3 or newer and run from PowerShell 7:

```powershell
./installer/build-installer.ps1
```

The installer is written to `dist/`.

## Support

Report bugs and request features through [GitHub Issues](https://github.com/mhjlam/Tailout/issues).
