# Niubash v2 Architecture

> A Windows-native, Bash-compatible terminal built on rubash + winuxcmd

## Positioning

Niubash is a Windows-native, non-isolated, Bash-compatible terminal for both
humans and agents. It does **not implement the shell language itself** — it is
the interactive front end to the **rubash lib** (the bash-compatible engine)
plus the routing layer onto **winuxcmd** (coreutils). Its core value is the
Windows-native process/environment experience: a reedline REPL, the
completion system, the theme system, Ctrl+C handling, terminal integration,
and a stable non-interactive agent execution contract.

Niubash is not an MSYS2, Git Bash, Cygwin, or WSL-style isolated
environment. `~` points at the normal Windows user home (`USERPROFILE` /
`dirs::home_dir()`); `PATH`, cwd, env, stdout, stderr, and exit codes are
ordinary Windows process state.

## Three-layer architecture

```text
niu.exe
├── Niubash host layer (Rust)
│   ├── rubash::Executor         ← shell language engine (lexer/parser/execution/builtins)
│   ├── reedline REPL            ← line editing, history, completion
│   ├── completion/              ← shell definitions + bash auto-import + 3-level cache
│   ├── theme/                   ← theme API / schema / bundle loader
│   ├── config                   ← legacy/managed machine-state reads
│   ├── plugins                  ← official Niubash plugin registry / bundle control plane
│   └── ctrl_c                   ← Win32 Ctrl+C handling
├── rubash lib (Rust)
│   ├── lexer/parser/ast
│   ├── executor (pipeline/redirect/alias/function/array/job)
│   └── builtins (cd/source/export/set/test/printf...)
└── winuxcmd.exe (C++)           ← Unix coreutils (ls/cat/grep/find/cp/mv...)
```

## Key design decisions

### 1. rubash as a lib dependency

Niubash links rubash directly as a Rust crate dependency:

```toml
[dependencies]
rubash = { git = "https://github.com/unixwin/rubash.git", branch = "master" }
```

All shell semantics (parsing, execution, built-ins, variable expansion,
redirection, pipelines, job control) are delegated to rubash. Niubash does
not re-implement lexer/parser/ast/builtins.

### 2. WinuxCmd is selected by Niubash and integrated via PATH

Not via FFI/DLL — the rubash Executor still finds external commands through
`PATH`. Version selection is a Niubash session/config responsibility. At
startup:

1. Read the explicit `WINUXCMD_PATH`, then fall back to Niubash's own
   install/bundle/`PATH` discovery rules to locate a `winuxcmd.exe`.
2. Prepend the directory of that **same** exe to the process `PATH` so that
   command links such as `ls`/`cat`/`grep` resolve.
3. Pass the exact resolved exe path to rubash via
   `Executor::set_winuxcmd_path`.

Rubash never guesses a different `winuxcmd.exe` from `PATH`. This way, stale
command links from an old bundle that are still on the Windows `PATH` can
never mix the dispatcher with the wrong links.

### 3. Windows real installation tree

Niubash derives one shell root from the selected installed
`winuxcmd.exe`. For example, the executable
`<install>/usr/bin/winuxcmd.exe` makes `<install>` the root. Niubash creates
the ordinary directories below that root:

```text
<install>/usr/bin
<install>/bin
<install>/usr/local/bin
<install>/etc
<install>/var
<install>/tmp
<install>/dev
<install>/.wpm
```

`usr/bin` is canonical for WinuxCmd, WPM, command links, and filename-only WPM
targets. Explicit package targets keep their requested real directory.
Niubash passes the selected installation root to Rubash through
`NIU_ROOT`; there is no second `~/.niubash/root` tree and no provider
union. Rubash maps `/`, `/bin`, `/usr/bin`, `/etc`, and `/tmp` directly below
the real root. `/dev/null` maps to Windows `NUL`; other `/dev` entries remain
unsupported capabilities.

### 4. Completion is independent of the engine

The completion system (shell definitions + bash script auto-import +
`cmd -h` description scraping + a 3-level cache) is implemented on the
Niubash side and does not depend on rubash. It is one of Niubash's core
differentiators.

### 5. Configuration and startup entry points

- `~/.niubashrc` is the primary interactive entry point. Plain
  niubash/bash syntax declares plugin lists, theme, prompt template,
  `export`, `alias`, functions, and local startup logic.
- `~/.winshrc` is the compatibility fallback; it is read as the legacy user
  rc only when `~/.niubashrc` does not exist.
- Machine state — plugin CLI enable/disable records, permissions, bundle
  versions, legacy managed blocks, test isolation, completion directories —
  is maintained by the internal managed-state mechanism and is not a user
  configuration entry point.
