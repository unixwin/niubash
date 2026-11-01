# Completion options matrix baseline

- base: `3018703`
- niu: `/data/data/com.termux/files/home/wt/niu-completion-matrix/target/debug/niu`
- version: `Niubash 1.4.4 — bash-compatible shell for Windows`
- **PASS 38 / FAIL 18 / MISSING 2 / SKIP 0** (58 cases)

Expected values are the documented contract. Selection/highlight assertions are
frame-level (reverse-video cell), not buffer text.

## aliases

| Case | Status | Expected | Actual | Source |
| --- | --- | --- | --- | --- |
| `aliases/readline_gnu` | PASS | readline -> gnu | expected: alias 'readline' behaves exactly like 'gnu': ('ls alp', (), 'ls alp', ((2, 'alpha.txt   alpine.txt'),)); actual: alias screen ('ls alp', (), 'ls alp', ((2, 'alpha.txt   alpine.txt'),)) | config.rs CompletionStyle::parse aliases |
| `aliases/col_column` | PASS | col -> column | expected: alias 'col' behaves exactly like 'column': ('ls alp', (), 'ls alp', ((2, 'alpha.txt           alpine.txt'),)); actual: alias screen ('ls alp', (), 'ls alp', ((2, 'alpha.txt           alpine.txt'),)) | config.rs CompletionStyle::parse aliases |
| `aliases/grid_column` | PASS | grid -> column | expected: alias 'grid' behaves exactly like 'column': ('ls alp', (), 'ls alp', ((2, 'alpha.txt           alpine.txt'),)); actual: alias screen ('ls alp', (), 'ls alp', ((2, 'alpha.txt           alpine.txt'),)) | config.rs CompletionStyle::parse aliases |
| `aliases/vertical_list` | PASS | vertical -> list | expected: alias 'vertical' behaves exactly like 'list': ('ls al', ((2, '0: alpha.txt'), (3, '1: alpine.txt'), (4, 'Page 1: records 0 - 1  total: 2')), 'ls al', ((2, '0: alpha.txt'), (3, '1: alpine.txt'), (4, 'Page 1: records 0 - 1  total: 2'))); actual: alias screen ('ls al', ((2, '0: alpha.txt'), (3, '1: alpine.txt'), (4, 'Page 1: records 0 - 1  total: 2')), 'ls al', ((2, '0: alpha.txt'), (3, '1: alpine.txt'), (4, 'Page 1: records 0 - 1  total: 2'))) | config.rs CompletionStyle::parse aliases |
| `aliases/cycle_inline` | PASS | cycle -> inline | expected: alias 'cycle' behaves exactly like 'inline': ('ls al', ((2, '0: alpha.txt'), (3, 'Page 1: records 0 - 0  total: 2')), 'ls al', ((2, '0: alpha.txt'), (3, 'Page 1: records 0 - 0  total: 2'))); actual: alias screen ('ls al', ((2, '0: alpha.txt'), (3, 'Page 1: records 0 - 0  total: 2')), 'ls al', ((2, '0: alpha.txt'), (3, 'Page 1: records 0 - 0  total: 2'))) | config.rs CompletionStyle::parse aliases |
| `aliases/menu_inline` | PASS | menu -> inline | expected: alias 'menu' behaves exactly like 'inline': ('ls al', ((2, '0: alpha.txt'), (3, 'Page 1: records 0 - 0  total: 2')), 'ls al', ((2, '0: alpha.txt'), (3, 'Page 1: records 0 - 0  total: 2'))); actual: alias screen ('ls al', ((2, '0: alpha.txt'), (3, 'Page 1: records 0 - 0  total: 2')), 'ls al', ((2, '0: alpha.txt'), (3, 'Page 1: records 0 - 0  total: 2'))) | config.rs CompletionStyle::parse aliases |
| `aliases/smart_ide_code` | PASS | smart -> ide | expected: alias 'smart' behaves exactly like 'ide': ('ls alp', ((2, 'alpha.txt'), (3, 'alpine.txt')), 'ls alp', ((2, 'alpha.txt'), (3, 'alpine.txt'))); actual: alias screen ('ls alp', ((2, 'alpha.txt'), (3, 'alpine.txt')), 'ls alp', ((2, 'alpha.txt'), (3, 'alpine.txt'))) | config.rs CompletionStyle::parse aliases |
| `aliases/popup_ide_code` | PASS | popup -> ide | expected: alias 'popup' behaves exactly like 'ide': ('ls alp', ((2, 'alpha.txt'), (3, 'alpine.txt')), 'ls alp', ((2, 'alpha.txt'), (3, 'alpine.txt'))); actual: alias screen ('ls alp', ((2, 'alpha.txt'), (3, 'alpine.txt')), 'ls alp', ((2, 'alpha.txt'), (3, 'alpine.txt'))) | config.rs CompletionStyle::parse aliases |

