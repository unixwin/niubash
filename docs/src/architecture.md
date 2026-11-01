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
Windows-native process/environment experience: a ruline REPL, the
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
│   ├── ruline REPL              ← line editing, history, completion
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
- Editor capabilities come from ruline and Niubash-native keybinding
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
│           ├── repl.rs       # ruline REPL
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

### 6. In-process readline C ABI (niubash#775, ruline#8)

GNU readline is not a separate library inside niu: the editor is ruline
(the house GNU-readline-compatible editor; the crate still carries
the `reedline` package name during the rename, ruline#9) compiled
into the executable. Bash loadable builtins
(`enable -f ./mybuiltin.so mybuiltin`) are `dlopen`ed into *that* process, so
their `rl_*` / `history_*` references must resolve against niu's main
executable — the live editor — rather than against a second readline
instance.

The ABI definitions live in the ruline `readline-abi` crate
(<https://github.com/unixwin/ruline/issues/8>), pinned as a git dependency
of the root `niu` package. Its lib target is named `readline`
(`readline_abi` is the package name); `src/main.rs` calls `rl_initialize()`
once at startup so the exported globals (`rl_line_buffer`, keymaps, funmaps,
completion defaults) exist before any plugin can load. The host does not
implement `rl_*` itself: `readline()`, history, keymaps, funmaps, hooks and
completion belong to the ABI crate.

The exported set is checked in at `scripts/readline-abi-exports.txt`
(one `F <name>` / `D <name>` per line, generated from the built
`libreadline` with `scripts/gen-readline-abi-exports.sh`). Publishing it
needs a per-platform link switch, done in `build.rs` for the `niu` binary
only (the `bash` binary re-execs niu and never hosts an editor):

| Target | Flag | Why |
|---|---|---|
| ELF (Linux, Android/Termux, musl) | `-rdynamic` (`--export-dynamic`) | Adds global definitions to the dynamic symbol table so `dlopen` resolves them. |
| Mach-O (macOS) | `-Wl,-export_dynamic` | ld64's documented "main executable may load a plug-in" switch; `-rdynamic` is not an ld64 option. |
| Windows MSVC | `/DEF:<generated .def>` | link.exe exports nothing by default; a curated `EXPORTS` list is the MSVC equivalent and avoids publishing every Rust internal. |
| Windows MinGW | `-Wl,--export-all-symbols` | MinGW ld's host-to-plugin export switch. |

A static ELF build (the `*-unknown-linux-musl` default) has no dynamic symbol
table and cannot load shared objects, so `build.rs` skips export setup for
`crt-static` targets; those builds use the standalone `libreadline.so.8`
embedding mode instead.

Visibility is not retention: a definition in an rlib member is only linked
when something references it, and plugins reach the ABI through `dlsym`, not
through Rust code. Every name in the checked-in list is therefore also
anchored as undefined (`-Wl,--undefined=<sym>`, `-Wl,-u,_<sym>`,
`/INCLUDE:<sym>`) so its member is pulled into the link even under thin LTO
and `--gc-sections`. MSVC's generated `.def` marks data symbols with `DATA`
and leaves functions bare.

`tests/readline_abi_exports.rs` audits the entire list against the built
binary (`nm -D` / `readelf --dyn-syms`, `dumpbin /exports` on Windows), with
a second focused test for the symbols the `loadable-builtin-niu` plugin
probe resolves; `scripts/check-readline-abi-exports.sh` runs the same audit
on release artifacts.

Two embedding modes are supported, and only one shares niu's editor:

- **Host mode (this executable)** — loadable builtins resolve the symbols
  above from running niu. Today the ABI crate owns a *standalone* editor
  state that `rl_initialize()` sets up; routing it to the live ruline
  editor (`rl_line_buffer` mirroring, `rl_*` edits landing in the prompt) is
  the remaining follow-up.
- **Standalone `libreadline.so.8` / `libhistory.so.8`** — the ruline ABI
  crate's `cdylib`; an external embedder that links it owns its own editor
  instance and does not see niu's process state.

`bind -x` (engine bindings applied to the editor through the `__niu_bindx`
host command), `__niu_readline` host commands, `READLINE_*` read-back and
inputrc handling keep their existing paths; the ABI export changes no
behaviour until the live routing lands.

## Data flow

```text
user types "ls -la | grep foo"
         │
         ▼
   ruline (line editing + completion)
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
