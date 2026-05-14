"""Programmatically generate the macOS tray spinner frames.

Output: `src-tauri/icons/spinner/frame_00.png` … `frame_11.png` (12
rotations, 30° each).

Design choices
--------------
- **Two resolutions** — 16x16 and 16x16@2x (= 32x32). macOS NSStatusItem
  rasterises whatever size we give it, but a 16-aware retina pair keeps
  the dot edges crisp on both Retina and non-Retina menubars.
- **Template-image friendly** — every pixel is pure black on transparent.
  We flip `set_icon_as_template(true)` on the Rust side and let macOS
  invert in dark mode. Saves us shipping a dark-mode set.
- **12 frames at 30°** — matches the 120ms / frame cadence in
  `TrayController::start_spinner` (12 × 120ms = 1.44s per rotation).
- **8 ticks per frame** — classic NSProgressIndicator dot count, with
  brightness fading along the rotation. Two trailing ticks are at full
  opacity (the "head" of the spinner), the others fade linearly.
- **Anti-aliasing** — Pillow's `ImageDraw.ellipse` is anti-aliased only
  via supersampling; we render at 4x scale then downsample with
  `Image.LANCZOS` for clean edges.

Re-run anytime: `python3 scripts/gen_tray_spinner_icons.py`.
The script is idempotent — overwrites existing frames in place.
"""

from __future__ import annotations

import math
from pathlib import Path

from PIL import Image, ImageDraw

# --- Layout -----------------------------------------------------------------
NUM_FRAMES = 12
NUM_TICKS = 8  # Number of dots arranged around the circle.
TARGET_SIZES = [16, 32]  # 1x + 2x retina; macOS uses the higher one when available.
SUPERSAMPLE = 4  # Render at NxN, scale down for AA.

# Dot geometry as fractions of the canvas. Tweak these to taste.
INNER_RADIUS_FRAC = 0.22  # Where the dots start (from centre).
OUTER_RADIUS_FRAC = 0.42  # Where the dots end. Edge of canvas is 0.5.
DOT_RADIUS_FRAC = 0.07  # Each dot's radius.

# Opacity curve — `NUM_TICKS` evenly-spaced values from "head" → "tail".
# Two values at the head sit at full opacity so the spinner reads as
# clearly directional even at 16x16.
MAX_OPACITY = 255
MIN_OPACITY = 35


def _opacity_for_tick(tick_idx: int) -> int:
    """Tick 0 is the head (brightest), tick NUM_TICKS-1 is the tail."""
    if tick_idx == 0 or tick_idx == 1:
        return MAX_OPACITY
    progress = (tick_idx - 1) / (NUM_TICKS - 2)
    return int(MAX_OPACITY - (MAX_OPACITY - MIN_OPACITY) * progress)


def render_frame(size: int, frame_idx: int) -> Image.Image:
    """Render one spinner frame at the given target canvas size."""
    big = size * SUPERSAMPLE
    canvas = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    draw = ImageDraw.Draw(canvas)

    cx = big / 2.0
    cy = big / 2.0
    inner_r = big * INNER_RADIUS_FRAC
    outer_r = big * OUTER_RADIUS_FRAC
    dot_r = big * DOT_RADIUS_FRAC

    # Frame rotation: each frame steps the "head" position one tick clockwise.
    head_angle_offset = (frame_idx / NUM_FRAMES) * 2 * math.pi
    # Negative = clockwise (Pillow's y axis points down → positive angle
    # is clockwise, but starting from "12 o'clock" feels more natural).
    head_angle_offset = -head_angle_offset

    for tick in range(NUM_TICKS):
        tick_angle = head_angle_offset - (tick / NUM_TICKS) * 2 * math.pi
        mid_r = (inner_r + outer_r) / 2.0
        x = cx + mid_r * math.sin(tick_angle)
        y = cy - mid_r * math.cos(tick_angle)
        opacity = _opacity_for_tick(tick)
        draw.ellipse(
            (x - dot_r, y - dot_r, x + dot_r, y + dot_r),
            fill=(0, 0, 0, opacity),
        )

    # Downsample with LANCZOS for clean anti-aliased edges.
    return canvas.resize((size, size), Image.LANCZOS)


def main() -> None:
    project_root = Path(__file__).resolve().parent.parent
    out_dir = project_root / "src-tauri" / "icons" / "spinner"
    out_dir.mkdir(parents=True, exist_ok=True)

    for frame_idx in range(NUM_FRAMES):
        for size in TARGET_SIZES:
            suffix = "" if size == 16 else f"@{size // 16}x"
            out_path = out_dir / f"frame_{frame_idx:02}{suffix}.png"
            img = render_frame(size, frame_idx)
            img.save(out_path, "PNG", optimize=True)
            print(f"  ✓ {out_path.relative_to(project_root)}")

    print(f"\n✅ Wrote {NUM_FRAMES * len(TARGET_SIZES)} frames to {out_dir.relative_to(project_root)}")


if __name__ == "__main__":
    main()
