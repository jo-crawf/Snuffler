#!/usr/bin/env python3
"""Draws packaging/AppIcon.png, the 1024px master make_app.sh turns into an
.icns: GhostBuster's ghost, in the window's palette, on a macOS-shaped tile.

    python packaging/make_icon.py
"""

import math
import os

from PIL import Image, ImageDraw, ImageFilter

SIZE = 1024
SS = 4  # supersampling, for smooth edges
BG = (0x1E, 0x21, 0x26)
PANEL = (0x2B, 0x2E, 0x36)
EDGE = (0x56, 0x5E, 0x6E)
TEXT = (0xEC, 0xEE, 0xF2)
ACCENT = (0x58, 0x65, 0xF2)
SQUISH = (0xD8, 0x8A, 0x3A)


def ghost_points(cx, cy, r):
    """The window's ghost: domed head, straight sides, scalloped hem."""
    top = cy - r / 3
    bottom = cy + r
    pts = []
    for i in range(97):
        a = math.pi + math.pi * i / 96
        pts.append((cx + r * math.cos(a), top + r * math.sin(a)))
    pts.append((cx + r, bottom))
    lw = 2 * r / 4
    for i in range(4):
        xr = cx + r - i * lw
        xl = xr - lw
        pts.append(((xl + xr) / 2, bottom - lw / 2))
        pts.append((xl, bottom))
    pts.append((cx - r, top))
    return pts, top


def main():
    s = SIZE * SS
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))

    # The tile: Apple's grid puts an 824px rounded square in a 1024px canvas.
    inset, radius = 100 * SS, 185 * SS
    tile = Image.new("RGBA", (s, s))
    grad = Image.linear_gradient("L").resize((s, s))
    tile = Image.composite(Image.new("RGBA", (s, s), BG + (255,)),
                           Image.new("RGBA", (s, s), PANEL + (255,)), grad)
    mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [inset, inset, s - inset, s - inset], radius=radius, fill=255)
    img.paste(tile, (0, 0), mask)
    ImageDraw.Draw(img).rounded_rectangle(
        [inset, inset, s - inset, s - inset], radius=radius, outline=EDGE + (255,), width=4 * SS)

    cx, cy, r = s // 2, int(s * 0.50), int(s * 0.25)

    # A soft accent glow under the ghost, like the lit BUST button.
    glow = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    gd = ImageDraw.Draw(glow)
    gd.ellipse([cx - r * 1.05, cy + r * 0.95, cx + r * 1.05, cy + r * 1.35], fill=ACCENT + (170,))
    glow = glow.filter(ImageFilter.GaussianBlur(28 * SS))
    img = Image.alpha_composite(img, Image.composite(glow, Image.new("RGBA", (s, s)), mask))

    d = ImageDraw.Draw(img)
    pts, top = ghost_points(cx, cy, r)
    d.polygon(pts, fill=TEXT + (255,))
    ex, ew = r / 3, r / 5
    for dx in (-ex, ex):
        d.ellipse([cx + dx - ew, top - ew, cx + dx + ew, top + ew], fill=BG + (255,))
    # A snuffling nose: the one thing GhostBuster's ghost doesn't have.
    nw = r / 7
    d.ellipse([cx - nw, top + ew * 1.1, cx + nw, top + ew * 1.1 + nw * 1.4], fill=SQUISH + (255,))

    out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "AppIcon.png")
    img.resize((SIZE, SIZE), Image.LANCZOS).save(out, optimize=True)
    print("wrote", out)


if __name__ == "__main__":
    main()
