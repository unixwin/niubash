#!/usr/bin/env python3
"""Completion-options matrix: every documented niu completion knob, on a PTY.

The assertions are frame-level (input line, grid rows, reverse-video selection,
descriptions), not buffer-only. Each case starts a fresh shell with an isolated
HOME/INPUTRC, a fixed window, and a deterministic fixture tree.

Status policy
  PASS    the documented contract holds
  FAIL    the shell does something else; expected vs actual is recorded
  MISSING the option is documented/claimed but not exposed at all
  SKIP    external precondition absent (currently unused)

Run: scripts/completion-options-matrix/run.sh [--filter SUBSTR] [--record-baseline]
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

from session import (
    BACKTAB,
    DOWN,
    ENTER,
    ESC_KEY,
    HOME,
    LEFT,
    PGDN,
    PGUP,
    RIGHT,
    TAB,
    UP,
    CTRL_C,
    Session,
    SessionError,
    which_binary,
)

SCRIPT_DIR = Path(__file__).resolve().parent
DEFAULT_WORK = SCRIPT_DIR / ".work"
BASELINE = SCRIPT_DIR / "baseline"

STYLE_DOC = "docs/src/completion.md#completion-styles"
CONFIG_DOC = "docs/src/configuration.md#the-three-sources"
SELECT_DOC = "docs/src/advanced-usage.md#completion-styles"
EXT_DOC = "docs/src/external-completion-providers.md"

# zsh source anchors for the menu-select contract (local checkout zsh 5.9.2):
ZSH_COMPLIST = "/data/data/com.termux/files/home/refs/zsh/Src/Zle/complist.c"
ZSH_YO = "/data/data/com.termux/files/home/refs/zsh/Doc/Zsh/mod_complist.yo"
OMZ_COMPLETION = "/data/data/com.termux/files/home/refs/oh-my-zsh/lib/completion.zsh"

CANDIDATES = [f"cmp-{x}{x}" for x in "abcdef"]
LONE = "only-dir"


@dataclass
class Case:
    id: str
    group: str
    source: str
    expect: str
    run: Callable[["Env"], tuple[bool, str]]
    note: str = ""
    issue: str = ""


@dataclass
class Env:
    binary: str
    work_root: Path
    case: Case
    session_env: dict[str, str] = field(default_factory=dict)
    cols: int = 80
    rc: str = ""

    @property
    def root(self) -> Path:
        return self.work_root / self.case.id.replace("/", "__")

    @property
    def home(self) -> Path:
        return self.root / "home"

    @property
    def work(self) -> Path:
        return self.root / "work"

    def prepare(self, dirs=(), files=(), rc: str | None = None):
        if self.root.exists():
            shutil.rmtree(self.root)
        for d in (self.home, self.work, self.root / "tmp"):
            d.mkdir(parents=True, exist_ok=True)
        (self.home / ".history").write_text("")
        (self.root / "inputrc").write_text(
            "set editing-mode emacs\nset bell-style none\nset keyseq-timeout 500\n"
            "set completion-ignore-case off\nset page-completions off\n"
        )
        for name in dirs:
            (self.work / name).mkdir(parents=True, exist_ok=True)
        for name in files:
            (self.work / name).write_text("")
        (self.home / ".niubashrc").write_text(rc if rc is not None else self.rc)

    def session(self, extra_env: dict[str, str] | None = None) -> Session:
        env = dict(self.session_env)
        if extra_env:
            env.update(extra_env)
        return Session(self.binary, self.work, self.home, env, cols=self.cols, rows=24)

    def config_rows(self, extra_env: dict[str, str] | None = None) -> str:
        env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": str(self.home),
            "TERM": "xterm-256color",
            "LANG": "C.UTF-8",
            "LC_ALL": "C.UTF-8",
            "NIU_NO_UPDATE_CHECK": "1",
        }
        env.update(self.session_env)
        if extra_env:
            env.update(extra_env)
        proc = subprocess.run(
            [self.binary, "config"],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=15,
            check=False,
        )
        return (proc.stdout + proc.stderr).decode("utf-8", "replace")


# ── assertions ───────────────────────────────────────────────────────────
def verdict(ok: bool, expected: str, actual: str) -> tuple[bool, str]:
    return ok, f"expected: {expected}; actual: {actual}"


def first_highlight(sess: Session, names: list[str]) -> list[str]:
    return sess.highlighted(names)


def line_is(sess: Session, expected: str) -> tuple[bool, str]:
    actual = sess.input_line()
    return verdict(actual == expected, repr(expected), repr(actual))


def all_of(*parts):
    ok = True
    details = []
    for part_ok, detail in parts:
        ok = ok and part_ok
        details.append(detail)
    return ok, " | ".join(details)


# ── setup helpers ────────────────────────────────────────────────────────
def select_env(style: str | None = None, interaction: str | None = None):
    env = {}
    if style is not None:
        env["NIU_COMPLETION_STYLE"] = style
    if interaction is not None:
        env["NIU_COMPLETION_INTERACTION"] = interaction
    return env


RC_DESCRIPTIONS = r'''_mytool_comp() {
  NIU_COMP_RESULT="$(printf 'start\tdesc-start\nstop\tdesc-stop\nstatus\tdesc-status')"
}
NIU_COMPDEFS="mytool:_mytool_comp"
'''

RC_GIT_COMPDEF = r'''_niu_git_comp() {
  NIU_COMP_RESULT="$(printf 'defonly-1\ndefonly-2')"
}
NIU_COMPDEFS="git:_niu_git_comp"
'''

RC_GIT_COMPSPEC = "complete -W 'spec-one spec-two' git\n"
RC_GIT_BOTH = RC_GIT_COMPSPEC + RC_GIT_COMPDEF


def run_candidates(env: Env, keys: bytes, initial: bytes = b"cd cmp-"):
    with env.session() as sess:
        sess.wait_prompt()
        sess.type_text(initial)
        if keys:
            for part in split_keys(keys):
                sess.key(part)
        return sess.input_line(), list(sess.snapshot()), sess.visible_text(), bytes(sess.raw)


def split_keys(keys: bytes) -> list[bytes]:
    """Split a key sequence into individually-settled keys."""
    known = [BACKTAB, UP, DOWN, RIGHT, LEFT, HOME, PGDN, PGUP, TAB, ENTER, ESC_KEY]
    out = []
    i = 0
    while i < len(keys):
        for seq in known:
            if keys.startswith(seq, i):
                out.append(seq)
                i += len(seq)
                break
        else:
            out.append(keys[i : i + 1])
            i += 1
    return out


# ── case builders ────────────────────────────────────────────────────────
def cases() -> list[Case]:
    out: list[Case] = []

    def add(case: Case):
        out.append(case)

    def env_for(c: Case) -> Env:
        return Env(BINARY, WORK_ROOT, c)

    # ── defaults / option inventory ──────────────────────────────────
    def default_rows(c: Case):
        e = env_for(c)
        e.prepare()
        text = e.config_rows()
        ok = (
            "completion.style = column (default)" in text
            and "completion.interaction = select (default)" in text
        )
        return verdict(ok, "completion.style = column (default); completion.interaction = select (default)", text.strip())

    add(Case("defaults/config_rows", "defaults", CONFIG_DOC,
             "built-in default is column + select", default_rows))

    def default_select_tab1(c: Case):
        e = env_for(c)
        e.prepare(dirs=CANDIDATES + [LONE])
        with e.session() as s:
            s.wait_prompt()
            s.type_text("cd cmp-")
            s.key(TAB)
            line = s.input_line()
            hi = first_highlight(s, CANDIDATES)
            ok = line == "cd cmp-aa/" and hi == ["cmp-aa"]
            return verdict(ok, "line 'cd cmp-aa/' and first cell cmp-aa highlighted",
                           f"line {line!r}, highlighted {hi!r}")
    add(Case("defaults/select_tab1_first_candidate", "defaults",
             "config.rs CompletionInteraction::Select (owner ruling #742/#778); zsh "
             f"complist.c:2813 `mselect = (*(minfo.cur))->gnum` (first match starts selected)",
             "TAB #1 splices first candidate and highlights it", default_select_tab1,
             issue="#778"))

    def default_select_tab2(c: Case):
        e = env_for(c)
        e.prepare(dirs=CANDIDATES)
        with e.session() as s:
            s.wait_prompt()
            s.type_text("cd cmp-")
            s.key(TAB)
            s.key(TAB)
            line = s.input_line()
            hi = first_highlight(s, CANDIDATES)
            ok = line == "cd cmp-bb/" and hi == ["cmp-bb"]
            return verdict(ok, "line 'cd cmp-bb/' and cell cmp-bb highlighted",
                           f"line {line!r}, highlighted {hi!r}")
    add(Case("defaults/select_tab2_cycles", "defaults", f"{STYLE_DOC}; zsh {ZSH_YO} "
             "movement wraps", "second TAB moves to the second candidate",
             default_select_tab2, issue="#778"))

    # ── menu-select movement (zsh bindings) ─────────────────────────
    def move_case(name: str, keys: bytes, expected: str, source: str, note: str = ""):
        def run(c: Case):
            e = Env(BINARY, WORK_ROOT, c)
            e.session_env = select_env("column", "select")
            e.cols = 40
            e.prepare(dirs=CANDIDATES)
            with e.session() as s:
                s.wait_prompt()
                s.type_text("cd cmp-")
                s.key(TAB)
                for part in split_keys(keys):
                    s.key(part)
                line = s.input_line()
                hi = first_highlight(s, CANDIDATES)
                ok = line == expected and len(hi) == 1 and hi[0] in expected
                return verdict(ok, f"line {expected!r} with that candidate highlighted",
                               f"line {line!r}, highlighted {hi!r}")
        add(Case(name, "select", source, f"line {expected!r}", run, note=note, issue="#778"))

    move_case("select/shift_tab_previous", BACKTAB, "cd cmp-ff/",
              f"zsh {ZSH_COMPLIST} menuselect_bindings: TAB = complete-word; "
              f"{ZSH_YO} movement wraps at edges")
    move_case("select/down_one_row", DOWN, "cd cmp-bb/",
              f"zsh {ZSH_COMPLIST} menuselect_bindings: Down = down-line-or-history; "
              f"{ZSH_YO} movement wraps")
    move_case("select/up_one_row_wraps", UP, "cd cmp-ff/",
              f"zsh {ZSH_COMPLIST}: Up = up-line-or-history; {ZSH_YO} wrap")
    move_case("select/right_one_column", RIGHT, "cd cmp-cc/",
              f"zsh {ZSH_COMPLIST}: Right = forward-char; {ZSH_YO} wrap; column-major "
              "grid row stride = 2 for 6 candidates at 40 columns")
    move_case("select/left_one_column_wraps", LEFT, "cd cmp-ee/",
              f"zsh {ZSH_COMPLIST}: Left = backward-char; {ZSH_YO} wrap")

    def select_enter(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = select_env("column", "select")
        e.cols = 40
        e.prepare(dirs=CANDIDATES)
        with e.session() as s:
            s.wait_prompt()
            s.type_text("cd cmp-")
            s.key(TAB)
            s.key(ENTER)
            s.type_text("basename \"$PWD\"\n")
            text = s.visible_text()
            ok = "cmp-aa" in text
            shown = [ln for ln in text.splitlines() if ln.strip()][-4:]
            return verdict(ok, "Enter after TAB runs cd into cmp-aa", shown)
    add(Case("select/enter_accept_runs", "select",
             f"zsh {ZSH_COMPLIST} menuselect_bindings: Enter = accept-line", "Enter accepts and runs",
             select_enter, issue="#778"))

    def select_esc(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = select_env("column", "select")
        e.cols = 40
        e.prepare(dirs=CANDIDATES)
        with e.session() as s:
            s.wait_prompt()
            s.type_text("cd cmp-")
            s.key(TAB)
            s.key(TAB)
            before = s.input_line()
            s.key(ESC_KEY)
            after = s.input_line()
            ok = after == "cd cmp-" and before != after
            return verdict(ok, "Esc restores pre-completion 'cd cmp-'", f"before {before!r}, after {after!r}")
    add(Case("select/esc_rollback", "select",
             "docs/src/completion.md two-step rule; niu CycleSession rollback contract",
             "Esc rolls the line back", select_esc, issue="#778"))

    def select_lone(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = select_env("column", "select")
        e.cols = 80
        e.prepare(dirs=[LONE])
        with e.session() as s:
            s.wait_prompt()
            s.type_text("cd only-")
            s.key(TAB)
            line = s.input_line()
            ok = line == "cd only-dir/"
            return verdict(ok, "lone candidate completes outright 'cd only-dir/'", repr(line))
    add(Case("select/lone_candidate", "select", f"{STYLE_DOC} two-step rule",
             "lone candidate completes outright", select_lone))

    def select_bare_tab(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = select_env("column", "select")
        e.prepare(files=["alpha.txt", "alpine.txt"])
        with e.session() as s:
            s.wait_prompt()
            s.key(TAB)
            line = s.input_line()
            menu = s.menu_rows()
            raw = bytes(s.raw)
            indented = b"    \x1b7" in raw[-120:]
            ok = line.strip() == "" and indented and not menu
            return verdict(ok, "four-space indent, no candidate grid",
                           f"line {line!r}, indent-paint={'yes' if indented else 'no'}, menu {menu!r}")
    add(Case("select/bare_tab_indent", "select", "config.rs BARE_TAB_INDENT (#742)",
             "blank-line TAB inserts four spaces", select_bare_tab))

    def select_grid_geometry(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = select_env("column", "select")
        e.cols = 40
        e.prepare(dirs=CANDIDATES)
        with e.session() as s:
            s.wait_prompt()
            s.type_text("cd cmp-")
            s.key(TAB)
            pos = s.candidate_positions(CANDIDATES)
            rows = sorted({r for r, _ in pos.values()})
            by_row = {}
            for name, (r, _c) in pos.items():
                by_row.setdefault(r, []).append(name)
            for r in by_row:
                by_row[r].sort(key=lambda n: pos[n][1])
            expected = ["cmp-aa", "cmp-cc", "cmp-ee"]
            expected2 = ["cmp-bb", "cmp-dd", "cmp-ff"]
            actual = [by_row.get(rows[0], []), by_row.get(rows[1], [])] if len(rows) >= 2 else [by_row]
            ok = actual == [expected, expected2]
            return verdict(ok, f"column-major grid rows {expected} / {expected2}", str(actual))
    add(Case("select/grid_column_major", "select",
             "gnu_menu.rs packed_column_layout (zsh list-packed geometry)",
             "6 candidates at 40 cols form 3x2 column-major grid", select_grid_geometry))

    def select_lscolors(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = select_env("column", "select")
        e.session_env["LS_COLORS"] = "di=01;35"
        e.cols = 40
        e.prepare(dirs=CANDIDATES)
        with e.session() as s:
            s.wait_prompt()
            s.type_text("cd cmp-")
            s.key(TAB)
            raw = bytes(s.raw)
            ok = b"\x1b[1;35m" in raw
            return verdict(ok, "unselected directory cells paint LS_COLORS di=01;35",
                           "di=01;35 SGR present" if ok else f"no 1;35 SGR; tail={raw[-200:]!r}")
    add(Case("select/lscolors", "select", "completion/file_colors.rs (#743)",
             "directories paint through LS_COLORS", select_lscolors))

    # ── cycle / inline ───────────────────────────────────────────────
    def cycle_case(name: str, keys: bytes, expected: str, interaction="cycle", style="column"):
        def run(c: Case):
            e = Env(BINARY, WORK_ROOT, c)
            e.session_env = select_env(style, interaction)
            e.cols = 40
            e.prepare(dirs=CANDIDATES)
            with e.session() as s:
                s.wait_prompt()
                s.type_text("cd cmp-")
                s.key(TAB)
                for part in split_keys(keys):
                    s.key(part)
                line = s.input_line()
                hi = first_highlight(s, CANDIDATES)
                ok = line == expected
                return verdict(ok, f"line {expected!r}", f"line {line!r}, highlighted {hi!r}")
        add(Case(name, "cycle", "config.rs CompletionInteraction::Cycle; " + STYLE_DOC,
                 f"line {expected!r}", run))

    cycle_case("cycle/tab1_first_candidate", b"", "cd cmp-aa/")
    cycle_case("cycle/tab2_second", TAB, "cd cmp-bb/")
    cycle_case("cycle/shift_tab_wraps_last", BACKTAB, "cd cmp-ff/")

    def cycle_enter(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = select_env("column", "cycle")
        e.cols = 40
        e.prepare(dirs=CANDIDATES)
        with e.session() as s:
            s.wait_prompt()
            s.type_text("cd cmp-")
            s.key(TAB)
            s.key(ENTER)
            s.type_text("basename \"$PWD\"\n")
            text = s.visible_text()
            ok = "cmp-aa" in text
            shown = [ln for ln in text.splitlines() if ln.strip()][-4:]
            return verdict(ok, "Enter runs cd into cmp-aa", shown)
    add(Case("cycle/enter_accept_runs", "cycle", "niubash#631 cycle contract",
             "Enter accepts and runs", cycle_enter))

    def cycle_esc(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = select_env("column", "cycle")
        e.cols = 40
        e.prepare(dirs=CANDIDATES)
        with e.session() as s:
            s.wait_prompt()
            s.type_text("cd cmp-")
            s.key(TAB)
            s.key(TAB)
            before = s.input_line()
            s.key(ESC_KEY)
            after = s.input_line()
            ok = after == "cd cmp-" and before != after
            return verdict(ok, "Esc restores pre-completion 'cd cmp-'", f"before {before!r}, after {after!r}")
    add(Case("cycle/esc_rollback", "cycle", "niubash#631 cycle contract",
             "Esc rolls back", cycle_esc))

    def inline_cycle(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = select_env("inline", "cycle")
        e.cols = 40
        e.prepare(dirs=CANDIDATES)
        with e.session() as s:
            s.wait_prompt()
            s.type_text("cd cmp-")
            s.key(TAB)
            first = s.input_line()
            grid = bool(s.menu_rows())
            s.key(TAB)
            second = s.input_line()
            ok = first == "cd cmp-aa/" and second == "cd cmp-bb/"
            return verdict(ok, "inline cycle: TAB1 'cd cmp-aa/', TAB2 'cd cmp-bb/'",
                           f"TAB1 {first!r} (grid={grid}), TAB2 {second!r}")
    add(Case("inline/cycle_first_match_then_next", "inline",
             f"{STYLE_DOC} inline = first match inserted, TAB cycles (bash menu-complete)",
             "inline style cycles candidates in place", inline_cycle))

    # ── insert: common prefix / two-step ─────────────────────────────
    def insert_prefix_case(name: str, style: str, initial="ls al", expected="ls alp"):
        def run(c: Case):
            e = Env(BINARY, WORK_ROOT, c)
            e.session_env = select_env(style, "insert")
            e.prepare(files=["alpha.txt", "alpine.txt", "beta.txt"])
            with e.session() as s:
                s.wait_prompt()
                s.type_text(initial)
                s.key(TAB)
                line = s.input_line()
                menu1 = s.menu_rows()
                s.key(TAB)
                line2 = s.input_line()
                menu2 = s.menu_rows()
                ok = line == expected and not menu1 and line2 == expected and bool(menu2)
                return verdict(ok, f"TAB1 {expected!r} with no menu, TAB2 lists",
                               f"TAB1 {line!r} menu={menu1!r}, TAB2 {line2!r} menu={menu2!r}")
        add(Case(name, "insert", f"{STYLE_DOC} two-step rule; style={style}",
                 f"TAB1 completes common prefix {expected!r}, TAB2 lists", run))

    insert_prefix_case("insert/column_prefix_two_step", "column")
    insert_prefix_case("insert/gnu_prefix_two_step", "gnu")
    insert_prefix_case("insert/ide_prefix_two_step", "ide")
    insert_prefix_case("insert/list_prefix_two_step", "list")
    insert_prefix_case("insert/inline_first_match", "inline", expected="ls alpha.txt")

    def insert_enter(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = select_env("column", "insert")
        e.cols = 40
        e.prepare(dirs=CANDIDATES)
        with e.session() as s:
            s.wait_prompt()
            s.type_text("cd cmp-")
            s.key(TAB)
            s.key(ENTER)
            committed = s.input_line()
            s.key(ENTER)
            s.type_text("basename \"$PWD\"\n")
            text = s.visible_text()
            ok = committed == "cd cmp-aa/" and "cmp-aa" in text
            return verdict(ok, "first Enter commits 'cd cmp-aa/' without running; second runs it",
                           f"after first Enter {committed!r}, screen tail={[ln for ln in text.splitlines() if ln.strip()][-4:]!r}")
    add(Case("insert/enter_confirms_then_runs", "insert",
             f"{STYLE_DOC} Enter with the menu open; config.rs Insert contract",
             "Enter commits the highlighted candidate, a second Enter runs", insert_enter))

    def insert_esc(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = select_env("column", "insert")
        e.prepare(files=["alpha.txt", "alpine.txt"])
        with e.session() as s:
            s.wait_prompt()
            s.type_text("ls al")
            s.key(TAB)
            before = s.input_line()
            s.key(ESC_KEY)
            after = s.input_line()
            ok = before == "ls alp" and after == "ls alp"
            return verdict(ok, "Esc keeps the inserted common prefix", f"before {before!r}, after {after!r}")
    add(Case("insert/esc_keeps_prefix", "insert", "config.rs Insert contract",
             "Esc keeps what was inserted", insert_esc))

    # ── presentation styles under select (default interaction) ───────
    def style_select(name: str, style: str, assertion: str):
        def run(c: Case):
            e = Env(BINARY, WORK_ROOT, c)
            e.session_env = select_env(style, "select")
            e.prepare(rc=RC_DESCRIPTIONS)
            with e.session() as s:
                s.wait_prompt()
                s.type_text("mytool ")
                s.key(TAB)
                text = s.visible_text()
                lines = [ln for _r, ln in s.menu_rows()]
                hi = s.highlighted(["start", "stop", "status"])
                if assertion == "list_descriptions":
                    ok = (
                        len(lines) >= 3
                        and all(desc in text for desc in ("desc-start", "desc-stop", "desc-status"))
                        and all(sum(1 for n in ("start", "stop", "status") if n in ln) == 1 for ln in lines[:3])
                    )
                    return verdict(ok, "vertical list, one candidate per row, all descriptions visible",
                                   f"lines={lines!r}, highlighted={hi!r}")
                if assertion == "ide_descriptions":
                    ok = all(desc in text for desc in ("desc-start", "desc-stop", "desc-status"))
                    return verdict(ok, "popup shows each candidate's description", f"lines={lines!r}")
                if assertion == "column_description_vertical":
                    ok = (
                        len(lines) >= 3
                        and all(desc in text for desc in ("desc-start", "desc-stop", "desc-status"))
                        and all(sum(1 for n in ("start", "stop", "status") if n in ln) == 1 for ln in lines[:3])
                    )
                    return verdict(ok, "any description forces the column style to a single vertical column with descriptions",
                                   f"lines={lines!r}")
                if assertion == "gnu_printed_no_selection":
                    # GNU style is exempt from the interaction axis: TAB #1
                    # prefixes, TAB #2 lists, nothing selected.
                    e2 = Env(BINARY, WORK_ROOT, c)
                    e2.session_env = select_env("gnu", "select")
                    e2.prepare(files=["alpha.txt", "alpine.txt"])
                    with e2.session() as s2:
                        s2.wait_prompt()
                        s2.type_text("ls al")
                        s2.key(TAB)
                        l1 = s2.input_line()
                        h1 = s2.highlighted(["alpha.txt", "alpine.txt"])
                        s2.key(TAB)
                        l2 = s2.input_line()
                        h2 = s2.highlighted(["alpha.txt", "alpine.txt"])
                        ok = l1 == "ls alp" and l2 == "ls alp" and not h1 and not h2
                        return verdict(ok, "TAB1 'ls alp', TAB2 list with no selection",
                                       f"TAB1 {l1!r} hi={h1!r}, TAB2 {l2!r} hi={h2!r}")
                raise AssertionError(assertion)
        add(Case(name, "styles", f"{STYLE_DOC}; style={style}",
                 assertion.replace("_", " "), run))

    style_select("styles/select_column_descriptions", "column", "column_description_vertical")
    style_select("styles/select_ide_descriptions", "ide", "ide_descriptions")
    style_select("styles/select_list_descriptions", "list", "list_descriptions")
    style_select("styles/select_gnu_printed", "gnu", "gnu_printed_no_selection")

    def style_cycle_descriptions(name: str, style: str):
        def run(c: Case):
            e = Env(BINARY, WORK_ROOT, c)
            e.session_env = select_env(style, "cycle")
            e.prepare(rc=RC_DESCRIPTIONS)
            with e.session() as s:
                s.wait_prompt()
                s.type_text("mytool ")
                s.key(TAB)
                text = s.visible_text()
                lines = [ln for _r, ln in s.menu_rows()]
                ok = (
                    len(lines) >= 3
                    and all(desc in text for desc in ("desc-start", "desc-stop", "desc-status"))
                    and all(sum(1 for n in ("start", "stop", "status") if n in ln) == 1 for ln in lines[:3])
                )
                return verdict(ok, "vertical list, one candidate per row, all descriptions visible",
                               f"lines={lines!r}")
        add(Case(name, "styles", f"{STYLE_DOC}; style={style}, interaction=cycle",
                 "the style's documented shape survives the cycle interaction", run))

    style_cycle_descriptions("styles/cycle_column_descriptions", "column")
    style_cycle_descriptions("styles/cycle_ide_descriptions", "ide")
    style_cycle_descriptions("styles/cycle_list_descriptions", "list")

    def style_cycle_gnu(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = select_env("gnu", "cycle")
        e.prepare(files=["alpha.txt", "alpine.txt"])
        with e.session() as s:
            s.wait_prompt()
            s.type_text("ls al")
            s.key(TAB)
            l1 = s.input_line()
            h1 = s.highlighted(["alpha.txt", "alpine.txt"])
            s.key(TAB)
            l2 = s.input_line()
            h2 = s.highlighted(["alpha.txt", "alpine.txt"])
            ok = l1 == "ls alp" and l2 == "ls alp" and not h1 and not h2
            return verdict(ok, "gnu is exempt: TAB1 'ls alp', TAB2 printed list, no selection",
                           f"TAB1 {l1!r} hi={h1!r}, TAB2 {l2!r} hi={h2!r}")
    add(Case("styles/cycle_gnu_printed", "styles",
             f"{STYLE_DOC}; gnu interaction axis is inert",
             "gnu style stays a printed list under cycle", style_cycle_gnu))

    def style_inline_select(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = select_env("inline", "select")
        e.cols = 40
        e.prepare(dirs=CANDIDATES)
        with e.session() as s:
            s.wait_prompt()
            s.type_text("cd cmp-")
            s.key(TAB)
            line = s.input_line()
            grid = s.menu_rows()
            s.key(TAB)
            line2 = s.input_line()
            ok = line == "cd cmp-aa/" and line2 == "cd cmp-bb/"
            return verdict(ok, "inline presentation cycles in place; TAB1 cmp-aa/, TAB2 cmp-bb/",
                           f"TAB1 {line!r} grid={len(grid)} rows, TAB2 {line2!r}")
    add(Case("styles/select_inline_cycles", "styles",
             f"{STYLE_DOC} inline = first match inserted, TAB cycles",
             "inline style cycles candidates in place", style_inline_select, issue="#778"))

    # ── aliases ──────────────────────────────────────────────────────
    def alias_equiv(name: str, alias: str, canonical: str):
        def run(c: Case):
            def snapshot(style: str):
                e = Env(BINARY, WORK_ROOT / ("alias-" + style), c)
                e.session_env = select_env(style, "insert")
                e.prepare(files=["alpha.txt", "alpine.txt"])
                with e.session() as s:
                    s.wait_prompt()
                    s.type_text("ls al")
                    s.key(TAB)
                    a = s.input_line()
                    m1 = tuple(s.menu_rows())
                    s.key(TAB)
                    b = s.input_line()
                    m2 = tuple(s.menu_rows())
                    return a, m1, b, m2
            got = snapshot(alias)
            want = snapshot(canonical)
            ok = got == want
            return verdict(ok, f"alias {alias!r} behaves exactly like {canonical!r}: {want!r}",
                           f"alias screen {got!r}")
        add(Case(name, "aliases", "config.rs CompletionStyle::parse aliases",
                 f"{alias} -> {canonical}", run))

    alias_equiv("aliases/readline_gnu", "readline", "gnu")
    alias_equiv("aliases/col_column", "col", "column")
    alias_equiv("aliases/grid_column", "grid", "column")
    alias_equiv("aliases/vertical_list", "vertical", "list")
    alias_equiv("aliases/cycle_inline", "cycle", "inline")
    alias_equiv("aliases/menu_inline", "menu", "inline")
    alias_equiv("aliases/smart_ide_code", "smart", "ide")
    alias_equiv("aliases/popup_ide_code", "popup", "ide")

    # ── precedence / invalid fallback / config file ─────────────────
    def env_style_wins(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = select_env("gnu", "insert")
        e.prepare(files=["alpha.txt", "alpine.txt"])
        with e.session() as s:
            s.wait_prompt()
            s.type_text("ls al")
            s.key(TAB)
            l1 = s.input_line()
            s.key(TAB)
            l2 = s.input_line()
            ok = l1 == "ls alp" and l2 == "ls alp" and bool(s.menu_rows())
            return verdict(ok, "env NIU_COMPLETION_STYLE=gnu gives gnu prefix/list",
                           f"TAB1 {l1!r}, TAB2 {l2!r}, menu={s.menu_rows()!r}")
    add(Case("precedence/env_style_gnu", "precedence", CONFIG_DOC,
             "env > default for style", env_style_wins))

    def env_interaction_wins(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = select_env(None, "insert")
        e.prepare(files=["alpha.txt", "alpine.txt"])
        with e.session() as s:
            s.wait_prompt()
            s.type_text("ls al")
            s.key(TAB)
            l1 = s.input_line()
            ok = l1 == "ls alp" and not s.menu_rows()
            return verdict(ok, "env NIU_COMPLETION_INTERACTION=insert prefixes and stops",
                           f"TAB1 {l1!r}, menu={s.menu_rows()!r}")
    add(Case("precedence/env_interaction_insert", "precedence", CONFIG_DOC,
             "env > default for interaction", env_interaction_wins))

    def invalid_style(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = select_env("bogus-style")
        e.prepare(files=["alpha.txt", "alpine.txt"])
        rows = ""
        with e.session() as s:
            s.wait_prompt()
            text = s.visible_text() + "\n" + bytes(s.raw).decode("utf-8", "replace")
        rows = e.config_rows({"NIU_COMPLETION_STYLE": "bogus-style"})
        ok = (
            "NIU_COMPLETION_STYLE must be one of: gnu, ide, column, list, inline" in text
            and "completion.style = column (default)" in rows
        )
        return verdict(ok, "one diagnostic and fallback to column (default)",
                           f"diagnostic={'yes' if 'must be one of' in text else 'no'}; rows={rows!r}")
    add(Case("precedence/invalid_style_falls_back", "precedence", CONFIG_DOC,
             "invalid style warns once and falls back to column", invalid_style))

    def invalid_interaction(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = select_env(None, "bogus")
        e.prepare(files=["alpha.txt"])
        with e.session() as s:
            s.wait_prompt()
            text = bytes(s.raw).decode("utf-8", "replace")
        rows = e.config_rows({"NIU_COMPLETION_INTERACTION": "bogus"})
        ok = (
            "NIU_COMPLETION_INTERACTION must be one of: insert, cycle, select" in text
            and "completion.interaction = select (default)" in rows
        )
        return verdict(ok, "one diagnostic and fallback to select (default)",
                           f"diagnostic={'yes' if 'must be one of' in text else 'no'}; rows={rows!r}")
    add(Case("precedence/invalid_interaction_falls_back", "precedence", CONFIG_DOC,
             "invalid interaction warns once and falls back to select", invalid_interaction))

    def config_ignored(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.prepare()
        (e.home / ".niubash").mkdir(parents=True, exist_ok=True)
        config = e.home / ".niubash" / "config.toml"
        config.write_text('[completion]\nstyle = "gnu"\ninteraction = "cycle"\n')
        rows = e.config_rows({"NIU_CONFIG": str(config)})
        ok = (
            "completion.style = column (default)" in rows
            and "completion.interaction = select (default)" in rows
        )
        return verdict(ok, "config.toml is not read; rows stay column/select defaults", rows.strip())
    add(Case("precedence/config_file_ignored", "precedence",
             "CHANGELOG.md 1.5.0 (config.toml gone); docs/src/configuration.md",
             "completion.style/completion.interaction in a config file are not honored", config_ignored))

    def env_over_config(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.prepare()
        (e.home / ".niubash").mkdir(parents=True, exist_ok=True)
        config = e.home / ".niubash" / "config.toml"
        config.write_text('[completion]\nstyle = "gnu"\ninteraction = "cycle"\n')
        rows = e.config_rows({
            "NIU_CONFIG": str(config),
            "NIU_COMPLETION_STYLE": "inline",
            "NIU_COMPLETION_INTERACTION": "insert",
        })
        ok = (
            "completion.style = inline (env)" in rows
            and "completion.interaction = insert (env)" in rows
        )
        return verdict(ok, "env inline/insert wins; config gnu/cycle is not a source", rows.strip())
    add(Case("precedence/env_over_config", "precedence",
             "docs/src/configuration.md precedence: NIU_* env > built-in default; config.toml gone",
             "env overrides a leftover config file", env_over_config))

    def config_migration_notice(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.prepare()
        (e.home / ".niubash").mkdir(parents=True, exist_ok=True)
        config = e.home / ".niubash" / "config.toml"
        config.write_text('[completion]\nstyle = "gnu"\n')
        with e.session({"NIU_CONFIG": str(config)}) as s:
            s.wait_prompt()
            s.drain(0.4)
            text = bytes(s.raw).decode("utf-8", "replace")
            ok = ("is no longer read" in text) and ("niu config migr" in text)
            return verdict(ok, "startup notice names 'is no longer read' and 'niu config migrate'",
                           text.splitlines()[:6])
    add(Case("precedence/config_migration_notice", "precedence",
             "tests/issue631_completion_interaction.rs legacy-config notice",
             "leftover config.toml prints one migration notice and is never read",
             config_migration_notice))

    # ── page size / behavior / other knobs ──────────────────────────
    def page_size_probe(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = {"NIU_COMPLETION_PAGE_SIZE": "3"}
        e.cols = 40
        e.prepare(dirs=[f"page-{i:02}" for i in range(12)])
        with e.session() as s:
            s.wait_prompt()
            s.type_text("cd page-")
            s.key(TAB)
            shown = sum(1 for i in range(12) if f"page-{i:02}" in s.visible_text())
            rows = e.config_rows({"NIU_COMPLETION_PAGE_SIZE": "3"})
            ok = "completion.page_size" in rows or "page_size" in rows
            return verdict(ok, "a completion.page_size/env knob exists and limits the page",
                           f"env ignored; no config row; {shown}/12 candidates shown")
    add(Case("knobs/page_size_not_exposed", "knobs",
             "config.rs MenuConfig::completion_page_size (fixed 10); no env/config key documented",
             "page size is exposed as a documented knob", page_size_probe))

    def behavior_probe(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = {"NIU_COMPLETION_BEHAVIOR": "substring"}
        e.prepare(files=["alpha.txt", "alpine.txt"])
        with e.session() as s:
            s.wait_prompt()
            s.type_text("ls pha")
            s.key(TAB)
            line = s.input_line()
            ok = "alpha" in s.visible_text()
            return verdict(ok, "completion.behavior=substring matches inside the word",
                           f"line={line!r}, menu={s.menu_rows()!r}")
    add(Case("knobs/completion_behavior_not_exposed", "knobs",
             "config.rs only resolves style/interaction; no completion.behavior key",
             "prefix/substring behavior is a documented, working knob", behavior_probe))

    # ── NIU_COMPDEFS / compspec priority ────────────────────────────
    def compdef_priority(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.prepare(rc=RC_GIT_COMPDEF)
        with e.session() as s:
            s.wait_prompt()
            s.type_text("git ")
            s.key(TAB)
            text = s.visible_text()
            ok = "defonly-1" in text and "defonly-2" in text and "checkout" not in text
            return verdict(ok, "compdef candidates only; static table (checkout/commit) does not contribute",
                           [ln for _r, ln in s.menu_rows()])
    add(Case("compdefs/compdef_wins_static", "compdefs", EXT_DOC,
             "NIU_COMPDEFS wins over static tables", compdef_priority))

    def compspec_priority(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.prepare(rc=RC_GIT_COMPSPEC)
        with e.session() as s:
            s.wait_prompt()
            s.type_text("git ")
            s.key(TAB)
            text = s.visible_text()
            ok = "spec-one" in text and "spec-two" in text and "checkout" not in text
            return verdict(ok, "compspec candidates only; static table does not contribute",
                           [ln for _r, ln in s.menu_rows()])
    add(Case("compdefs/compspec_wins_static", "compdefs", EXT_DOC,
             "a GNU compspec owns the command over static tables", compspec_priority))

    def compdef_over_compspec(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.prepare(rc=RC_GIT_BOTH)
        with e.session() as s:
            s.wait_prompt()
            s.type_text("git ")
            s.key(TAB)
            text = s.visible_text()
            # external-completion-providers.md "Priority": compdefs run ahead
            # of the compspec, but "a provider can never shadow a compspec" —
            # both surfaces contribute, static tables do not.
            ok = (
                "defonly-1" in text
                and "spec-one" in text
                and "checkout" not in text
            )
            return verdict(ok, "compdef and compspec candidates merge; static table does not contribute",
                           [ln for _r, ln in s.menu_rows()])
    add(Case("compdefs/compdef_and_compspec_merge", "compdefs", EXT_DOC,
             "provider never shadows a compspec; both contribute, static table does not",
             compdef_over_compspec))

    def compdef_dir(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        defs = e.root / "defs"
        e.prepare()
        defs.mkdir(parents=True, exist_ok=True)
        (defs / "mytool.toml").write_text(
            'command = "mytool"\ndescription = "fixture"\n\n'
            '[[flags]]\nlong = "--fixture-flag"\ndescription = "from the dir"\n'
        )
        (defs / "bashtool.bash").write_text(
            '_bashtool() {\n    local opts\n    opts="-h --verbose --output"\n'
            '    case "${prev}" in\n        *) COMPREPLY=() ;;\n    esac\n}\n'
            'complete -F _bashtool bashtool\n'
        )
        rc = f'NIU_COMPDEFS="{defs}"\n'
        with e.session({"NIU_COMPDEFS": str(defs)}) as s:
            s.wait_prompt()
            s.type_text("mytool -")
            s.key(TAB)
            t1 = s.visible_text()
            s.key(CTRL_C)
            s.type_text("bashtool --v")
            s.key(TAB)
            t2 = s.visible_text()
            ok = "--fixture-flag" in t1 and "--verbose" in t2
            return verdict(ok, "TOML file serves --fixture-flag; bash-import file serves --verbose",
                           f"toml={'yes' if '--fixture-flag' in t1 else 'no'}, bash={'yes' if '--verbose' in t2 else 'no'}; tail={t2.splitlines()[-3:]}")
    add(Case("compdefs/external_directory", "compdefs",
             f"{EXT_DOC} section 3 (completion definition files)",
             "NIU_COMPDEFS directory loads TOML and bash completion files", compdef_dir))

    # ── explicit unclaimed probes ───────────────────────────────────
    def page_keys_probe(c: Case):
        e = Env(BINARY, WORK_ROOT, c)
        e.session_env = select_env("column", "select")
        e.cols = 40
        e.prepare(dirs=CANDIDATES)
        with e.session() as s:
            s.wait_prompt()
            s.type_text("cd cmp-")
            s.key(TAB)
            before = s.input_line()
            s.key(PGDN)
            after = s.input_line()
            ok = after == "cd cmp-"
            return verdict(ok, "PageDown is not claimed by the docs; line stays put",
                           f"before {before!r}, after {after!r}")
    add(Case("select/page_keys_unclaimed", "select",
             f"{ZSH_COMPLIST} menuselect_bindings has no PageUp/PageDown; niu docs do not claim them",
             "PageDown does not select or splice a candidate", page_keys_probe))

    return out


# ── runner ───────────────────────────────────────────────────────────────
def main() -> int:
    global BINARY, WORK_ROOT
    ap = argparse.ArgumentParser()
    ap.add_argument("--filter", default="")
    ap.add_argument("--record-baseline", action="store_true")
    ap.add_argument("--work", default=str(DEFAULT_WORK))
    args = ap.parse_args()

    BINARY = which_binary()
    WORK_ROOT = Path(args.work).resolve()
    if WORK_ROOT.exists():
        shutil.rmtree(WORK_ROOT)
    WORK_ROOT.mkdir(parents=True, exist_ok=True)

    all_cases = cases()
    selected = all_cases
    if args.filter:
        needle = args.filter.lower()
        selected = [
            c for c in all_cases
            if needle in c.id.lower() or needle in c.group.lower() or needle in c.expect.lower()
        ]
    if not selected:
        print(f"no cases match {args.filter!r}", file=sys.stderr)
        return 2

    version = subprocess.run([BINARY, "--version"], stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                             timeout=10, check=False).stdout.decode("utf-8", "replace").splitlines()
    print("== completion options matrix ==")
    print(f"niu: {BINARY} :: {' / '.join(version[:2])}")
    print(f"cases: {len(selected)}")
    print()

    results = []
    counts = {"PASS": 0, "FAIL": 0, "MISSING": 0, "SKIP": 0}
    for case in selected:
        try:
            ok, detail = case.run(case)
            status = "PASS" if ok else "FAIL"
        except Exception as exc:  # noqa: BLE001 - any harness exception is a FAIL
            status = "FAIL"
            detail = f"harness error: {type(exc).__name__}: {exc}"
        if case.id.startswith("knobs/") and status == "FAIL":
            status = "MISSING"
        counts[status] += 1
        results.append({
            "id": case.id,
            "group": case.group,
            "status": status,
            "expected": case.expect,
            "actual": detail,
            "source": case.source,
            "note": case.note,
            "issue": case.issue,
        })
        print(f"{status:<7} {case.id:<44} :: {detail}")
        if case.issue:
            print(f"        baseline ({case.issue})")
        sys.stdout.flush()

    print()
    print("SUMMARY " + " ".join(f"{k}={v}" for k, v in counts.items()) + f" total={len(results)}")

    if args.record_baseline:
        write_baseline(results, counts, BINARY, "\n".join(version))
    return 1 if counts["FAIL"] else 0


def git_head() -> str:
    try:
        return subprocess.run(
            ["git", "-C", str(REPO_ROOT), "rev-parse", "--short", "HEAD"],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5, check=False,
        ).stdout.decode().strip()
    except (OSError, subprocess.SubprocessError):
        return "unknown"


def write_baseline(results, counts, binary, version):
    BASELINE.mkdir(parents=True, exist_ok=True)
    base = git_head()
    payload = {
        "harness": 1,
        "description": "niubash completion option matrix (styles x interactions, precedence, compdefs)",
        "base": base,
        "niu": {"bin": binary, "version": version},
        "counts": counts,
        "cases": results,
    }
    (BASELINE / "results.json").write_text(json.dumps(payload, indent=2) + "\n")
    lines = [
        "# Completion options matrix baseline",
        "",
        f"- base: `{base}`",
        f"- niu: `{binary}`",
        f"- version: `{version.splitlines()[0] if version else 'unknown'}`",
        f"- **PASS {counts['PASS']} / FAIL {counts['FAIL']} / MISSING {counts['MISSING']} / SKIP {counts['SKIP']}** ({len(results)} cases)",
        "",
        "Expected values are the documented contract. Selection/highlight assertions are",
        "frame-level (reverse-video cell), not buffer text.",
        "",
    ]
    for group in sorted({r["group"] for r in results}):
        lines += [f"## {group}", "", "| Case | Status | Expected | Actual | Source |", "| --- | --- | --- | --- | --- |"]
        for r in results:
            if r["group"] != group:
                continue
            esc = lambda s: str(s).replace("|", "\\|").replace("\n", " ")
            lines.append(
                f"| `{r['id']}` | {r['status']} | {esc(r['expected'])} | {esc(r['actual'])} | {esc(r['source'])} |"
            )
        lines.append("")
    (BASELINE / "summary.md").write_text("\n".join(lines) + "\n")


BINARY = ""
WORK_ROOT = DEFAULT_WORK
REPO_ROOT = SCRIPT_DIR.parent.parent

if __name__ == "__main__":
    sys.exit(main())
