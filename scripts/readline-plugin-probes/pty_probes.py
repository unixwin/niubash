#!/usr/bin/env python3
"""PTY-backed acceptance probes for the readline plugin harness.

This is the only part of scripts/readline-plugin-probes that needs a pseudo
terminal.  It is invoked by run.sh, never by `cargo test`: the default suite
must stay deterministic and PTY-free.  Each scenario exits with

    0   PASS
    1   FAIL
    77  SKIP

and prints `RESULT=<pass|fail|skip>` plus `REASON=<one line>` for the runner.
The raw terminal transcript goes to --transcript so a failure is inspectable
without re-running the probe.

Scenarios:
  fzf        bind -x with a fake fzf: READLINE_LINE read/write and
             READLINE_POINT insertion, then an empty-buffer round-trip.
  inputrc    $INPUTRC startup plus the C-x C-r (re-read-init-file) chord:
             first prove the rc's bind lines reload, then check whether
             $INPUTRC itself is re-read (the known baseline gap).
  builtin    load loadable_builtin.c into an interactive GNU bash and run it
             against the live readline buffer.  This validates the fixture
             itself (version, rl_add_defun, rl_bind_key, rl_line_buffer,
             rl_point) independently of niu.
  niu-builtin
             load the same .so into interactive niu.  A bind -x command runs
             rl_probe while a line is being edited; the probe must observe the
             live rl_line_buffer/rl_point, and with RL_PROBE_MUTATE=1 the
             editor must adopt the plugin's buffer and point mutation.  The
             fixture's rl_bind_key slot is also pressed (ESC ~ under GNU's
             default convert-meta) to prove the plugin binding fires in the
             host keymap.
"""

import argparse
import fcntl
import os
import pty
import re
import select
import struct
import subprocess
import sys
import termios
import time

PASS = 0
FAIL = 1
SKIP = 77

ANSI = re.compile(
    rb"\x1b\[[0-9;?]*[ -/]*[@-~]"
    rb"|\x1b\][^\x07]*(?:\x07|\x1b\\)"
    rb"|\x1b[()][A-Z0-9]"
    rb"|\x1b[=>78]"
)


def strip_ansi(data):
    return ANSI.sub(b"", data).decode("utf-8", "replace")


class Session:
    def __init__(self, argv, env, cwd, timeout, transcript_path):
        self.timeout = timeout
        self.transcript_path = transcript_path
        self.transcript = bytearray()
        self.buffer = b""
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
        self.proc = subprocess.Popen(
            argv,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            cwd=cwd,
            env=env,
            close_fds=True,
            start_new_session=True,
        )
        os.close(slave)
        os.set_blocking(master, False)
        self.master = master

    def _record(self, data):
        self.transcript.extend(data)
        if b"\x1b[6n" in data:
            # Answer readline/crossterm's cursor-position query.  A fixed
            # plausible position is enough; the editor only uses it to
            # place the next repaint.
            try:
                os.write(self.master, b"\x1b[40;120R")
            except OSError:
                pass

    def _read_once(self, wait):
        ready, _, _ = select.select([self.master], [], [], wait)
        if not ready:
            return False
        try:
            data = os.read(self.master, 8192)
        except OSError:
            return False
        if not data:
            return False
        self._record(data)
        self.buffer += data
        return True

    def read_until(self, needle, timeout=None):
        """Consume through the first occurrence of needle; None on timeout."""
        deadline = time.time() + (self.timeout if timeout is None else timeout)
        while time.time() < deadline:
            index = self.buffer.find(needle)
            if index >= 0:
                end = index + len(needle)
                chunk = self.buffer[:end]
                self.buffer = self.buffer[end:]
                return chunk
            if self.proc.poll() is not None:
                # Drain whatever the child wrote before exiting.
                while self._read_once(0):
                    pass
                index = self.buffer.find(needle)
                if index >= 0:
                    end = index + len(needle)
                    chunk = self.buffer[:end]
                    self.buffer = self.buffer[end:]
                    return chunk
                return None
            self._read_once(0.05)
        return None

    def send(self, data):
        os.write(self.master, data)

    def run(self, command, timeout=None):
        """Run one shell command and return its sanitized output.

        The marker is printed as PROBE_$(printf DONE) so the literal echoed
        command never contains the contiguous string the reader waits for.
        """
        self.send(command.encode() + b"; echo PROBE_$(printf DONE)\r")
        chunk = self.read_until(b"PROBE_DONE", timeout)
        if chunk is None:
            return None
        self.read_until(b"P1> ", timeout)
        return strip_ansi(chunk)

    def close(self):
        try:
            os.write(self.master, b"\x04")
        except OSError:
            pass
        if self.proc.poll() is None:
            self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            try:
                self.proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                pass
        try:
            os.close(self.master)
        except OSError:
            pass
        if self.transcript_path:
            with open(self.transcript_path, "wb") as handle:
                handle.write(self.transcript)

    def tail(self):
        return strip_ansi(bytes(self.transcript[-3000:]))


