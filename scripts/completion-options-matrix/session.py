"""PTY session driver for the completion-options matrix.

A real terminal is attached, the child gets an isolated HOME/INPUTRC and a
fixed 40x24 or 80x24 window. Cursor-position requests are answered from the
VT model's live cursor instead of a hard-coded 1;1, so menu geometry matches
what the editor painted.
"""

from __future__ import annotations

import errno
import fcntl
import os
import pty
import select
import shutil
import signal
import struct
import termios
import time
from pathlib import Path

from vt import Screen

PROMPT = "__NIU_CMP__> "
PROMPT_NS = PROMPT.rstrip()
ESC = b"\x1b"
TAB = b"\t"
BACKTAB = b"\x1b[Z"
UP = b"\x1b[A"
DOWN = b"\x1b[B"
RIGHT = b"\x1b[C"
LEFT = b"\x1b[D"
HOME = b"\x1b[H"
END = b"\x1b[F"
PGUP = b"\x1b[5~"
PGDN = b"\x1b[6~"
ENTER = b"\r"
ESC_KEY = b"\x1b"
CTRL_C = b"\x03"


class SessionError(RuntimeError):
    pass


class Session:
    def __init__(self, binary: str, cwd: Path, home: Path, env: dict[str, str], cols=80, rows=24):
        self.binary = binary
        self.cwd = cwd
        self.cols = cols
        self.rows = rows
        full = {
            "PATH": "/data/data/com.termux/files/usr/bin:/usr/bin:/bin",
            "HOME": str(home),
            "TERM": "xterm-256color",
            "COLUMNS": str(cols),
            "LINES": str(rows),
            "LANG": "C.UTF-8",
            "LC_ALL": "C.UTF-8",
            "PS1": PROMPT,
            "PS2": "__NIU_CMP2__> ",
            "INPUTRC": str(home.parent / "inputrc"),
            "HISTFILE": str(home / ".history"),
            "HISTSIZE": "100",
            "HISTFILESIZE": "100",
            "TMPDIR": str(home.parent / "tmp"),
            "TMP": str(home.parent / "tmp"),
            "TEMP": str(home.parent / "tmp"),
            "XDG_CONFIG_HOME": str(home / ".config"),
            "XDG_DATA_HOME": str(home / ".local" / "share"),
            "XDG_CACHE_HOME": str(home / ".cache"),
            "NIU_NO_UPDATE_CHECK": "1",
            "CDPATH": "",
            "GLOBIGNORE": "",
        }
        full.update(env)
        self.env = full
        self.screen = Screen(rows, cols)
        self.raw = bytearray()
        self._cpr_seen = 0
        self._eof = False
        self._start()

    def _start(self):
        pid, fd = pty.fork()
        if pid == 0:
            try:
                os.chdir(self.cwd)
                os.environ.clear()
                os.environ.update(self.env)
                os.execvpe(self.binary, [self.binary, "-i"], os.environ)
            except BaseException as exc:  # pragma: no cover - child path
                try:
                    os.write(2, f"exec failed: {exc}\n".encode())
                except OSError:
                    pass
                os._exit(127)
        self.pid = pid
        self.fd = fd
        try:
            fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", self.rows, self.cols, 0, 0))
        except OSError:
            pass
        os.set_blocking(fd, False)

    def _feed(self, data: bytes):
        self.raw.extend(data)
        self.screen.feed(data)
        while self.screen.cpr_requests > self._cpr_seen:
            self._cpr_seen += 1
            try:
                os.write(
                    self.fd,
                    f"\x1b[{self.screen.row + 1};{self.screen.col + 1}R".encode(),
                )
            except OSError:
                pass

    def read_once(self, timeout: float):
        if self._eof:
            return False
        try:
            ready, _, _ = select.select([self.fd], [], [], timeout)
        except OSError:
            self._eof = True
            return False
        if not ready:
            return False
        try:
            data = os.read(self.fd, 65536)
        except OSError as exc:
            if exc.errno in (errno.EAGAIN, errno.EWOULDBLOCK):
                return False
            self._eof = True
            return False
        if not data:
            self._eof = True
            return False
        self._feed(data)
        return True

    def drain(self, seconds: float):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self.read_once(min(0.03, max(0.0, deadline - time.monotonic())))

    def settle(self, base: float = 0.18, quiet: float = 0.10, timeout: float = 3.0):
        """Read until the PTY has been quiet for `quiet` seconds."""
        deadline = time.monotonic() + timeout
        last = time.monotonic()
        while time.monotonic() < deadline:
            if self.read_once(0.02):
                last = time.monotonic()
            elif time.monotonic() - last >= quiet:
                break
        self.drain(base)

    def wait_prompt(self, timeout: float = 10.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if any(PROMPT_NS in row for row in self.screen.text()):
                self.settle()
                return
            self.read_once(0.05)
        raise SessionError(f"prompt {PROMPT!r} not seen; tail={bytes(self.raw[-500:])!r}")

    def send(self, data: bytes):
        view = memoryview(data)
        while view:
            try:
                n = os.write(self.fd, view)
            except OSError as exc:
                if exc.errno in (errno.EAGAIN, errno.EWOULDBLOCK):
                    select.select([], [self.fd], [], 0.05)
                    continue
                raise SessionError(str(exc))
            view = view[n:]

    def type_text(self, text: bytes | str):
        data = text.encode() if isinstance(text, str) else text
        self.send(data)
        self.settle()

    def key(self, data: bytes, delay: float = 0.30):
        self.send(data)
        self.settle(base=delay, quiet=delay / 2)

    def snapshot(self):
        return [row for row in self.screen.text()]

    def input_line(self) -> str:
        """The text after the prompt on the prompt row, menu marker removed."""
        for row in self.screen.text():
            idx = row.find(PROMPT_NS)
            if idx >= 0:
                text = row[idx + len(PROMPT_NS) :].strip()
                for marker in ("| ", "? "):
                    if text.startswith(marker):
                        text = text[len(marker) :]
                        break
                return text.strip()
        return ""

    def menu_rows(self) -> list[tuple[int, str]]:
        """Rows below the prompt row that are not blank."""
        prompt_row = None
        for r, row in enumerate(self.screen.text()):
            if PROMPT_NS in row:
                prompt_row = r
                break
        if prompt_row is None:
            return []
        out = []
        for r in range(prompt_row + 1, self.rows):
            text = self.screen.text()[r].strip()
            if text:
                out.append((r, text))
        return out

    def candidate_positions(self, names: list[str]):
        """{name: (row, col)} for names in the menu area (row > prompt row)."""
        found = {}
        prompt_row = 0
        for r, row in enumerate(self.screen.text()):
            if PROMPT_NS in row:
                prompt_row = r
                break
        for name in names:
            for r, row in enumerate(self.screen.text()):
                if r <= prompt_row:
                    continue
                c = row.find(name)
                if c >= 0:
                    found[name] = (r, c)
                    break
        return found

    def highlighted(self, names: list[str]) -> list[str]:
        out = []
        for name, (r, c) in self.candidate_positions(names).items():
            if self.screen.is_reversed(r, c):
                out.append(name)
        return out

    def visible_text(self) -> str:
        return "\n".join(self.screen.text())

    def close(self):
        try:
            self.send(CTRL_C)
            self.drain(0.12)
            self.send(b"exit\n")
            self.drain(0.15)
        except (OSError, SessionError):
            pass
        try:
            os.kill(self.pid, signal.SIGKILL)
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

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False


def which_binary() -> str:
    env = os.environ.get("NIU_BIN")
    if env:
        return env
    here = Path(__file__).resolve()
    root = here.parent.parent.parent
    for candidate in (root / "target" / "debug" / "niu", root / "target" / "release" / "niu"):
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return str(candidate)
    found = shutil.which("niu")
    if found:
        return found
    raise SystemExit("completion-options-matrix: no niu binary (set NIU_BIN)")
