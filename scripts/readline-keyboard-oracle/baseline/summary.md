# Readline keyboard oracle baseline

- GNU bash: `/data/data/com.termux/files/usr/bin/bash`
- GNU version: `GNU bash, version 5.3.20(1)-release (aarch64-unknown-linux-android) / Copyright (C) 2025 Free Software Foundation, Inc. / License GPLv3+: GNU GPL version 3 or later <http://gnu.org/licenses/gpl.html>`
- niu: `/data/data/com.termux/files/home/wt/niu-completion-gnu/target/debug/niu`
- niu version: `Niubash 1.4.4 — bash-compatible shell for Windows / rubash   git c9604443ae09 / Home: <https://unixwin.dev>`
- niu parity env: `NIU_COMPLETION_STYLE=gnu`

**PASS 50 / FAIL 0 / KNOWN 0 / SKIP 0** (50 cases)

Expected values are captured from GNU bash 5.3.20 in the same run; the oracle compares raw buffer and point bytes.

## Passes

| Case | Keys | GNU (line, point) | niu (line, point) |
| --- | --- | --- | --- |
| `text/chars` | `` | `b'hello'/b'5'` | `b'hello'/b'5'` |
| `text/chars_mid_insert` | `C-a X` | `b'Xabc'/b'1'` | `b'Xabc'/b'1'` |
| `text/left_right` | `Left Left Right` | `b'abc'/b'2'` | `b'abc'/b'2'` |
| `text/home` | `Home` | `b'abc def'/b'0'` | `b'abc def'/b'0'` |
| `text/end` | `End` | `b'abc def'/b'7'` | `b'abc def'/b'7'` |
| `text/c_a` | `C-a` | `b'abc def'/b'0'` | `b'abc def'/b'0'` |
| `text/c_e` | `C-e` | `b'abc def'/b'7'` | `b'abc def'/b'7'` |
| `text/c_b` | `C-b C-b` | `b'abc'/b'1'` | `b'abc'/b'1'` |
| `text/c_f_mid` | `C-a C-f C-f` | `b'abc'/b'2'` | `b'abc'/b'2'` |
| `text/m_b` | `M-b` | `b'abc def'/b'4'` | `b'abc def'/b'4'` |
| `text/m_f` | `C-a M-f` | `b'abc def'/b'3'` | `b'abc def'/b'3'` |
| `text/c_w` | `C-w` | `b'abc '/b'4'` | `b'abc '/b'4'` |
| `text/c_u` | `C-u` | `b''/b'0'` | `b''/b'0'` |
| `text/c_k_mid` | `C-a C-f C-f C-k` | `b'ab'/b'2'` | `b'ab'/b'2'` |
| `text/c_y` | `C-w C-y` | `b'foo bar'/b'7'` | `b'foo bar'/b'7'` |
| `text/m_y` | `C-w C-a C-k C-y M-y` | `b'beta'/b'4'` | `b'beta'/b'4'` |
| `text/c_t` | `C-a C-f C-f C-t` | `b'acbd'/b'3'` | `b'acbd'/b'3'` |
| `text/m_t` | `C-e M-t` | `b'one three two'/b'13'` | `b'one three two'/b'13'` |
| `text/m_u` | `C-a M-u` | `b'HELLO world'/b'5'` | `b'HELLO world'/b'5'` |
| `text/m_l` | `C-a M-l` | `b'hello world'/b'5'` | `b'hello world'/b'5'` |
| `text/m_c` | `C-a M-c` | `b'Hello world'/b'5'` | `b'Hello world'/b'5'` |
| `text/set_mark_m_f` | `C-a C-@ M-f` | `b'abc def'/b'3'` | `b'abc def'/b'3'` |
| `text/exchange_point_mark` | `C-a C-@ M-f C-x C-x` | `b'abc def'/b'0'` | `b'abc def'/b'0'` |
| `text/exchange_then_insert` | `C-a C-@ M-f C-x C-x X` | `b'Xabc def'/b'1'` | `b'Xabc def'/b'1'` |
| `text/undo` | `C-_` | `b''/b'0'` | `b''/b'0'` |
| `text/undo_c_x_c_u` | `C-x C-u` | `b''/b'0'` | `b''/b'0'` |
| `text/c_g` | `C-g` | `b'abc'/b'3'` | `b'abc'/b'3'` |
| `text/c_g_then_key` | `C-g X` | `b'abcX'/b'4'` | `b'abcX'/b'4'` |
| `text/bracketed_paste` | `ESC[200~echo hi ESC[201~` | `b'echo hi'/b'7'` | `b'echo hi'/b'7'` |
| `history/up` | `Up` | `b'gamma three'/b'11'` | `b'gamma three'/b'11'` |
| `history/down` | `Up Down` | `b''/b'0'` | `b''/b'0'` |
| `history/m_p` | `M-p beta Enter` | `b'beta two'/b'0'` | `b'beta two'/b'0'` |
| `history/m_n` | `Up Up Up M-n beta Enter` | `b'beta two'/b'0'` | `b'beta two'/b'0'` |
| `history/c_r` | `C-r beta C-j` | `b'beta two'/b'0'` | `b'beta two'/b'0'` |
| `history/c_r_abort` | `C-r beta C-g` | `b''/b'0'` | `b''/b'0'` |
| `history/c_o` | `Up Up C-o` | `b'gamma three'/b'11'` | `b'gamma three'/b'11'` |
| `complete/tab_common_prefix` | `TAB` | `b'cat alp'/b'7'` | `b'cat alp'/b'7'` |
| `complete/tab_single` | `TAB` | `b'cat alpha.txt '/b'14'` | `b'cat alpha.txt '/b'14'` |
| `complete/tab_double` | `TAB TAB` | `b'cat alp'/b'7'` | `b'cat alp'/b'7'` |
| `complete/m_question` | `M-?` | `b'cat al'/b'6'` | `b'cat al'/b'6'` |
| `complete/m_equal` | `M-=` | `b'cat al'/b'6'` | `b'cat al'/b'6'` |
| `complete/m_star` | `M-*` | `b'cat alpha.txt alpine.txt '/b'25'` | `b'cat alpha.txt alpine.txt '/b'25'` |
| `complete/m_slash` | `M-/` | `b'cat alpha.txt '/b'14'` | `b'cat alpha.txt '/b'14'` |
| `complete/m_tilde` | `M-~` | `b'~ro'/b'3'` | `b'~ro'/b'3'` |
| `complete/m_at` | `M-@` | `b'@localhost '/b'11'` | `b'@localhost '/b'11'` |
| `complete/m_lbrace` | `M-{` | `b'cat alp{ha.txt,ine.txt} '/b'24'` | `b'cat alp{ha.txt,ine.txt} '/b'24'` |
| `complete/m_g` | `M-g` | `b'cat alp'/b'7'` | `b'cat alp'/b'7'` |
| `complete/default_style_tab` | `TAB` | `b'cat alp'/b'7'` | `b'cat al'/b'6'` |
| `complete/m_question_after_prefix` | `TAB M-?` | `b'cat alp'/b'7'` | `b'cat alp'/b'7'` |
| `macro/record_replay` | `C-x ( ab C-x ) C-x e` | `b'aba'/b'3'` | `b'aba'/b'3'` |

Raw PTY transcripts: `scripts/readline-keyboard-oracle/transcripts/latest`
