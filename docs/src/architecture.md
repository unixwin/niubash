# Niubash v2 Architecture

> 基于 rubash + winuxcmd 的 Windows 原生 Bash-compatible terminal

## 项目定位

niubash 是一个 Windows 原生、无隔离、给人和 agent 都可以直接使用的 Bash-compatible terminal。它**不自己实现 shell 语言**，而是作为 **rubash lib**（bash 兼容引擎）的交互式前端 + **winuxcmd**（coreutils）的路由层。它的核心价值在于 Windows 原生进程/环境体验：reedline REPL、补全系统、主题系统、Ctrl+C 处理、终端集成，以及稳定的非交互式 agent 执行契约。

niubash 不是 MSYS2、Git Bash、Cygwin 或 WSL 风格的隔离环境。`~` 指向普通 Windows 用户 home（PowerShell 中的 home / `USERPROFILE` / `dirs::home_dir()`），PATH、cwd、env、stdout、stderr、exit code 都是正常 Windows 进程状态。

## 三层架构

```
niu.exe
├── niubash 自身层 (Rust)
│   ├── rubash::Executor         ← shell 语言引擎 (lexer/parser/execution/builtins)
│   ├── reedline REPL            ← 行编辑、历史、补全
│   ├── completion/              ← shell definitions + bash 自动导入 + 三级缓存
│   ├── theme/                   ← 主题 API / schema / bundle loader
│   ├── config                   ← legacy/managed machine-state 读取
│   ├── plugins                  ← Niubash 官方插件 registry / bundle 控制面
│   └── ctrl_c                   ← Win32 Ctrl+C 处理
├── rubash lib (Rust)
│   ├── lexer/parser/ast
│   ├── executor (pipeline/redirect/alias/function/array/job)
│   └── builtins (cd/source/export/set/test/printf...)
└── winuxcmd.exe (C++)           ← Unix coreutils (ls/cat/grep/find/cp/mv/rm...)
```

## 关键设计决策

### 1. rubash 作为 lib 依赖

niubash 直接链接 rubash 作为 Rust crate 依赖：

```toml
[dependencies]
rubash = { git = "https://github.com/unixwin/rubash.git", branch = "master" }
```

所有 shell 语义（解析、执行、内建命令、变量展开、重定向、管道、作业控制）委托给 rubash。niubash 不重复实现 lexer/parser/ast/builtins。

### 2. WinuxCmd 由 Niubash 选择并通过 PATH 集成

不是通过 FFI/DLL——rubash Executor 仍然通过 PATH 查找外部命令。版本选择
属于 Niubash 的 session/config 责任，在启动时：

1. 读取显式 `WINUXCMD_PATH`，必要时再按 Niubash 自己的安装/bundle/PATH
   规则寻找一个 `winuxcmd.exe`
2. 将**同一个** exe 所在目录前置到进程 `PATH`，以提供 `ls`/`cat`/`grep`
   等 command links
3. 将解析出的精确 exe 路径通过 `Executor::set_winuxcmd_path` 传给 rubash

Rubash 不会再从 PATH 猜测另一个 `winuxcmd.exe`。这样即使 Windows PATH 中
同时存在旧 bundle 的 command links，也不会把 dispatcher 和 links 混用。

### 3. Windows real installation tree

Niubash derives one shell root from the selected installed
`winuxcmd.exe`. For example, the executable
`<install>/usr/bin/winuxcmd.exe` makes `<install>` the root. Niubash creates
the ordinary directories below that root:

```text
<install>/usr/bin
<install>/bin
<install>/usr/local/bin
<install>/etc
<install>/var
<install>/tmp
<install>/dev
<install>/.wpm
```

`usr/bin` is canonical for WinuxCmd, WPM, command links, and filename-only WPM
targets. Explicit package targets keep their requested real directory.
Niubash passes the selected installation root to Rubash through
`NIU_ROOT`; there is no second `~/.niubash/root` tree and no provider
union. Rubash maps `/`, `/bin`, `/usr/bin`, `/etc`, and `/tmp` directly below
the real root. `/dev/null` maps to Windows `NUL`; other `/dev` entries remain
unsupported capabilities.

### 4. 补全系统独立于引擎

补全系统（shell 定义 + bash 脚本自动导入 + `cmd -h` 描述抓取 + 三级缓存）在 niubash 侧实现，不依赖 rubash。这是 niubash 的核心差异化能力。

### 5. 配置与启动入口

- `~/.niubashrc` 是主要交互式入口，用普通 niubash/bash 语法声明插件列表、
  主题、prompt 模板、`export`、`alias`、函数和本地启动逻辑。
- `~/.winshrc` 是兼容 fallback；只有 `~/.niubashrc` 不存在时才作为旧用户
  rc 启动文件读取。
- plugin CLI 的启停记录、权限、bundle 版本、legacy managed blocks、测试隔离、补全
  目录等机器状态由内部 managed-state 机制维护，不是用户配置入口。