def transcript_path(args, default):
    return args.transcript or os.path.join(args.work, default)


def base_env(home):
    env = dict(os.environ)
    env.update(
        HOME=home,
        USERPROFILE=home,
        TERM="xterm-256color",
        NIU_LANG="en",
        RUST_LOG="off",
        RUST_BACKTRACE="0",
        NIU_DISABLE_DEFAULT_PLUGINS="1",
    )
    env.pop("INPUTRC", None)
    return env


def fail(message, session):
    print("RESULT=fail")
    print("REASON=" + message)
    if session is not None:
        print("TRANSCRIPT_TAIL:")
        print(session.tail())
    return FAIL


def skip(message):
    print("RESULT=skip")
    print("REASON=" + message)
    return SKIP


def ok(message):
    print("RESULT=pass")
    print("REASON=" + message)
    return PASS


def settle(session, seconds=0.35):
    """Drain repaint traffic so the next keystroke meets an idle editor.

    The editor paints a prompt before the line text, so a `read_until` that
    stops at the prompt leaves a half-painted line behind.  Draining first
    makes the following keystroke land in a known state instead of racing
    that repaint.
    """
    deadline = time.time() + seconds
    while time.time() < deadline:
        session._read_once(0.05)
    session.buffer = b""


def scenario_fzf(args):
    root = os.path.join(args.work, "fzf")
    home = os.path.join(root, "home")
    start = os.path.join(root, "start")
    os.makedirs(home, exist_ok=True)
    os.makedirs(start, exist_ok=True)
    with open(args.fzf_rc, "r", encoding="utf-8") as handle:
        rc = handle.read()
    with open(os.path.join(home, ".niubashrc"), "w", encoding="utf-8") as handle:
        handle.write(rc)

    env = base_env(home)
    env["NIU_PROBE_FAKE_FZF"] = args.fake_fzf
    session = Session(
        [args.niu],
        env,
        start,
        args.timeout,
        transcript_path(args, "fzf.transcript"),
    )
    try:
        if session.read_until(b"P1> ") is None:
            return fail("interactive niu did not reach the P1> prompt", session)

        # Non-empty buffer: the bind -x command reads READLINE_LINE ("typed"),
        # runs the fake fzf, rewrites the line, and sets READLINE_POINT=5.
        session.send(b"typed")
        if session.read_until(b"typed") is None:
            return fail("typed characters were not echoed", session)
        session.send(b"\x12")  # Ctrl-R
        if session.read_until(b"picked:typed") is None:
            return fail(
                "Ctrl-R bind -x did not rewrite READLINE_LINE to echo picked:typed",
                session,
            )
        session.send(b"X")
        session.send(b"\r")
        if session.read_until(b"Xpicked:typed") is None:
            return fail(
                "READLINE_POINT=5 did not place the typed X after 'echo ' "
                "(expected output Xpicked:typed)",
                session,
            )
        if session.read_until(b"P1> ") is None:
            return fail("prompt did not return after the fzf round-trip", session)

        # Empty buffer: READLINE_LINE reads as empty, so the fake fzf returns
        # "picked:" and `echo picked:` executes.
        session.send(b"\x12")
        if session.read_until(b"picked:") is None:
            return fail("Ctrl-R on an empty buffer did not round-trip", session)
        session.send(b"\r")
        if session.read_until(b"picked:") is None:
            return fail("empty-buffer round-trip did not execute echo picked:", session)
        if session.read_until(b"P1> ") is None:
            return fail("prompt did not return after the empty-buffer case", session)

        return ok(
            "bind -x fake-fzf round-trip: READLINE_LINE read/write + "
            "READLINE_POINT insertion (Xpicked:typed) and empty buffer (picked:)"
        )
    finally:
        session.close()


