#!/usr/bin/env python3
"""Render captured terminal output, ANSI colour and all, to a PNG.

A screenshot of a terminal window would need a desktop session, and asciinema
players need JavaScript that a README cannot run. Parsing the escape codes
ourselves keeps the whole thing to one file and one dependency, and the result
is a plain image that renders anywhere.

Only what the CLI actually emits is handled: SGR colour and reset. Cursor
movement is not interpreted — the caller is expected to hand over a single
already-composed frame, which is what `--frame` extracts.

    superfind | scripts/render-terminal.py out.png
    scripts/render-terminal.py out.png < captured.raw
"""
import re
import sys

from PIL import Image, ImageDraw, ImageFont

FONT_PATH = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
FONT_SIZE = 22
PAD = 28
LINE_SPACING = 1.32
TITLE_BAR = 38

BACKGROUND = (14, 17, 22)
CHROME = (24, 28, 36)
DEFAULT_FG = (222, 228, 238)

# The 16 ANSI colours, tuned to sit on this background rather than taken from a
# stock palette — the CLI leans on red and yellow for "far", and the stock
# versions vibrate against near-black.
PALETTE = {
    30: (70, 78, 92), 31: (248, 113, 113), 32: (74, 222, 128), 33: (253, 224, 71),
    34: (96, 165, 250), 35: (192, 132, 252), 36: (34, 211, 238), 37: (222, 228, 238),
    90: (100, 110, 128), 91: (252, 165, 165), 92: (134, 239, 172), 93: (254, 240, 138),
    94: (147, 197, 253), 95: (216, 180, 254), 96: (103, 232, 249), 97: (255, 255, 255),
}

SGR = re.compile(r"\x1B\[([0-9;]*)m")
# Everything except SGR. The final class deliberately omits `m`: an earlier
# version ended in [A-Za-z], which swallowed every colour code before the
# parser ever saw one and rendered the whole capture in grey.
OTHER_ESCAPES = re.compile(r"\x1B\[[0-9;?]*[a-ln-zA-LN-Z]|\x1B[()][A-B0-2]|\x1B[=>]")


def parse(text):
    """Split into lines of (text, colour) runs."""
    lines = []
    colour, dim = DEFAULT_FG, False

    for raw in text.split("\n"):
        runs, pos = [], 0
        for match in SGR.finditer(raw):
            chunk = raw[pos:match.start()]
            if chunk:
                runs.append((chunk, colour, dim))
            for code in (match.group(1) or "0").split(";"):
                code = int(code or 0)
                if code == 0:
                    colour, dim = DEFAULT_FG, False
                elif code == 2:
                    dim = True
                elif code == 22:
                    dim = False
                elif code in PALETTE:
                    colour = PALETTE[code]
            pos = match.end()
        tail = raw[pos:]
        if tail:
            runs.append((tail, colour, dim))
        lines.append(runs)
    return lines


def render(text, path):
    # Carriage returns would otherwise render as tofu.
    lines = parse(OTHER_ESCAPES.sub("", text).replace("\r", ""))
    font = ImageFont.truetype(FONT_PATH, FONT_SIZE)

    probe = ImageDraw.Draw(Image.new("RGB", (1, 1)))
    advance = probe.textlength("M", font=font)
    line_height = int(FONT_SIZE * LINE_SPACING)

    width_chars = max((sum(len(t) for t, _, _ in runs) for runs in lines), default=80)
    width = int(advance * width_chars) + PAD * 2
    height = line_height * len(lines) + PAD * 2 + TITLE_BAR

    img = Image.new("RGB", (width, height), BACKGROUND)
    d = ImageDraw.Draw(img)

    # A title bar, so the image reads as a terminal rather than as stray text.
    d.rectangle([0, 0, width, TITLE_BAR], fill=CHROME)
    for i, colour in enumerate([(255, 95, 86), (255, 189, 46), (39, 201, 63)]):
        cx = 20 + i * 20
        d.ellipse([cx - 6, TITLE_BAR // 2 - 6, cx + 6, TITLE_BAR // 2 + 6], fill=colour)
    d.text((92, TITLE_BAR // 2), "superfind", font=ImageFont.truetype(FONT_PATH, 15),
           fill=(140, 150, 168), anchor="lm")

    y = TITLE_BAR + PAD
    for runs in lines:
        x = PAD
        for chunk, colour, dim in runs:
            if dim:
                colour = tuple(int(c * 0.55 + b * 0.45) for c, b in zip(colour, BACKGROUND))
            d.text((x, y), chunk, font=font, fill=colour)
            x += advance * len(chunk)
        y += line_height

    img.save(path)
    print(f"  {img.size[0]}x{img.size[1]}  {path}")


def main():
    if len(sys.argv) < 2:
        sys.exit("usage: render-terminal.py OUT.png [< captured-ansi]")
    data = sys.stdin.buffer.read().decode("utf-8", errors="replace")
    render(data.rstrip("\n"), sys.argv[1])


if __name__ == "__main__":
    main()
