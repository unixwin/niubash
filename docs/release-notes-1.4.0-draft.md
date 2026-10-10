# niubash 1.4.0 — Release Notes (DRAFT)

> **Status: draft.** Do not tag or trigger the release workflow from this
> branch. Entries marked **[in flight]** depend on work the release
> coordinator is collecting before the tag (winuxcmd bundle items);
> resolve or prune them when cutting the real release. Engine for this
> cycle: rubash 1.4.0 (its own release notes live in unixwin/rubash).

---

## English

niubash 1.4.0 is a compatibility-and-comfort release: the engine tracks
GNU Bash 5.3.0 much more closely (83-suite diff −40%), login shells and
real readline editing arrive, pasting a multiline script finally behaves
like bash on both platforms, and the plugin/doctor/config surfaces get a
round of honesty fixes.

### Added

- **Login shells** (engine, rubash 1.4.0): `-l`/`--login` (or an
  `argv[0]` starting with `-`) runs the GNU startup chain on Unix —
  `/etc/profile`, then the first of `~/.bash_profile` | `~/.bash_login` |
  `~/.profile`; non-login interactive shells source `/etc/bash.bashrc` +
  `~/.bashrc`. `--noprofile`/`--norc` suppress the corresponding branch.
  (Windows accepts the flags with unchanged behavior.)
- **Real readline REPL on the bare console** (engine): a console-attached
  `rubash`/`niu` now has raw-mode line editing — history recall (arrows,
  C-p/C-n), in-line cursor motion, C-r reverse search — instead of
  conhost's cooked line buffer.
