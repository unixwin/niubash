#!/usr/bin/env python3
"""Offline ConPTY golden: the theme gallery's live prompt preview.

niubash#170 owner shape: when the gallery highlight moves to a theme, the
pane below the menu renders that theme's ACTUAL prompt — the real PS1
expanded with that theme's config (multi-line structure, colors, right
alignment, powerline glyphs) — like lazy.nvim's floating preview. This probe
walks a real `niu setup` gallery over a real ConPTY (pywinpty + pyte, the
scripts/smoke-wizard-journey.py pattern) and snapshots the preview block for
every representative theme class:

    classic-face    ASCII-only single line
    twoline         multi-line PS1 structure
    colored         ANSI colors + engine-expanded escapes
    right-aligned   visible-width padding (right segment feel)
    powerline-mini  powerline glyph (wide, non-ASCII)
    noprompt        loads, sets no PS1  -> "(preview unavailable: ...)"
    hung            never finishes      -> killed at the 1.5s bound

Everything runs inside a throwaway sandbox: HOME/USERPROFILE overridden,
the oh-my-bash source installed from a local staged tree (no network), and
at the end the pick still lands in the rc through the exact J2 flow — the
preview must not break the theme pick, and a render must not leak OSH_THEME
into the session.

Artifacts (journey manifest format, docs/journey-gate.md): verdict.json
(per-theme snapshot + checks), verdict.txt, transcripts/*.txt.

Exit codes: 0 pass, 1 fail, 2 skip (missing python deps / pty).

Usage: python scripts/smoke-theme-gallery-preview.py <niu.exe> [sandbox-root]
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

try:
    import pyte
    from winpty import PtyProcess
except ImportError as err:  # pragma: no cover - environment guard
    print(f"SKIP: python deps missing ({err}); needs pywinpty + pyte")
    sys.exit(2)

REPO = Path(__file__).resolve().parent.parent
FIXTURE = REPO / "tests" / "fixtures" / "sources" / "oh-my-bash"

# Menus drop input queued while they draw (the smoke-wizard pattern): settle
# before pressing, digit and Enter as separate writes.
SETTLE_SECONDS = 0.6
PREVIEW_RENDER_WAIT = 2.5  # > the 1.5s render bound, so hung resolves
ENTER_GAP_SECONDS = 0.2
ENTER = "\r"
DOWN = "\x1b[B"
UP = "\x1b[A"

# The preview pane is a fixed 5 rows (1 theme header + up to 4 prompt lines,
# plugins::theme_preview::MAX_PREVIEW_LINES + 1).
PANE_ROWS = 5

# Representative themes: (gallery label, theme file body, expected pane
# fingerprints). Bodies are bash; the PS1 escape spellings are the engine's
# to expand in the preview child.
THEMES = {
    "classic": "PS1='classic-face $ '\n",
    "twoline": "PS1='twoline-top \\w\n twoline-bottom $ '\n",
    "colored": (
        "PS1='\\[\\e[32m\\]colored-user\\[\\e[0m\\]@\\[\\e[35m\\]colored-host"
        "\\[\\e[0m\\] \\[\\e[33m\\]\\w\\[\\e[0m\\]$ '\n"
    ),
    # Right-aligned feel the way width-correct rendering must show it: a
    # padded left block, then the face at the right edge of the padding.
    "right-aligned": "PS1='" + " " * 56 + "right-aligned-face $ '\n",
    # The powerline glyph rides in the file as raw UTF-8 (U+E0B0 ); the
    # preview prints the theme's bytes, not a description of them.
    "powerline-mini": "PS1=' powerline-mini \ue0b0 face$ '\n",
    "noprompt": "# loads fine, sets no prompt\ntrue\n",
    "hung": "# never sets PS1 and never finishes\nsleep 300\n",
}


class Session:
    """A niu.exe under ConPTY with a pyte-parsed screen."""

    def __init__(self, argv, cwd, env, cols=120, rows=40):
        self.proc = PtyProcess.spawn(argv, cwd=str(cwd), env=env,
                                     dimensions=(rows, cols))
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.Stream(self.screen)
        self._raw = []
        self._reader = threading.Thread(target=self._pump, daemon=True)
        self._reader.start()

    def _pump(self):
        while self.proc.isalive():
            try:
                data = self.proc.read()
            except Exception:
                break
            if data:
                self._raw.append(data)
                self.stream.feed(data)

    def text(self) -> str:
        return "\n".join(self.screen.display)

    def raw(self) -> str:
        return "".join(self._raw)

    def wait_for(self, *needles, timeout=60):
        deadline = time.time() + timeout
        while time.time() < deadline:
            body = self.text()
            for needle in needles:
                if needle in body:
                    return needle
            time.sleep(0.05)
        raise TimeoutError(
            f"timed out waiting for {needles}; screen:\n{self.text()}"
        )

    def wait_change(self, before: str, timeout=10.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.text() != before:
                return True
            time.sleep(0.05)
        return False

    def settle(self):
        """Let the menu finish its draw before the next key."""
        time.sleep(SETTLE_SECONDS)

    def press(self, keys):
        before = self.text()
        self.proc.write(keys)
        self.wait_change(before)
        self.settle()

    def jiggle(self):
        """Repaint the pane with two navigation keys (the pane refreshes on
        the next highlight move — the shape the menu callback owns)."""
        self.press(DOWN)
        self.press(UP)

    def capture_pane(self):
        """The settled preview pane: wait out the rendering placeholder (the
        first paint may still be inside the render grace) by jiggling."""
        for _ in range(8):
            pane = preview_pane(self)
            if "rendering preview" not in "\n".join(pane):
                return pane
            time.sleep(0.5)
            self.jiggle()
        return preview_pane(self)

    def close(self):
        try:
            self.proc.terminate(force=True)
        except Exception:
            pass


def preview_pane(session: Session):
    """The fixed preview pane: the 5 rows below the menu hint block."""
    rows = session.text().split("\n")
    hint = max(i for i, row in enumerate(rows) if "navigate" in row)
    start = hint + 2
    pane = rows[start:start + PANE_ROWS]
    while pane and not pane[-1].strip():
        pane.pop()
    return pane


def build_source_tree(staged: Path):
    """The staged oh-my-bash tree: repo fixture + representative themes."""
    shutil.copytree(FIXTURE, staged)
    for name, body in THEMES.items():
        theme_dir = staged / "themes" / name
        theme_dir.mkdir(parents=True, exist_ok=True)
        (theme_dir / f"{name}.theme.sh").write_text(body, encoding="utf-8")
    # The deep-walk tail: enough tiny themes to reproduce the golden
    # journey's J2 shape (>60-entry gallery, walk DOWN to a deep row). A
    # preview that cost real time per key would stall this walk — the walk
    # must stay as fast as it was before the live pane existed.
    for index in range(110):
        name = f"zzz-walk-{index:03d}"
        theme_dir = staged / "themes" / name
        theme_dir.mkdir(parents=True, exist_ok=True)
        (theme_dir / f"{name}.theme.sh").write_text(
            f"PS1='walk-{index:03d} $ '\n", encoding="utf-8")


def run_golden(exe: Path, root: Path) -> None:
    # Absolute sandbox paths are load-bearing: the source registry records
    # the staged origin verbatim, and the niu children run with cwd=home — a
    # relative root would register a path that degrades from every process
    # whose cwd differs.
    root = root.resolve()
    exe = exe.resolve()
    home = root / "home"
    sources = root / "sources"
    staged = root / "staged-oh-my-bash"
    (home / ".niubash").mkdir(parents=True, exist_ok=True)
    sources.mkdir(parents=True, exist_ok=True)
    build_source_tree(staged)

    # Install + trust through the real verbs (local tree; no network).
    system_root = os.environ.get("SystemRoot", r"C:\Windows")
    env = {
        "SystemRoot": system_root,
        "COMSPEC": system_root + r"\System32\cmd.exe",
        "HOME": str(home),
        "USERPROFILE": str(home),
        "LOCALAPPDATA": str(home / "local-appdata"),
        "APPDATA": str(home / "appdata"),
        "TEMP": str(home / "tmp"),
        "TMP": str(home / "tmp"),
        "TERM": "xterm",
        "NIU_LANG": "en",
        "NIU_PLUGIN_SOURCES_ROOT": str(sources),
        "NIU_PLUGIN_SPEC": str(home / ".niubash" / "plugins.json"),
        "PATH": os.pathsep.join([
            str(Path(exe).parent), system_root + r"\System32", system_root,
        ]),
    }
    (home / "tmp").mkdir(exist_ok=True)
    for verb_args in (["plugin", "add", str(staged)], ["plugin", "trust", "oh-my-bash"]):
        done = subprocess.run([str(exe), *verb_args], env=env,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                              timeout=120)
        assert done.returncode == 0, (
            f"niu {' '.join(verb_args)} failed: "
            f"{done.stderr.decode(errors='replace')}"
        )

    checks = []
    previews = []
    transcripts = root / "transcripts"
    transcripts.mkdir(exist_ok=True)

    def check(name, ok, detail=""):
        checks.append({"name": name, "pass": bool(ok), "detail": detail})

    s = Session([str(exe), "setup"], home, env)
    try:
        s.wait_for("Pick a theme")
        s.settle()  # the option window paints right after the label
        check("gallery opens", True)
        # The gallery must list the representative themes (>5 entries from
        # the one trusted source: 3 fixture + 7 representative + Skip).
        gallery = s.text()
        listed = sum(1 for name in THEMES if name in gallery)
        check("gallery lists the representative themes", listed >= 5,
              f"{listed}/{len(THEMES)} visible")

        # Jump to each representative theme (digit keys move the highlight;
        # the pane below the menu paints that theme's real prompt).
        # Sorted gallery order (fixture + representative, alphabetical):
        # 1 Skip, 2 agnoster, 3 classic, 4 colored, 5 demox, 6 hung,
        # 7 noprompt, 8 powerline-mini, 9 right-aligned, then
        # robbyrussell/twoline past the digit range.
        jumps = {"classic": "3", "colored": "4", "hung": "6",
                 "noprompt": "7", "powerline-mini": "8", "right-aligned": "9"}
        # Deterministic per-theme expectations.
        expected = {
            "classic": ["classic-face $"],
            "twoline": ["twoline-top", "twoline-bottom"],
            "colored": ["colored-user@colored-host"],
            "right-aligned": ["right-aligned-face $"],
            "powerline-mini": ["powerline-mini", "\ue0b0"],
            "noprompt": ["(preview unavailable: theme set no prompt)"],
            "hung": ["(preview unavailable:", "timed out"],
        }
        for name in ["classic", "colored", "right-aligned", "powerline-mini",
                     "noprompt"]:
            s.press(jumps[name])
            pane = s.capture_pane()
            fingerprints = expected[name]
            ok = all(fp in "\n".join(pane) for fp in fingerprints)
            previews.append({"theme": name, "pane": pane, "pass": ok})
            check(f"preview renders {name}", ok,
                  f"pane={pane!r} expected={fingerprints!r}")
            (transcripts / f"preview-{name}.txt").write_text(
                s.text(), encoding="utf-8")

        # The hung theme: placeholder first, then the bounded degradation.
        s.press(jumps["hung"])
        placeholder_pane = preview_pane(s)
        (transcripts / "preview-hung-pending.txt").write_text(
            s.text(), encoding="utf-8")
        time.sleep(PREVIEW_RENDER_WAIT)
        # The pane repaints on the next navigation: jiggle down and back.
        s.jiggle()
        pane = s.capture_pane()
        fingerprints = expected["hung"]
        ok = all(fp in "\n".join(pane) for fp in fingerprints)
        previews.append({"theme": "hung", "pane": pane, "pass": ok})
        check("hung theme degrades within the bound", ok,
              f"pane={pane!r}")
        check("hung theme placeholder while rendering",
              "rendering preview" in "\n".join(placeholder_pane),
              f"pane={placeholder_pane!r}")
        (transcripts / "preview-hung.txt").write_text(s.text(), encoding="utf-8")

        # The pick still lands (the J2 flow must survive the preview): the
        # highlighted twoline is confirmed, Skip through the niu-git
        # question, then Apply. But first — the deep walk (the J2 shape):
        # from twoline, walk DOWN through the zzz-walk tail to the last row
        # (journey walk dynamics: one DOWN per poll, poll the highlight,
        # no per-key resend). A preview that cost real time per key would
        # stall this; the walk must reach the bottom quickly.
        s.press(jumps["right-aligned"])
        s.press(DOWN)  # 10 robbyrussell
        s.press(DOWN)  # 11 twoline
        pane = s.capture_pane()
        fingerprints = expected["twoline"]
        ok = all(fp in "\n".join(pane) for fp in fingerprints)
        previews.append({"theme": "twoline", "pane": pane, "pass": ok})
        check("preview renders twoline (multi-line structure)", ok,
              f"pane={pane!r}")
        (transcripts / "preview-twoline.txt").write_text(
            s.text(), encoding="utf-8")

        target = "zzz-walk-109"
        reached = False
        started = time.time()
        for _ in range(600):
            rows = s.text().split("\n")
            if any("\u25c6" in row and target in row for row in rows):
                reached = True
                break
            before = s.text()
            s.proc.write(DOWN)
            s.wait_change(before, timeout=5.0)
            time.sleep(0.02)
        elapsed = time.time() - started
        check("deep walk reaches the last theme (no preview stall)",
              reached, f"{elapsed:.1f}s for the 120-row gallery")
        (transcripts / "deep-walk-end.txt").write_text(
            s.text(), encoding="utf-8")
        # Walk back to twoline for the pick (digits jump: '3' is classic…
        # simplest is to re-jump by digit: right-aligned is '9', then two
        # DOWNs land on twoline again).
        s.press("9")
        s.press(DOWN)
        s.press(DOWN)
        s.capture_pane()

        # The highlighted twoline is confirmed, Skip through the niu-git
        # question, then Apply.
        s.proc.write(ENTER)
        s.wait_for("niu-git")
        s.settle()
        s.proc.write(ENTER)
        s.wait_for("Apply this configuration?")
        s.settle()
        before = s.text()
        s.proc.write(ENTER)
        s.wait_change(before)
        deadline = time.time() + 60
        while s.proc.isalive() and time.time() < deadline:
            time.sleep(0.1)
        check("wizard completes with the previewed pick", not s.proc.isalive())
    finally:
        s.close()

    rc = (home / ".niubashrc")
    rc_text = rc.read_text(encoding="utf-8") if rc.is_file() else ""
    check("the picked theme is in the rc", "OSH_THEME='twoline'" in rc_text,
          rc_text[-400:])
    # Isolation (niubash#170): renders ran in throwaway children — the
    # gallery browse must not have written any other theme into the rc, and
    # no OSH_THEME leaked into the environment files the sandbox keeps.
    stray = [name for name in THEMES
             if name != "twoline" and f"OSH_THEME='{name}'" in rc_text]
    check("no other theme leaked into the rc", not stray, f"{stray}")

    pass_count = sum(1 for entry in checks if entry["pass"])
    verdict = {
        "gate": "theme-gallery-preview-golden",
        "ticket": "niubash#170",
        "binary": str(exe),
        "checks": checks,
        "preview_snapshots": previews,
        "summary": {
            "total": len(checks),
            "passed": pass_count,
            "failed": len(checks) - pass_count,
        },
    }
    (root / "verdict.json").write_text(
        json.dumps(verdict, indent=2, ensure_ascii=False), encoding="utf-8")
    lines = ["THEME GALLERY PREVIEW GOLDEN (niubash#170)", ""]
    for entry in checks:
        mark = "PASS" if entry["pass"] else "FAIL"
        lines.append(f"  [{mark}] {entry['name']} {entry['detail']}".rstrip())
    lines.append("")
    for snap in previews:
        lines.append(f"  --- {snap['theme']} ---")
        lines.extend(f"  | {row}" for row in snap["pane"])
    lines.append("")
    lines.append(f"verdict: {pass_count}/{len(checks)} checks passed")
    (root / "verdict.txt").write_text("\n".join(lines), encoding="utf-8")
    print("\n".join(lines))
    if pass_count != len(checks):
        raise AssertionError(
            f"{len(checks) - pass_count} golden checks failed; "
            f"artifacts in {root}")


def main() -> int:
    args = [arg for arg in sys.argv[1:] if arg != "--keep"]
    keep = len(args) != len(sys.argv) - 1
    if not args:
        print(__doc__)
        return 2
    exe = Path(args[0])
    if not exe.is_file():
        print(f"SKIP: niu binary not found: {exe}")
        return 2
    parent = Path(args[1]) if len(args) > 1 else None
    if parent is not None:
        parent.mkdir(parents=True, exist_ok=True)
    if keep:
        sandbox = parent or Path(tempfile.gettempdir()) / "theme-preview-golden-keep"
        sandbox = sandbox / time.strftime("%Y%m%d-%H%M%S")
        sandbox.mkdir(parents=True, exist_ok=True)
        try:
            run_golden(exe, sandbox)
        except (AssertionError, TimeoutError) as err:
            print(f"FAIL: {err}")
            return 1
        print(f"PASS: theme gallery preview golden (sandbox kept: {sandbox})")
        return 0
    with tempfile.TemporaryDirectory(prefix="theme-preview-golden-", dir=parent) as tmp:
        try:
            run_golden(exe, Path(tmp))
        except (AssertionError, TimeoutError) as err:
            print(f"FAIL: {err}")
            return 1
    print("PASS: theme gallery preview golden")
    return 0


if __name__ == "__main__":
    sys.exit(main())
