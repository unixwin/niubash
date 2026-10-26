# TAB Completion & Ghost Text

How niu completes, key by key. The contract below is pinned by tests —
if the shell ever surprises you here, that's a bug, not a feature.

## The two-step rule (zsh/bash parity)

TAB never shows you things you did not ask for:

1. **First TAB: complete and stop.** The longest common prefix is spliced
   into the line and the menu closes. Typing `cd proj` and pressing TAB
   lands on `cd projects/` — no grid, no list, nothing else. If that's
   all you wanted, press Enter and it runs exactly what's on the line.
2. **Second TAB: show candidates.** Pressing TAB again on the settled
   line opens the grid with the children (`projects/` contents) or the
   ambiguous set.
3. **TAB with nothing to splice shows the grid immediately.** When the
   typed text is already the whole common prefix (e.g. `cd r` matching
   `rubash/` and `repro/`), there is nothing to complete, so the grid
   acts as the disambiguation aid right away.

## Completion styles

Five shapes, one TAB contract. Switch any time; it takes effect on the
next prompt:

```sh
niu config set completion.style column   # default: multi-per-row grid
niu config set completion.style gnu      # GNU readline: printed transcript list
niu config set completion.style ide      # VS Code-style popup with descriptions
niu config set completion.style list     # fish-style vertical list
niu config set completion.style inline   # cycle candidates in place (bash menu-complete)
```

`NIU_COMPLETION_STYLE` overrides the file for one session. Setting the
default (`column`) removes the saved key again.

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

If the grid (or any interactive menu) is showing candidates, Enter
commits the highlighted candidate into the line instead of submitting
it. Press Enter again to run the completed line. With no menu open,
Enter always submits what you see.
