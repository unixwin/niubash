# Configuration sources and precedence

Since **1.5.0** Niubash has **no `~/.niubash/config.toml`**. The file is
never read at startup. Every setting has exactly one kind of source, and
`niu config` shows the effective value together with where it came from.

## The three sources

| Source | What lives there |
| --- | --- |
| **Environment** | niu-proprietary `NIU_*` variables (`NIU_COMPLETION_STYLE`, `NIU_COMPLETION_INTERACTION`, `NIU_COMMAND_NOT_FOUND_HINT`, `NIU_RC_POLICY`, ...). Export them from `~/.niubashrc` or the terminal environment. |
| **inputrc** | GNU readline variables and key bindings: `$INPUTRC`, else `~/.inputrc` (`set editing-mode vi`, `set keyseq-timeout 120`, `"\C-t": transpose-chars`, ...). |
| **rc** | `~/.niubashrc` shell code: `bind`, `set -o`, `shopt`, aliases, functions, plugin/theme lines. |

Anything not set by a source uses the built-in default. There is no
second copy of a GNU knob in a niu file, and no niu file that can
disagree with the rc.

## Precedence

For niu-proprietary knobs the chain is simply:

```
NIU_* environment variable  >  built-in default
```

An invalid value prints one diagnostic and falls back to the default; it
never breaks startup.

For readline state, Niubash follows GNU's order:

```
~/.niubashrc (bind / set -o)  →  $INPUTRC or ~/.inputrc  →  editor
```

The rc runs first, then readline initializes and reads the inputrc file.
That means an inputrc `set editing-mode vi` overrides a `set -o emacs`
from the rc, and inputrc bindings are visible to the line editor
together with rc `bind` lines. `keyseq-timeout` and the editing mode are
read from readline, not from any niu setting.

## niu-proprietary settings

| Setting | Environment variable | Values | Default |
| --- | --- | --- | --- |
| TAB presentation | `NIU_COMPLETION_STYLE` | `gnu`, `ide`, `column`, `list`, `inline` | `column` |
| TAB interaction | `NIU_COMPLETION_INTERACTION` | `insert`, `cycle` | `cycle` |
| Command-not-found hint | `NIU_COMMAND_NOT_FOUND_HINT` | `off`, `wpm` | `off` |
| Personal-rc policy | `NIU_RC_POLICY` | `primary-only`, `bashrc-fallback` | `primary-only` |

Example — put these in `~/.niubashrc`:

```bash
export NIU_COMPLETION_STYLE=ide
export NIU_COMPLETION_INTERACTION=insert
```

`NIU_RC_POLICY` is read **before** any rc is sourced (it decides which
rc may be sourced), so a line inside the rc only reaches sessions
started from a shell that already exports it. To use
`bashrc-fallback` before the rc exists, set the variable in the
terminal environment (or the Windows user environment / shell profile).
`niu setup --rc-policy bashrc-fallback` prints the export line for
you to add yourself: niu never edits `~/.niubashrc` (it is your shell
code, not a product database), and creating a primary rc to hold the
line would suppress the fallback it selects. A marked policy block
left in an rc by an older niu release is reported by the command and
left byte-identical — removing or replacing those lines is a hand
edit.

## GNU-shaped settings

Bindings and readline variables belong in the rc or the inputrc file:

```bash
# ~/.niubashrc
bind '"\C-g": redisplay'
bind -x '"\C-t": my-command'
bind 'set keyseq-timeout 120'
set -o vi
```

```text
# ~/.inputrc
set editing-mode vi
set keyseq-timeout 120
"\C-t": transpose-chars
```

`$INPUTRC` replaces `~/.inputrc` for the whole process, exactly as in
GNU bash. There is no `[keymap]` section and no `niu config set
keymap.bind`: the rc `bind` builtin and the inputrc file are the only
binding channels.

## Inspecting settings

`niu config` is read-only:

```text
niu config                 # same as `niu config list`
niu config get <key>       # one line: key = value (source)
```

The source is `env`, `inputrc`, or `default`. Example:

```text
completion.style = ide (env)
completion.interaction = select (default)
startup.rc-policy = primary-only (default)
editing-mode = vi (inputrc)
keyseq-timeout = 120 (inputrc)
```

`niu config set ...` was removed in 1.5.0. It now explains the
replacement: rc `bind`/`set`/`shopt`, an inputrc line, or an
`export NIU_*`.

## Migrating a pre-1.5.0 config.toml

If `~/.niubash/config.toml` still exists, interactive startup prints a
one-line notice naming the migration command. The file itself is never
read. Run:

```text
niu config migrate
```

The migration:

1. translates `[keymap].bind` entries to `bind '<spec>'` rc lines and
   `[keymap].unbind` entries to `bind -r '<key>'`;
2. translates `command-not-found-hint`, `completion.style`,
   `completion.interaction`, and `startup.rc-policy` to
   `export NIU_*` lines;
3. appends a commented `# >>> niu config migrate >>>` block to
   `~/.niubashrc` (an existing block is updated in place, so running
   it twice never duplicates lines);
4. renames the old file to
   `config.toml.migrated-<UTC timestamp>` (for example
   `config.toml.migrated-20261030-034500Z`).

Unrecognized values are reported as skipped instead of being guessed.
A malformed file is refused and left byte-for-byte untouched. After a
successful migration the startup notice disappears because the old
name no longer exists.

`niu setup` no longer writes a config file either. Its completion
choices become `export NIU_*` lines in the rc it creates — and only
when no rc exists yet: over an existing rc the wizard parks the same
text as a proposal sample under `~/.niubash/` for you to apply by
hand, and `niu setup --rc-policy` only prints the export line
described above.
