"""
check_video.py — CI video validation for gamedata-recorder

Checks:
  1. Not black screen (mean brightness > threshold)
  2. FPS >= 27
  3. Duration >= minimum seconds

Usage:
  python check_video.py <video_file> [--min-brightness 10] [--min-fps 27] [--min-duration 3]

Exit codes:
  0 = all checks passed
  1 = one or more checks failed
  2 = ffmpeg not found or video unreadable
"""

import argparse
import json
import subprocess
import sys


def run_ffprobe(video_path: str) -> dict:
    cmd = [
        "ffprobe", "-v", "quiet",
        "-print_format", "json",
        "-show_streams", "-show_format",
        video_path,
    ]
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
    except FileNotFoundError:
        print("ERROR: ffprobe not found. Install ffmpeg and add it to PATH.")
        sys.exit(2)
    except subprocess.TimeoutExpired:
        print("ERROR: ffprobe timed out.")
        sys.exit(2)

    if result.returncode != 0:
        print(f"ERROR: ffprobe failed:\n{result.stderr}")
        sys.exit(2)

    return json.loads(result.stdout)


def get_mean_brightness(video_path: str) -> float:
    """Use ffmpeg signalstats filter to get mean luma (brightness) of the video."""
    cmd = [
        "ffmpeg", "-i", video_path,
        "-vf", "signalstats,metadata=print:file=-",
        "-an", "-f", "null", "-",
    ]
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
    except FileNotFoundError:
        print("ERROR: ffmpeg not found. Install ffmpeg and add it to PATH.")
        sys.exit(2)
    except subprocess.TimeoutExpired:
        print("ERROR: ffmpeg brightness check timed out.")
        sys.exit(2)

    # Parse YAVG (luma average) values from metadata output
    yavg_values = []
    for line in result.stderr.splitlines():
        if "lavfi.signalstats.YAVG" in line:
            try:
                val = float(line.split("=")[1])
                yavg_values.append(val)
            except (IndexError, ValueError):
                continue

    if not yavg_values:
        # Fallback: try a simpler mean volume check via thumbnail
        return _brightness_via_thumbnail(video_path)

    return sum(yavg_values) / len(yavg_values)


def _brightness_via_thumbnail(video_path: str) -> float:
    """Fallback: extract a single frame and check its mean brightness."""
    import tempfile, os
    with tempfile.NamedTemporaryFile(suffix=".png", delete=False) as f:
        thumb_path = f.name

    try:
        cmd = [
            "ffmpeg", "-i", video_path,
            "-vframes", "1", "-ss", "00:00:01",
            "-y", thumb_path,
        ]
        subprocess.run(cmd, capture_output=True, timeout=15)

        # Use ffmpeg to get mean brightness of the thumbnail
        cmd2 = [
            "ffmpeg", "-i", thumb_path,
            "-vf", "scale=1:1,format=gray",
            "-f", "rawvideo", "-",
        ]
        result = subprocess.run(cmd2, capture_output=True, timeout=10)
        if result.stdout:
            return float(result.stdout[0])
    except Exception:
        pass
    finally:
        os.unlink(thumb_path)

    return 0.0  # Assume black if we can't determine


def parse_fps(fps_str: str) -> float:
    """Parse fps string like '30/1' or '29.97' into a float."""
    if "/" in fps_str:
        num, den = fps_str.split("/")
        return float(num) / float(den) if float(den) != 0 else 0.0
    return float(fps_str)


def main():
    parser = argparse.ArgumentParser(description="CI video validation")
    parser.add_argument("video", help="Path to the video file to check")
    parser.add_argument("--min-brightness", type=float, default=10.0,
                        help="Minimum mean brightness (0-255), default 10")
    parser.add_argument("--min-fps", type=float, default=27.0,
                        help="Minimum FPS, default 27")
    parser.add_argument("--min-duration", type=float, default=3.0,
                        help="Minimum duration in seconds, default 3")
    args = parser.parse_args()

    print(f"\n=== Checking video: {args.video} ===\n")
    failures = []

    # --- Probe metadata ---
    probe = run_ffprobe(args.video)
    video_stream = next(
        (s for s in probe.get("streams", []) if s.get("codec_type") == "video"),
        None,
    )

    if not video_stream:
        print("ERROR: No video stream found in file.")
        sys.exit(2)

    # --- Check FPS ---
    fps_str = video_stream.get("r_frame_rate", "0/1")
    fps = parse_fps(fps_str)
    fps_ok = fps >= args.min_fps
    print(f"FPS:        {fps:.2f}  (min: {args.min_fps})  {'✓' if fps_ok else '✗ FAIL'}")
    if not fps_ok:
        failures.append(f"FPS {fps:.2f} is below minimum {args.min_fps}")

    # --- Check duration ---
    duration = float(probe.get("format", {}).get("duration", 0))
    duration_ok = duration >= args.min_duration
    print(f"Duration:   {duration:.2f}s  (min: {args.min_duration}s)  {'✓' if duration_ok else '✗ FAIL'}")
    if not duration_ok:
        failures.append(f"Duration {duration:.2f}s is below minimum {args.min_duration}s")

    # --- Check brightness ---
    print(f"Brightness: checking...", end=" ", flush=True)
    brightness = get_mean_brightness(args.video)
    brightness_ok = brightness >= args.min_brightness
    print(f"{brightness:.2f}  (min: {args.min_brightness})  {'✓' if brightness_ok else '✗ FAIL (black screen?)'}")
    if not brightness_ok:
        failures.append(f"Mean brightness {brightness:.2f} is below {args.min_brightness} — possible black screen")

    # --- Result ---
    print()
    if failures:
        print("=== FAILED ===")
        for f in failures:
            print(f"  • {f}")
        sys.exit(1)
    else:
        print("=== ALL CHECKS PASSED ===")
        sys.exit(0)


if __name__ == "__main__":
    main()