#!/usr/bin/env python3
"""Generate the codex-vs-powershell drama GIF with PIL, frame by frame.

Storyboard: a developer chats with codex; codex's suggestion gets eaten by
PowerShell's argument parsing; the user loses it; niubash saves the day.

Usage: uv run --with pillow python scripts/gen-demo-drama.py [ffmpeg.exe]
Output: assets/demo-drama.gif (18s @ 12fps, 880x480)
"""
import math
import os
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw, ImageFont

W, H = 880, 480
FPS = 12
DURATION = 18.0
BG = (13, 17, 23)          # 0x0d1117
WHITE = (255, 255, 255)
BLUE = (121, 192, 255)     # 0x79c0ff
RED = (255, 85, 85)        # 0xff5555
YELLOW = (255, 215, 0)     # 0xffd700
GREEN = (80, 250, 123)     # 0x50fa7b
GREY = (139, 148, 158)     # 0x8b949e

CONSOLA = r"C:/Windows/Fonts/consola.ttf"
MSYHBD = r"C:/Windows/Fonts/msyhbd.ttc"

# (text, start, end, color, font_path, base_size, x, y, align, typewriter_cps, jitter, pop, breathe)
LINES = [
    ("you: node eats my arguments, fix it", 0.4, 4.5, WHITE, CONSOLA, 24, 70, 90, "left", 30, 0.0, 0, 0.0),
    ("codex: sure. quote them like this:", 1.6, 5.5, BLUE, CONSOLA, 24, 70, 130, "left", 30, 0.0, 0, 0.0),
    (r'> node -e "..." "a b" "" "c\"d" "e\f"', 2.8, 6.5, WHITE, CONSOLA, 24, 70, 170, "left", 25, 0.0, 0, 0.0),
    ("ParserError: TerminatorExpectedAtEndOfString", 4.4, 8.0, RED, CONSOLA, 26, 70, 210, "left", 60, 3.0, 0, 0.0),
    ("you: IT ATE MY ARGUMENTS", 6.2, 7.6, YELLOW, MSYHBD, 40, 0, 110, "center", 40, 0.0, 1, 0.0),
    ("(╯°□°)╯︵ ┻━┻", 7.4, 10.6, RED, MSYHBD, 96, 0, 130, "center", 0, 4.0, 0, 0.0),
    ("--- same command, one shell later ---", 10.2, 18.0, GREY, CONSOLA, 22, 70, 90, "left", 0, 0.0, 0, 0.0),
    (r'> node -e "..." "a b" "" "c\"d" "e\f"', 11.2, 18.0, WHITE, CONSOLA, 24, 70, 130, "left", 30, 0.0, 0, 0.0),
    (r'["a b","","c\"d","e\\f","---"]', 12.4, 18.0, GREEN, CONSOLA, 26, 70, 170, "left", 30, 0.0, 0, 0.0),
    ("you: never going back to pwsh", 14.2, 18.0, GREEN, MSYHBD, 36, 0, 250, "center", 40, 0.0, 1, 0.0),
    ("niubash — bash, native on Windows", 15.6, 18.0, BLUE, MSYHBD, 46, 0, 330, "center", 0, 0.0, 0, 0.8),
]


def mix(a, b, t):
    return tuple(int(x + (y - x) * t) for x, y in zip(a, b))


def draw_line(draw, text, font, color, x, y, align, jitter, breathe):
    if jitter:
        x += int(4 * math.sin(25 * (y / 10))) + 2 * math.sin(3 * (y / 10) + 1)
        y += int(2 * math.sin(30 * (y / 10)))
    if breathe > 0:
        k = 0.6 + 0.4 * math.sin(2 * math.pi * breathe * (y / 10))
        color = mix(BG, color, k)
    bbox = draw.textbbox((0, 0), text, font=font)
    tw, th = bbox[2] - bbox[0], bbox[3] - bbox[1]
    if align == "center":
        x = (W - tw) // 2
    draw.text((x, y), text, font=font, fill=color)


def main():
    ffmpeg = sys.argv[1] if len(sys.argv) > 1 else (
        os.path.expandvars(
            r"%LOCALAPPDATA%/Microsoft/WinGet/Packages/Gyan.FFmpeg_Microsoft.Winget.Source_8wekyb3d8bbwe/"
            r"ffmpeg-8.1.2-full_build/bin/ffmpeg.exe"
        )
    )
    here = os.path.dirname(os.path.abspath(__file__))
    out = os.path.join(here, "..", "assets", "demo-drama.gif")

    tmpdir = tempfile.mkdtemp(prefix="niubash-drama-")
    font_cache = {}

    def font(path, size):
        key = (path, size)
        if key not in font_cache:
            font_cache[key] = ImageFont.truetype(path, size)
        return font_cache[key]

    n_frames = int(DURATION * FPS)
    frames = []
    for frame in range(n_frames):
        t = frame / FPS
        img = Image.new("RGB", (W, H), BG)
        draw = ImageDraw.Draw(img)

        # red flash while the ParserError lands
        if 4.6 <= t < 5.4:
            overlay = Image.new("RGBA", (W, H), (255, 0, 0, 60))
            img = Image.alpha_composite(img.convert("RGBA"), overlay).convert("RGB")
            draw = ImageDraw.Draw(img)

        for (text, start, end, color, path, size, x, y, align, cps, jitter, pop, breathe) in LINES:
            if t < start or t > end:
                continue
            shown = text
            if cps > 0:
                n_chars = int((t - start) * cps)
                if n_chars < len(text):
                    shown = text[:n_chars]
            cur_size = size
            if pop:
                k = min(1.0, (t - start) / 0.4)
                cur_size = max(8, int(size * k))
            draw_line(draw, shown, font(path, cur_size), color, x, y, align, jitter, breathe)

        frames.append(img)

    frames[0].save(
        out,
        save_all=True,
        append_images=frames[1:],
        duration=int(1000 / FPS),
        loop=0,
        optimize=True,
        disposal=2,
    )
    print("done: %.1fMB %d frames -> %s" % (os.path.getsize(out) / 1e6, n_frames, out))


if __name__ == "__main__":
    main()
