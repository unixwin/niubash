# Microsoft Store 一览文案（Store Listing）

> 「商店一览」页直接粘贴。中文简体与英文（美国）各建一份 listing。

---

## 简体中文

### 显示名称

Niubash — Windows 原生 Bash Shell

### 描述

Niubash 是一个 Windows 原生的 bash 兼容命令行 shell。它不是模拟层，也不是
虚拟机：原生 Win32 实现，直接在 Windows 上运行 GNU Bash 语法。

**为什么选 Niubash**

- **bash 兼容**：脚本、管道、重定向、函数、任务控制，按 GNU Bash 语义执行
- **开箱即用的命令**：内置 WinuxCmd 命令集（ls、grep、sed、awk 等常用工具）
- **插件系统**：oh-my-niu 官方插件分发，主题、提示、效率插件一键启用
- **IDE 级补全**：带描述的补全菜单，敲命令不用背
- **对人和对 AI 都友好**：非交互模式安静、确定、退出码精确，适合脚本与
  智能体调用
- **轻量**：单二进制约 14 MB，冷启动毫秒级，无后台服务

**适合谁**

- 在 Windows 上写脚本、被 PowerShell 折磨过的开发者
- 需要 bash 环境但不想装 WSL/MSYS2 的用户
- 让 AI 智能体在 Windows 上跑命令的场景

安装后打开终端输入 `niu` 即可进入。

### 功能列表

- bash 兼容的脚本执行与交互 shell
- 内置 WinuxCmd 常用命令集
- oh-my-niu 插件与主题系统
- IDE 风格补全菜单
- 静默、确定性的脚本模式（精确退出码）

### 关键词

bash, shell, terminal, 终端, command line, 命令行, wsl alternative, ssh agent, developer tools, 开发者工具

### 新增功能（此版本）

v1.5.0：**原生脚本调试器**（`niu --debug` 终端交互调试、`niu --dap` 直接
对接 VS Code，随包附带零依赖扩展）、GNU `possible-completions`（`M-?`/`M-=`）、
readline ABI 双向开放（`enable -f` 插件可直接驱动编辑器与补全）、config 体系
GNU 化（TOML 移除，提供 `niu config migrate` 幂等迁移），以及 MSIX 布局
`NIU_ROOT` 修复——本商店包的命令树解析正依赖这一修复。

### 截图计划（≥4 张，1366x768 以上）

1. 进入 niu 交互 shell 的欢迎界面（带主题）
2. 补全菜单弹出的操作画面
3. 跑一段 bash 脚本（管道 + 重定向 + 函数）的输出
4. oh-my-niu 插件列表 / 主题切换效果

---

## English (US)

### Display Name

Niubash — Native Bash Shell for Windows

### Description

Niubash is a Windows-native, bash-compatible command-line shell. No emulation,
no VM: a native Win32 implementation that runs GNU Bash syntax directly on
Windows.

**Why Niubash**

- **Bash-compatible**: scripts, pipelines, redirects, functions, and job
  control follow GNU Bash semantics
- **Commands out of the box**: bundled WinuxCmd command set (ls, grep, sed,
  awk, and other everyday tools)
- **Plugin system**: the official oh-my-niu distribution — themes, prompts,
  and productivity plugins, one command away
- **IDE-style completion**: a completion menu with descriptions
- **Built for humans and AI agents**: quiet, deterministic non-interactive
  mode with exact exit codes
- **Lightweight**: ~4 MB core binary, fast startup

Open a terminal and type `niu` to start.

### Features

- Bash-compatible scripting and interactive shell
- Bundled WinuxCmd command set
- oh-my-niu plugin and theme system
- IDE-style completion menu
- Quiet, deterministic script mode with exact exit codes

### Keywords

bash, shell, terminal, command line, wsl alternative, unix tools, developer tools, scripting

### What's new in this version

v1.1.0: IDE-style completion menu, colorized plugin CLI, NIU_ENV/BASH_ENV
one-shot env files, reduced startup overhead, smaller release binary, and the
upstream rubash v1.1.0 CTLESC leak fix.