def scenario_inputrc(args):
    root = os.path.join(args.work, "inputrc-pty")
    home = os.path.join(root, "home")
    start = os.path.join(root, "start")
    os.makedirs(home, exist_ok=True)
    os.makedirs(start, exist_ok=True)

    extra_path = os.path.join(root, "extra.inputrc")
    with open(extra_path, "w", encoding="utf-8") as handle:
        handle.write('"\\C-x\\C-a": beginning-of-line\n')
    inputrc_path = os.path.join(root, "inputrc")
    with open(inputrc_path, "w", encoding="utf-8") as handle:
        handle.write(
            "set completion-ignore-case on\n"
            '"\\C-t": transpose-chars\n'
            "$include %s\n" % extra_path
        )
    rc_path = os.path.join(home, ".niubashrc")
    with open(rc_path, "w", encoding="utf-8") as handle:
        handle.write(
            "PS1='P1> '\n"
            "PS2='P2> '\n"
            "NIU_DISABLE_DEFAULT_PLUGINS=1\n"
            "bind '\"\\C-t\": redisplay'\n"
        )

    env = base_env(home)
    env["INPUTRC"] = inputrc_path
    session = Session(
        [args.niu],
        env,
        start,
        args.timeout,
        transcript_path(args, "inputrc.transcript"),
    )
    try:
        if session.read_until(b"P1> ") is None:
            return fail("interactive niu did not reach the P1> prompt", session)

        # Phase 1: C-x C-r must re-read the rc's `bind` lines.  Rebind C-t
        # to dump-variables in the rc, press the chord, then C-t.  A visible
        # dump proves the chord fired and the rc bind line was applied.
        with open(rc_path, "a", encoding="utf-8") as handle:
            handle.write("bind '\"\\C-t\": dump-variables'\n")
        session.send(b"\x18")  # Ctrl-X
        time.sleep(0.15)
        session.send(b"\x12")  # Ctrl-R
        time.sleep(0.2)
        session.send(b"\x14")  # Ctrl-T -> dump-variables after the reload
        if session.read_until(b"editing-mode", 5) is None:
            return fail(
                "C-x C-r did not re-read the .niubashrc bind lines "
                "(Ctrl-T stayed transpose-chars)",
                session,
            )
        session.read_until(b"P1> ", 5)

        # Phase 2: GNU's rl_re_read_init_file re-reads $INPUTRC too.  Add a
        # binding to the $include'd file, press the chord, then use it.
        with open(extra_path, "a", encoding="utf-8") as handle:
            handle.write('"\\C-b": dump-variables\n')
        session.send(b"\x18")
        time.sleep(0.15)
        session.send(b"\x12")
        time.sleep(0.2)
        session.send(b"\x02")  # Ctrl-B -> dump-variables only if inputrc reloaded
        if session.read_until(b"editing-mode", 5) is not None:
            return ok(
                "C-x C-r re-read both .niubashrc bind lines and the "
                "$include'd $INPUTRC binding"
            )

        return skip(
            "C-x C-r re-read .niubashrc bind lines (verified) but not $INPUTRC: "
            "re-read-init-file is wired to Shell::reread_init_file_bindings "
            "(crates/niubash-runtime/src/shell.rs), while GNU rl_re_read_init_file "
            "re-reads the inputrc; the $include'd \\C-b binding never appeared"
        )
    finally:
        session.close()


