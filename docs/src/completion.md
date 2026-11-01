# TAB Completion & Ghost Text

How niu completes, key by key. The contract below is pinned by tests —
if the shell ever surprises you here, that's a bug, not a feature.

## Interaction: what TAB does

Completion has two independent axes: **interaction** (what TAB does to the
line) and **presentation** (how the candidate list is painted). The default
is the owner's zsh-style rule: `select`.

### `select` (default) — zsh menu-select

TAB #1 queries the candidates once, puts the **first candidate** into the
line and paints the grid with that candidate highlighted:

* **TAB / Shift-TAB** cycle through the frozen set (wrap-around).
* **Down / Up** move one row.
* **Right / Left** move one column; both edge directions wrap, as in zsh.
* **Enter** accepts the highlighted candidate and runs the command in the
  same press.
* **Esc** rolls the line back to its pre-completion snapshot.
* A **lone candidate** completes outright and closes the menu.
* **Bare TAB** on a blank line inserts a four-space indent instead of
  opening the command-position grid.

The candidate set is frozen when the session starts, so cycling never
re-queries against the spliced text. This is a product rule: zsh's
menu-select starts with the first match selected; niu also enters that state
on TAB #1 rather than after oh-my-zsh's `auto_menu` delay.

### `insert` — two-step common-prefix contract

TAB never shows you things you did not ask for:

1. **First TAB: complete and stop.** The longest common prefix is spliced
   into the line and no menu opens. If that is all you wanted, Enter runs
   exactly what is on the line.
2. **Second TAB: show candidates.** Pressing TAB again on the settled line
   opens the menu for the ambiguous set (or the completed directory's
   children).
3. **TAB with nothing to splice shows the menu immediately.** When the
   typed text is already the whole common prefix, the menu is the
   disambiguation aid right away.

With the menu open, **Enter** commits the highlighted candidate into the
line without running it; a second Enter submits. **Esc** keeps what was
already inserted.

### `cycle` — TAB cycles candidates

TAB #1 puts the first candidate into the line and opens the grid; further
TAB / Shift-TAB steps through the frozen set with wrap-around. **Enter**
accepts the candidate in the line and runs it; **Esc** restores the
pre-completion line.

Choose it with `export NIU_COMPLETION_INTERACTION=insert|cycle|select` in
`~/.niubashrc` or the environment.

## Where candidates come from

GNU surfaces are authoritative, in this order:

1. shell-function completers (`NIU_COMPDEFS`, compdef protocol) — opt-in;
2. a GNU compspec registered by `complete` (bash-completion included) — when
   it exists it owns the command's arguments, and neither the static tables
   nor the path fallback contribute;
3. niu's completion tables — the embedded winuxcmd applet tables first,
   then bundle and user completion dirs (the official oh-my-niu bundle
   carries the non-applet tables: git, docker, kubectl, npm, …) — only for
   commands without a compspec;
4. path / variable / command-name completion.

The embedded tables are generated data (`assets/completions/winuxcmd/*.toml`),
not configuration: nothing about them is user-editable, and they never
override what `complete` registered.

## Presentation styles

The style controls how candidates are painted; it does not change the
interaction contract above (the one exception is `gnu`, whose literal
readline behavior is intentionally inert to the interaction axis). Switch
any time; it takes effect on the next prompt:

```sh
export NIU_COMPLETION_STYLE=column   # default: multi-column grid
export NIU_COMPLETION_STYLE=gnu      # opt-in: GNU readline printed transcript list
export NIU_COMPLETION_STYLE=ide      # VS Code-style popup with descriptions
export NIU_COMPLETION_STYLE=list     # fish-style vertical list
export NIU_COMPLETION_STYLE=inline   # candidate stays in the line and cycles
```

* `column` (default) — multi-column grid. When any candidate carries a
  description, the whole list is shown as a single vertical column with
  each description beside its value.
* `gnu` (opt-in) — literal GNU readline: TAB completes the longest common
  prefix, TAB again prints a width-adaptive columnar list and repaints the
  prompt. Nothing is selected; arrow keys stay on history. This preset keeps
  that behavior regardless of the interaction axis.
* `ide` — multi-column popup with descriptions beside their candidates.
* `list` — vertical list with descriptions (fish-style).
* `inline` — candidates are inserted into the line and TAB cycles through
  them in place.

Put the line in `~/.niubashrc` (or export it in the environment). The
environment is the only source for this knob — there is no config file
since 1.5.0. See
[Configuration Sources & Precedence](configuration.md).

## Ghost text (history autosuggest)

While you type, niu dims the rest of the most recent matching history
entry after the cursor. It is a suggestion, never a commitment:

* Accept it whole: `→`, `End`, `Ctrl+E`, or `Ctrl+Space`.
* Accept one word: `Ctrl+→`.
* Ignore it: keep typing — the ghost is display-only. TAB never takes
  the ghost; it only completes filesystem/command candidates. `→` always
  accepts the whole suggestion, so there is no plain "step past it".

The ghost and the grid never compete: while a completion menu is open,
the ghost stands down; it returns as soon as the menu closes.

## Enter with the menu open

The interaction decides what Enter means while candidates are visible:

* `select` / `cycle`: Enter accepts the highlighted candidate and runs the
  completed line in the same press.
* `insert`: Enter commits the highlighted candidate into the line without
  running it; press Enter again to submit.
* `gnu`: the list is printed output, not a menu, so Enter always submits the
  current line.

With no menu open, Enter always submits what you see.

## Key bindings from other tools

niu mirrors GNU `bind` entries from your startup files into its line
editor, including the two shapes a flat keymap cannot express on its own
(niubash#630):

* **Multi-key sequences** (`"\C-x\C-e"`, fzf's `"\C-\e("`): delivered by a
  chord layer above the editor. The first unit is held only when a chord
  starts with it; a matching second unit fires the binding, and a
  non-matching one replays the held key followed by the new one, so
  nothing is swallowed. The window is readline's `keyseq-timeout`
  (500 ms): past it the prefix is replayed on its own and the key stands
  alone.
* **Editing-mode toggles** (`vi-editing-mode` / `emacs-editing-mode`):
  these switch the session's machine for real. The other mode is built as
  a standby with the same widgets, `bind -m` entries and user keymap
  rules, so a flip keeps your bindings.

In emacs mode a bare `ESC` followed within ~50 ms by a key is read as the
meta prefix, the way a terminal sends `M-x` (`ESC x`); pressing Escape
alone keeps its immediate meaning.

Anything genuinely unusable — an unparseable sequence, an unknown
function — still produces a startup warning, with a per-item fix. Example:
`eval "$(fzf --bash)"` registers one binding of each shape (a multi-key
`redraw-current-line` binding and `"\C-z"` mode toggles for its fallback
macros); both now apply, and the rest of the integration works in full:
Ctrl+T (file widget), Ctrl+R (history widget) and Alt+C (cd widget).
