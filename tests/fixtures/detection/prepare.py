#!/usr/bin/env python3
"""Rebuild the small, public detection corpus (Python 3 + FFmpeg; no pip packages)."""
import argparse
import hashlib
import json
import subprocess
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
HERE = Path(__file__).resolve().parent
CACHE = ROOT / ".cache" / "detection"
SOURCES = {
    "vhs.webm": (
        "https://upload.wikimedia.org/wikipedia/commons/9/93/Old_VHS_tape_ending_black_screen_static.webm",
        "af718316ef202b9e601d433ceb339916f0df5e275791b67a367a31186df9bb56"),
    "bunny.webm": (
        "https://upload.wikimedia.org/wikipedia/commons/transcoded/c/c0/Big_Buck_Bunny_4K.webm/Big_Buck_Bunny_4K.webm.240p.vp9.webm",
        "855a499daf249c529618f1a9a31b3bb4c474ddc4f2a119187304b5ea553c393b"),
    "firefox.webm": (
        "https://upload.wikimedia.org/wikipedia/commons/c/ce/CodeMirror_brackets_in_discussion_on_Firefox.webm",
        "53159c7f70c7381a089a5860706b2ecc065e9d450e33a27fa7f73daab39bc073"),
    "fullscreen.webm": (
        "https://upload.wikimedia.org/wikipedia/commons/transcoded/5/5a/Fullscreen_and_double_column_layout.ogv/Fullscreen_and_double_column_layout.ogv.240p.vp9.webm",
        "4e6b1c5564d7001a51a6b61a786831bd2c030a36a3e4a3afb7cede417b89d5c4"),
    "youtube.gif": (
        "https://upload.wikimedia.org/wikipedia/commons/a/ad/YouTube_loading_symbol_3_%28transparent%29.gif",
        "d545b9845df5f120e894f3f3207a147719584ed11a629c27bb29006aaa521c9a"),
    "tango.gif": (
        "https://upload.wikimedia.org/wikipedia/commons/d/d2/Spinning_wheel_throbber.gif",
        "d97315b32378218c2b638de86777da7b84839d2f31cf3dd7ee017b8878ef100b"),
}


def ffmpeg(*args):
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", *map(str, args)], check=True)


def fetch(offline):
    folder = CACHE / "sources"
    folder.mkdir(parents=True, exist_ok=True)
    for name, (url, digest) in SOURCES.items():
        path = folder / name
        if not path.exists():
            if offline:
                raise RuntimeError(f"Missing {path.name}; run without --offline to download sources")
            for attempt in range(3):
                try:
                    request = urllib.request.Request(url, headers={"User-Agent": "TailoutTestCorpus/1.0"})
                    with urllib.request.urlopen(request, timeout=60) as response:
                        data = response.read(40 * 1024 * 1024 + 1)
                    if len(data) > 40 * 1024 * 1024:
                        raise RuntimeError("Source exceeds 40 MiB download limit")
                    if hashlib.sha256(data).hexdigest() != digest:
                        raise RuntimeError(f"Source checksum changed: {name}")
                    path.write_bytes(data)
                    break
                except urllib.error.HTTPError as error:
                    if error.code not in (429, 503) or attempt == 2:
                        raise
                    time.sleep(min(30, int(error.headers.get("Retry-After", "10"))))
            time.sleep(2)
        if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            raise RuntimeError(f"Source checksum mismatch: {name}")
    return folder


def encode(output, inputs, graph):
    ffmpeg(*inputs, "-filter_complex", graph, "-map", "[v]", "-an", "-sn", "-dn",
           "-map_metadata", "-1", "-c:v", "libx264", "-preset", "veryfast", "-crf", "26",
           "-pix_fmt", "yuv420p", "-g", "1800", "-threads", "2", "-movflags", "+faststart", output)