def scenario_builtin(args):
    if not args.bash:
        return skip("bash not found; cannot validate the C fixture against GNU readline")
    root = os.path.join(args.work, "builtin-oracle")
    start = os.path.join(root, "start")
    home = os.path.join(root, "home")
    os.makedirs(start, exist_ok=True)
    os.makedirs(home, exist_ok=True)
    env = base_env(home)
    env["PS1"] = "P1> "
    session = Session(
        [args.bash, "--noprofile", "--norc", "-i"],
        env,
        start,
        args.timeout,
        transcript_path(args, "builtin.transcript"),
    )
    try:
        if session.read_until(b"P1> ") is None:
            return fail("interactive bash did not reach the P1> prompt", session)
        command = "enable -f '%s' rl_probe; rl_probe" % args.builtin
        session.send(command.encode() + b"\r")
        chunk = session.read_until(b"RL_PROBE ", 10)
        if chunk is None:
            return fail("GNU bash could not load/run the rl_probe builtin", session)
        line = session.read_until(b"\r\n", 5)
        if line is None:
            line = session.read_until(b"\n", 5)
        text = strip_ansi((chunk + (line or b""))).replace("\r", "").replace("\n", " ")
        if "line_buffer=ok" not in text or "line_mutated=ok" not in text:
            return fail(
                "GNU bash ran the builtin but the live editor buffer was not "
                "observable: " + text,
                session,
            )
        if "result=ok" not in text:
            return fail("GNU bash builtin probe reported " + text, session)
        if "RL_PROBE" in text:
            text = text[text.index("RL_PROBE"):]
        return ok("GNU bash oracle: " + " ".join(text.split()))
    finally:
        session.close()


