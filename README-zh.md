<p align="center">
  <img src="assets/niubash-banner.svg" alt="niubash — real Bash, native on Windows, Linux, and macOS." />
</p>

> **真·Bash，Windows、Linux、macOS 原生构建。**"只能跑 Windows"是旧印象：
> 现在有五个平台从这条树里构建，Windows 依旧是旗舰——不用 WSL，不开虚拟机，
> 没有 `/mnt/c`，没有 cmdlet 方言。一个二进制：你手指肌肉记得的那个 shell，
> 也是你 AI agent 天生会说的那个 shell。

<div align="center">

[English](README.md) · [中文](README-zh.md)

[![niubash CI](https://github.com/unixwin/niubash/actions/workflows/ci.yml/badge.svg)](https://github.com/unixwin/niubash/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/unixwin/niubash)](https://github.com/unixwin/niubash/releases)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-blue)](https://github.com/unixwin/niubash)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange)](https://github.com/unixwin/niubash)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Stars](https://img.shields.io/github/stars/unixwin/niubash)](https://github.com/unixwin/niubash/stargazers)

</div>

**niubash** 是用 Rust 原生实现的 Bash 兼容 shell。在旗舰平台 Windows 上，
一个 `niu.exe` 打包全部：[rubash](https://github.com/unixwin/rubash) 语言引擎、
来自 [winuxcmd](https://github.com/unixwin/winuxcmd) 的真 Unix 命令、
带 git 状态的 prompt，以及带权限模型的插件系统。Linux 与 macOS 构建是
便携 `niu` 二进制，直接用系统自带的工具。niu 不是 MSYS2、Cygwin、Git Bash，
也不是 WSL——没有 POSIX 模拟层，也没有路径转换机器。每个平台的完整契约
（包括每个目标的诚实状态）见
[platform-support.md](docs/src/platform-support.md)。

## 平台支持

| 平台 | 状态 | 发布形态 |
|---|---|---|
| Windows x64 / ARM64 | 已发布；每次发布过烟测门禁 | 安装器 `.exe` + `.zip`，Unix 命令随包 |
| Linux x86_64 / aarch64（glibc 2.35+） | 已发布；每次发布过烟测门禁 | 便携 `.tar.gz`，用系统自带工具 |
| macOS aarch64 / x86_64 | 已发布；每次发布过烟测门禁 | 便携 `.tar.gz`，用系统自带工具 |
| Android aarch64 / armv7 | 仅 CI 构建，未发布 | NDK 链接 zip，只进 workflow 产物，未过烟测 |
| OpenHarmony（`aarch64-unknown-linux-ohos`） | 仅 CI check，未发布 | CI 保持可编译，未在真机运行 |

"烟测门禁"指产物构建完成后、上传之前，在它自己的 OS 上跑四项检查
（`--version`、`-c` 执行、一条穿过原生 coreutils 的管道、离线插件栈）。
Android 和 OpenHarmony 还没过这道门：Android 产物只做了构建验证，
OpenHarmony 只做了编译检查，两者都不宣称可日常使用。各平台完整契约与
全量状态矩阵见 [platform-support.md](docs/src/platform-support.md)。

快答：

- **niu 需要 WSL 吗？** 不需要。niu 在所有发布平台上都是原生构建：Windows 上没有 Linux 虚拟机、没有模拟层、没有 `/mnt/c`。
- **niu 能跑在哪些平台？** 已发布：Windows（x64、ARM64）、Linux（x86_64、aarch64；glibc 2.35+）、macOS（aarch64、x86_64）。Android（aarch64、armv7）与 OpenHarmony（aarch64）在 CI 里构建，尚无发布。
- **niu 跑得了真 Bash 脚本吗？** 跑得了。rubash 引擎以 GNU Bash 官方上游测试套件作门禁，记录在[兼容性矩阵](docs/src/rubash-bash-compat-matrix.md)。
- **现在能在 Android 或鸿蒙上装 niu 吗？** 还不能。两个目标都能在 CI 里编译，但都没跑过运行时烟测，没有可安装的产物。

## 实证

下面每条 claim 都链到产出它的工件：入仓日志、基线文件、CI 门禁。

**6,336 个真实生态资产，逐个收割、逐个回放。** 管线从 GitHub 抓取 shell
脚本、框架、主题、dotfiles，逐个在 niu 下 source 并记录裁决：4,891 个
OK，350 个慢，224 个 GNU Bash 也过不了，101 个挂起，698 个拉取失败。
全量日志入仓（[eco-harvest.py](scripts/harvest/eco-harvest.py)、
[eco-test.py](scripts/harvest/eco-test.py)、快照
[wt92-local-20261005](scripts/harvest/snapshots/wt92-local-20261005/)）。

**生态框架是被 source、被计时的，不是一句"支持了"。** 逐资产计时基线
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

GNU Bash 上游 golden 套件的实测记录也属于这里；等手上的修复落地，它就回来。

## 快速开始

Windows：去 [Releases](https://github.com/unixwin/niubash/releases) 下载
`niubash-v*-win-*-setup.exe` 双击——不要管理员权限，PATH 和 Windows
Terminal 配置自动配好。要便携版就拿 `.zip`。

Linux（x86_64、aarch64；glibc 2.35+）与 macOS（aarch64、x86_64）：拿
便携 tarball，解包即跑：

```sh
tar -xzf niubash-v*-linux-x86_64.tar.gz && niubash-v*-linux-x86_64/niu
```

源码构建（Rust 1.70+）：
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
