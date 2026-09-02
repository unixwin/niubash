#!/usr/bin/env python3
"""Generate niubash demo GIFs with PIL - pure data, no real terminal, no screen recording.

Renders animated terminal windows in the niubash brand palette (deep navy,
cyan/green/tan accents, Consolas).  Outputs:

    assets/demo.gif           main niubash interactive session
    assets/demo-gitbash.gif   Git Bash (MSYS) failing where niubash just works

Usage:  python scripts/gen-demos.py
"""

import os

from PIL import Image, ImageDraw, ImageFont

# ---------------- brand palette ----------------
NAVY    = (13, 17, 43)      # terminal background
NAVY_D  = (9, 13, 36)       # title bar
NAVY_L  = (24, 32, 68)      # separators
WHITE   = (232, 238, 248)
GREY    = (150, 160, 186)
DIM     = (100, 112, 144)
CYAN    = (88, 232, 255)
GREEN   = (126, 231, 135)
YELLOW  = (245, 205, 120)
RED     = (255, 110, 110)
TAN     = (240, 192, 112)
CURSOR  = (170, 226, 255)

FONT    = r"C:/Windows/Fonts/consola.ttf"
FONT_B  = r"C:/Windows/Fonts/consolab.ttf"
FPS     = 6

DOT_COLORS = ((255, 95, 86), (255, 189, 46), (39, 201, 63))


def line(spans, t, cps=0):
    """spans: list of (text, color); t: start second; cps: typewriter speed (0 = instant)."""
    return {"t": t, "cps": cps, "spans": spans}


