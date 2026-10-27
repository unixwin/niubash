<p align="center">
  <img src="assets/niubash-banner.svg" alt="niubash — real Bash, native on Windows, Linux, and macOS." />
</p>

# Niubash — 原生 Windows Bash（Git Bash alternative）

> **真·Bash，Windows、Linux、macOS 原生构建。** 不用 WSL，不开虚拟机，没有 `/mnt/c`，没有 cmdlet 方言。一个二进制：你手指肌肉记得的那个 shell，也是你 AI agent 天生会说的那个 shell。

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

**niubash** 是用 Rust 原生实现的 Bash 兼容 shell（Git Bash alternative）。在旗舰平台 Windows 上，一个 `niu.exe` 打包全部：[rubash](https://github.com/unixwin/rubash) 语言引擎、来自 [WinuxCmd](https://github.com/unixwin/winuxcmd) 的真 Unix 命令、带 git 状态的 prompt，以及带权限模型的插件系统。git 来自 [niu-git](https://github.com/unixwin/niu-git)：用 MSVC 构建的 git-for-windows 真 C 源码，PATH 上的 `git` 就是 git 本体——没有 MSYS 运行时，没有启动器跳板，不带 perl；git 的 shell 脚本部分（hooks、`git bisect`、`git mergetool`）跑在 niubash 自己的 `sh` 上。从 niu 会话启动的 agent 会经 `SHELL` 继承它作为 shell；哪些 agent 跑什么 shell、怎么把其余的指过来，见 [Coding agents on Windows](docs/src/agents.md)。Linux 与 macOS 构建是便携 `niu` 二进制，直接用系统自带的工具。

niu 不是 MSYS2、Cygwin、Git Bash，也不是 WSL——没有 POSIX 模拟层，也没有启发式的路径转换。它是 Git Bash 的 from-scratch replacement：原生 Windows 二进制，无 POSIX 模拟层。每个平台的完整契约（包括每个目标的诚实状态）见 [platform-support.md](docs/src/platform-support.md)。

<p align="center">
  <img src="assets/demo-gitbash.gif" alt="同一组命令，在 Git Bash 与 niubash 下的两种下场" width="880" />
</p>

*同一组命令，两种下场——Git Bash 的 MSYS 层对比 niu，并排实录（`assets/demo-gitbash.gif`）。*

## 快速开始

下载统一从 [unixwin.dev](https://unixwin.dev) 进入。最新版本：**v1.4.4**。

Windows（winget，今天装到的是 **1.4.3**；1.4.4 的包更新正在微软审核中——落地前，最新构建在下面的安装器里）：

```powershell
winget install --id Unixwin.Niubash
```

Windows（安装器）：去 [Releases](https://github.com/unixwin/niubash/releases) 下载 `niubash-v*-win-*-setup.exe` 双击——不要管理员权限，PATH 和 Windows Terminal 配置自动配好。要便携版就拿 `.7z`——捆绑的 git/awk 载荷硬链接原样保留，体积最小；`.tar.gz` 也行（`tar -xf niubash-v*-win-x64.tar.gz`，Windows 10+ 自带）。别用 `.zip`：它存不了硬链接，载荷被重复存放，包大得多，只留给除 zip 什么都不认的工具链。

Linux（x86_64、aarch64；glibc 2.35+）与 macOS（aarch64、x86_64）：拿便携 tarball，解包即跑：

```sh
tar -xzf niubash-v*-linux-x86_64.tar.gz && niubash-v*-linux-x86_64/niu
```

Android（aarch64、armv7）：v1.4.3 起 release 自带 `.zip`，真机（Termux）可运行——各平台的诚实状态见[平台支持](#平台支持)。

源码构建（Rust 1.95+）：
`git clone https://github.com/unixwin/niubash.git && cd niubash && cargo build --release`

配置只有一个文件：`~/.niubashrc`，纯 Bash 语法——它是 niu 唯一读取的启动文件（`~/.bashrc` 永不被 source；`niu plugin migrate-rc --diff` 会告诉你哪些东西值得搬过来）——从[快速上手](docs/src/getting-started.md)开始。

## 为什么不用 Git Bash / MSYS2 / Cygwin

**Git Bash** 官网自称是「a BASH emulation used to run Git from the command line」——为跑 Git 而设的模拟。代价是结构性的：`echo >/dev/tty` 等控制台设备重定向在它的 MSYS 层直接 `Invalid argument` 失败，niubash 与 GNU 一致；它捆绑的 bash 停在 5.2 系，5.3 的 `${ cmd; }` 无 fork 命令替换不存在；它的提示符每一帧都跑 `__git_ps1`——一个 git 进程——niu 的 git 段在绘制提示符时进程内算出；真实安装的 Git for Windows 约 440 MB 外加 20 MB 模拟 DLL，niu 约 16 MB、零 DLL（项目自测口径，截至 2026-10-25）。

**MSYS2** 的官方文档自承：参数只要像 Unix 路径，传给原生程序前就会被自动改写，而且「并不完美」——会命中根本不是路径的参数、或不是路径列表的参数。niubash 的翻译面刻意收窄：引号参数永不改写，唯一设备翻译是精确 token `/dev/null` → `nul`；每次被发现的过度翻译都立公开 issue 并固化回归。

**Cygwin** 官网自承其核心是一个提供 POSIX API 功能的 DLL，并明说应用必须从源码重编才能在它的世界里运行。模拟层是它的官方定义。niubash 是原生构建，没有模拟 DLL。

## Niubash vs Git Bash

| | **Niubash** | **Git Bash** |
| --- | --- | --- |
| 引擎 | Rust 原生重实现的 bash 语义（[rubash](https://github.com/unixwin/rubash)）；无模拟层 | GNU Bash 移植到 MSYS2 POSIX 模拟运行时之上 |
| Bash 语言层级 | **GNU Bash 5.3** 语义——含 5.3 独有的无 fork 命令替换 `${ cmd; }`（[rubash#195](https://github.com/unixwin/rubash/issues/195)） | GNU Bash **5.2**（Git for Windows 捆绑版本） |
| 路径处理 | Windows 路径即原生表示；参数翻译是确定性的成文契约——引号参数永不改写，不靠猜 | 对参数的启发式路径转换，官方逃生舱是 `MSYS_NO_PATHCONV` |
| `git` | 用 MSVC 构建的 git-for-windows 真 C 源码（[niu-git](https://github.com/unixwin/niu-git)）——就是二进制本体；无 MSYS 运行时、无启动器跳板 | 经 MSYS 运行时启动的 `git` |
| 提示符 git 段 | 绘制提示符时进程内算出 | 每个提示符都跑 `__git_ps1`——spawn 一个 git 进程 |
| 子壳 | 进程内——`( )` 与 `$( )` 不创建进程 | `fork()` 模拟 |
| 控制台设备 | `/dev/tty` 重定向与 GNU 一致——引擎把 `CON` 拆为 `CONIN$`/`CONOUT$`（见[快答](#快答)） | 控制台设备重定向在 MSYS 层直接失败 |
| 安装体积 | ≈16 MB，零 DLL | ≈440 MB + 20 MB `msys-2.0.dll`（项目自测口径，真实安装，截至 2026-10-25） |

长版论据——包括同一命令行下「到底有什么抵达了原生进程」的并排对照——在 [Why niubash](docs/src/why-niubash.md)。

## 命令层：为 shell 而造

捆绑的命令是 **WinuxCmd**：176 个命令 / 1,924 个选项，原生 Windows 构建，为 shell 使用设计、与这个 shell 同节奏演进——路径形态、参数传递、与 shell 的配合面都按 shell 语义设计。显而易见的替代品——微软自家的 coreutils 包——是把上游 uutils *打包* 给 PowerShell/CMD 用的（其仓库自述为「Installer & Packaging」，包还在 preview）。它自己的 README 写明了边界：**没有 `/dev/null`**、**没有 POSIX 信号**、`kill` 与 `timeout` 不发货，`chmod`、`chown`、`id`、`tty`、`mkfifo`、`nohup`、`stty` 等二十多个命令以「POSIX-only concepts」为由整批丢弃。给 shell 用的命令层，不能从丢掉 shell 赖以生存的概念开始。

## 还有「轻量替代」shell 们

新一代 Rust shell 卖的是轻量。同行里：hush 是一门基于 Lua 的语言，从没自称要替代 bash；Oils/OSH 自称「从 bash 升级出去的路径」，不是无感 drop-in。**brush** 宣称的正是 drop-in：「Run it as your everyday shell」「Production-ready for most use cases」。我们用 306 例差分语料（对 GNU Bash 5.3.20 逐字节比对）实测：其发行版 v0.4.0 是 181/306，其 master 最新提交源码构建复测 183/306——同一失败剖面——对照 rubash 在同一把尺子上的 304/306。它丢的全是基础机制，不是边角：alias 时机模型与 GNU 相反（20 探针 6 条实质分歧）；`printf x y` 在发行版上无限循环挂死脚本（master 已修复）；脚本里 `wait $!` 未实现；globstar 被列为「Fully Supported」却不递归；nameref 的 `unset` 报「not yet implemented」——真实的 oh-my-bash 加载正好撞上这一条。它的 Windows 支持自述为「Preview」，v0.4.0 根本没有 Windows 发行二进制。

轻量不是残疾的遮羞布。完整 bash 语义不用 fork 逐路径复刻，加上跨平台本身，是 474,985 行（rubash 385,618 + niubash 89,367，静态审计口径 2026-10-25）的完工价。过不了这些基础机制的实现，既无法成为 bash 的替代产物，也无法成为可靠的登录态 shell。

## 为 AI agent 而建

模型是按 Bash 训练的；在 Windows 机器上，它们的命令理应由一个「照原文执行」的 shell 来跑。

- **agent 继承 niu 作为 shell。** 从 niu 会话启动的一切都经 `SHELL` 拿到它；[Coding agents on Windows](docs/src/agents.md) 逐个 agent 写明了各自跑什么 shell、怎么把其余的指过来。
- **真实的 agent 使用在先。** OpenCode 用户把 `niu.exe` 设为 agent 的 shell，并直接推动了环境变量驱动的 init 工作（[#82](https://github.com/unixwin/niubash/issues/82)）；一处 Hermes 集成故障——字面引号漏进外部命令参数——被立案并修复（[#124](https://github.com/unixwin/niubash/issues/124)）；一天之内三起假基线事故之后，`--version` 现在带构建元数据，agent 永远能确切知道自己在跟哪个构建说话（[rubash#488](https://github.com/unixwin/rubash/issues/488)）。
- **非交互行为是确定性的。** `niu -c` 与脚本运行安静而稳定：无横幅，stdout/stderr 稳定，退出码精确传播——agent 的工具循环靠的正是这些性质。
- **没有路径轮盘赌。** 没有需要记的 `MSYS_NO_PATHCONV`，没有在子进程背后偷偷改写参数的启发式——`/flags` 还是 `/flags`，路径形操作数按一份成文契约翻译（[windows-path-contract.md](docs/src/windows-path-contract.md)）。
- **agent 技能包随每个 release 发行**：[Releases](https://github.com/unixwin/niubash/releases) 页上的 `niubash-skill-v*.zip` 把这个 shell 的成文行为打包给 agent 消费（[#188](https://github.com/unixwin/niubash/issues/188)）；引擎仓在根目录 `SKILL.md` 里自带披露（含平台身份语义）。

催生这一节的事故清单——agent 在 PowerShell 与模拟层 shell 上的真实翻车，带凭据——在 [Why niubash](docs/src/why-niubash.md#agent-casualties-with-receipts)。

## 兼容性与测试

niu 的语言语义就是 rubash 引擎的语义，门禁靠机械手段、不靠轶事：

- **GNU 官方 83 套上游语料是标尺。** 引擎台账（2026-10-12 全量重跑）：62 套 Windows 上 stdout 逐字节一致，另 20 套仅在环境绑定行上有差异，nameref 的唯一残差行经证实是 GNU 自身的收割时序竞争——完整台账在 [rubash README](https://github.com/unixwin/rubash#compatibility-at-a-glance)，rubash CI 每次推送都在 Linux 上回放该语料。
- **独立的 246 例差分套件**对本地自编译 GNU Bash 5.3.20：当前 master **244/246 逐字节一致**（首次审计 2026-10-22 时是 222/246）。
- **306 例差分语料**：rubash **304/306**（同行实测见上）。
- **每个已修 issue 都带一条以它命名的回归测试**，断言 GNU 行为——引擎侧与宿主侧皆然（`tests/compat.rs`、`tests/host_contract.rs`，及[兼容性矩阵](docs/src/rubash-bash-compat-matrix.md)里的逐能力夹具）。
- **发布过烟测门与预算门**：产物在自己的 OS 上跑不通、或超出逐资产计时预算，就发不出去（见[实证](#实证)）。

截至 2026-10-25，账都在 tracker 上：rubash 累计 1,050 个 issue、已关 1,039；niubash 累计 292 个、已关 285。

## 实证

下面每条 claim 都链到产出它的工件：入仓日志、基线文件、CI 门禁。

**6,336 个真实生态资产，逐个收割、逐个回放。** 管线从 GitHub 抓取 shell 脚本、框架、主题、dotfiles，逐个在 niu 下 source 并记录裁决：4,891 个 OK，350 个慢，224 个 GNU Bash 也过不了，101 个挂起，698 个拉取失败。全量日志入仓（[eco-harvest.py](scripts/harvest/eco-harvest.py)、[eco-test.py](scripts/harvest/eco-test.py)、快照 [wt92-local-20261005](scripts/harvest/snapshots/wt92-local-20261005/))。

**生态框架是被 source、被计时的。** 逐资产计时基线加载 437 个真实 oh-my-bash / bash-it 资产（166 款主题加上 plugins、completions、aliases），每资产测三个阶段（source、首帧 prompt、重渲染），取三次运行的中位数。两个框架自带的 nvm 资产就在这份基线里；真实用户 dotfiles 里加载 nvm.sh 的链路也走同一套 harness，分相计时在案（[基线](scripts/perf/baselines/asset-timing-baseline.json)、[快照](scripts/harvest/snapshots/wt92-local-20261005/))。

**慢的发布发不出去。** [budgets.toml](scripts/perf/budgets.toml) 里每个资产都带实测预算（健康主题重渲染 11–35 ms；预算线 120 ms；超预算 5 倍硬失败）。[发布流水线](.github/workflows/release.yml)跑 `perfbudget` 门禁，超限即 fail 本次发布；另有每夜趋势跑盯漂移。

## 性能

下面每个数字都来自同一份已发布规程——2026-10-25 基线（PERF-BASELINE-2026-10-25，存于公开的 [shellbench](https://github.com/unixwin/shellbench) 仓）：release 构建、Linux、对自编译 GNU Bash 5.3.20。

**先说诚实结论。** 内嵌的 rubash 引擎在解释器受限的微观负载上付一层近似均匀的 **~3× 地板税**（算术、函数调用、赋值、read 循环的单操作成本 2.8–3.2×）。「全表超越 GNU」是公开目标、是旗舰版的硬门，现状如实发布、不藏。成本在进程创建上的负载则反过来：管道段与命令替换在进程内执行，耗时是 GNU 的 **0.05×**（管道）与 **0.11×**（命令替换循环）。

**启动——唯一可引用的口径。** `-c 'true'`，50 样本交错采样取 p50，release 构建，Linux：**niu 3.96 ms，rubash 3.43 ms，GNU Bash 5.3.20 2.59 ms**。Windows 面的启动数字在真机按同一规程复测之前，一律不引用。

**Windows 进程经济学。** 无 console 会话过去每次外部 spawn 都要付一次隐藏 console 分配。修法是一个会话一个隐藏 console——Windows 11 24H2+/Server 2025 上预分配无窗 console，其他系统认领首个 `CREATE_NO_WINDOW` 子进程的——由 14 条回归测试钉死（设计见 [#462](https://github.com/unixwin/niubash/issues/462)，引擎侧落地于 [rubash#913](https://github.com/unixwin/rubash/pull/913)）。`$(tput …)` 终端查询对准入的 TERM×能力形状进程内作答，与 GNU ncurses 在 269 行差分矩阵上字节相等，零 spawn（[rubash#884](https://github.com/unixwin/rubash/pull/884)）。

**Android，明说。** 机上实测基线存在（[#352](https://github.com/unixwin/niubash/issues/352)）；其引擎侧根因已被定位并关闭（rubash#607–#609），fork 受限负载在那份基线里本就更快；修后真机复测尚欠——我们如实说，而不是引用修前的旧数字。

## 平台支持

| 平台 | 状态 | 发布形态 |
|---|---|---|
| Windows x64 / ARM64 | 已发布；每次发布过烟测门禁 | 安装器 `.exe` + 便携 `.tar.gz`/`.7z`/`.zip`，Unix 命令随包 |
| Linux x86_64 / aarch64（glibc 2.35+） | 已发布；每次发布过烟测门禁 | 便携 `.tar.gz`，用系统自带工具 |
| macOS aarch64 / x86_64 | 已发布；每次发布过烟测门禁 | 便携 `.tar.gz`，用系统自带工具 |
| Android aarch64 / armv7 | v1.4.3 起随 release 发货；在真机上运行（Termux），CI 自动烟测腿尚未接上 | NDK 链接 `.zip` release 资产 |
| OpenHarmony（`aarch64-unknown-linux-ohos`） | 仅 CI check，未发布 | CI 保持可编译，未在真机运行 |

"烟测门禁"指产物构建完成后、上传之前，在它自己的 OS 上跑四项检查（`--version`、`-c` 执行、一条穿过原生 coreutils 的管道、离线插件栈）——门禁在 `release.yml` 里（"no smoke, no artifact"：Linux 腿在 `ubuntu-22.04`/`ubuntu-22.04-arm` 原生跑，macOS x86_64 腿在 Rosetta 2 下跑），发布整体还额外被 golden user journey 与逐资产计时预算两道门拦着。OpenHarmony 还没过这道门（只做编译检查，没在真机上跑过）。Android 自 v1.4.3 起随 release 发货 `.zip`，在真机上运行（Termux aarch64/armv7，日常使用 + 机上实测基线）；缺的是 CI 里那条自动化逐 release 烟测腿，目前只接了 Windows/Linux/macOS。各平台完整契约与全量状态矩阵见 [platform-support.md](docs/src/platform-support.md)。

## 快答

- **niu 需要 WSL 吗？** 不需要。niu 在所有发布平台上都是原生构建：Windows 上没有 Linux 虚拟机、没有模拟层、没有 `/mnt/c`。
- **niu 能跑在哪些平台？** 已发布：Windows（x64、ARM64）、Linux（x86_64、aarch64；glibc 2.35+）、macOS（aarch64、x86_64）、Android（aarch64、armv7；v1.4.3 起 `.zip` 随 release 发货，真机可运行）。OpenHarmony（aarch64）在 CI 里构建，尚无发布。
- **niu 跑得了真 Bash 脚本吗？** 跑得了。rubash 引擎以 GNU Bash 官方上游测试套件作门禁，记录在[兼容性矩阵](docs/src/rubash-bash-compat-matrix.md)。
- **不止于 Git Bash 兼容。** 控制台设备重定向在连 Git Bash 都失败的地方照常工作：`echo >/dev/tty`、`exec 3<>/dev/tty`、`test -e /dev/tty` 全部与 GNU 一致——引擎把 `/dev/tty` 映射到控制台，并把 `CON` 拆成 `CONIN$`/`CONOUT$` 两半，绕开 Windows 对 `CON` 的设备打开限制（`CON` 上拒绝 `FILE_APPEND_DATA` 与读写打开）。`git`/`ssh` 的 askpass 提示靠的正是这个。（引擎侧线索：[rubash#1312](https://github.com/unixwin/rubash/issues/1312)、[rubash#1314](https://github.com/unixwin/rubash/issues/1314)、[niubash#519](https://github.com/unixwin/niubash/issues/519)。）
- **现在能在 Android 或鸿蒙上装 niu 吗？** Android 能：v1.4.3 起 release 自带 `.zip`，真机（Termux aarch64/armv7）可运行。鸿蒙还不能：CI 能编译，但没有可安装产物。

## 已知限制，明说

- **安装器尚未代码签名。** 首次运行时 Windows SmartScreen 会警告（[#511](https://github.com/unixwin/niubash/issues/511)——证书已选定、签名在计划中）。在此之前，请对照发布的 SHA-256 校验和验证下载。
- **`stty` 在 niu 下尚不可用。** 根因已定位——宿主尚未把真控制台句柄透传给引擎的 fd 0——修复路径在 [#535](https://github.com/unixwin/niubash/issues/535) 跟踪。
- **解释器速度是一份实测在案、公开发布的差距**（见[性能](#性能)）——不是藏着的。进程受限负载已经领先；解释器受限的微观负载是旗舰版的硬门。

## 文档

[文档站](https://unixwin.github.io/niubash/) · [Why niubash](docs/src/why-niubash.md)（长版论据 + agent 受害者案卷） · [平台支持](docs/src/platform-support.md) · [Bash 兼容性矩阵](docs/src/rubash-bash-compat-matrix.md) · [Roadmap](docs/src/niubash-roadmap.md)

引擎侧有两项在设计阶段（尚未发布）：**1.5.0 可嵌入引擎 API**（[rubash#595](https://github.com/unixwin/rubash/issues/595)，定版设计在 rubash 仓）与**原生脚本调试器**——`rubash --debug` 给人用、`--dap` 给编辑器，同一调试会话的两个前端（[rubash#497](https://github.com/unixwin/rubash/issues/497)，定版设计在 rubash 仓）。

---

如果 niu 帮你省掉了一台虚拟机、一段被改坏的引号、一个被 PowerShell 吃掉的参数，[给仓库点个 Star](https://github.com/unixwin/niubash)，转告下一位开发者。★

## 许可证

MIT，详见 [LICENSE](LICENSE)。
