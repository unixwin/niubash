<p align="center">
  <img src="assets/niubash-banner.svg" alt="niubash — real Bash, native on Windows, Linux, and macOS." />
</p>

# Niubash — 原生 Windows Bash（Git Bash alternative）

> **真·Bash，Windows、Linux、macOS 原生构建。**"只能跑 Windows"是旧印象：
> 现在有五个平台从这条树里构建，Windows 依旧是旗舰——不用 WSL，不开虚拟机，
> 没有 `/mnt/c`，没有 cmdlet 方言。一个二进制：你手指肌肉记得的那个 shell，
> 也是你 AI agent 天生会说的那个 shell。

<div align="center">

[English](README.md) · [中文](README-zh.md)

文档与下载：[unixwin.dev](https://unixwin.dev)

[![niubash CI](https://github.com/unixwin/niubash/actions/workflows/ci.yml/badge.svg)](https://github.com/unixwin/niubash/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/unixwin/niubash)](https://github.com/unixwin/niubash/releases)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-blue)](https://github.com/unixwin/niubash)
[![Rust](https://img.shields.io/badge/rust-1.95%2B-orange)](https://github.com/unixwin/niubash)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Stars](https://img.shields.io/github/stars/unixwin/niubash)](https://github.com/unixwin/niubash/stargazers)

</div>

**niubash** 是用 Rust 原生实现的 Bash 兼容 shell（Git Bash alternative）。在旗舰平台 Windows 上，
一个 `niu.exe` 打包全部：[rubash](https://github.com/unixwin/rubash) 语言引擎、
来自 [winuxcmd](https://github.com/unixwin/winuxcmd) 的真 Unix 命令、
带 git 状态的 prompt，以及带权限模型的插件系统。git 来自
[niu-git](https://github.com/unixwin/niu-git)：用 MSVC 构建的 git-for-windows
真 C 源码，PATH 上的 `git` 就是 git 本体——没有 MSYS 运行时，没有启动器
跳板，不带 perl。`git --version` 起步 **~14 ms**（实测均值 13.7 ms / 最低
11.5 ms；`git status` 22.6 ms），Git Bash 的 `git` 要 45-103 ms
（[niu-git#12](https://github.com/unixwin/niu-git/pull/12)、
[niubash#374](https://github.com/unixwin/niubash/pull/374)）；git 的
shell 脚本部分（hooks、`git bisect`、`git mergetool`）跑在 niubash 自己的
`sh` 上。从 niu 会话启动的 agent 会经 `SHELL` 继承它作为 shell；哪些
agent 跑什么 shell、怎么把其余的指过来，见
[Coding agents on Windows](docs/src/agents.md)。Linux 与 macOS 构建是
便携 `niu` 二进制，直接用系统自带的工具。niu 不是 MSYS2、Cygwin、Git Bash，
也不是 WSL——没有 POSIX 模拟层，也没有启发式的路径转换。它是 Git Bash 的 from-scratch replacement：原生 Windows 二进制，无 POSIX 模拟层。每个平台的完整契约
（包括每个目标的诚实状态）见
[platform-support.md](docs/src/platform-support.md)。

## 平台支持

| 平台 | 状态 | 发布形态 |
|---|---|---|
| Windows x64 / ARM64 | 已发布；每次发布过烟测门禁 | 安装器 `.exe` + 便携 `.tar.gz`/`.7z`/`.zip`，Unix 命令随包 |
| Linux x86_64 / aarch64（glibc 2.35+） | 已发布；每次发布过烟测门禁 | 便携 `.tar.gz`，用系统自带工具 |
| macOS aarch64 / x86_64 | 已发布；每次发布过烟测门禁 | 便携 `.tar.gz`，用系统自带工具 |
| Android aarch64 / armv7 | v1.4.3 起随 release 发货；在真机上运行（Termux），CI 自动烟测腿尚未接上 | NDK 链接 `.zip` release 资产 |
| OpenHarmony（`aarch64-unknown-linux-ohos`） | 仅 CI check，未发布 | CI 保持可编译，未在真机运行 |

"烟测门禁"指产物构建完成后、上传之前，在它自己的 OS 上跑四项检查
（`--version`、`-c` 执行、一条穿过原生 coreutils 的管道、离线插件栈）
——门禁在 `release.yml` 里（"no smoke, no artifact"：Linux 腿在
`ubuntu-22.04`/`ubuntu-22.04-arm` 原生跑，macOS x86_64 腿在 Rosetta 2 下跑），
发布整体还额外被 golden user journey 与逐资产计时预算两道门拦着。
OpenHarmony 还没过这道门（只做编译检查，没在真机上跑过）。Android 自
v1.4.3 起随 release 发货 `.zip`，在真机上运行（Termux aarch64/armv7，
日常使用 + 机上实测基线）；缺的是 CI 里那条自动化逐 release 烟测腿，
目前只接了 Windows/Linux/macOS。OpenHarmony 仍不宣称可用。各平台完整契约与
全量状态矩阵见 [platform-support.md](docs/src/platform-support.md)。

快答：

- **niu 需要 WSL 吗？** 不需要。niu 在所有发布平台上都是原生构建：Windows 上没有 Linux 虚拟机、没有模拟层、没有 `/mnt/c`。
- **niu 能跑在哪些平台？** 已发布：Windows（x64、ARM64）、Linux（x86_64、aarch64；glibc 2.35+）、macOS（aarch64、x86_64）、Android（aarch64、armv7；v1.4.3 起 `.zip` 随 release 发货，真机可运行）。OpenHarmony（aarch64）在 CI 里构建，尚无发布；Android 另有一份实测性能基线（见[性能](#性能)）。
- **niu 跑得了真 Bash 脚本吗？** 跑得了。rubash 引擎以 GNU Bash 官方上游测试套件作门禁，记录在[兼容性矩阵](docs/src/rubash-bash-compat-matrix.md)。
- **现在能在 Android 或鸿蒙上装 niu 吗？** Android 能：v1.4.3 起 release 自带 `.zip`，真机（Termux aarch64/armv7）可运行。鸿蒙还不能：CI 能编译，但没有可安装产物。

## 为什么不用 Git Bash / MSYS2 / Cygwin，也不用「轻量替代」shell

**Git Bash** 官网自称是「a BASH emulation used to run Git from the command line」——
为跑 Git 而设的模拟。代价是结构性的：`echo >/dev/tty` 等控制台设备重定向在 MSYS 层
直接 `Invalid argument` 失败（niubash 与 GNU 一致，项目自测口径）；捆绑 bash 停在
5.2 系，5.3 的 `${ cmd; }` 无 fork 命令替换不存在；提示符每轮 spawn git 取分支
（约 100–300 ms），niu 直读 `.git/HEAD`、永不 spawn（冷 39 ms / 暖 p50 2.5 ms，
project-measured, as of 2026-10-25）；真实安装 440 MB + 20 MB 模拟 DLL，
niu 约 16 MB、零 DLL。

**MSYS2** 的官方文档自承：参数只要像 Unix 路径就会被自动改写，且「并不完美」，
会误伤不是路径的参数。niubash 的翻译面刻意收窄：引号参数永不改写，唯一设备翻译
是精确 token `/dev/null` → `nul`；每次过度翻译都立公开 issue 并固化回归。
**Cygwin** 官网自承要跑它的世界必须「从源码重编」应用——模拟层是它的官方定义。
niubash 是原生构建，没有模拟 DLL。

**新一代「bash 替代」shell**（如 Rust 写的 brush）主打轻量。轻量不是残疾的遮羞布：
完整 bash 语义不用 fork 逐路径复刻 + 跨平台，是 474,985 行（rubash 385,618 +
niubash 89,367，静态审计口径 2026-10-25）的完工价。我们用 306 例差分语料（对 GNU bash 5.3.20
逐字节比对）实测其发行版 v0.4.0：181/306；其 master 最新提交
源码构建复测 183/306，同一失败剖面——alias 时机模型错误（20 探针 6 条实质分歧）、
`printf x y` 无限循环挂死脚本、脚本里 `wait $!` 未实现、globstar 不递归、
nameref 的 `unset` 报「not yet implemented」。这类实现无法成为 bash 的替代产物，
也无法成为可靠的登录态 shell（证据链见宣传线证据文件）。

**niubash 的口径**：自研 246 例差分套件与 GNU 字节级一致 244/246；以 GNU 官方
83 套测试集为一致性语料（measured against）；nvm/bats-core/bash-it/oh-my-bash
等真实生态逐项跑，分歧全量立 issue 并固化回归（截至 2026-10-25：rubash
1,050 个 issue 关 1,039；niubash 292 个关 285）。命令层捆绑 WinuxCmd：
176 个命令 / 1,924 个选项，为 shell 而设计——对照微软 coreutils 的 README 自述：
没有 `/dev/null`、没有 POSIX 信号、`chmod`/`id`/`tty` 等二十多个命令整批丢弃。

## 实证

下面每条 claim 都链到产出它的工件：入仓日志、基线文件、CI 门禁。

**6,336 个真实生态资产，逐个收割、逐个回放。** 管线从 GitHub 抓取 shell
脚本、框架、主题、dotfiles，逐个在 niu 下 source 并记录裁决：4,891 个
OK，350 个慢，224 个 GNU Bash 也过不了，101 个挂起，698 个拉取失败。
全量日志入仓（[eco-harvest.py](scripts/harvest/eco-harvest.py)、
[eco-test.py](scripts/harvest/eco-test.py)、快照
[wt92-local-20261005](scripts/harvest/snapshots/wt92-local-20261005/)）。

**生态框架是被 source、被计时的。** 逐资产计时基线
加载 437 个真实 oh-my-bash / bash-it 资产（166 款主题加上 plugins、
completions、aliases），每资产测三个阶段（source、首帧 prompt、重渲染），
取三次运行的中位数。两个框架自带的 nvm 资产就在这份基线里；真实用户
dotfiles 里加载 nvm.sh 的链路也走同一套 harness，分相计时在案
（[基线](scripts/perf/baselines/asset-timing-baseline.json)、
[快照](scripts/harvest/snapshots/wt92-local-20261005/)）。

**慢的发布发不出去。** [budgets.toml](scripts/perf/budgets.toml) 里每个
资产都带实测预算（健康主题重渲染 11-35 ms；预算线 120 ms；超预算 5 倍
硬失败）。[发布流水线](.github/workflows/release.yml)跑 `perfbudget`
门禁，超限即 fail 本次发布；另有每夜趋势跑盯漂移。

GNU Bash 上游 golden 套件的实测记录随引擎走：[rubash README](https://github.com/unixwin/rubash#readme)
带着台账（2026-10-12 全量重跑 83/83），rubash CI 每次推送都会在 Linux 上
回放上游语料（"Bash upstream test progress" job，
`scripts/run-bash-upstream-tests.sh`）。

## 性能

下面每个数字都钉在测出它的 issue/PR 上，守住它的测试或门禁也一并写明。

**Windows 进程 spawn 成本——一个会话一个隐藏 console，而不是每个子进程
一个。** 无 console 会话（直启 `niu -c`、detached、CI）过去每次外部 spawn
都要付一次隐藏 console 分配，~22 ms/子进程（设计与形状见
[niubash#462](https://github.com/unixwin/niubash/issues/462)）。修法已在
引擎侧落地（[rubash#913](https://github.com/unixwin/rubash/pull/913)）：
Windows 11 24H2+/Server 2025 上一次性预分配无窗 console
（`AllocConsoleWithOptions`），更老系统则认领首个 `CREATE_NO_WINDOW`
子进程的隐藏 console（`AttachConsole`）——整棵 `niu -c 'a; b; c'` 最多付
一次分配，后续 spawn 直接继承。14 条回归测试钉死零可见窗与缩放行为
（rubash `tests/console_window_gate.rs`）。

**`$(tput …)` 终端查询进程内作答。** tput 白名单内联臂
（[rubash#884](https://github.com/unixwin/rubash/pull/884)）对准入的
TERM×能力形状进程内作答，与 GNU ncurses 字节相等（269 行差分矩阵，
diff = 0；8 条回归测试）。票面探针——64 次 tput 调用走命令替换——
墙钟 1165 ms → 47 ms（~25x，零 spawn）。winuxcmd 侧的同族修复去掉了真
二进制里 ~17 ms/次 的 i18n 开销（修后进程内 ~0.4 ms；3208 行回放逐字节
一致：[WinuxCmd#1269](https://github.com/unixwin/WinuxCmd/pull/1269)）。
harvest 里另一张 SLOW 面——`read` 撞 EOF ~4.8 s/次——仍在
[rubash#800](https://github.com/unixwin/rubash/issues/800) 开着。

**引擎热路径**（内嵌的
[rubash](https://github.com/unixwin/rubash) 引擎；每刀都带实测差值和
GNU 差分电池）：

- 多词字面量命令 fast path：`[ 1 -eq 2 ]` ×20k ≈177→155 ms
  （[rubash#967](https://github.com/unixwin/rubash/pull/967)）。
- `[[ ]]` 数值叶直接处理 + 十进制 fast path：50k `[[ 1 -eq 2 ]]`
  对 GNU 从 1.41x → 1.10x
  （[rubash#966](https://github.com/unixwin/rubash/pull/966)）。
- procsub 元数据扫描白名单跳过：1.56x→1.39x（~11%）
  （[rubash#974](https://github.com/unixwin/rubash/pull/974)）。
- 十进制下标数组写 fast lane：交错 8k 从 3.65x → 3.16x；顺手修掉
  `a[$((i++))]=v` 的真实双副作用
  （[rubash#959](https://github.com/unixwin/rubash/pull/959)）。
- case：`$BASH_COMMAND` 只渲染头臂：20 臂 ×10k −10%，附真实分歧修复
  （[rubash#958](https://github.com/unixwin/rubash/pull/958)）。

这波背后的解释器归因已带数字关闭：标量赋值 ~1.06x GNU（rubash#609 →
#672/#678），算术循环与参数扩展走 rubash#607/#608（#665/#671/#679、
#666）；O(n²) 数组元素写变 O(1) 均摊（rubash#618/#616：assoc 5000 键
写入对 GNU 从 445x → 9.9x，indexed 736x → 3.8x；台账在 rubash 的
`docs/performance.md`）。export/unset 快车道家族已落地（#917/#918/#920，
#943 做正确性返工）；[rubash#916](https://github.com/unixwin/rubash/issues/916)
对残留保持开放。

**Android 基线——实测、诚实、尚未复测。** Termux aarch64 平板上，
niubash 1.4.0 对 GNU bash 5.3.20
（[niubash#352](https://github.com/unixwin/niubash/issues/352)，方法与
全矩阵在票内）：解释器受限负载慢 2.3-3.7x（`${x^^}` 单操作最高 7.3x），
`niu -c true` 21.1 ms 对 bash 8.3 ms（30 次中位数），峰值 RSS 2.2x——
而 fork 受限负载最高快 5.2x，真实 mkdir/写/回读脚本快 2.1x。引擎侧根因
已修完关闭（rubash#607-609），wrapper 一次性启动缺口也已关闭
（[niubash#351](https://github.com/unixwin/niubash/issues/351)：修后
`niu -c true` 15 ms 对 bash 13 ms，零 tput spawn）。
[niubash#349](https://github.com/unixwin/niubash/issues/349) 是最早的
~2.1x 算术/函数数据点。Android 真机上的修后复测还不存在——#352 为此
保持开放，Android 腿目前仍只有构建验证。

## 快速开始

下载统一从 [unixwin.dev](https://unixwin.dev) 进入。

Windows：去 [Releases](https://github.com/unixwin/niubash/releases) 下载
`niubash-v*-win-*-setup.exe` 双击——不要管理员权限，PATH 和 Windows
Terminal 配置自动配好。
Windows（winget）：`winget install --id Unixwin.Niubash`——不要管理员权限，
PATH 和 Windows Terminal 配置自动配好。
要便携版就拿 `.7z`——捆绑的 git/awk 载荷硬链接原样保留，体积最小；
`.tar.gz` 也行（`tar -xf niubash-v*-win-x64.tar.gz`，Windows 10+ 自带）。
别用 `.zip`：它存不了硬链接，载荷被重复存放，包大得多，只留给除 zip
什么都不认的工具链。

Linux（x86_64、aarch64；glibc 2.35+）与 macOS（aarch64、x86_64）：拿
便携 tarball，解包即跑：

```sh
tar -xzf niubash-v*-linux-x86_64.tar.gz && niubash-v*-linux-x86_64/niu
```

源码构建（Rust 1.95+）：
`git clone https://github.com/unixwin/niubash.git && cd niubash && cargo build --release`

配置只有一个文件：`~/.niubashrc`，纯 Bash 语法——从
[快速上手](docs/src/getting-started.md)开始。

## 文档

[文档站](https://unixwin.github.io/niubash/) ·
[Why niubash](docs/src/why-niubash.md)（长版论据 + agent 受害者案卷） ·
[平台支持](docs/src/platform-support.md) ·
[Bash 兼容性矩阵](docs/src/rubash-bash-compat-matrix.md) ·
[Roadmap](docs/src/niubash-roadmap.md)

---

如果 niu 帮你省掉了一台虚拟机、一段被改坏的引号、一个被 PowerShell 吃掉
的参数，[给仓库点个 Star](https://github.com/unixwin/niubash)，转告下一位
开发者。★

## 许可证

MIT，详见 [LICENSE](LICENSE)。