class Term:
    def __init__(self, lines, cols=88, rows=20, font_size=15, pad=18,
                 title="niubash", duration=24.0):
        self.lines = sorted(lines, key=lambda l: l["t"])
        self.font = ImageFont.truetype(FONT, font_size)
        self.font_b = ImageFont.truetype(FONT_B, font_size)
        self.pad = pad
        self.cw = self.font.getlength("M")
        asc, desc = self.font.getmetrics()
        self.lh = asc + desc + 4
        self.rows = rows
        self.title = title
        self.duration = duration
        self.title_h = 38
        self.term_w = int(self.cw * cols) + 2 * pad
        self.term_h = self.lh * rows + 2 * pad
        self.W = self.term_w
        self.H = self.title_h + self.term_h
        self.full_text = ["".join(s[0] for s in l["spans"]) for l in self.lines]

    def visible(self, t):
        started = [l for l in self.lines if t >= l["t"]]
        top = max(0, len(started) - self.rows)
        out = []
        for i, l in enumerate(started):
            if i < top:
                continue
            row = i - top
            text = self.full_text[self.lines.index(l)]
            cps = l["cps"]
            if cps > 0:
                shown = int((t - l["t"]) * cps)
                if shown < len(text):
                    out.append((row, l, shown, True))
                    continue
            out.append((row, l, len(text), False))
        return out

    def render_frame(self, t):
        img = Image.new("RGB", (self.W, self.H), NAVY_D)
        d = ImageDraw.Draw(img)
        # ---- title bar ----
        d.rectangle([0, 0, self.W, self.title_h], fill=NAVY_D)
        dy = (self.title_h - 11) // 2
        for i, c in enumerate(DOT_COLORS):
            x = 16 + i * 22
            d.ellipse([x, dy, x + 11, dy + 11], fill=c)
        tbbox = d.textbbox((0, 0), self.title, font=self.font_b)
        tw = tbbox[2] - tbbox[0]
        d.text(((self.W - tw) // 2, (self.title_h - 16) // 2 - 2), self.title,
               font=self.font_b, fill=(218, 226, 244))
        d.line([0, self.title_h, self.W, self.title_h], fill=NAVY_L, width=1)
        # ---- terminal area ----
        d.rectangle([0, self.title_h, self.W, self.H], fill=NAVY)
        term_top = self.title_h + self.pad
        rows = self.visible(t)
        blink = int(t * 2) % 2 == 0
        for row, l, shown, typing in rows:
            y = term_top + row * self.lh
            x = self.pad
            remaining = shown
            for text, color in l["spans"]:
                if remaining <= 0:
                    break
                take = min(len(text), remaining)
                if take:
                    seg = text[:take]
                    d.text((x, y), seg, font=self.font, fill=color)
                    x += self.font.getlength(seg)
                remaining -= take
            if typing and blink:
                d.rectangle([x, y, x + self.cw, y + self.lh - 3], fill=CURSOR)
        return img

    def save(self, out):
        n = int(self.duration * FPS)
        frames = [self.render_frame(i / FPS) for i in range(n)]
        frames[0].save(out, save_all=True, append_images=frames[1:],
                       duration=int(1000 / FPS), loop=0, optimize=True, disposal=2)
        print("wrote %s  %dx%d  requested=%d  %.2f MB"
              % (out, self.W, self.H, n, os.path.getsize(out) / 1e6))


def demo_main(out="assets/demo.gif"):
    lines = [
        line([("  niubash - bash, native on Windows", YELLOW),
              ("   one binary, no VM, no /mnt/c, no cmdlet dialect", DIM)], t=0.0),
        line([("$ pwd", CYAN)], t=0.7, cps=26),
        line([("C:\\Users\\caomengxuan\\repo\\niubash", WHITE)], t=1.6),
        line([("$ cd \"C:/Program Files\" && pwd", CYAN)], t=2.3, cps=26),
        line([("C:\\Program Files", WHITE)], t=4.0),
        line([("$ cd /c/Users/caomengxuan/repo/niubash", CYAN),
              ("   # msys-style input also works", DIM)], t=4.7, cps=22),
        line([("C:\\Users\\caomengxuan\\repo\\niubash", WHITE)], t=6.8),
        line([("$ printf \"%s\\n\" alpha beta gamma | grep -v gamma", CYAN)], t=7.5, cps=28),
        line([("alpha", WHITE)], t=9.7),
        line([("beta", WHITE)], t=10.1),
        line([("$ for i in 1 2 3 4 5; do sum=$((sum+i)); done; echo \"sum 1..5 = $sum\"", CYAN)], t=10.6, cps=34),
        line([("sum 1..5 = 15", GREEN)], t=12.8),
        line([("$ cat <<EOF", CYAN)], t=13.5, cps=18),
        line([("native windows paths, unix commands,", WHITE)], t=14.5),
        line([("bash syntax - pick any two.", WHITE)], t=15.0),
        line([("we ship all three.", TAN)], t=15.5),
        line([("EOF", DIM)], t=16.0),
        line([("$ hello() { echo \"hello from $1\"; }; hello niubash", CYAN)], t=16.5, cps=30),
        line([("hello from niubash", GREEN)], t=18.2),
        line([("---- same command line, two shells ----", DIM)], t=18.8),
        line([("node -e \"console.log(JSON.stringify(process.argv.slice(1)))\" \"a b\" \"\" 'c\"d' \"e\\f\" \"---\"", DIM)], t=19.4, cps=42),
        line([("niubash : [\"a b\",\"\",\"c\\\"d\",\"e\\\\f\",\"---\"]", GREEN)], t=21.2),
        line([("pwsh    : [\"a b\",\"cd e\\\\f ---\"]", RED),
              ("   # empty arg eaten, quote flattened", DIM)], t=22.3),
        line([("$ git status --short", CYAN)], t=23.3, cps=16),
        line([(" M README.md", RED)], t=24.2),
        line([("?? assets/niubash-banner.svg", GREEN)], t=24.6),
        line([("bash syntax, windows paths, real binaries. all three.", CYAN)], t=25.4),
    ]
    Term(lines, title="niubash - bash, native on Windows", duration=27.2).save(out)


def demo_gitbash(out="assets/demo-gitbash.gif"):
    lines = [
        line([("$ cd C:\\Users\\caomengxuan\\repo\\niubash", CYAN)], t=0.0, cps=22),
        line([("bash: cd: C:Userscaomengxuanreponiubash: No such file or directory", RED)], t=2.0),
        line([("# backslashes eaten by MSYS", DIM)], t=2.9),
        line([("$ cmd /c cd /c/Users/caomengxuan", CYAN)], t=3.7, cps=20),
        line([("The system cannot find the path specified.", RED)], t=5.4),
        line([("# /c/ paths rejected by cmd", DIM)], t=6.3),
        line([("$ git grep -n niubash -- src/main.rs", CYAN)], t=7.1, cps=20),
        line([("fatal: cannot chdir to 'C:Userscaomengxuanreponiubash'", RED)], t=9.1),
        line([("# MSYS mangled the backslashes again", DIM)], t=10.0),
        line([("---- every one of these just works in niubash ----", GREY)], t=10.8),
    ]
    Term(lines, cols=82, rows=13, title="Git Bash (MSYS)", duration=13.4).save(out)


if __name__ == "__main__":
    here = os.path.dirname(os.path.abspath(__file__))
    os.chdir(os.path.join(here, ".."))
    demo_main()
    demo_gitbash()
