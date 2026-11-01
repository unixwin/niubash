# Completion option inventory

Enumerated from code and docs, not from the task list. "Matrix" names the case
that pins it in `baseline/summary.md`.

## Presentation: `NIU_COMPLETION_STYLE`

| Value | Aliases (`CompletionStyle::parse`) | Documented shape | Matrix |
| --- | --- | --- | --- |
| `column` (default) | `col`, `grid` | multi-column grid; any description makes one vertical column with descriptions | `styles/select_column_descriptions`, `insert/column_prefix_two_step`, `aliases/col_column`, `aliases/grid_column` |
| `gnu` | `readline` | literal readline: TAB #1 common prefix, TAB #2 printed columnar list, nothing selected | `insert/gnu_prefix_two_step`, `styles/select_gnu_printed`, `aliases/readline_gnu` |
| `ide` | `smart`, `popup` (code-only, undocumented) | multi-column popup with descriptions; selected description beside it | `insert/ide_prefix_two_step`, `styles/select_ide_descriptions`, `aliases/smart_ide_code`, `aliases/popup_ide_code` |
| `list` | `vertical` | fish-style vertical list with descriptions | `insert/list_prefix_two_step`, `styles/select_list_descriptions`, `aliases/vertical_list` |
| `inline` | `cycle`, `menu` | first match inserted directly, TAB cycles (bash `menu-complete`) | `insert/inline_prefix_two_step` (insert axis), `inline/cycle_first_match_then_next` (cycle style), `styles/select_inline_cycles`, `aliases/cycle_inline`, `aliases/menu_inline` |

Source: `crates/niubash-runtime/src/config.rs:129-176` (parse/aliases),
`docs/src/completion.md:41-50`, `docs/src/advanced-usage.md:202-231`,
`docs/src/configuration.md:46`.

## Interaction: `NIU_COMPLETION_INTERACTION`

| Value | Documented contract | Matrix |
| --- | --- | --- |
| `select` (default) | zsh menu-select: TAB #1 first candidate + highlight; TAB/Shift-TAB cycle; Up/Down rows; Left/Right columns; Enter accept/run; Esc rollback | `defaults/select_*`, `select/*` |
| `cycle` | TAB #1 first candidate + grid; later TAB/Shift-TAB cycle; Enter runs; Esc rollback | `cycle/*`, `inline/cycle_first_match_then_next` |
| `insert` | TAB completes common prefix and stops; second TAB lists; Enter confirms, Esc keeps | `insert/*`, `precedence/env_interaction_insert` |

Source: `config.rs:178-210`, `docs/src/completion.md:6-29`,
`docs/src/configuration.md:47`, `docs/src/advanced-usage.md`.
The zsh sources behind the `select` bindings are cited in `zsh-parity.md`.

## Resolution and precedence

| Knob | Sources (highest first) | Result | Matrix |
| --- | --- | --- | --- |
| style | `NIU_COMPLETION_STYLE` → default `column` | env wins; invalid warns and falls back | `precedence/env_style_gnu`, `precedence/invalid_style_falls_back` |
| interaction | `NIU_COMPLETION_INTERACTION` → default `select` | env wins; invalid warns and falls back | `precedence/env_interaction_insert`, `precedence/invalid_interaction_falls_back` |
| `completion.style` / `completion.interaction` in a config file | **not a source since 1.5.0** | file is never read; one migration notice | `precedence/config_file_ignored`, `precedence/config_migration_notice` |

There is **no config-file source**. The real chain is
`NIU_* environment variable > built-in default`; a leftover
`~/.niubash/config.toml` (or `$NIU_CONFIG`) only triggers the one-time
"no longer read / `niu config migrate`" notice.

Source: `config.rs:482-487, 556-590`,
`docs/src/configuration.md:1-30`, `CHANGELOG.md:14`.

## Other completion knobs

| Knob | Claim | Status | Matrix |
| --- | --- | --- | --- |
| `completion.page_size` | internal `MenuConfig::completion_page_size` only, default 10 | **N/A**: internal-only; no env/config key is documented | `knobs/page_size_not_an_option` |
| `completion.behavior` (prefix/substring) | internal `CompletionBehavior` (mod.rs); no env/config key | **N/A**: internal-only, not an option | `knobs/completion_behavior_not_an_option` |
| Bare-TAB indent | blank line + TAB inserts four spaces, never opens the command grid | PASS (select) | `select/bare_tab_indent` |
| Menu Enter | commits highlighted candidate, does not submit; second Enter runs | PASS (insert) | `insert/enter_confirms_then_runs` |
| Menu Esc | select/cycle roll the line back to the pre-completion snapshot; insert keeps the inserted prefix | PASS (select/cycle/insert) | `select/esc_rollback`, `cycle/esc_rollback`, `insert/esc_keeps_prefix` |
| PageUp/PageDown | not claimed for the menu (`menuselect_bindings` has no page keys) | unclaimed; probe only | `select/page_keys_unclaimed` |

## External candidates

| Surface | Contract | Matrix |
| --- | --- | --- |
| `NIU_COMPDEFS` shell function (`cmd:function`) | runs ahead of static tables; sees `NIU_COMP_WORDS`/`NIU_COMP_CWORD`, writes `NIU_COMP_RESULT` | `compdefs/compdef_wins_static` |
| GNU `complete` compspec | owns the command over static tables | `compdefs/compspec_wins_static` |
| compdef + compspec | both contribute; provider never shadows the compspec | `compdefs/compdef_and_compspec_merge` |
| `NIU_COMPDEFS` directory | loads `<cmd>.toml` and `<cmd>.bash` definition files as data | `compdefs/external_directory` |

Source: `docs/src/external-completion-providers.md:5-100`,
`crates/niubash-runtime/src/shell.rs:3012-3055`.

## Default rule

`column` + `select` is the built-in default and the matrix asserts it stayed
so (`defaults/config_rows`). No product default or option was changed by this
branch; the harness only records behavior.