## compdefs

| Case | Status | Expected | Actual | Source |
| --- | --- | --- | --- | --- |
| `compdefs/compdef_wins_static` | PASS | NIU_COMPDEFS wins over static tables | expected: compdef candidates only; static table (checkout/commit) does not contribute; actual: ['defonly-1  defonly-2'] | docs/src/external-completion-providers.md |
| `compdefs/compspec_wins_static` | PASS | a GNU compspec owns the command over static tables | expected: compspec candidates only; static table does not contribute; actual: ['spec-one  spec-two'] | docs/src/external-completion-providers.md |
| `compdefs/compdef_and_compspec_merge` | PASS | provider never shadows a compspec; both contribute, static table does not | expected: compdef and compspec candidates merge; static table does not contribute; actual: ['defonly-1  defonly-2  spec-one  spec-two'] | docs/src/external-completion-providers.md |
| `compdefs/external_directory` | PASS | NIU_COMPDEFS directory loads TOML and bash completion files | expected: TOML file serves --fixture-flag; bash-import file serves --verbose; actual: toml=yes, bash=yes; tail=['', '', ''] | docs/src/external-completion-providers.md section 3 (completion definition files) |

## cycle

| Case | Status | Expected | Actual | Source |
| --- | --- | --- | --- | --- |
| `cycle/tab1_first_candidate` | PASS | line 'cd cmp-aa/' | expected: line 'cd cmp-aa/'; actual: line 'cd cmp-aa/', highlighted ['cmp-aa'] | config.rs CompletionInteraction::Cycle; docs/src/completion.md#completion-styles |
| `cycle/tab2_second` | PASS | line 'cd cmp-bb/' | expected: line 'cd cmp-bb/'; actual: line 'cd cmp-bb/', highlighted ['cmp-bb'] | config.rs CompletionInteraction::Cycle; docs/src/completion.md#completion-styles |
| `cycle/shift_tab_wraps_last` | PASS | line 'cd cmp-ff/' | expected: line 'cd cmp-ff/'; actual: line 'cd cmp-ff/', highlighted ['cmp-ff'] | config.rs CompletionInteraction::Cycle; docs/src/completion.md#completion-styles |
| `cycle/enter_accept_runs` | PASS | Enter accepts and runs | expected: Enter runs cd into cmp-aa; actual: ['__NIU_CMP__> cd cmp-aa/', '__NIU_CMP__> basename "$PWD"', 'cmp-aa', '__NIU_CMP__>'] | niubash#631 cycle contract |
| `cycle/esc_rollback` | PASS | Esc rolls back | expected: Esc restores pre-completion 'cd cmp-'; actual: before 'cd cmp-bb/', after 'cd cmp-' | niubash#631 cycle contract |

## defaults

