# Hook Contract

Niubash runs shell functions at fixed lifecycle points, the same way zsh/fish
run their hooks. Hooks only fire in **interactive** sessions, and they need
the oh-my-niu framework loaded (source `oh-my-niu.winux` from
`~/.niubashrc`), which provides the registration helpers and the runners the
host calls.

## Registering a hook

Register the **name of a shell function** — not code:

```bash
# ~/.niubashrc
function notify_on_dir_change {
  echo "now in $NIU_PWD"
}
niubash_add_chpwd_hook notify_on_dir_change
```

Registration is idempotent: adding the same function twice registers it once.

## Lifecycle hooks

| Hook             | Fires                          | Context variables         |
| ---------------- | ------------------------------ | ------------------------- |
| `startup`        | once, after rc is loaded       | —                         |
| `precmd`         | before every prompt draw       | —                         |
| `preexec`        | before a command executes      | `NIU_PREEXEC_COMMAND`     |
| `postcmd`        | after a command finishes       | `NIU_LAST_EXIT_CODE`      |
| `chpwd`          | after the directory changes    | `NIU_OLDPWD`, `NIU_PWD`   |
| `period`         | every `NIU_PERIOD_SECONDS` s   | —                         |
| `zshaddhistory`  | after a command enters history | `NIU_HISTORY_COMMAND`     |
| `zshexit`        | when the shell exits           | —                         |
| `greeting`       | at startup (fish-style hello)  | —                         |
| `title`          | terminal title update          | `NIU_TITLE`               |

Example — time every command with `preexec`/`postcmd`:

```bash
function __timer_start { NIU_CMD_START=$SECONDS; }
function __timer_stop  { echo "last command took $((SECONDS - NIU_CMD_START))s (exit $NIU_LAST_EXIT_CODE)"; }
niubash_add_preexec_hook __timer_start
niubash_add_postcmd_hook __timer_stop
```

Periodic work (battery check, git fetch, …):

```bash
NIU_PERIOD_SECONDS=60
function fetch_reminders { date; }
niubash_add_period_hook fetch_reminders
```

## Trap hooks

Signal-flavoured hooks mirror zsh traps and register the same way:

`trapdebug`, `traperr`, `trapint`, `trapwinch`, `trapusr1`, `trapusr2`,
`trappipe`, `trapterm`, `trapchld`, `trapzerr`.

## Practical notes

- Hook functions run in the current shell: they can read the real
  environment, set variables, and change the prompt, but heavy work belongs
  in background jobs so prompt drawing stays fast.
- The host keeps the cold-start budget (~170 ms) — avoid slow work in
  `startup`/`greeting`.
- `precmd` runs before every prompt: keep it cheap, and prefer `chpwd` over
  re-statting the directory on every prompt.
- Framework plugins register their own hooks through the same helpers, so a
  plugin pack and your `.niubashrc` compose cleanly.
