# Niubash v2 Architecture

> A native, Bash-compatible shell built on rubash + winuxcmd — Windows flagship,
> Linux and macOS shipped beside it

## Positioning

Niubash is a native, non-isolated, Bash-compatible shell for both
humans and agents — released for Windows (x64, ARM64), Linux (x86_64,
aarch64), and macOS, with Windows the flagship. It does **not implement the
shell language itself** — it is
the interactive front end to the **rubash lib** (the bash-compatible engine)
plus the routing layer onto **winuxcmd** (coreutils). Its core value is the
Windows-native process/environment experience: a reedline REPL, the
completion system, the prompt floor, Ctrl+C handling, terminal integration,
and a stable non-interactive agent execution contract.

Niubash is not an MSYS2, Git Bash, Cygwin, or WSL-style isolated
environment. `~` points at the normal user home (`USERPROFILE` /
`dirs::home_dir()`); `PATH`, cwd, env, stdout, stderr, and exit codes are
ordinary host process state.

## Three-layer architecture

```text
niu.exe
├── Niubash host layer (Rust)
│   ├── rubash::Executor         ← shell language engine (lexer/parser/execution/builtins)
│   ├── reedline REPL            ← line editing, history, completion
│   ├── completion/              ← shell definitions + bash auto-import + 3-level cache
│   ├── prompt/                  ← prompt floor (defaults-as-floor, §14.5 of the ecosystem design)
│   ├── config                   ← legacy/managed machine-state reads
│   ├── plugins                  ← external bash-ecosystem sources (add/enable, no bundles)
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
- `~/.niubashrc` is also the **only** entry point: pre-rename legacy
  rc names and `~/.bashrc` are never read, migrated, or written —
  a session without `~/.niubashrc` starts with the defaults.
- Plugin activation is a derived plane, not an rc edit (niubash#359): the
  manifest (`~/.niubash/plugins.json` + `~/.niubash/sources/registry.json`)
  drives the generated loader `~/.niubash/generated/plugins.niu`, which the
  host sources read-only before the user rc. No `niu plugin` verb writes
  `~/.niubashrc`.
- Machine state — the plugin manifest, source registry state, the
  generated loader, test isolation, completion
  directories — is maintained by the internal managed-state mechanism and is
  not a user configuration entry point.
- Plain `niu -c`, script files, and stdin scripts stay quiet and
  deterministic: no interactive rc, no generated loader, no source plugins.

The design principle is to minimize user-visible entry points: users edit
`~/.niubashrc` day to day; machine state is maintained by Niubash itself and
users never need to edit its storage format.

### 6. Plugin system

The built-in plugin/theme stack (and the `oh-my-niu` bundle format) is
retired (niubash#145): the plugin verbs hard-bail on bundles, and release
packages stage no plugin content (niubash#161). The plugin system is the
**external bash ecosystem as first-class content**.

- Sources are real upstream projects — oh-my-bash, bash-it,
  bash-completion, any wild plugin repo or single file — installed over
  git clone on explicit user command (`niu plugin add`), never vendored
  into the product.
- Sources activate only through an explicit `niu plugin enable`;
  framework assets enable through each
  manager's own selection mechanism (rc arrays, `enabled/` entries), never
  through shims that re-implement upstream library functions.
- The declarative spec `~/.niubash/plugins.json` is the source of truth
  for what is declared; the registry pins installed trees (commit), and
  `niu plugin sync` reconciles the two.
- Plugins cannot extend the rubash parser/executor, and cannot source
  arbitrary legacy `.winsh` files or rc fragments found in user
  directories beyond what the enabled sources themselves declare.
- Editor capabilities come from reedline and Niubash-native keybinding
  presets.

## Repository layout

```text
niubash/
├── Cargo.toml
├── LICENSE                   # MIT
├── README.md / README-zh.md
├── .niubashrc                 # primary interactive user entry (~/)
├── managed state              # internal machine-managed state (~/.niubash/)
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
│           ├── prompt.rs     # prompt rendering (floor)
│           ├── prompt_segments.rs # segment presets
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
| Themes | theme.rs (8 themes) | built-in stack retired (niubash#145); themes come from external sources |
| Plugins | Plugin trait + Oh-My-Niubash | external bash ecosystem as sources (add/enable; no bundles) |
| License | MIT | MIT |

## Version planning

- v2.2: stabilize the rubash rewrite, completion enhancements, Vi mode and
  Ctrl+R, configuration consistency, user themes
- v2.3: Windows-native terminal contract, agent-friendly non-interactive
  behavior, history/prompt/completion UX
- v2.4: interactive polish (right prompt, hints, completion menu, defaults)
- v3: the external-ecosystem plugin system — first-party packs replaced by
  real upstream sources (oh-my-bash, bash-it, bash-completion, wild
  plugins); the `oh-my-niu` bundle format was
  retired along the way (niubash#145/#161). Activation moved off the user rc
  in 1.4.2: the manifest drives a generated loader at
  `~/.niubash/generated/plugins.niu` (niubash#359).
- Shipping beyond Windows is no longer a non-goal: Linux and macOS portable
  builds are released and smoke-gated beside the Windows installer (see
  [Platform Support](platform-support.md)); Android and OpenHarmony remain
  incubating CI targets.

---

*Last updated: 2026-10-22*
