"""Build Tailout's PNG sizes and multi-image Windows ICO from the master artwork."""
from pathlib import Path
import struct
import subprocess

ASSETS = Path(__file__).resolve().parent
SIZES = (16, 20, 24, 32, 40, 48, 64, 96, 128, 256)


def main():
    for size in (*SIZES, 512):
        subprocess.run([
            "ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
            "-i", str(ASSETS / "tailout-master.png"),
            "-vf", f"scale={size}:{size}:flags=lanczos,format=rgba",
            "-frames:v", "1", str(ASSETS / f"tailout-{size}.png"),
        ], check=True)
    frames = [(ASSETS / f"tailout-{size}.png").read_bytes() for size in SIZES]
    offset = 6 + 16 * len(frames)
    entries = []
    for size, frame in zip(SIZES, frames):
        assert frame[:8] == b"\x89PNG\r\n\x1a\n"
        assert struct.unpack(">II", frame[16:24]) == (size, size)
        # ICO encodes 256 px as zero; the actual image dimensions are in PNG.
        edge = size if size < 256 else 0
        entries.append(struct.pack("<BBBBHHII", edge, edge, 0, 0, 1, 32, len(frame), offset))
        offset += len(frame)
    (ASSETS / "tailout.ico").write_bytes(
        struct.pack("<HHH", 0, 1, len(frames)) + b"".join(entries) + b"".join(frames)
    )
    print("Created tailout.ico:", ", ".join(str(size) for size in SIZES), "px")


if __name__ == "__main__":
    main()
