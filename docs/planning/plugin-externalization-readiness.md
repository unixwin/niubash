# Plugin Externalization Readiness

Use this matrix before moving a first-party capability out of Winuxsh core and
into the bundled plugin distribution.

| Pack | Runtime | Readiness | Notes |
| --- | --- | --- | --- |
| `git` | `source` + builtin/bridge prompt segment | Ready | Aliases/completions can live in the bundle; Git status may stay host-owned or be Starship-backed. |
| `docker` | asset/source | Ready | Alias and completion focused. |
| `kubectl` | asset/process | Ready | Static aliases plus optional external completion generator. |
| `npm` | asset/process | Ready | Static aliases plus optional runtime completion adapter. |
| `zoxide` | source/process | Needs careful shell mutation tests | Must update current-shell cwd helpers without hiding side effects. |
| `direnv` | process/source | Needs env mutation boundary | External binary produces shell exports; source pack applies reviewed output handling. |
| `dotenv` | source | Needs parser tests | Reads local `.env` and mutates shell env. |
| `fzf` | process | Ready with binary detection | Interactive external process adapter. |
| `command-not-found` | builtin/bridge/process | Needs provider contract | Suggestion provider should not block prompt or command execution. |
| `thefuck` | process | Ready with timeout | External correction provider. |
| `keybindings` | asset/bridge | Ready | Maps named presets to reedline-owned behavior. |
| `themes` | asset | Ready | Theme assets and prompt templates only. |

Do not externalize rubash semantics, process cwd/env synchronization, history
storage, line editor primitives, or native builtins only to make the bundle
appear larger.