- When `~/.niubashrc` exists, it is the single entry point for sourcing
  plugins/frameworks; the host no longer silently sources the official
  source plugins a second time from managed-state defaults, avoiding double
  entry points and duplicate prompt/git-state refreshes.
- Plain `niu -c`, script files, and stdin scripts stay quiet and
  deterministic: no interactive rc, no source plugins.

The design principle is to minimize user-visible entry points: users edit
`~/.niubashrc` day to day; machine state is maintained by Niubash itself and
users never need to edit its storage format.

### 6. Plugin system

The v3 plugin system is Niubash-native.

- `oh-my-niu` ships as the official bundled plugin distribution.
- Shell helpers such as git/docker/kubectl/npm can ship as first-party
  `kind = "source"` packs, loaded from a bundle-local `init.winux`.
- Capabilities that need stronger host behavior (zoxide, direnv, dotenv,
  fzf, ...) continue to be served by `kind = "builtin"` or a future explicit
  effect/runtime API.
- Third-party plugins currently enter through reviewed source packs and
  process adapters; the permission model is declared uniformly in the
  manifest.
- Process/IPC plugins are bridges for external tools and debug backends.
- Plugins cannot extend the rubash parser/executor, and cannot source
  arbitrary legacy `.winsh` files or rc fragments found in user
  directories. A source pack may only load manifest-declared bundle-local
  `.winux` files, and requires the `shell:source` permission.
- Editor capabilities come from reedline and Niubash-native keybinding
  presets.

## Repository layout

```text
niubash/
├── Cargo.toml
├── LICENSE                   # GPL-3.0-or-later
├── README.md / README-zh.md
├── .niubashrc                 # primary interactive user entry
├── .winshrc                   # legacy fallback rc
├── managed state              # internal machine-managed state
├── crates/
│   └── niubash-runtime/
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs        # library entry
│           ├── shell.rs      # shell state
│           ├── repl.rs       # reedline REPL
│           ├── ctrl_c.rs     # Win32 Ctrl+C
│           ├── config.rs     # config parsing
│           ├── winuxcmd.rs   # winuxcmd discovery
│           ├── prompt.rs     # prompt rendering
│           ├── theme.rs      # theme system
│           └── completion/   # completion system
├── src/
│   └── main.rs               # binary entry
└── docs/
    ├── src/
    │   └── (this documentation book)
    └── planning/
```

## Data flow

```text
user types "ls -la | grep foo"
         │
         ▼
   reedline (line editing + completion)
         │
         ▼
   shell.execute_line(line)      ← niubash-runtime
         │
         ├─ rubash::lexer::tokenize(line)
         ├─ rubash::parser::parse(tokens) → Ast
         └─ executor.execute_ast(&ast)    ← rubash owns all semantics
                │
                ├─ built-ins (cd/source/echo...)
                ├─ external command → find_user_command("ls")
                │                   │ (PATH already carries the winuxcmd dir)
                │                   ▼
                │              winuxcmd.exe ls -la
                │
                ├─ pipeline: | grep foo → find_user_command("grep")
                └─ output to stdout
```

## Differences from the old architecture

| Aspect | v1 (old niubash) | v2 (current niubash) |
| --- | --- | --- |
| Shell engine | in-house winsh lexer/parser/ast | rubash lib |
| Coreutils | winuxcmd FFI (DLL, disabled) | winuxcmd.exe process (PATH injection) |
| Command routing | command_router.rs classification table | rubash-internal find_user_command |
| Built-ins | self-implemented builtins.rs | rubash::builtins |
| Completion | src/completion/ | fully preserved, migrated |
| Themes | theme.rs (8 themes) | trimmed to 4 built-in themes |
| Plugins | Plugin trait + Oh-My-Niubash | moved out of v1, iterated later |
| License | MIT | GPL-3.0-or-later |

## Version planning

- v2.2: stabilize the rubash rewrite, completion enhancements, Vi mode and
  Ctrl+R, configuration consistency, user themes
- v2.3: Windows-native terminal contract, agent-friendly non-interactive
  behavior, history/prompt/completion UX
- v2.4: interactive polish (right prompt, hints, completion menu, defaults)
- v3: Niubash-native plugin system; `oh-my-niu` as the official bundled
  plugin distribution; first unify existing first-party packs under the
  `builtin` registry, then bring third-party plugins in through source/
  process runtimes
- Non-goals: Linux/macOS native shell products; rubash itself is reusable
  across platforms, but the niubash product targets Windows

---

*Last updated: 2026-09-08*