| Case | Status | Expected | Actual | Source |
| --- | --- | --- | --- | --- |
| `defaults/config_rows` | PASS | built-in default is column + select | expected: completion.style = column (default); completion.interaction = select (default); actual: Effective settings (source: env, inputrc, or default):   command-not-found-hint = off (default)   completion.style = column (default)   completion.interaction = select (default)   startup.rc-policy = primary-only (default)   editing-mode = emacs (default)   keyseq-timeout = 500 (default)  Key bindings are read from rc `bind` lines and $INPUTRC/~/.inputrc; they are not a niu setting. | docs/src/configuration.md#the-three-sources |
| `defaults/select_tab1_first_candidate` | FAIL | TAB #1 splices first candidate and highlights it | expected: line 'cd cmp-aa/' and first cell cmp-aa highlighted; actual: line 'cd cmp-', highlighted [] | config.rs CompletionInteraction::Select (owner ruling #742/#778); zsh complist.c:2813 `mselect = (*(minfo.cur))->gnum` (first match starts selected) |
| `defaults/select_tab2_cycles` | FAIL | second TAB moves to the second candidate | expected: line 'cd cmp-bb/' and cell cmp-bb highlighted; actual: line 'cd cmp-aa/', highlighted [] | docs/src/completion.md#completion-styles; zsh /data/data/com.termux/files/home/refs/zsh/Doc/Zsh/mod_complist.yo movement wraps |

## inline

| Case | Status | Expected | Actual | Source |
| --- | --- | --- | --- | --- |
| `inline/cycle_first_match_then_next` | PASS | inline style cycles candidates in place | expected: inline cycle: TAB1 'cd cmp-aa/', TAB2 'cd cmp-bb/'; actual: TAB1 'cd cmp-aa/' (grid=True), TAB2 'cd cmp-bb/' | docs/src/completion.md#completion-styles inline = first match inserted, TAB cycles (bash menu-complete) |

## insert

| Case | Status | Expected | Actual | Source |
| --- | --- | --- | --- | --- |
| `insert/column_prefix_two_step` | PASS | TAB1 completes common prefix 'ls alp', TAB2 lists | expected: TAB1 'ls alp' with no menu, TAB2 lists; actual: TAB1 'ls alp' menu=[], TAB2 'ls alp' menu=[(2, 'alpha.txt           alpine.txt')] | docs/src/completion.md#completion-styles two-step rule; style=column |
| `insert/gnu_prefix_two_step` | PASS | TAB1 completes common prefix 'ls alp', TAB2 lists | expected: TAB1 'ls alp' with no menu, TAB2 lists; actual: TAB1 'ls alp' menu=[], TAB2 'ls alp' menu=[(2, 'alpha.txt   alpine.txt')] | docs/src/completion.md#completion-styles two-step rule; style=gnu |
| `insert/ide_prefix_two_step` | FAIL | TAB1 completes common prefix 'ls alp', TAB2 lists | expected: TAB1 'ls alp' with no menu, TAB2 lists; actual: TAB1 'ls alp' menu=[(2, 'alpha.txt'), (3, 'alpine.txt')], TAB2 'ls alp' menu=[(2, 'alpha.txt'), (3, 'alpine.txt')] | docs/src/completion.md#completion-styles two-step rule; style=ide |
| `insert/list_prefix_two_step` | FAIL | TAB1 completes common prefix 'ls alp', TAB2 lists | expected: TAB1 'ls alp' with no menu, TAB2 lists; actual: TAB1 'ls al' menu=[(2, '0: alpha.txt'), (3, '1: alpine.txt'), (4, 'Page 1: records 0 - 1  total: 2')], TAB2 'ls al' menu=[(2, '0: alpha.txt'), (3, '1: alpine.txt'), (4, 'Page 1: records 0 - 1  total: 2')] | docs/src/completion.md#completion-styles two-step rule; style=list |
| `insert/inline_first_match` | FAIL | TAB1 completes common prefix 'ls alpha.txt', TAB2 lists | expected: TAB1 'ls alpha.txt' with no menu, TAB2 lists; actual: TAB1 'ls al' menu=[(2, '0: alpha.txt'), (3, 'Page 1: records 0 - 0  total: 2')], TAB2 'ls al' menu=[(2, '0: alpha.txt'), (3, 'Page 1: records 0 - 0  total: 2')] | docs/src/completion.md#completion-styles two-step rule; style=inline |
| `insert/enter_confirms_then_runs` | PASS | Enter commits the highlighted candidate, a second Enter runs | expected: first Enter commits 'cd cmp-aa/' without running; second runs it; actual: after first Enter 'cd cmp-aa/', screen tail=['__NIU_CMP__> cd cmp-aa/', '__NIU_CMP__> basename "$PWD"', 'cmp-aa', '__NIU_CMP__>'] | docs/src/completion.md#completion-styles Enter with the menu open; config.rs Insert contract |
| `insert/esc_keeps_prefix` | PASS | Esc keeps what was inserted | expected: Esc keeps the inserted common prefix; actual: before 'ls alp', after 'ls alp' | config.rs Insert contract |