- **GNU paste semantics on both platforms**: Unix terminals get real
  bracketed paste (pasted newlines no longer execute mid-paste, #234),
  and the Windows console path gets PSReadLine-style paste-chunk
  detection (#252) — a pasted 257-line script executes exactly once, on
  your Enter, not once per line.
- **Android and OpenHarmony targets incubating in release CI** (#247):
  bionic-linked aarch64/armv7 Android zips (ELF-verified), OpenHarmony
  aarch64 check-only for now; both legs are continue-on-error and outside
  the release needs while they season.
- **carapace-bin completion source** (#243): Tab completion for
  third-party commands (git, curl, gh, docker, … ~1200 upstream specs)
  with descriptions, whenever carapace-bin is installed
  (`NIU_CARAPACE=off` disables; local definitions always win).
- **`niu doctor` timing advice** (#241): points timing-sensitive scripts
  at the bash 5 builtin `$EPOCHREALTIME`/`$EPOCHSECONDS` instead of
  paying Windows process creation for every `$(date +%s%N)`.
- **"Calling PowerShell from niu" documentation** (#253, #203), plus a
  plain statement that niu's MSYS labels are a persona of a pure native
  Windows build — no msys-2.0.dll, no cygwin1.dll (#248).
- **Vi editing mode** (#184): `set -o vi` / `set -o emacs` switch the
  live line editor mid-session, with a floor vi-mode indicator.
- **Agent skill bundle** (#188): `niu skill install` / `niu skill status`
  ship the generated niubash skill (SKILL.md + quickref) into agent skill
  dirs; releases attach `niubash-skill-v*.zip`.
- **Batteries pre-installed by the release pipeline** (#189, #230): the
  Windows release zip and installer carry gawk (GNU Awk 5.4.1, with an
  `awk` shim), niugit 2.55.0.3, ripgrep 15.2.0 and fd 10.4.2 installed
  from the wpm index at packaging time — plugin distros and search work
  on a fresh machine with no network fetch.
- **WinuxCmd applet completions compiled in** (#172): all 180 bundled
  applets complete with real flags and descriptions with zero
  configuration.

### Fixed

- **Pasted multiline scripts no longer execute line-by-line** (#202):
  fixed per platform — bracketed paste on Unix (#234); on Windows, burst
  detection buffers the paste and submits it once (#252).
- **bash-it theme history interlock** (#182): `history -a && history -c
  && history -r` from PROMPT_COMMAND doubled the history file every
  prompt and wedged the session; GNU append/read/clear semantics are in,
  pinned by a 12-prompt-cycle ConPTY regression (#250).
- **A corrupt plugin source registry is never silently rewritten**
  (#178): unparsable registry.toml bytes are preserved to
  `registry.toml.corrupt`, writes are refused, and `niu doctor` names the
  parse error — pin commits, checksums and trust state survive (#246).
- **Command-not-found no longer advertises package managers** (#249):
  the unsolicited winget/scoop/choco hint block is gone; a wpm-only hint
  is available opt-in via `niu config set command-not-found-hint wpm`
  (#251).
- **Setup wizard audit P2 batch** (#179): the undo receipt covers the
  generated rc, Ctrl-C prints the cancelled note, `niu plugin discover`
  enumerates themes, zh strings realign with the runtime, gutted sources
  name the repair verb (#239).
- **`niu doctor` on Linux/macOS** (#194): Windows-only checks report n/a
  instead of failing the critical tally (#242).
- **Raw ESC bytes survive** (#200): a word-initial ESC inside quotes in a
  script file is no longer dropped by the lexer (#235, engine fix
  rubash#474).
- **Engine (rubash 1.4.0)**: command-substitution `$?` publishes during
  expansion (rubash#485); same-line `#` comments after `f() {`; `set -u`
  arithmetic no longer flags assigned variables; unquoted variable
  command words split by IFS in pipelines; `eval` inside `$( )`
  re-parses; restricted-rbash / dbg-support / invocation / trap / func /
  complete families zeroed against the GNU true-baseline; Linux signal
  numbering table (kill -l/trap -l match GNU); SIGPIPE streams like GNU
  (`yes | head`); GNU cp port; `history -d start-end`; function-def name
  words with unquoted `$( )` (rubash#462, rubash#476).
- **winuxcmd bundle** **[in flight — coordinator collects]**: #1151
  (i18n: WPM catalog resolves at the WinuxCmd install root — already on
  winuxcmd main), #1154/#1157 (wpm download cache keyed by version +
  artifact hash), #1155 (grouped bare output + locale hint), #1159 (`du`
  trailing separators).

### Performance

- Multiline paste on Windows: mid-paste executions 186 → 0, output
  volume 176 KB → 3.3 KB, wall time 15–17 s → 3.9 s on a 200-line paste;
  the #202 author payload drops from a >300 s timeout to ~4.6 s (#252).
- Release binaries are stripped (#228): smaller downloads and
  self-updates across all platforms.
- carapace completion results are cached per session (5-minute TTL):
  warm invocations cost 104–146 ms once per shape, then filter locally.
- Timing scripts: `$EPOCHREALTIME` expands in microseconds where each
  external `date` call paid 10–30 ms of process creation (#241).
- Engine: O(1) per-command boundary on arithmetic loops (rubash, #437
  follow-up on master for this cycle).

---

## 中文

niubash 1.4.0 是一个兼容性与手感版本：引擎进一步对齐 GNU Bash 5.3.0
（83 套件差量 −40%），登录 shell 与真 readline 行编辑落地，多行粘贴在
两个平台都恢复 bash 语义，插件/doctor/config 面完成一轮诚实性修复。

### 新增

- **登录 shell**（引擎 rubash 1.4.0）：`-l`/`--login`（或 argv[0] 以
  `-` 开头）在 Unix 上走 GNU 启动链 —— `/etc/profile` → 首个存在的
  `~/.bash_profile` | `~/.bash_login` | `~/.profile`；非登录交互 shell
  读 `/etc/bash.bashrc` + `~/.bashrc`；`--noprofile`/`--norc` 分别抑制。
  （Windows 接受这些标志，行为不变。）
- **裸控制台真 readline REPL**（引擎）：控制台直连的 `rubash`/`niu`
  获得原始模式行编辑 —— 历史召回（方向键、C-p/C-n）、行内光标移动、
  C-r 反向搜索，不再受限于 conhost 的 cooked 行缓冲。
- **两平台 GNU 粘贴语义**：Unix 终端启用真括号粘贴（粘贴的换行不再
  逐行执行，#234）；Windows 控制台路径采用 PSReadLine 式粘贴块检测
  （#252）—— 257 行脚本只在你按下回车时整体执行一次。
- **Android / OpenHarmony 目标进入发版 CI 孵化**（#247）：bionic 链接
  的 aarch64/armv7 Android zip（ELF 校验）；OpenHarmony aarch64 暂为
  check-only；两条腿 continue-on-error，不进 release needs。
- **carapace-bin 补全源**（#243）：安装 carapace-bin 后，第三方命令
  （git、curl、gh、docker……上游约 1200 份 spec）的 Tab 补全带描述
  （`NIU_CARAPACE=off` 关闭；本地定义始终优先）。
- **`niu doctor` 计时建议**（#241）：把计时敏感脚本指向 bash 5 内建
  `$EPOCHREALTIME`/`$EPOCHSECONDS`，不再为每次 `$(date +%s%N)` 支付
  Windows 进程创建开销。
- **"从 niu 调用 PowerShell"文档**（#253、#203），并明确 niu 的 MSYS
  标签只是纯原生 Windows 构建的兼容 persona —— 无 msys-2.0.dll、无
  cygwin1.dll（#248）。
- **Vi 编辑模式**（#184）：`set -o vi` / `set -o emacs` 会话内即时切换
  行编辑器，底部提示带 vi 模式指示符。
- **Agent skill 包**（#188）：`niu skill install` / `niu skill status`
  把生成式 niubash skill（SKILL.md + quickref）装进 agent skill 目录；
  发版附件新增 `niubash-skill-v*.zip`。
- **发版管线预装常用件**（#189、#230）：Windows release zip 与安装包
  携带 gawk（GNU Awk 5.4.1，含 `awk` shim）、niugit 2.55.0.3、
  ripgrep 15.2.0、fd 10.4.2（打包时经 wpm 索引安装）—— 全新机器无需
  联网即可用插件发行版与搜索。
- **WinuxCmd applet 补全内置**（#172）：180 个内置 applet 全部零配置
  带真实 flag 与描述补全。

### 修复

- **多行粘贴不再逐行执行**（#202）：Unix 走括号粘贴（#234）；Windows
  走粘贴块检测，缓冲后一次性提交（#252）。
- **bash-it 主题历史互锁**（#182）：PROMPT_COMMAND 里的
  `history -a && history -c && history -r` 曾让历史文件每个提示符翻倍
  并卡死会话；GNU 追加/读取/清空语义到位，12 轮提示符周期 ConPTY
  回归钉死（#250）。
- **损坏的插件源 registry 不再被静默重写**（#178）：解析失败的
  registry.toml 原字节快照到 `registry.toml.corrupt`，拒绝一切写入，
  `niu doctor` 指明解析错误 —— pin commit、校验和与信任状态得以存活
  （#246）。
- **command-not-found 不再打包管理器广告**（#249）：无请自来的
  winget/scoop/choco 提示块移除；可用
  `niu config set command-not-found-hint wpm` 选择性开启仅 wpm 的提示
  （#251）。
- **安装向导审计 P2 批次**（#179）：撤销回执覆盖生成的 rc、Ctrl-C
  输出取消提示、`niu plugin discover` 枚举主题层、zh 文案与运行时
  字符串对齐、被掏空的源给出修复动词（#239）。
- **Linux/macOS 上的 `niu doctor`**（#194）：Windows 专属检查项报 n/a，
  不再拖垮 critical 计数（#242）。
- **原始 ESC 字节存活**（#200）：脚本文件内引号中行首 ESC 不再被词法
  器吞掉（#235，引擎修复 rubash#474）。
- **引擎（rubash 1.4.0）**：命令替换 `$?` 在展开期即刻可见
  （rubash#485）；`f() {` 同行 `#` 注释；`set -u` 算术不再误报已赋值
  变量；管道中未加引号变量命令字按 IFS 分词；`$( )` 内 `eval` 完整
  重解析；restricted-rbash / dbg-support / invocation / trap / func /
  complete 各家族对 GNU true-baseline 归零；Linux 信号编号表
  （kill -l/trap -l 对齐 GNU）；SIGPIPE 如 GNU 流式（`yes | head`）；
  GNU cp 移植；`history -d start-end`；函数名含未引号 `$( )`
  （rubash#462、rubash#476）。
- **winuxcmd 捆绑** **[在途 —— 由协调者收齐]**：#1151（i18n：WPM
  目录在 WinuxCmd 安装根解析 —— 已在 winuxcmd main）、#1154/#1157
  （wpm 下载缓存按版本 + artifact 哈希键控）、#1155（裸输出分组 +
  locale 提示）、#1159（`du` 尾随分隔符）。

### 性能

- Windows 多行粘贴：粘贴期执行 186 → 0，输出量 176 KB → 3.3 KB，
  200 行粘贴墙钟 15–17 s → 3.9 s；#202 作者载荷从 >300 s 超时降到
  约 4.6 s（#252）。
- 发版二进制去除符号表（#228）：全平台下载与自更新更小。
- carapace 补全结果按会话缓存（5 分钟 TTL）：每形状仅一次 104–146 ms
  热调用，其余本地过滤。
- 计时脚本：`$EPOCHREALTIME` 微秒级展开，替代每次 10–30 ms 的外部
  `date` 进程创建（#241）。
- 引擎：算术循环每命令边界 O(1)（rubash，#437 跟进，本轮 master）。

---

## Verification log (this draft branch)

Recorded during release prep on `release/1.4.0-prep` (local Windows
x64 MSVC; the release runner repeats all of this per target):

- `cargo build --release --workspace`: **PASS** (3m 45s full build,
  1m 52s incremental re-verify with the final manifests). Artifacts:
  `target/release/niu.exe` — 10,618,368 bytes. Two pre-existing
  warnings in `niubash-runtime` (unused `adapter` binding in
  `plugins/assets.rs`, never-constructed `ShellChannel::Stdout`) — both
  on master, not introduced by this branch. Built `--offline --locked`
  against the `[patch]`-ed local `../rubash` worktree; CI keeps its
  normal resolution.
- MSIX (draft, **unsigned**): `python scripts/make_msix.py` produced
  `target/msix/Niubash_1.4.0.0_x64.msix` — 5,149,660 bytes, packed with
  makeappx (Windows SDK 10.0.26100). Version read from Cargo.toml as
  `1.4.0.0` (Store reserves the fourth segment). Identity fields are
  clearly-labeled placeholders (`unixwin-niubash-draft` /
  `CN=00000000-…`); no real Partner Center identity exists in the repo,
  so the Store submission run must re-invoke with the real ones. The
  script ships unsigned by design — no signing step was run.
- wpm index cross-check (unixwin/wpm-source `main`, `index.json`): all
  four `scripts/release/preinstall.json` entries resolvable — gawk
  5.4.1 (windows-x64), niugit 2.55.0.3 (windows-x64), ripgrep 15.2.0
  (windows-x64 + windows-arm64), fd 10.4.2 (windows-x64 +
  windows-arm64). Matches the manifest's claims, including the arm64
  availability notes.
- `cargo publish --dry-run` (owner-configured crates.io credentials;
  formal publish stays post-tag): **blocked upstream, by design of the
  chain** —
  - `niubash-runtime 1.4.0`: fails selecting `rubash "^1.3.4"` from
    crates.io (index carries only ≤1.2.0). Unblocks when the engine
    publishes rubash ≥1.3.4 (ideally 1.4.0 in lockstep).
  - `niubash 1.4.0`: fails selecting `niubash-runtime "^1.4.0"`
    (index carries 1.2.0 / 0.0.1). Unblocks after niubash-runtime 1.4.0
    is published — publish order: rubash → niubash-runtime → niubash.
  - Lint note: `niubash-runtime`'s manifest has no
    `homepage`/`repository` (warning only).
