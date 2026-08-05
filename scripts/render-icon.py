#!/usr/bin/env python3
"""Render the launcher icon to PNG from the same geometry as the vector.

The app itself ships vector drawables — they scale to every density and add
nothing to the APK. But stores and README badges want bitmaps, so this renders
the identical shapes at whatever size is asked for.

Geometry is expressed in the vector's 108-unit viewport and scaled up, so the
PNG and the in-app icon cannot drift apart: change one, re-run this, and they
still agree.
"""
import math
import os
import sys

from PIL import Image, ImageDraw

VIEWPORT = 108.0
GREEN = (74, 222, 128)      # #4ADE80
BACKGROUND = (14, 17, 22)   # #0E1116

# Supersample, then downscale. PIL has no anti-aliased drawing primitives, and
# arcs at icon sizes look visibly jagged without it.
SUPERSAMPLE = 8


def render(size: int, circular: bool = True) -> Image.Image:
    s = size * SUPERSAMPLE
    scale = s / VIEWPORT
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    def px(v: float) -> float:
        return v * scale

    if circular:
        d.ellipse([0, 0, s - 1, s - 1], fill=BACKGROUND)
    else:
        d.rectangle([0, 0, s, s], fill=BACKGROUND)

    cx = cy = px(54)

    def ring(radius: float, width: float, alpha: int) -> None:
        r = px(radius)
        d.ellipse(
            [cx - r, cy - r, cx + r, cy + r],
            outline=GREEN + (alpha,),
            width=max(1, int(px(width))),
        )

    ring(29, 2.5, 115)   # outer, 0.45 alpha
    ring(19, 2.5, 179)   # middle, 0.7 alpha

    # Sweep wedge: 0 degrees is East in PIL, and it turns clockwise, which
    # matches the compass convention the app uses everywhere else.
    r = px(29)
    d.pieslice([cx - r, cy - r, cx + r, cy + r], start=-60, end=-28,
               fill=GREEN + (217,))

    dot = px(6.5)
    d.ellipse([cx - dot, cy - dot, cx + dot, cy + dot], fill=GREEN + (255,))

    return img.resize((size, size), Image.LANCZOS)


def main() -> None:
    root = os.path.join(os.path.dirname(__file__), "..")
    targets = [
        # F-Droid and Play both want 512x512.
        ("fastlane/metadata/android/en-US/images/icon.png", 512, True),
        ("docs/images/icon-192.png", 192, True),
        # Square variant, for anywhere a circular mask is not applied.
        ("docs/images/icon-square-512.png", 512, False),
    ]
    for rel, size, circular in targets:
        path = os.path.normpath(os.path.join(root, rel))
        os.makedirs(os.path.dirname(path), exist_ok=True)
        image = render(size, circular)
        # F-Droid rejects alpha in the store icon, so flatten it.
        image.convert("RGB").save(path)
        print(f"  {size}x{size}  {rel}")


if __name__ == "__main__":
    sys.exit(main())