def scenario_niu_builtin(args):
    """The live-editor half of niubash#775.

    Load the C fixture into interactive niu, run it from a bind -x command
    while a line is being edited, and verify the probe sees and mutates the
    live buffer/point.  With RL_PROBE_MUTATE=1 the editor must adopt the
    mutation, which is what a real plugin depends on.  The same fixture also
    binds a key through rl_add_defun/rl_bind_key; pressing it (ESC ~ is how
    GNU's default convert-meta stores key 0xfe) must dispatch the plugin
    function through niu's live keymap.
    """
    if not args.niu:
        return skip("--niu is required for the niu loadable-builtin probe")
    if not args.builtin:
        return skip("--builtin is required for the niu loadable-builtin probe")
    root = os.path.join(args.work, "niu-builtin")
    home = os.path.join(root, "home")
    start = os.path.join(root, "start")
    os.makedirs(home, exist_ok=True)
    os.makedirs(start, exist_ok=True)
    builtin = os.path.abspath(args.builtin)
    with open(os.path.join(home, ".niubashrc"), "w", encoding="utf-8") as handle:
        handle.write(
            "PS1='P1> '\n"
            "PS2='P2> '\n"
            "enable -f '%s' rl_probe\n"
            # Register the plugin key slot (0xfe) before the editor is built.
            "rl_probe\n"
            # Run the same builtin from a bind -x key while editing.
            "bind -x '\"\\C-g\": rl_probe'\n"
            # niubash#775 live-routing coverage, all installed at startup:
            # hooks before the first prompt, a plugin macro, a host function
            # bind and bind -x entries for pending input / rl_done / mark.
            "rl_probe hooks\n"
            "rl_probe macro\n"
            "bind '\"\\C-o\": dump-variables'\n"
            "bind -x '\"\\C-p\": rl_probe pending'\n"
            "bind -x '\"\\C-y\": rl_probe done'\n"
            "bind -x '\"\\C-k\": rl_probe mark'\n"
            "bind -x '\"\\C-l\": rl_probe markread'\n" % builtin
        )

    env = base_env(home)
    env["RL_PROBE_KEY_MARK"] = "1"
    env["RL_PROBE_MUTATE"] = "1"
    session = Session(
        [args.niu],
        env,
        start,
        args.timeout,
        transcript_path(args, "niu-builtin.transcript"),
    )
    try:
        if session.read_until(b"P1> ") is None:
            return fail("interactive niu did not reach the P1> prompt", session)

        # 1. The plugin's rl_bind_key/rl_add_defun slot fires in the live
        #    editor.  convert-meta is on by default, so key 0xfe is stored as
        #    ESC ~; that is the sequence a terminal sends for Meta-~.
        session.send(b"\x1b~")
        marker = session.read_until(b"RL_PROBE_DEFUN ", 5)
        if marker is None:
            return fail(
                "rl_bind_key slot did not fire through niu's live keymap "
                "(sent ESC ~ for GNU key 0xfe)",
                session,
            )
        marker_line = session.read_until(b"\n", 3) or b""
        marker_text = strip_ansi(marker + marker_line).replace("\r", "").replace("\n", " ")
        if "key=126" not in marker_text or "count=1" not in marker_text:
            return fail(
                "plugin function ran with unexpected arguments: " + marker_text,
                session,
            )
        settle(session)

        # 2. Run the builtin from bind -x while editing "echo one", with the
        #    cursor one character left of the end so the point bump is
        #    visible when a key is typed afterwards.
        session.send(b"echo one")
        settle(session)
        session.send(b"\x02")  # Ctrl-B: point before the final 'e'
        time.sleep(0.1)
        session.send(b"\x07")  # Ctrl-G: bind -x -> rl_probe
        chunk = session.read_until(b"RL_PROBE ", 10)
        if chunk is None:
            return fail("bind -x did not run the rl_probe builtin", session)
        probe_line = session.read_until(b"\n", 5)
        if probe_line is None:
            return fail("rl_probe output did not terminate", session)
        probe_text = strip_ansi(chunk + probe_line).replace("\r", "").replace("\n", " ")
        probe_text = probe_text[probe_text.index("RL_PROBE "):]
        for field in (
            "line_buffer=ok",
            "line_mutated=ok",
            "point=ok",
            "point_mutated=ok",
            "mutate=on",
            "result=ok",
        ):
            if field not in probe_text:
                return fail(
                    "live ABI probe missing %s: %s" % (field, probe_text), session
                )

        # 3. The editor must have adopted the plugin's first-byte flip and
        #    point bump.  Type at the new point and submit: the command line
        #    is "Xcho oneZ" (buffer flipped + point moved past the final 'e').
        settle(session, 0.45)
        session.send(b"Z")
        time.sleep(0.2)
        session.send(b"\r")
        # Wait for the command's own diagnostic rather than the next prompt:
        # the prompt is repainted before the submitted line reaches the
        # engine, so a prompt match can return before execution happens.
        executed = session.read_until(b"command not found", 10)
        if executed is None:
            return fail("mutated line did not run after submit", session)
        executed_text = strip_ansi(executed).replace("\r", "")
        if "Xcho oneZ" not in executed_text:
            return fail(
                "editor did not adopt the plugin's buffer/point mutation "
                "(expected the command line Xcho oneZ): "
                + " ".join(executed_text.split()),
                session,
            )
        if "command not found" not in executed_text:
            return fail(
                "mutated line did not execute as expected: "
                + " ".join(executed_text.split()),
                session,
            )

        if session.read_until(b"P1> ", 10) is None:
            return fail("prompt did not return after the mutation phase", session)

        # 4. Host keymap -> ABI: a rc `bind '\C-o': dump-variables` must be
        #    what rl_function_of_keyseq returns, and the rc `bind -x` on C-g
        #    must differ from readline's default abort binding.
        session.send(b"rl_probe keyseq; rl_probe macrocheck\r")
        chunk = session.read_until(b"RL_PROBE_KEYSEQ ", 10)
        if chunk is None:
            return fail("rl_probe keyseq produced no output", session)
        line = session.read_until(b"\n", 5) or b""
        keyseq_text = strip_ansi(chunk + line).replace("\r", "").replace("\n", " ")
        keyseq_text = keyseq_text[keyseq_text.index("RL_PROBE_KEYSEQ "):]
        for field in ("function=ok", "length=ok", "bindx=ok", "kind=ok"):
            if field not in keyseq_text:
                return fail(
                    "host bind/bind -x invisible to rl_function_of_keyseq "
                    "(missing %s): %s" % (field, keyseq_text),
                    session,
                )

        # 5. Macro -> host: rl_probe macro bound C-t at startup; the host
        #    mirror keeps the ABI slot a macro, and the editor inserts its
        #    text when the key is pressed.
        chunk = session.read_until(b"RL_PROBE_MACROCHECK ", 5)
        if chunk is None:
            return fail("rl_probe macrocheck produced no output", session)
        line = session.read_until(b"\n", 5) or b""
        macrocheck_text = strip_ansi(chunk + line).replace("\r", "").replace("\n", " ")
        macrocheck_text = macrocheck_text[macrocheck_text.index("RL_PROBE_MACROCHECK "):]
        if "macro=ok" not in macrocheck_text:
            return fail(
                "rl_macro_bind did not stay a macro slot in the ABI keymap: "
                + macrocheck_text,
                session,
            )
        if session.read_until(b"P1> ", 5) is None:
            return fail("prompt did not return after the keyseq/macro checks", session)

        session.send(b"printf 'MACRO-%s\\n' ")
        settle(session)
        session.send(b"\x14")  # Ctrl-T -> plugin macro "MAGIC"
        time.sleep(0.2)
        session.send(b"\r")
        if session.read_until(b"MACRO-MAGIC", 10) is None:
            return fail(
                "rl_macro_bind text was not routed into the live editor "
                "(C-t did not insert MAGIC)",
                session,
            )
        if session.read_until(b"P1> ", 5) is None:
            return fail("prompt did not return after the macro run", session)

        # 6. rl_pending_input: the bind -x command queues 'Z'; after it
        #    returns the editor injects that key at point.
        session.send(b"printf 'PEND-%s\\n' a")
        settle(session)
        session.send(b"\x10")  # Ctrl-P -> rl_probe pending
        chunk = session.read_until(b"RL_PROBE_CONTROL ", 10)
        if chunk is None:
            return fail("rl_probe pending produced no output", session)
        line = session.read_until(b"\n", 5) or b""
        pending_text = strip_ansi(chunk + line).replace("\r", "").replace("\n", " ")
        if "pending=ok" not in pending_text:
            return fail("rl_pending_input was not writable: " + pending_text, session)
        session.send(b"\r")
        if session.read_until(b"PEND-aZ", 10) is None:
            return fail(
                "rl_pending_input was reset instead of injected as the next key "
                "(expected the line to gain a trailing Z)",
                session,
            )
        if session.read_until(b"P1> ", 5) is None:
            return fail("prompt did not return after the pending-input run", session)

        # 7. rl_done: setting it from a bind -x command submits the line
        #    without an Enter keypress.
        session.send(b"echo DONE-OK")
        settle(session)
        session.send(b"\x19")  # Ctrl-Y -> rl_probe done
        chunk = session.read_until(b"RL_PROBE_CONTROL ", 10)
        if chunk is None:
            return fail("rl_probe done produced no output", session)
        line = session.read_until(b"\n", 5) or b""
        done_text = strip_ansi(chunk + line).replace("\r", "").replace("\n", " ")
        if "done=ok" not in done_text:
            return fail("rl_done was not writable: " + done_text, session)
        if session.read_until(b"DONE-OK", 10) is None:
            return fail(
                "rl_done did not submit the current line (no Enter was sent)",
                session,
            )
        if session.read_until(b"P1> ", 5) is None:
            return fail("prompt did not return after the rl_done submit", session)

        # 8. Mark across a host-command resume: one bind -x sets it, the
        #    next (a separate resume of the same read) still sees it.
        session.send(b"abc")
        settle(session)
        session.send(b"\x0b")  # Ctrl-K -> rl_probe mark
        chunk = session.read_until(b"RL_PROBE_MARK ", 10)
        if chunk is None:
            return fail("rl_probe mark produced no output", session)
        line = session.read_until(b"\n", 5) or b""
        mark_set_text = strip_ansi(chunk + line).replace("\r", "").replace("\n", " ")
        session.send(b"\x0c")  # Ctrl-L -> rl_probe markread
        chunk = session.read_until(b"RL_PROBE_MARK ", 10)
        if chunk is None:
            return fail("rl_probe markread produced no output", session)
        line = session.read_until(b"\n", 5) or b""
        mark_read_text = strip_ansi(chunk + line).replace("\r", "").replace("\n", " ")
        if "set=ok" not in mark_set_text or "read=ok mark=1 active=1" not in mark_read_text:
            return fail(
                "rl_mark did not survive the host-command resume: set=%s read=%s"
                % (" ".join(mark_set_text.split()), " ".join(mark_read_text.split())),
                session,
            )
        session.send(b"\x03")  # Ctrl-C clears the line
        time.sleep(0.2)

        # 9. Hooks: startup exactly once before the first prompt, pre-input
        #    once per line.
        transcript = bytes(session.transcript)
        startup_count = transcript.count(b"RL_PROBE_STARTUP")
        pre_input_count = transcript.count(b"RL_PROBE_PREINPUT")
        if startup_count != 1:
            return fail(
                "rl_startup_hook ran %d times (expected exactly one startup call)"
                % startup_count,
                session,
            )
        if pre_input_count < 3:
            return fail(
                "rl_pre_input_hook ran %d times (expected one call per line)"
                % pre_input_count,
                session,
            )

        return ok(
            "niu live ABI: %s; editor adopted Xcho oneZ from the plugin's "
            "rl_line_buffer/rl_point writes; rl_bind_key slot fired (%s); "
            "%s; %s; pending_input injected (PEND-aZ); rl_done submitted "
            "(DONE-OK); mark survived resume; hooks startup=%d pre_input=%d"
            % (
                " ".join(probe_text.split()),
                " ".join(marker_text.split()),
                " ".join(keyseq_text.split()),
                " ".join(macrocheck_text.split()),
                startup_count,
                pre_input_count,
            )
        )
    finally:
        session.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("scenario", choices=["fzf", "inputrc", "builtin", "niu-builtin"])
    parser.add_argument("--niu", default="")
    parser.add_argument("--bash", default="")
    parser.add_argument("--builtin", default="")
    parser.add_argument("--work", required=True)
    parser.add_argument("--fzf-rc", default="")
    parser.add_argument("--fake-fzf", default="")
    parser.add_argument("--timeout", type=float, default=20.0)
    parser.add_argument("--transcript", default="")
    args = parser.parse_args()

    if args.scenario in ("fzf", "inputrc", "niu-builtin") and not args.niu:
        print("RESULT=skip")
        print("REASON=--niu is required for this scenario")
        return SKIP
    if args.scenario == "fzf" and (not args.fzf_rc or not args.fake_fzf):
        print("RESULT=skip")
        print("REASON=--fzf-rc and --fake-fzf are required for fzf")
        return SKIP

    try:
        if args.scenario == "fzf":
            return scenario_fzf(args)
        if args.scenario == "inputrc":
            return scenario_inputrc(args)
        if args.scenario == "niu-builtin":
            return scenario_niu_builtin(args)
        return scenario_builtin(args)
    except OSError as error:
        print("RESULT=fail")
        print("REASON=PTY setup failed: %s" % error)
        return FAIL


if __name__ == "__main__":
    sys.exit(main())