def build(offline):
    source = fetch(offline)
    CACHE.mkdir(parents=True, exist_ok=True)
    assets = CACHE / "parts"
    assets.mkdir(exist_ok=True)
    # Clips keep actual public footage; uniform size/rate bounds corpus size.
    for name, raw, begin, duration in [
        ("motion", "bunny.webm", 28, 8), ("natural-scene", "bunny.webm", 21, 10),
        ("natural-black", "bunny.webm", 606, 9), ("browser", "fullscreen.webm", 0, 6),
        ("web-text", "firefox.webm", 0, 6), ("vhs", "vhs.webm", 0, 6),
    ]:
        encode(assets / f"{name}.mp4", ["-ss", begin, "-t", duration, "-i", source / raw],
               "[0:v]setpts=PTS-STARTPTS,scale=384:216:flags=area,fps=30,setsar=1,format=yuv420p[v]")
    for name in ["motion", "natural-scene", "natural-black"]:
        ffmpeg("-i", assets / f"{name}.mp4", "-frames:v", "1", assets / f"{name}.png")

    cases = []
    encoded = {}
    def case(name, detector, segments, expected, *, base="motion", **options):
        inputs, filters = [], []
        # A real frame held for the protected two minutes keeps each complete
        # video tiny, while exercising the real grace period and FFmpeg seeks.
        for index, (kind, duration) in enumerate([(f"still:{base}", 120), *segments]):
            if kind.startswith("still:"):
                label = kind.split(":", 1)[1]
                inputs += ["-loop", "1", "-framerate", "30", "-t", duration, "-i", assets / f"{label}.png"]
                transform = ""
            elif kind.startswith("color:"):
                inputs += ["-f", "lavfi", "-i", f"color=c={kind[6:]}:s=384x216:r=30:d={duration}"]
                transform = ""
            elif kind == "alternating-solid":
                inputs += ["-f", "lavfi", "-i", f"color=c=black:s=384x216:r=30:d={duration}"]
                transform = "negate=enable='gte(mod(t,2),1)',"
            elif kind == "slow-drift":
                inputs += ["-f", "lavfi", "-i", f"color=c=gray:s=384x216:r=30:d={duration}"]
                transform = "geq=lum='40+N*0.8':cb=128:cr=128,"
            elif kind.startswith("flicker:"):
                width, height = kind.split(":")[1:]
                inputs += ["-loop", "1", "-framerate", "30", "-t", duration, "-i", assets / "motion.png"]
                transform = f"drawbox=x=30:y=40:w={width}:h={height}:c=white:t=fill:enable='lt(mod(t,0.7),0.35)',"
            elif kind == "white80":
                inputs += ["-f", "lavfi", "-i", f"color=c=white:s=384x216:r=30:d={duration},drawbox=x=0:y=0:w=77:h=216:c=black:t=fill"]
                transform = ""
            elif kind.startswith("spinner:"):
                _, style, size, x, y = kind.split(":")
                inputs += ["-stream_loop", "-1", "-t", duration, "-i", source / f"{style}.gif"]
                filters += [f"color=c=0x303030:s=384x216:r=30:d={duration}[bg{index}]",
                            f"[{index}:v]scale={size}:{size},format=rgba[icon{index}]",
                            f"[bg{index}][icon{index}]overlay=x={x}:y={y}:shortest=1[spin{index}]"]
                filters += [f"[spin{index}]trim=duration={duration},setpts=PTS-STARTPTS,fps=30,format=yuv420p,setsar=1[s{index}]"]
                continue
            elif kind == "static-spinner":
                # First create a real, composited spinner still, then freeze it.
                still = assets / "static-spinner.png"
                if not still.exists():
                    ffmpeg("-f", "lavfi", "-i", "color=c=0x303030:s=384x216", "-i", source / "tango.gif",
                           "-filter_complex", "[0:v][1:v]overlay=(W-w)/2:(H-h)/2", "-frames:v", "1", still)
                inputs += ["-loop", "1", "-framerate", "30", "-t", duration, "-i", still]
                transform = ""
            else:
                raw = "motion" if kind == "letterbox" else kind
                inputs += ["-stream_loop", "-1", "-t", duration, "-i", assets / f"{raw}.mp4"]
                transform = "drawbox=x=0:y=0:w=iw:h=24:c=black:t=fill,drawbox=x=0:y=192:w=iw:h=24:c=black:t=fill," if kind == "letterbox" else ""
            filters += [f"[{index}:v]trim=duration={duration},setpts=PTS-STARTPTS,scale=384:216:flags=area,{transform}fps=30,format=yuv420p,setsar=1[s{index}]"]
        filters += ["".join(f"[s{i}]" for i in range(len(segments) + 1)) + f"concat=n={len(segments)+1}:v=1:a=0[v]"]
        key = (base, tuple(segments))
        path = encoded.get(key)
        if path is None:
            path = HERE / f"{name}.mp4"
            encode(path, inputs, ";".join(filters))
            encoded[key] = path
        cases.append(dict(id=name, detector=detector, file=path.name, expected=expected,
                          tolerance=options.pop("tolerance", 0.15), **options,
                          bytes=path.stat().st_size, sha256=hashlib.sha256(path.read_bytes()).hexdigest()))
        print(name, path.stat().st_size, flush=True)

    motion = lambda seconds: ("motion", seconds)
    still = lambda seconds: ("still:motion", seconds)
    black = lambda seconds: ("color:black", seconds)
    case("black-tail", "black", [motion(4), black(4)], 124)
    case("black-flash-recovery", "black", [motion(2), black(0.5), motion(3), black(4)], 125.5)
    case("black-short-only", "black", [motion(3), black(1.5), motion(3)], None)
    case("black-exact-hold", "black", [motion(3), black(2), motion(3)], 123)
    case("black-interrupted", "black", [motion(3), black(1.3), motion(0.1), black(1.3), motion(2)], None)
    case("white-tail", "white", [motion(4), ("color:white", 4)], 124)
    case("white-percentage", "white", [motion(4), ("white80", 4)], 124, pixel_fraction=0.75)
    case("white-percentage-reject", "white", [motion(4), ("white80", 4)], None, pixel_fraction=0.85)
    case("blue-tail", "color", [motion(4), ("color:blue", 4)], 124, color=[0, 0, 255])
    case("wrong-color", "color", [motion(4), ("color:red", 4)], None, color=[0, 0, 255])
    case("natural-black", "black", [("natural-black", 9)], None, base="natural-black")
    cases.append({**cases[-1], "id": "natural-black-90", "expected": 121.066667, "pixel_fraction": 0.90})
    case("natural-scene", "scene", [("natural-scene", 10)], 126.4, base="natural-scene", tolerance=0.3)
    case("scene-sustained", "scene", [still(4), ("color:white", 4)], 124)
    case("scene-short-return", "scene", [still(3), ("color:white", 0.5), still(4)], None)
    case("browser-chrome", "fullscreen", [motion(4), ("browser", 4)], 124, tolerance=0.2)
    case("letterboxing", "fullscreen", [("letterbox", 8)], None)
    case("cropped-web-text", "fullscreen", [("web-text", 8)], 120)
    case("youtube-spinner", "still", [motion(4), ("spinner:youtube:36:(W-w)/2:(H-h)/2", 4)], 124, tolerance=0.3)
    case("off-center-spinner", "still", [motion(4), ("spinner:youtube:36:72:132", 4)], 124, tolerance=0.3)
    case("large-spinner", "still", [motion(4), ("spinner:youtube:56:(W-w)/2:(H-h)/2", 4)], 124, tolerance=0.3)
    case("tango-spinner", "still", [motion(4), ("spinner:tango:32:(W-w)/2:(H-h)/2", 4)], 124, tolerance=0.3)
    case("static-spinner", "still", [motion(4), ("static-spinner", 4)], 124)
    case("moving-picture", "still", [motion(8)], None)
    case("reference-match", "reference", [motion(4), ("browser", 4)], 124, reference_time=125)
    case("reference-reject", "reference", [motion(8)], None, reference_file="browser-chrome.mp4", reference_time=125)
    case("additive-earliest", "additive", [motion(3), black(2.5), ("browser", 3)], 123)
    case("additive-short-signals", "additive", [motion(3), black(1.2), ("browser", 1.2), motion(2)], None)
    case("range-end", "black", [motion(4), black(4)], None, search_end=125)
    case("long-no-match", "black", [motion(120)], None, benchmark=True)
    by_id = {item["id"]: item for item in cases}
    for original, name, options in [
        ("black-short-only", "short-down-period", {"hold_seconds": 0.1, "expected": 123}),
        ("black-tail", "begin-offset", {"search_start": 4, "expected": 124}),
        ("black-tail", "protected-whole-range", {"search_start": 10, "expected": None}),
        ("black-tail", "grace-boundary", {"search_start": 5, "expected": 125}),
        ("long-no-match", "long-default-detectors", {"detector": "defaults"}),
    ]:
        cases.append({**by_id[original], "id": name, **options})
    case("window-boundary", "black", [motion(15.4), black(4)], 135.4)
    case("noisy-vhs-black", "black", [motion(4), ("vhs", 4)], 125.933333)
    case("first-before-final-black", "black", [motion(20), black(2.5), motion(20), black(4)], 140)
    case("still-tail", "still", [motion(4), still(4)], 124)
    case("still-short-recovery", "still", [motion(3), still(1.5), motion(3)], None)
    case("still-interrupted", "still", [motion(3), still(1.3), ("color:white", 0.1), still(1.3), motion(2)], None)
    case("still-slow-drift", "still", [("slow-drift", 8)], None)
    case("still-small-animation", "still", [motion(4), ("flicker:19:16", 4)], 124)
    case("still-large-animation", "still", [motion(4), ("flicker:96:54", 4)], None)
    by_id = {item["id"]: item for item in cases}
    cases.append({**by_id["youtube-spinner"], "id": "still-strict-margin", "expected": None, "still_changed_fraction": 0.0})
    cases.append({**by_id["still-large-animation"], "id": "still-relaxed-margin", "expected": 124, "still_changed_fraction": 0.1})
    case("overlapping-candidates", "black", [motion(4), ("alternating-solid", 40)], None)
    cases.append({**by_id["long-no-match"], "id": "long-all-detectors", "detector": "all", "reference_file": "blue-tail.mp4", "reference_time": 125})
    manifest = dict(version=2, description="See README.md for independent labels and provenance.", cases=cases)
    (HERE / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    total = sum({c["file"]: c["bytes"] for c in cases}.values())
    if total > 8 * 1024 * 1024:
        raise RuntimeError(f"Corpus exceeded 8 MiB budget: {total}")
    print(f"{len(cases)} cases, {total:,} bytes", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--offline", action="store_true", help="Use previously downloaded, checksum-verified sources")
    parser.add_argument("--verify", action="store_true", help="Check bundled fixtures without downloading or rebuilding")
    args = parser.parse_args()
    if args.verify:
        manifest = json.loads((HERE / "manifest.json").read_text())
        for case in manifest["cases"]:
            data = (HERE / case["file"]).read_bytes()
            assert len(data) == case["bytes"], case["id"]
            assert hashlib.sha256(data).hexdigest() == case["sha256"], case["id"]
        total = sum({item["file"]: item["bytes"] for item in manifest["cases"]}.values())
        assert total <= 8 * 1024 * 1024, "Corpus exceeds 8 MiB"
        print(f"Verified {len(manifest['cases'])} public test cases ({total:,} bytes)")
    else:
        build(args.offline)


if __name__ == "__main__":
    main()