## knobs

| Case | Status | Expected | Actual | Source |
| --- | --- | --- | --- | --- |
| `knobs/page_size_not_exposed` | MISSING | page size is exposed as a documented knob | expected: a completion.page_size/env knob exists and limits the page; actual: env ignored; no config row; 12/12 candidates shown | config.rs MenuConfig::completion_page_size (fixed 10); no env/config key documented |
| `knobs/completion_behavior_not_exposed` | MISSING | prefix/substring behavior is a documented, working knob | expected: completion.behavior=substring matches inside the word; actual: line='ls pha', menu=[] | config.rs only resolves style/interaction; no completion.behavior key |

## precedence

| Case | Status | Expected | Actual | Source |
| --- | --- | --- | --- | --- |
| `precedence/env_style_gnu` | PASS | env > default for style | expected: env NIU_COMPLETION_STYLE=gnu gives gnu prefix/list; actual: TAB1 'ls alp', TAB2 'ls alp', menu=[(2, 'alpha.txt   alpine.txt')] | docs/src/configuration.md#the-three-sources |
| `precedence/env_interaction_insert` | PASS | env > default for interaction | expected: env NIU_COMPLETION_INTERACTION=insert prefixes and stops; actual: TAB1 'ls alp', menu=[] | docs/src/configuration.md#the-three-sources |
| `precedence/invalid_style_falls_back` | PASS | invalid style warns once and falls back to column | expected: one diagnostic and fallback to column (default); actual: diagnostic=yes; rows='Effective settings (source: env, inputrc, or default):\n  command-not-found-hint = off (default)\n  completion.style = column (default)\n  completion.interaction = select (default)\n  startup.rc-policy = primary-only (default)\n  editing-mode = emacs (default)\n  keyseq-timeout = 500 (default)\n\nKey bindings are read from rc `bind` lines and $INPUTRC/~/.inputrc; they are not a niu setting.\nniubash: NIU_COMPLETION_STYLE must be one of: gnu, ide, column, list, inline\n' | docs/src/configuration.md#the-three-sources |
| `precedence/invalid_interaction_falls_back` | PASS | invalid interaction warns once and falls back to select | expected: one diagnostic and fallback to select (default); actual: diagnostic=yes; rows='Effective settings (source: env, inputrc, or default):\n  command-not-found-hint = off (default)\n  completion.style = column (default)\n  completion.interaction = select (default)\n  startup.rc-policy = primary-only (default)\n  editing-mode = emacs (default)\n  keyseq-timeout = 500 (default)\n\nKey bindings are read from rc `bind` lines and $INPUTRC/~/.inputrc; they are not a niu setting.\nniubash: NIU_COMPLETION_INTERACTION must be one of: insert, cycle, select\n' | docs/src/configuration.md#the-three-sources |
| `precedence/config_file_ignored` | PASS | completion.style/completion.interaction in a config file are not honored | expected: config.toml is not read; rows stay column/select defaults; actual: Effective settings (source: env, inputrc, or default):   command-not-found-hint = off (default)   completion.style = column (default)   completion.interaction = select (default)   startup.rc-policy = primary-only (default)   editing-mode = emacs (default)   keyseq-timeout = 500 (default)  Key bindings are read from rc `bind` lines and $INPUTRC/~/.inputrc; they are not a niu setting. | CHANGELOG.md 1.5.0 (config.toml gone); docs/src/configuration.md |
| `precedence/env_over_config` | PASS | env overrides a leftover config file | expected: env inline/insert wins; config gnu/cycle is not a source; actual: Effective settings (source: env, inputrc, or default):   command-not-found-hint = off (default)   completion.style = inline (env)   completion.interaction = insert (env)   startup.rc-policy = primary-only (default)   editing-mode = emacs (default)   keyseq-timeout = 500 (default)  Key bindings are read from rc `bind` lines and $INPUTRC/~/.inputrc; they are not a niu setting. | docs/src/configuration.md precedence: NIU_* env > built-in default; config.toml gone |
| `precedence/config_migration_notice` | PASS | leftover config.toml prints one migration notice and is never read | expected: startup notice names 'is no longer read' and 'niu config migrate'; actual: ['Niubash 1.4.4', 'niubash: /data/data/com.termux/files/home/wt/niu-completion-matrix/scripts/completion-options-matrix/.work/precedence__config_migration_notice/home/.niubash/config.toml is no longer read — run `niu config migrate` to move its settings into ~/.niubashrc (the old file is then renamed)', '\x1b[?2004h\x1b[?25l\x1b[6n\x1b[0m\x1b[?25l\x1b[?2026h\x1b[6;1H\x1b[J\x1b[92m__NIU_CMP__> \x1b[39m\x1b[35m\x1b[0m\x1b[0m\x1b7\x1b8\x1b[?25h\x1b[?2026l'] | tests/issue631_completion_interaction.rs legacy-config notice |

