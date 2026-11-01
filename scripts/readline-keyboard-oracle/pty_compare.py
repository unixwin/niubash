#!/usr/bin/env python3
"""PTY differential keyboard oracle: GNU bash/readline vs niu.

Both shells run under a real PTY with an isolated HOME/INPUTRC/TERM/PS1/
COLUMNS.  A bind -x hook dumps the edit buffer and point after a key
sequence, and the two sides are compared byte-for-byte.

GNU reference: bash 5.3.20, lib/readline 8.3 (the bash 5.3 tree).
niu reference: the pinned target/debug/niu built from unixwin/niubash
master a883cde with ruline 0bfc785.

Readline source references used by the matrix:
  * emacs_keymap.c -- the default emacs keymap: TAB complete, M-? etc.
  * text.c        -- rl_forward_char/rl_backward_char, word motion,
                     rl_unix_word_rubout, rl_kill_line, rl_yank,
                     rl_yank_pop, rl_transpose_chars/words,
                     rl_upcase/downcase/capitalize_word,
                     rl_exchange_point_and_mark, rl_undo_command,
                     rl_operate_and_get_next.
  * search.c      -- rl_reverse_search_history (C-r), isearch accept/
                     abort (C-j/C-g/ESC), and the non-incremental M-p/M-n
                     search loop.
  * complete.c    -- rl_complete, rl_possible_completions,
                     rl_insert_completions, filename/username/hostname
                     completion, rl_complete_into_braces,
                     rl_glob_complete_word.
  * bashline.c    -- bash_execute_unix_command (bind -x), the
                     READLINE_LINE/READLINE_POINT protocol, and
                     maybe_make_readline_line; also operate-and-get-next
                     history integration.
  * macro.c       -- rl_start_kbd_macro/rl_end_kbd_macro/
                     rl_call_last_kbd_macro (C-x ( / C-x ) / C-x e).
  * readline.c    -- bracketed paste (ESC[200~ ... ESC[201~).

Status policy:
  PASS  both sides produced identical buffer bytes and point bytes.
  FAIL  any mismatch, crash, startup failure, or dump timeout.
  KNOWN a documented product-specific mismatch (see the case metadata).
  SKIP  an external tool is absent (bash, niu, or sh for fixtures).

Run with --record-baseline to refresh baseline/results.json and
baseline/summary.md after pinning a new niu.
"""

from __future__ import annotations

import argparse
import errno
import fcntl
import json
import os
import pty
import select
import shlex
import shutil
import signal
import struct
import subprocess
import sys
import termios
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
FIXTURES = SCRIPT_DIR / "fixtures.sh"

DEFAULT_NIU = "/data/data/com.termux/files/home/projects/niubash/target/debug/niu"
DEFAULT_BASH = "/data/data/com.termux/files/usr/bin/bash"

PROMPT = b"__RLP__> "
SETUP_OK = b"__RL_SETUP_OK__"
DUMP_KEY = b"\x18\x04"  # C-x C-d
CTRL_C = b"\x03"

# Terminal control bytes used by the matrix.
ESC = b"\x1b"
TAB = b"\t"
UP = b"\x1b[A"
DOWN = b"\x1b[B"
RIGHT = b"\x1b[C"
LEFT = b"\x1b[D"
HOME = b"\x1b[H"
END = b"\x1b[F"
C_A = b"\x01"
C_B = b"\x02"
C_E = b"\x05"
C_F = b"\x06"
C_G = b"\x07"
C_J = b"\x0a"
C_K = b"\x0b"
C_O = b"\x0f"
C_R = b"\x12"
C_T = b"\x14"
C_U = b"\x15"
C_W = b"\x17"
C_Y = b"\x19"
C_UNDERSCORE = b"\x1f"
C_AT = b"\x00"
C_X = b"\x18"
M_B = ESC + b"b"
M_F = ESC + b"f"
M_T = ESC + b"t"
M_U = ESC + b"u"
M_L = ESC + b"l"
M_C = ESC + b"c"
M_Y = ESC + b"y"
M_P = ESC + b"p"
M_N = ESC + b"n"
M_QUESTION = ESC + b"?"
M_EQUAL = ESC + b"="
M_STAR = ESC + b"*"
M_SLASH = ESC + b"/"
M_TILDE = ESC + b"~"
M_AT = ESC + b"@"
M_LBRACE = ESC + b"{"
M_G = ESC + b"g"
C_X_LEFT_PAREN = b"\x18("
C_X_RIGHT_PAREN = b"\x18)"
C_X_E = b"\x18e"
C_X_C_U = b"\x18\x15"


@dataclass(frozen=True)
class Case:
    case_id: str
    group: str
    text: bytes
    keys: bytes
    keys_label: str
    niu_env: dict[str, str] = field(default_factory=dict)
    known_reason: Optional[str] = None
    known_url: Optional[str] = None
    note: Optional[str] = None
    # Candidate names the printed list must contain on BOTH sides.  This
    # catches a list-only command that silently leaves the screen blank
    # while the buffer/point comparison alone would still pass.
    expect_listed: tuple[bytes, ...] = ()
    # Explicit expected-product-divergence: niu's documented behavior for
    # this case, asserted instead of GNU's.  The GNU observation is still
    # recorded as the oracle reference; it is not the pass/fail target.
    niu_expect: Optional[tuple[bytes, bytes]] = None
    product_rule: Optional[str] = None