- 当 `~/.niubashrc` 存在时，它是 source plugin/framework 的入口；host 不再
  同时从 managed-state 默认插件状态偷偷 source 一遍官方 source plugins，避免双入口和
  prompt/Git 状态重复刷新。
- 普通 `niu -c`、脚本文件和 stdin 脚本仍保持安静确定，不加载交互式 rc 或
  source plugins。

设计原则是减少人类可见入口：用户日常只改 `~/.niubashrc`；机器状态由
Niubash 自己维护，用户不需要编辑其存储格式。

### 5. 插件系统

v3 插件系统是 Niubash 自己的插件系统。

- `oh-my-winuxsh` 作为官方 bundled plugin distribution 随 niubash 发行。
- git/docker/kubectl/npm 这类 shell helper 可以作为
  `kind = "source"` 的 first-party pack，从 bundle 内 `init.winux` 加载。
- zoxide/direnv/dotenv/fzf 等需要更强 host 行为的能力继续由
  `kind = "builtin"` 或后续显式 effect/runtime API 承接。
- 第三方插件当前通过受审阅的 source packs 和 process adapters 接入，权限模型由 manifest 统一声明。
- process/IPC 插件是外部工具桥和调试后端。
- 插件不能扩展 rubash parser/executor，也不能 source 任意 legacy
  `.winsh` 或用户目录里发现的 rc 片段。source pack 只能加载
  manifest 声明的 bundle-local `.winux` 文件，并且需要 `shell:source`
  权限。
- Niubash 编辑器能力由 reedline 和 Niubash 原生 keybinding presets 提供。

## 目录结构

```
niubash/
├── Cargo.toml
├── LICENSE                   # GPL-3.0-or-later
├── README.md / README-zh.md
├── .niubashrc                 # primary interactive user entry
├── .winshrc                   # legacy fallback rc
├── managed state              # internal machine-managed state
├── crates/
│   └── niubash-runtime/
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs        # 库入口
│           ├── shell.rs      # Shell 状态
│           ├── repl.rs       # reedline REPL
│           ├── ctrl_c.rs     # Win32 Ctrl+C
│           ├── config.rs     # 配置解析
│           ├── winuxcmd.rs   # winuxcmd 探测
│           ├── prompt.rs     # Prompt 渲染
│           ├── theme.rs      # 主题系统
│           └── completion/   # 补全系统
├── src/
│   └── main.rs               # 入口
└── docs/
    ├── src/
    │   └── (this documentation book)
    └── planning/
```

## 数据流

```
用户输入 "ls -la | grep foo"
         │
         ▼
   reedline (行编辑 + 补全)
         │
         ▼
   shell.execute_line(line)      ← niubash-runtime
         │
         ├─ rubash::lexer::tokenize(line)
         ├─ rubash::parser::parse(tokens) → Ast
         └─ executor.execute_ast(&ast)    ← rubash 处理全部语义
                │
                ├─ 内建命令 (cd/source/echo...)
                ├─ 外部命令 → find_user_command("ls")
                │                   │ (PATH 已注入 winuxcmd 目录)
                │                   ▼
                │              winuxcmd.exe ls -la
                │
                ├─ 管道: | grep foo → find_user_command("grep")
                └─ 输出到 stdout
```

## 与旧架构的差异

| 方面 | v1 (旧 niubash) | v2 (新 niubash) |
|------|-----------------|-----------------|
| Shell 引擎 | 自研 winsh-lexer/parser/ast | rubash lib |
| Coreutils | winuxcmd FFI (DLL, 已禁用) | winuxcmd.exe 进程 (PATH 注入) |
| 命令路由 | command_router.rs 分类表 | rubash 内部 find_user_command |
| 内建命令 | builtins.rs 自实现 | rubash::builtins |
| 补全系统 | src/completion/ | 完整保留迁移 |
| 主题系统 | theme.rs (8 主题) | 精简为 4 内置主题 |
| 插件系统 | Plugin trait + Oh-My-Niubash | 移出 v1，后续迭代 |
| 许可协议 | MIT | GPL-3.0-or-later |

## 版本规划

- v2.2: rubash rewrite 稳定化、补全增强、Vi/Ctrl+R、配置一致性、用户主题
- v2.3: Windows 原生 terminal contract、agent 友好的非交互式行为、history/prompt/completion UX
- v2.4: 交互体验 polish（右 prompt、提示、补全菜单、默认配置）
- v3: 内置 Niubash 插件系统；`oh-my-winuxsh` 作为官方 bundled plugin
  distribution；先用 `builtin` registry 统一现有 first-party packs，再引入
  第三方插件通过 source/process runtime 接入。
- 非目标: Linux/macOS 原生 shell 产品；rubash 可跨平台复用，但 niubash 产品目标是 Windows

---

*Last updated: 2026-07-30*