## select

| Case | Status | Expected | Actual | Source |
| --- | --- | --- | --- | --- |
| `select/shift_tab_previous` | FAIL | line 'cd cmp-ff/' | expected: line 'cd cmp-ff/' with that candidate highlighted; actual: line 'cd cmp-ff/', highlighted [] | zsh /data/data/com.termux/files/home/refs/zsh/Src/Zle/complist.c menuselect_bindings: TAB = complete-word; /data/data/com.termux/files/home/refs/zsh/Doc/Zsh/mod_complist.yo movement wraps at edges |
| `select/down_one_row` | FAIL | line 'cd cmp-bb/' | expected: line 'cd cmp-bb/' with that candidate highlighted; actual: line 'cd cmp-', highlighted [] | zsh /data/data/com.termux/files/home/refs/zsh/Src/Zle/complist.c menuselect_bindings: Down = down-line-or-history; /data/data/com.termux/files/home/refs/zsh/Doc/Zsh/mod_complist.yo movement wraps |
| `select/up_one_row_wraps` | FAIL | line 'cd cmp-ff/' | expected: line 'cd cmp-ff/' with that candidate highlighted; actual: line 'cd cmp-', highlighted [] | zsh /data/data/com.termux/files/home/refs/zsh/Src/Zle/complist.c: Up = up-line-or-history; /data/data/com.termux/files/home/refs/zsh/Doc/Zsh/mod_complist.yo wrap |
| `select/right_one_column` | FAIL | line 'cd cmp-cc/' | expected: line 'cd cmp-cc/' with that candidate highlighted; actual: line 'cd cmp-', highlighted [] | zsh /data/data/com.termux/files/home/refs/zsh/Src/Zle/complist.c: Right = forward-char; /data/data/com.termux/files/home/refs/zsh/Doc/Zsh/mod_complist.yo wrap; column-major grid row stride = 2 for 6 candidates at 40 columns |
| `select/left_one_column_wraps` | FAIL | line 'cd cmp-ee/' | expected: line 'cd cmp-ee/' with that candidate highlighted; actual: line 'cd cmp-', highlighted [] | zsh /data/data/com.termux/files/home/refs/zsh/Src/Zle/complist.c: Left = backward-char; /data/data/com.termux/files/home/refs/zsh/Doc/Zsh/mod_complist.yo wrap |
| `select/enter_accept_runs` | FAIL | Enter accepts and runs | expected: Enter after TAB runs cd into cmp-aa; actual: ['irectory', '__NIU_CMP__> basename "$PWD"', 'work', '__NIU_CMP__>'] | zsh /data/data/com.termux/files/home/refs/zsh/Src/Zle/complist.c menuselect_bindings: Enter = accept-line |
| `select/esc_rollback` | PASS | Esc rolls the line back | expected: Esc restores pre-completion 'cd cmp-'; actual: before 'cd cmp-aa/', after 'cd cmp-' | docs/src/completion.md two-step rule; niu CycleSession rollback contract |
| `select/lone_candidate` | PASS | lone candidate completes outright | expected: lone candidate completes outright 'cd only-dir/'; actual: 'cd only-dir/' | docs/src/completion.md#completion-styles two-step rule |
| `select/bare_tab_indent` | PASS | blank-line TAB inserts four spaces | expected: four-space indent, no candidate grid; actual: line '', indent-paint=yes, menu [] | config.rs BARE_TAB_INDENT (#742) |
| `select/grid_column_major` | PASS | 6 candidates at 40 cols form 3x2 column-major grid | expected: column-major grid rows ['cmp-aa', 'cmp-cc', 'cmp-ee'] / ['cmp-bb', 'cmp-dd', 'cmp-ff']; actual: [['cmp-aa', 'cmp-cc', 'cmp-ee'], ['cmp-bb', 'cmp-dd', 'cmp-ff']] | gnu_menu.rs packed_column_layout (zsh list-packed geometry) |
| `select/lscolors` | PASS | directories paint through LS_COLORS | expected: unselected directory cells paint LS_COLORS di=01;35; actual: di=01;35 SGR present | completion/file_colors.rs (#743) |
| `select/page_keys_unclaimed` | PASS | PageDown does not select or splice a candidate | expected: PageDown is not claimed by the docs; line stays put; actual: before 'cd cmp-', after 'cd cmp-' | /data/data/com.termux/files/home/refs/zsh/Src/Zle/complist.c menuselect_bindings has no PageUp/PageDown; niu docs do not claim them |

## styles

| Case | Status | Expected | Actual | Source |
| --- | --- | --- | --- | --- |
| `styles/select_column_descriptions` | FAIL | column description vertical | expected: any description forces the column style to a single vertical column with descriptions; actual: lines=['start  stop  status'] | docs/src/completion.md#completion-styles; style=column |
| `styles/select_ide_descriptions` | FAIL | ide descriptions | expected: popup shows each candidate's description; actual: lines=['start  stop  status'] | docs/src/completion.md#completion-styles; style=ide |
| `styles/select_list_descriptions` | FAIL | list descriptions | expected: vertical list, one candidate per row, all descriptions visible; actual: lines=['start  stop  status'], highlighted=[] | docs/src/completion.md#completion-styles; style=list |
| `styles/select_gnu_printed` | PASS | gnu printed no selection | expected: TAB1 'ls alp', TAB2 list with no selection; actual: TAB1 'ls alp' hi=[], TAB2 'ls alp' hi=[] | docs/src/completion.md#completion-styles; style=gnu |
| `styles/cycle_column_descriptions` | FAIL | the style's documented shape survives the cycle interaction | expected: vertical list, one candidate per row, all descriptions visible; actual: lines=['start  stop  status'] | docs/src/completion.md#completion-styles; style=column, interaction=cycle |
| `styles/cycle_ide_descriptions` | FAIL | the style's documented shape survives the cycle interaction | expected: vertical list, one candidate per row, all descriptions visible; actual: lines=['start  stop  status'] | docs/src/completion.md#completion-styles; style=ide, interaction=cycle |
| `styles/cycle_list_descriptions` | FAIL | the style's documented shape survives the cycle interaction | expected: vertical list, one candidate per row, all descriptions visible; actual: lines=['start  stop  status'] | docs/src/completion.md#completion-styles; style=list, interaction=cycle |
| `styles/cycle_gnu_printed` | PASS | gnu style stays a printed list under cycle | expected: gnu is exempt: TAB1 'ls alp', TAB2 printed list, no selection; actual: TAB1 'ls alp' hi=[], TAB2 'ls alp' hi=[] | docs/src/completion.md#completion-styles; gnu interaction axis is inert |
| `styles/select_inline_cycles` | FAIL | inline style cycles candidates in place | expected: inline presentation cycles in place; TAB1 cmp-aa/, TAB2 cmp-bb/; actual: TAB1 'cd cmp-' grid=2 rows, TAB2 'cd cmp-aa/' | docs/src/completion.md#completion-styles inline = first match inserted, TAB cycles |