# The matrix.  Every case starts from a fresh PTY and a fresh fixture tree:
# kill-ring, undo history, macro state, and completion menus must not leak
# between cases.  C-c is still sent after every capture, matching GNU's
# "clear the line" reset, before the shell is closed.
CASES: list[Case] = [
    # ------------------------------------------------------------------
    # text.c + emacs_keymap.c: insertion and cursor motion.
    # ------------------------------------------------------------------
    Case("text/chars", "text", b"hello", b"", ""),
    Case(
        "text/chars_mid_insert",
        "text",
        b"abc",
        C_A + b"X",
        "C-a X",
    ),
    Case(
        "text/left_right",
        "text",
        b"abc",
        LEFT + LEFT + RIGHT,
        "Left Left Right",
    ),
    Case("text/home", "text", b"abc def", HOME, "Home"),
    Case("text/end", "text", b"abc def", END, "End"),
    Case("text/c_a", "text", b"abc def", C_A, "C-a"),
    Case("text/c_e", "text", b"abc def", C_E, "C-e"),
    Case("text/c_b", "text", b"abc", C_B + C_B, "C-b C-b"),
    Case(
        "text/c_f_mid",
        "text",
        b"abc",
        C_A + C_F + C_F,
        "C-a C-f C-f",
    ),
    Case("text/m_b", "text", b"abc def", M_B, "M-b"),
    Case(
        "text/m_f",
        "text",
        b"abc def",
        C_A + M_F,
        "C-a M-f",
    ),
    # text.c rl_unix_word_rubout (C-w), rl_kill_line (C-k).
    Case("text/c_w", "text", b"abc def", C_W, "C-w"),
    Case("text/c_u", "text", b"abc def", C_U, "C-u"),
    Case(
        "text/c_k_mid",
        "text",
        b"abc def",
        C_A + C_F + C_F + C_K,
        "C-a C-f C-f C-k",
    ),
    # text.c rl_yank / rl_yank_pop.  Two kills build a deterministic ring.
    Case("text/c_y", "text", b"foo bar", C_W + C_Y, "C-w C-y"),
    Case(
        "text/m_y",
        "text",
        b"alpha beta",
        C_W + C_A + C_K + C_Y + M_Y,
        "C-w C-a C-k C-y M-y",
        note="The non-kill C-a separates the two kills: GNU text.c "
        "coalesces consecutive kills into one ring entry, so C-w C-w C-y "
        "would yank the whole original line instead of exercising M-y.",
    ),
    # text.c transposition and case conversion.
    Case(
        "text/c_t",
        "text",
        b"abcd",
        C_A + C_F + C_F + C_T,
        "C-a C-f C-f C-t",
    ),
    Case(
        "text/m_t",
        "text",
        b"one two three",
        C_E + M_T,
        "C-e M-t",
        note="GNU transposes the two words before point at end-of-line: "
        "'one three two'.  niu leaves the buffer unchanged.",
    ),
    Case(
        "text/m_u",
        "text",
        b"hello world",
        C_A + M_U,
        "C-a M-u",
    ),
    Case(
        "text/m_l",
        "text",
        b"HELLO world",
        C_A + M_L,
        "C-a M-l",
    ),
    Case(
        "text/m_c",
        "text",
        b"hello world",
        C_A + M_C,
        "C-a M-c",
        note="GNU moves point to the end of the capitalized word (5); a "
        "point of 1 would put the cursor on the first letter.",
    ),
    # text.c rl_set_mark (C-@) and rl_exchange_point_and_mark (C-x C-x).
    Case(
        "text/set_mark_m_f",
        "text",
        b"abc def",
        C_A + C_AT + M_F,
        "C-a C-@ M-f",
    ),
    Case(
        "text/exchange_point_mark",
        "text",
        b"abc def",
        C_A + C_AT + M_F + C_X + C_X,
        "C-a C-@ M-f C-x C-x",
    ),
    Case(
        "text/exchange_then_insert",
        "text",
        b"abc def",
        C_A + C_AT + M_F + C_X + C_X + b"X",
        "C-a C-@ M-f C-x C-x X",
    ),
    # text.c rl_undo_command.  C-_ is the canonical emacs binding; C-x C-u
    # is the second default binding.
    Case(
        "text/undo",
        "text",
        b"abc",
        C_UNDERSCORE,
        "C-_",
        note="C-_ is the canonical GNU undo binding; niu leaves the buffer "
        "unchanged.  The next case shows niu's C-x C-u undo does work.",
    ),
    Case(
        "text/undo_c_x_c_u",
        "text",
        b"abc",
        C_X_C_U,
        "C-x C-u",
        note="C-x C-u is GNU's second default undo binding and is implemented "
        "in niu even though C-_ is not.",
    ),
    # readline.c rl_abort.  C-g aborts the current editing command; it must
    # not consume or discard the next keystroke.
    Case("text/c_g", "text", b"abc", C_G, "C-g"),
    Case(
        "text/c_g_then_key",
        "text",
        b"abc",
        C_G + b"X",
        "C-g X",
        note="GNU keeps keys queued behind C-g; niu discards a key that "
        "arrives in the same input burst. A key sent after a pause is "
        "inserted normally in niu, so this is an abort/flush difference, "
        "not a permanently stuck editor.",
    ),
    # readline.c bracketed paste: ESC[200~ ... ESC[201~ inserts literally.
    Case(
        "text/bracketed_paste",
        "text",
        b"",
        ESC + b"[200~echo hi" + ESC + b"[201~",
        "ESC[200~echo hi ESC[201~",
    ),
    # ------------------------------------------------------------------
    # history.c + search.c: history motion, C-o, C-r, M-p/M-n.
    # ------------------------------------------------------------------
    Case("history/up", "history", b"", UP, "Up"),
    Case("history/down", "history", b"", UP + DOWN, "Up Down"),
    Case(
        "history/m_p",
        "history",
        b"",
        M_P + b"beta" + b"\r",
        "M-p beta Enter",
    ),
    Case(
        "history/m_n",
        "history",
        b"",
        UP + UP + UP + M_N + b"beta" + b"\r",
        "Up Up Up M-n beta Enter",
        note="Forward non-incremental search from the oldest entry finds "
        "beta two.  Both shells start from the same controlled history.",
    ),
    # search.c rl_reverse_search_history.  C-j terminates isearch without
    # submitting the line; GNU leaves point at the start of the match.
    Case(
        "history/c_r",
        "history",
        b"",
        C_R + b"beta" + C_J,
        "C-r beta C-j",
        note="GNU search.c leaves point at the start of the match (0); niu "
        "leaves it at the end of the recalled line (7).",
    ),
    Case(
        "history/c_r_abort",
        "history",
        b"",
        C_R + b"beta" + C_G,
        "C-r beta C-g",
    ),
    # text.c rl_operate_and_get_next: accept beta two, load gamma three.
    Case(
        "history/c_o",
        "history",
        b"",
        UP + UP + C_O,
        "Up Up C-o",
    ),
    # ------------------------------------------------------------------
    # complete.c + bashline.c: filename completion in GNU parity mode.
    # The driver sets NIU_COMPLETION_STYLE=gnu for all cases except the
    # explicit default-style probe at the end of this group.
    # ------------------------------------------------------------------
    Case(
        "complete/tab_common_prefix",
        "complete",
        b"cat al",
        TAB,
        "TAB",
        note="alpha.txt/alpine.txt share 'alp'; GNU inserts the common "
        "prefix on the first TAB.",
    ),
    Case(
        "complete/tab_single",
        "complete",
        b"cat alph",
        TAB,
        "TAB",
    ),
    Case(
        "complete/tab_double",
        "complete",
        b"cat al",
        TAB + TAB,
        "TAB TAB",
        note="Second TAB lists candidates; the buffer stays at 'cat alp'.",
    ),
    # complete.c rl_possible_completions (M-?, also M-=).  List-only:
    # the candidates are printed and rl_line_buffer/rl_point stay put.
    Case(
        "complete/m_question",
        "complete",
        b"cat al",
        M_QUESTION,
        "M-?",
        note="GNU lists the candidates and leaves 'cat al' at point 6; niu "
        "must not splice the common prefix from a list-only event.",
        expect_listed=(b"alpha.txt", b"alpine.txt"),
    ),
    Case(
        "complete/m_equal",
        "complete",
        b"cat al",
        M_EQUAL,
        "M-=",
        note="M-= is GNU's second possible-completions binding.",
        expect_listed=(b"alpha.txt", b"alpine.txt"),
    ),
    # complete.c rl_insert_completions (M-*).
    Case(
        "complete/m_star",
        "complete",
        b"cat al",
        M_STAR,
        "M-*",
        note="complete.c rl_insert_completions: GNU inserts every matching "
        "candidate; niu installs the same M-* default and splices the list.",
    ),
    # complete.c filename completion (M-/).
    Case(
        "complete/m_slash",
        "complete",
        b"cat alph",
        M_SLASH,
        "M-/",
    ),
    # complete.c rl_username_completion_function (M-~).  This host has no
    # password database; both sides leave the word unchanged.
    Case(
        "complete/m_tilde",
        "complete",
        b"~ro",
        M_TILDE,
        "M-~",
        note="No username database on this host; parity is the unchanged "
        "word.",
    ),
    # complete.c rl_hostname_completion_function (M-@); /etc/hosts has
    # localhost on both sides because both run on the same host.
    Case(
        "complete/m_at",
        "complete",
        b"@loc",
        M_AT,
        "M-@",
    ),
    # complete.c rl_complete_into_braces (M-{).
    Case(
        "complete/m_lbrace",
        "complete",
        b"cat al",
        M_LBRACE,
        "M-{",
    ),
    # complete.c rl_glob_complete_word (M-g).
    Case(
        "complete/m_g",
        "complete",
        b"cat al*",
        M_G,
        "M-g",
        note="GNU rl_glob_complete_word expands al* to the common prefix "
        "'alp' and consumes the star; niu routes glob-complete-word through "
        "Session::glob_filename and matches.",
    ),
    # Owner ruling (niubash#342, restated by #778): TAB completion is
    # intentionally zsh menu-select — the first TAB splices the FIRST
    # candidate and highlights it in the grid.  This case asserts the
    # product rule against niu's own documented behavior; GNU's
    # common-prefix result stays in the report as the oracle reference,
    # not as the pass/fail target.
    Case(
        "complete/default_style_tab",
        "complete",
        b"cat al",
        TAB,
        "TAB",
        niu_env={"NIU_COMPLETION_STYLE": "column"},
        niu_expect=(b"cat alpha.txt ", b"14"),
        product_rule="zsh menu-select TAB: the first TAB puts candidate 0 "
        "into the line and highlights it in the grid; GNU's common-prefix "
        "splice is the recorded reference, not the product contract.",
        expect_listed=(b"alpha.txt", b"alpine.txt"),
        note="Owner ruling: column/zsh TAB is intentional, not a GNU parity "
        "bug. GNU reference is 'cat alp' at point 7.",
    ),
    # M-? after a prefix attempt: still list-only, anchored on the buffer
    # the first TAB produced.
    Case(
        "complete/m_question_after_prefix",
        "complete",
        b"cat al",
        TAB + M_QUESTION,
        "TAB M-?",
        note="TAB #1 splices 'alp'; M-? then lists without touching it.",
        expect_listed=(b"alpha.txt", b"alpine.txt"),
    ),
    # ------------------------------------------------------------------
    # macro.c: keyboard macros.
    # ------------------------------------------------------------------
    Case(
        "macro/record_replay",
        "macro",
        b"",
        C_X_LEFT_PAREN + b"ab" + C_X_RIGHT_PAREN + C_X_E,
        "C-x ( ab C-x ) C-x e",
        note="Record 'ab', then C-x e replays it: GNU buffer 'aba'.",
    ),
]


@dataclass
class Observation:
    line: bytes
    point: bytes
    transcript: bytes


@dataclass
class CaseResult:
    case: Case
    status: str
    gnu: Optional[Observation]
    niu: Optional[Observation]
    reason: str = ""

    @property
    def known(self) -> bool:
        return self.status == "KNOWN"


class SessionError(RuntimeError):
    pass


class DumpTimeout(SessionError):
    pass


class PtySession:
    def __init__(self, argv: list[str], cwd: Path, env: dict[str, str]):
        self.argv = argv
        self.cwd = cwd
        self.env = env
        self.transcript = bytearray()
        self._dsr_tail = b""
        self._eof = False
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            try:
                os.chdir(cwd)
                os.environ.clear()
                os.environ.update(env)
                os.execvpe(argv[0], argv, os.environ)
            except BaseException as exc:  # pragma: no cover - child path
                try:
                    os.write(2, f"pty exec failed: {exc}\n".encode())
                except OSError:
                    pass
                os._exit(127)
        try:
            fcntl.ioctl(
                self.fd,
                termios.TIOCSWINSZ,
                struct.pack("HHHH", 24, 80, 0, 0),
            )
        except OSError:
            pass
        os.set_blocking(self.fd, False)

    def send(self, data: bytes) -> None:
        if not data:
            return
        view = memoryview(data)
        while view:
            try:
                n = os.write(self.fd, view)
            except OSError as exc:
                if exc.errno in (errno.EAGAIN, errno.EWOULDBLOCK):
                    select.select([], [self.fd], [], 0.05)
                    continue
                raise
            view = view[n:]

    def _answer_terminal_queries(self, data: bytes) -> None:
        combined = self._dsr_tail + data
        answered = False
        if b"\x1b[6n" in combined:
            # Cursor position report.  niu asks before drawing the prompt.
            self.send(b"\x1b[1;1R")
            answered = True
        if b"\x1b[c" in combined or b"\x1b[>c" in combined:
            # Primary/device-attributes report.
            self.send(b"\x1b[?1;2c")
            answered = True
        self._dsr_tail = b"" if answered else data[-16:]

    def read_once(self, timeout: float) -> bytes:
        if self._eof:
            return b""
        try:
            ready, _, _ = select.select([self.fd], [], [], timeout)
        except OSError:
            self._eof = True
            return b""
        if not ready:
            return b""
        try:
            data = os.read(self.fd, 65536)
        except OSError as exc:
            if exc.errno in (errno.EAGAIN, errno.EWOULDBLOCK):
                return b""
            self._eof = True
            return b""
        if not data:
            self._eof = True
            return b""
        self.transcript.extend(data)
        self._answer_terminal_queries(data)
        return data

    def drain_for(self, seconds: float) -> None:
        """Read for a fixed wall-clock interval (deterministic settle)."""
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self.read_once(min(0.03, max(0.0, deadline - time.monotonic())))

    def wait_for(self, needle: bytes, timeout: float, what: str) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if needle in self.transcript:
                return
            if self._eof:
                break
            self.read_once(0.05)
        tail = bytes(self.transcript[-600:])
        raise SessionError(
            f"timed out waiting for {what} ({needle!r}); tail={tail!r}"
        )

    def close(self) -> None:
        try:
            self.send(CTRL_C)
            self.drain_for(0.12)
            self.send(b"exit\n")
            self.drain_for(0.20)
        except OSError:
            pass
        try:
            os.kill(self.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        except OSError:
            pass
        try:
            os.waitpid(self.pid, 0)
        except OSError:
            pass
        try:
            os.close(self.fd)
        except OSError:
            pass


def which_or_none(path: str) -> Optional[str]:
    if not path:
        return None
    if os.path.sep in path:
        return path if os.path.isfile(path) and os.access(path, os.X_OK) else None
    return shutil.which(path)


def version_of(binary: str, args: tuple[str, ...]) -> str:
    try:
        proc = subprocess.run(
            [binary, *args],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=10,
            check=False,
            env={"PATH": os.environ.get("PATH", "/usr/bin:/bin")},
        )
    except (OSError, subprocess.SubprocessError):
        return "unknown"
    text = proc.stdout.decode("utf-8", "replace").strip().splitlines()
    return " / ".join(line.strip() for line in text[:3] if line.strip()) or "unknown"


def run_fixtures(root: Path, sh_bin: str) -> None:
    proc = subprocess.run(
        [sh_bin, str(FIXTURES), str(root)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=20,
        check=False,
    )
    if proc.returncode != 0:
        raise SessionError(
            "fixtures.sh failed: "
            + proc.stderr.decode("utf-8", "replace").strip()
        )


def build_env(root: Path, kind: str, case: Case) -> dict[str, str]:
    home = root / "home"
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(home),
        "INPUTRC": str(root / "inputrc"),
        "HISTFILE": str(home / ".history"),
        "HISTSIZE": "100",
        "HISTFILESIZE": "100",
        "HISTCONTROL": "",
        "HISTIGNORE": "",
        "HISTTIMEFORMAT": "",
        "TERM": "xterm-256color",
        "COLUMNS": "80",
        "LINES": "24",
        "LANG": "C.UTF-8",
        "LC_ALL": "C.UTF-8",
        "PS1": "__RLP__> ",
        "PS2": "__RLS__> ",
        "CDPATH": "",
        "GLOBIGNORE": "",
        "TMPDIR": str(root / "tmp"),
        "TMP": str(root / "tmp"),
        "TEMP": str(root / "tmp"),
        "XDG_CONFIG_HOME": str(home / ".config"),
        "XDG_DATA_HOME": str(home / ".local" / "share"),
        "XDG_CACHE_HOME": str(home / ".cache"),
        "NIU_NO_UPDATE_CHECK": "1",
    }
    (root / "tmp").mkdir(parents=True, exist_ok=True)
    if kind == "niu":
        # GNU readline's completion surface for parity.  niu's default
        # zsh-style column grid is an intentional product rule; the
        # `complete/default_style_tab` case probes that explicitly.
        env["NIU_COMPLETION_STYLE"] = "gnu"
        env.update(case.niu_env)
    return env


def install_hook(session: PtySession, root: Path, settle: float) -> None:
    line_file = root / "dump" / "line"
    point_file = root / "dump" / "point"
    command = (
        "__rl_dump() { "
        f"printf '%s' \"$READLINE_LINE\" >{shlex.quote(str(line_file))}; "
        f"printf '%s' \"$READLINE_POINT\" >{shlex.quote(str(point_file))}; "
        "}; "
        'bind -x \'"\\C-x\\C-d": __rl_dump\'; '
        'history -c; history -r "$HOME/.history"; '
        "printf '%s%s\\n' '__RL_SET' 'UP_OK__'"
    )
    session.send(command.encode() + b"\n")
    session.wait_for(SETUP_OK, 10.0, "bind -x/history setup sentinel")
    session.drain_for(settle)


def clear_dump(root: Path) -> None:
    dump = root / "dump"
    dump.mkdir(parents=True, exist_ok=True)
    for name in ("line", "point"):
        try:
            (dump / name).unlink()
        except FileNotFoundError:
            pass


def wait_dump(root: Path, session: PtySession, timeout: float) -> tuple[bytes, bytes]:
    line_file = root / "dump" / "line"
    point_file = root / "dump" / "point"
    deadline = time.monotonic() + timeout
    last: Optional[tuple[bytes, bytes]] = None
    while time.monotonic() < deadline:
        if line_file.exists() and point_file.exists():
            try:
                current = (line_file.read_bytes(), point_file.read_bytes())
            except OSError:
                current = None
            if current is not None:
                if last is not None and current == last:
                    return current
                last = current
        session.read_once(0.03)
    if last is not None:
        return last
    raise DumpTimeout(
        f"dump hook did not fire within {timeout:.1f}s; "
        f"tail={bytes(session.transcript[-400:])!r}"
    )


def observe(
    case: Case,
    kind: str,
    binary: str,
    work_root: Path,
    sh_bin: str,
    settle: float,
    text_settle: float,
    dump_timeout: float,
    startup_timeout: float,
    transcript_dir: Path,
) -> Observation:
    root = work_root / case.case_id.replace("/", "__") / kind
    run_fixtures(root, sh_bin)
    env = build_env(root, kind, case)
    if kind == "bash":
        argv = [binary, "--noprofile", "--norc", "-i"]
    else:
        argv = [binary, "--norc", "-i"]

    session = PtySession(argv, root / "work", env)
    try:
        session.wait_for(PROMPT, startup_timeout, f"{kind} prompt")
        install_hook(session, root, settle)
        clear_dump(root)
        if case.text:
            session.send(case.text)
            session.drain_for(text_settle)
        session.send(case.keys)
        session.drain_for(settle)
        session.send(DUMP_KEY)
        line, point = wait_dump(root, session, dump_timeout)
        # C-c clears the line (GNU readline rl_abort/newline reset) before
        # the session is closed.
        session.send(CTRL_C)
        session.drain_for(0.20)
        transcript = bytes(session.transcript)
        return Observation(line=line, point=point, transcript=transcript)
    finally:
        transcript = bytes(session.transcript)
        session.close()
        try:
            (transcript_dir / f"{case.case_id.replace('/', '__')}.{kind}.raw").write_bytes(
                transcript
            )
        except OSError:
            pass


def point_int(point: bytes) -> Optional[int]:
    try:
        return int(point.decode("ascii"))
    except (ValueError, UnicodeDecodeError):
        return None


def mismatch_reason(gnu: Observation, niu: Observation) -> str:
    if gnu.line != niu.line:
        return "buffer differs"
    gp = point_int(gnu.point)
    np = point_int(niu.point)
    if gp is None or np is None:
        return "point is not an integer"
    if gp == np:
        return "point differs (same value?)"
    # The ubiquitous niu bind -x read-back defect: a cursor at the end of
    # the buffer is reported as len-1.  Shell.run_bind_x_command computes
    # safe_cursor with char_indices().filter(offset <= cursor_byte).max(),
    # and char_indices() has no entry at buffer.len().
    if gp == len(gnu.line) and np == max(0, len(niu.line) - 1):
        return (
            "EOL point -1 (bind -x READLINE_POINT read-back; GNU "
            "bashline.c:4540 reports len, niu reports len-1)"
        )
    return "point differs"


def format_obs(obs: Optional[Observation]) -> str:
    if obs is None:
        return "<none>"
    return f"{obs.line!r}/{obs.point!r}"


def run_matrix(opts: argparse.Namespace) -> int:
    bash_bin = which_or_none(opts.bash_bin)
    niu_bin = which_or_none(opts.niu_bin)
    sh_bin = which_or_none("sh") or which_or_none("/bin/sh")

    cases = CASES
    if opts.filter:
        needle = opts.filter.lower()
        cases = [
            c
            for c in CASES
            if needle in c.case_id.lower()
            or needle in c.group.lower()
            or needle in c.keys_label.lower()
        ]
    if not cases:
        print(f"no cases match filter {opts.filter!r}", file=sys.stderr)
        return 2

    transcript_dir = Path(opts.transcripts).resolve()
    if transcript_dir.exists():
        shutil.rmtree(transcript_dir)
    transcript_dir.mkdir(parents=True, exist_ok=True)

    work_root = Path(opts.work).resolve()
    if work_root.exists() and not opts.keep_work:
        shutil.rmtree(work_root)
    work_root.mkdir(parents=True, exist_ok=True)

    bash_version = version_of(bash_bin, ("--version",)) if bash_bin else "missing"
    niu_version = version_of(niu_bin, ("--version",)) if niu_bin else "missing"

    print("== readline keyboard oracle ==")
    print(f"bash: {bash_bin or '<missing>'} :: {bash_version}")
    print(f"niu : {niu_bin or '<missing>'} :: {niu_version}")
    print("niu parity env: NIU_COMPLETION_STYLE=gnu (zsh default probed separately)")
    print(f"cases: {len(cases)}")
    print()

    skip_reason = None
    if sh_bin is None:
        skip_reason = "external tool absent: sh (fixtures.sh)"
    elif bash_bin is None:
        skip_reason = f"external tool absent: GNU bash ({opts.bash_bin})"
    elif niu_bin is None:
        skip_reason = f"external tool absent: niu ({opts.niu_bin})"

    results: list[CaseResult] = []
    if skip_reason:
        for case in cases:
            result = CaseResult(case, "SKIP", None, None, skip_reason)
            results.append(result)
            print(
                f"SKIP  {case.case_id:<32} keys={case.keys_label!r} :: {skip_reason}"
            )
    else:
        for case in cases:
            gnu: Optional[Observation] = None
            niu: Optional[Observation] = None
            gnu_error = niu_error = None
            try:
                gnu = observe(
                    case,
                    "bash",
                    bash_bin,
                    work_root,
                    sh_bin,
                    opts.settle,
                    opts.text_settle,
                    opts.dump_timeout,
                    opts.startup_timeout,
                    transcript_dir,
                )
            except Exception as exc:  # noqa: BLE001 - report as FAIL
                gnu_error = f"GNU oracle failed: {exc}"

            try:
                niu = observe(
                    case,
                    "niu",
                    niu_bin,
                    work_root,
                    sh_bin,
                    opts.settle,
                    opts.text_settle,
                    opts.dump_timeout,
                    opts.startup_timeout,
                    transcript_dir,
                )
            except Exception as exc:  # noqa: BLE001 - report as FAIL
                niu_error = f"niu failed: {exc}"

            if gnu is None or niu is None:
                reason = "; ".join(x for x in (gnu_error, niu_error) if x)
                status = "FAIL"
            elif case.niu_expect is not None:
                # Explicit product divergence: assert niu's documented
                # behavior.  GNU's observation is the recorded reference.
                want_line, want_point = case.niu_expect
                if niu.line != want_line or niu.point != want_point:
                    status = "FAIL"
                    reason = (
                        "niu product behavior drifted: expected "
                        f"{want_line!r}/{want_point!r}, got "
                        f"{niu.line!r}/{niu.point!r}"
                    )
                    if case.product_rule:
                        reason += f"; {case.product_rule}"
                else:
                    missing = [
                        token
                        for token in case.expect_listed
                        if token not in niu.transcript
                    ]
                    if missing:
                        status = "FAIL"
                        reason = (
                            "expected candidate list missing from niu "
                            "transcript: "
                            + ", ".join(
                                repr(token.decode("utf-8", "replace"))
                                for token in missing
                            )
                        )
                    else:
                        status = "PASS"
                        reason = ""
            elif gnu.line == niu.line and gnu.point == niu.point:
                missing = [
                    token
                    for token in case.expect_listed
                    if token not in gnu.transcript or token not in niu.transcript
                ]
                if missing:
                    status = "FAIL"
                    sides = []
                    if any(token not in gnu.transcript for token in missing):
                        sides.append("GNU")
                    if any(token not in niu.transcript for token in missing):
                        sides.append("niu")
                    reason = (
                        "expected candidate list missing from "
                        + "/".join(sides)
                        + " transcript: "
                        + ", ".join(
                            repr(token.decode("utf-8", "replace")) for token in missing
                        )
                    )
                else:
                    status = "PASS"
                    reason = ""
            else:
                reason = mismatch_reason(gnu, niu)
                if case.known_reason:
                    status = "KNOWN"
                    reason = f"{reason}; {case.known_reason}"
                else:
                    status = "FAIL"

            result = CaseResult(case, status, gnu, niu, reason)
            results.append(result)

            line = (
                f"{status:<5} {case.case_id:<32} keys={case.keys_label!r} "
                f"GNU={format_obs(gnu)} niu={format_obs(niu)}"
            )
            if reason:
                line += f" :: {reason}"
            print(line)
            if case.note:
                print(f"      note: {case.note}")
            if case.known_url and status == "KNOWN":
                print(f"      known: {case.known_url}")
            sys.stdout.flush()

    counts = {name: 0 for name in ("PASS", "FAIL", "KNOWN", "SKIP")}
    for result in results:
        counts[result.status] += 1

    print()
    print(
        f"SUMMARY PASS={counts['PASS']} FAIL={counts['FAIL']} "
        f"KNOWN={counts['KNOWN']} SKIP={counts['SKIP']} total={len(results)}"
    )

    write_reports(
        opts,
        results,
        counts,
        bash_bin,
        bash_version,
        niu_bin,
        niu_version,
        transcript_dir,
    )
    return 1 if counts["FAIL"] else 0


def result_json(result: CaseResult) -> dict:
    def obs_json(obs: Optional[Observation]) -> Optional[dict]:
        if obs is None:
            return None
        return {
            "line": obs.line.decode("utf-8", "backslashreplace"),
            "line_repr": repr(obs.line),
            "line_hex": obs.line.hex(),
            "point": obs.point.decode("ascii", "backslashreplace"),
        }

    case = result.case
    return {
        "id": case.case_id,
        "group": case.group,
        "status": result.status,
        "keys_label": case.keys_label,
        "keys_hex": case.keys.hex(),
        "text": case.text.decode("utf-8", "backslashreplace"),
        "text_hex": case.text.hex(),
        "niu_env": case.niu_env,
        "expect_listed": [token.decode("utf-8", "replace") for token in case.expect_listed],
        "niu_expect": (
            [item.decode("utf-8", "replace") for item in case.niu_expect]
            if case.niu_expect is not None
            else None
        ),
        "product_rule": case.product_rule,
        "known_reason": case.known_reason,
        "known_url": case.known_url,
        "note": case.note,
        "reason": result.reason,
        "gnu": obs_json(result.gnu),
        "niu": obs_json(result.niu),
    }


def markdown_escape(text: str) -> str:
    return text.replace("|", "\\|").replace("\n", " ")


def write_reports(
    opts: argparse.Namespace,
    results: list[CaseResult],
    counts: dict[str, int],
    bash_bin: Optional[str],
    bash_version: str,
    niu_bin: Optional[str],
    niu_version: str,
    transcript_dir: Path,
) -> None:
    payload = {
        "harness": 1,
        "description": "PTY differential keyboard oracle: GNU bash/readline vs niu",
        "bash": {"bin": bash_bin, "version": bash_version},
        "niu": {
            "bin": niu_bin,
            "version": niu_version,
            "parity_env": {"NIU_COMPLETION_STYLE": "gnu"},
        },
        "counts": counts,
        "cases": [result_json(result) for result in results],
    }
    (transcript_dir / "results.json").write_text(
        json.dumps(payload, indent=2, sort_keys=False) + "\n",
        encoding="utf-8",
    )

    lines = [
        "# Readline keyboard oracle baseline",
        "",
        f"- GNU bash: `{bash_bin or 'missing'}`",
        f"- GNU version: `{bash_version}`",
        f"- niu: `{niu_bin or 'missing'}`",
        f"- niu version: `{niu_version}`",
        "- niu parity env: `NIU_COMPLETION_STYLE=gnu`",
        "",
        f"**PASS {counts['PASS']} / FAIL {counts['FAIL']} / "
        f"KNOWN {counts['KNOWN']} / SKIP {counts['SKIP']}** "
        f"({len(results)} cases)",
        "",
        "Expected values are captured from GNU bash 5.3.20 in the same run; "
        "the oracle compares raw buffer and point bytes.",
        "",
    ]

    failures = [r for r in results if r.status == "FAIL"]
    knowns = [r for r in results if r.status == "KNOWN"]
    skips = [r for r in results if r.status == "SKIP"]

    if failures:
        lines += [
            "## Failures",
            "",
            "| Case | Keys | GNU (line, point) | niu (line, point) | Reason |",
            "| --- | --- | --- | --- | --- |",
        ]
        for r in failures:
            lines.append(
                "| `{}` | `{}` | `{}` | `{}` | {} |".format(
                    r.case.case_id,
                    markdown_escape(r.case.keys_label),
                    markdown_escape(format_obs(r.gnu)),
                    markdown_escape(format_obs(r.niu)),
                    markdown_escape(r.reason),
                )
            )
        lines.append("")

    if knowns:
        lines += [
            "## Known product-specific",
            "",
            "| Case | Keys | GNU (line, point) | niu (line, point) | Reason |",
            "| --- | --- | --- | --- | --- |",
        ]
        for r in knowns:
            lines.append(
                "| `{}` | `{}` | `{}` | `{}` | {} |".format(
                    r.case.case_id,
                    markdown_escape(r.case.keys_label),
                    markdown_escape(format_obs(r.gnu)),
                    markdown_escape(format_obs(r.niu)),
                    markdown_escape(r.reason),
                )
            )
            if r.case.known_url:
                lines.append(f"| | | | | {r.case.known_url} |")
        lines.append("")

    if skips:
        lines += ["## Skipped", ""]
        for r in skips:
            lines.append(f"- `{r.case.case_id}`: {r.reason}")
        lines.append("")

    lines += [
        "## Passes",
        "",
        "| Case | Keys | GNU (line, point) | niu (line, point) |",
        "| --- | --- | --- | --- |",
    ]
    for r in results:
        if r.status != "PASS":
            continue
        lines.append(
            "| `{}` | `{}` | `{}` | `{}` |".format(
                r.case.case_id,
                markdown_escape(r.case.keys_label),
                markdown_escape(format_obs(r.gnu)),
                markdown_escape(format_obs(r.niu)),
            )
        )
    lines.append("")
    try:
        shown_transcripts = str(transcript_dir.relative_to(REPO_ROOT))
    except ValueError:
        shown_transcripts = str(transcript_dir)
    lines.append(f"Raw PTY transcripts: `{shown_transcripts}`")
    lines.append("")

    summary = "\n".join(lines)
    (transcript_dir / "summary.md").write_text(summary, encoding="utf-8")

    if opts.record_baseline:
        baseline = Path(opts.baseline).resolve()
        baseline.mkdir(parents=True, exist_ok=True)
        (baseline / "results.json").write_text(
            json.dumps(payload, indent=2, sort_keys=False) + "\n",
            encoding="utf-8",
        )
        (baseline / "summary.md").write_text(summary, encoding="utf-8")
        print(f"baseline written: {baseline}")


def list_cases() -> None:
    for case in CASES:
        env = f" niu_env={case.niu_env}" if case.niu_env else ""
        known = " KNOWN" if case.known_reason else ""
        print(
            f"{case.group:<9} {case.case_id:<32} "
            f"text={case.text!r:<20} keys={case.keys_label!r}{env}{known}"
        )


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="PTY differential keyboard oracle: GNU bash/readline vs niu",
    )
    parser.add_argument(
        "--bash-bin",
        default=os.environ.get("BASH_BIN", DEFAULT_BASH),
        help="GNU bash binary (default: $BASH_BIN or %(default)s)",
    )
    parser.add_argument(
        "--niu-bin",
        default=os.environ.get("NIU_BIN", DEFAULT_NIU),
        help="niu binary (default: $NIU_BIN or %(default)s)",
    )
    parser.add_argument(
        "--work",
        default=str(SCRIPT_DIR / ".rl-oracle-work"),
        help="deterministic scratch tree (default: %(default)s)",
    )
    parser.add_argument(
        "--transcripts",
        default=str(SCRIPT_DIR / "transcripts" / "latest"),
        help="raw PTY transcript directory (default: %(default)s)",
    )
    parser.add_argument(
        "--baseline",
        default=str(SCRIPT_DIR / "baseline"),
        help="baseline directory for --record-baseline (default: %(default)s)",
    )
    parser.add_argument(
        "--record-baseline",
        action="store_true",
        help="overwrite baseline/results.json and baseline/summary.md",
    )
    parser.add_argument(
        "--filter",
        default="",
        help="only run cases whose id/group/keys label contains this text",
    )
    parser.add_argument(
        "--list",
        action="store_true",
        help="list cases and exit",
    )
    parser.add_argument(
        "--settle",
        type=float,
        default=0.40,
        help="seconds to let the editor settle after the key sequence (default: %(default)s)",
    )
    parser.add_argument(
        "--text-settle",
        type=float,
        default=0.20,
        help="seconds to settle after typing the initial text (default: %(default)s)",
    )
    parser.add_argument(
        "--dump-timeout",
        type=float,
        default=3.0,
        help="seconds to wait for the bind -x dump files (default: %(default)s)",
    )
    parser.add_argument(
        "--startup-timeout",
        type=float,
        default=12.0,
        help="seconds to wait for the first prompt (default: %(default)s)",
    )
    parser.add_argument(
        "--keep-work",
        action="store_true",
        help="keep the scratch tree instead of deleting it first",
    )
    parser.add_argument(
        "--verbose",
        action="store_true",
        help="reserved for symmetry; every case is printed",
    )
    opts = parser.parse_args(argv)
    if opts.settle < 0 or opts.text_settle < 0 or opts.dump_timeout <= 0:
        parser.error("timings must be non-negative (dump-timeout must be positive)")
    return opts


def main(argv: list[str]) -> int:
    opts = parse_args(argv)
    if opts.list:
        list_cases()
        return 0
    try:
        return run_matrix(opts)
    except KeyboardInterrupt:
        print("\ninterrupted", file=sys.stderr)
        return 130


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
