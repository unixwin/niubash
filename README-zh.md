# Winuxsh

中文 | [English](README.md)

一个 Windows 原生、感觉像 bash 和 zsh 的 shell，不需要 MSYS2、Git Bash、
Cygwin、WSL 隔离层，也不走 PowerShell 语义。写给人类和 coding agent 用。

```text
caomengxuan@DESKTOP C:\Users\me\repo\winuxsh  git:(master) ●2 ✚1 ↑1 ?3
%
```

这个 prompt 直接显示了分支、dirty 状态、staged/unstaged 数量、ahead/behind
和 untracked 文件 —— oh-my-zsh 的 `git_prompt_status` 风格，全部用 Rust
原生实现。只要 `cd` 进一个 git 仓库就能用。

## 核心公式

```text
winuxsh = rubash shell engine + winuxcmd coreutils + reedline frontend
```

- `rubash` 提供 shell 语言能力：解析、执行、内置命令、管道、重定向、别名、
  函数和作业语义。
- `winuxcmd.exe` 通过 Windows `PATH` 注入提供 Unix 工具集（`ls`、`cat`、
  `grep`、`find`、`cp`、`mv`、`rm`、`mkdir`）。
- `reedline` 提供交互前端：编辑、历史、补全菜单、提示和 prompt 渲染。
- `~` 就是普通的 Windows 用户家目录。`PATH`、环境变量、cwd、stdout、stderr、
  exit code 都是正常 Windows 进程状态。

Winuxsh 不是 `zsh.exe`。它会安全地读取你的 `.zshrc` 和 Oh My Zsh 配置意图，
然后用 Rust 原生实现有用的部分。

## 为什么用它

如果你在 PowerShell 里打了个 `ls` 却看到一张属性表的输出，或者看着 coding agent
因为 `test -f Cargo.toml` 在 pwsh 里不存在而失败，winuxsh 就是答案。它给想要
在 Windows 上写 bash 的人用的，也给希望 agent 能跑和 Linux 上相同命令的人用的。

Winuxsh 给你：

- **Windows 上真正的 bash 兼容 shell**：`if`、`for`、`while`、`case`、函数、
  heredoc、管道、重定向、`$(...)` 全都可用。
- **Windows 原生进程行为**：没有假的 `/c` 文件系统、没有 MSYS 隔离、
  没有 PowerShell 的 wildcard 惊喜。`cd D:\repo` 后 `pwd` 输出
  `D:/repo`，就是 Windows 原生的路径。
- **Zsh 一样的交互体验**：自动建议、语法高亮、Vi/Emacs 编辑、Ctrl+R 历史搜索、
  多行 PS2 提示、丰富的 Tab 补全。
- **Agent 友好的 CLI**：`winuxsh -c ...` 和 `winuxsh script.sh`
  保持安静、确定性，正确传递 stdout、stderr 和 exit code。
- **安全的 zsh 迁移路径**：winuxsh 扫描你的 `.zshrc` 和 Oh My Zsh，
  输出可审阅的导入计划，只有你确认后才写入配置。

## 看一下效果

从源码构建：

```pwsh
cargo build --release
```

启动交互式 shell：

```pwsh
target\release\winuxsh.exe
```

如果你下载的是 release 包，第一次启动时 winuxsh 会自动执行：

```bash
winuxsh winuxcmd/activate-winuxcmd.sh
```

它只在缺少命令链接时运行一次，在 `winuxcmd/` 目录下生成本地链接，之后
`ls`、`cat`、`grep` 就能直接用。

试一下：

```bash
cd C:\Users\me\repo\winuxsh
pwd                          # 输出 C:/Users/me/repo/winuxsh
git status                   # prompt 自动显示分支和 dirty 状态
ls -la                       # winuxcmd 提供的 Unix 风格目录列表
echo $(git rev-parse --short HEAD)
for i in 1 2 3; do echo $i; done
if [ -f Cargo.toml ]; then echo "是 Rust 项目"; fi
```

原生 Windows 路径可直接作为 shell 输入：

```bash
ls C:\Users
ls C:/Users/YourName         # 两种路径风格都支持
cd C:/Users/YourName/repo
```

多行命令会等待块完成后再执行，和标准 shell 一样：

```bash
HTTP_CODE=200
if [ $HTTP_CODE -eq 200 ]; then
  echo "OK"
fi
```

## Zsh 用户从这里开始

不要直接把 zsh 插件源码拷贝进 winuxsh。先让 winuxsh 检查你现有的配置，
然后决定导入什么：

```pwsh
winuxsh --zsh-compat-report         # 人类可读的 ~/.zshrc 扫描报告
winuxsh --zsh-compat-report-json    # 同上,机器可读
winuxsh --zsh-compat-import-plan    # 看看会写什么到 ~/.winshrc.toml
```

如果计划看起来没问题，再显式应用：

```pwsh
winuxsh --zsh-compat-import-apply
winuxsh --zsh-compat-import-status
winuxsh --zsh-compat-doctor
```

回滚也是先出计划：

```pwsh
winuxsh --zsh-compat-import-rollback-plan
```

低风险日常 profile 或确定性 agent profile：

```pwsh
winuxsh --zsh-profile-plan zsh-lite
winuxsh --zsh-profile-plan agent
```

详细教程：[Zsh 迁移教程](DOCS/zsh-migration-guide.md)。

## 内置的 Zsh 风格 Pack

Winuxsh 内置了常见 zsh / Oh My Zsh 插件行为的原生支持。这些 pack 是用 Rust
在 rubash、reedline、winuxcmd 之上实现的。没有 vendor zsh 插件源码，
也没有在启动时 source zsh 脚本。

查看当前 inventory：

```pwsh
winuxsh --zsh-native-packs
winuxsh --zsh-native-packs-json
```

默认启用（安全 UI 能力）：

- `zsh-autosuggestions` —— 基于历史的 inline 建议
- `zsh-syntax-highlighting` —— 主要高亮子集

需要 opt-in 的 pack：

- `git` —— Oh My Zsh 风格别名（`g`、`gst`、`gco`、`gl`、`gp`、`glog`）
  加上读者在本页顶部看到的彩色 prompt 状态
- `zsh-history-substring-search`、标准 ZLE widget 映射
- `docker`、`kubectl`、`npm`
- `command-not-found`、`direnv`、`dotenv`、`zoxide`、`thefuck`、`fzf`、
  `zsh-interactive-cd`、`last-working-dir`

会读取项目文件、执行外部命令或改变 shell 状态的 pack 默认关闭，
必须在 `~/.winshrc.toml` 中显式启用。

## 配置

`~/.winshrc.toml` 是结构化设置文件，负责 prompt、editor、history、
completion、native packs 这类可声明配置。
`~/.winshrc` 是唯一的 REPL 启动脚本文件，负责 `export`、`alias`、函数、
以及用户想写的 shell 逻辑。
`.zshrc` 仅作为导入源读取，不作为运行时启动文件。

最小示例：

```toml
[shell]
prompt_format = "{user}@{host} {cwd} {git_prompt} {symbol}"
right_prompt_format = ""
multiline_indicator = "> "

[editor]
edit_mode = "emacs" # emacs | vi

[history]
path = "~/.winuxsh_history"
max_size = 10000
ignore_space_prefixed = true

[theme]
current_theme = "default" # default | dark | light | colorful | ~/.winuxsh/themes/<name>.toml

[completions]
matching = "prefix" # prefix | substring
case_sensitive = false
max_command_results = 500
completion_dirs = []

[menus]
completion_page_size = 10
history_page_size = 10
max_entry_lines = 5

[winuxcmd]
# 可选覆盖；省略时从 PATH 自动发现。
# path = "D:/tools/winuxcmd/winuxcmd.exe"
```

`~/.winshrc` 使用 shell 语法：

```bash
export EDITOR=vim
alias ll='ls -la'
alias gst='git status'

hello() {
  echo "hello from winuxsh"
}
```

`{git_prompt}` 占位符在 git 仓库里显示分支和紧凑状态符号，不在 git 目录时为空。
符号含义：`●N` 暂存、`✚N` 已修改、`?N` 未跟踪、`↑N` 领先、`↓N` 落后、
`⚑N` 储藏、`✖N` 冲突。分支颜色在 clean 时是绿色，dirty 时自动变黄。

zsh compat import 只在显式运行 `--zsh-compat-import-apply` 时写入 managed block。

## Rubash 负责的 shell 语义

Shell 语言层面的东西全部走 rubash，包括：

- 变量和命令替换
- 管道和重定向
- 别名和函数
- `if`、`for`、`while`、`case`、函数、heredoc、续行
- 脚本文件和 `-c` 整体执行
- 多行复合命令收集为一次执行

作业控制和可执行语法也归 rubash 管；winuxsh 不重复实现。这让 winuxsh 保持精炼，
shell 层面能直接受益于 rubash 通过的 bash 上游测试。

## 补全系统

Winuxsh 补全支持：

- 内置 winuxcmd 命令定义（`ls`、`grep`、`find`、`cat`、`cp`、`mv`、`rm`、
  `mkdir`、`touch`、`chmod`，现在还有 `git`）
- `git` 子命令感知：`git add <Tab>`、`git commit -<Tab>`、`git push --<Tab>`
  都能得到实际的 flag 候选
- 从 Windows `PATH` / `PATHEXT` 补全命令
- 支持空格路径 quote/escape 的路径补全
- 环境变量补全
- `completion_dirs` 中的用户 TOML 定义
- 可安全翻译的 zsh completion assets
- 显式 allowlist + timeout 的 dynamic/runtime completion providers

用户 TOML 定义会覆盖内置定义，所以你的 `~/.winuxsh/completions/ls.toml` 优先于
自带的 `ls.toml`。

## 架构

```text
winuxsh.exe
├── winuxsh runtime (Rust)
│   ├── rubash::Executor        shell 语言引擎
│   ├── reedline REPL           编辑、历史、菜单、提示
│   ├── completion system       TOML、zsh 导入、缓存、PATH 命令
│   ├── zsh compatibility       扫描器、导入计划、原生 pack
│   ├── theme/prompt            模板、内置和用户主题
│   ├── git status              通过 `git status --porcelain` 获取分支和计数
│   ├── config                  ~/.winshrc.toml
│   └── ctrl_c                  Win32 Ctrl+C 处理
├── rubash lib                  解析器/执行器/内置命令
└── winuxcmd.exe                通过 PATH 注入的 Unix coreutils
```

## 非目标

- 不做 PowerShell 语义或 wildcard 行为
- 不做 MSYS2 / Git Bash / Cygwin / WSL 隔离
- 不采用 Nushell 语法或结构化管道模型
- 不在 winuxsh 内实现 zsh 解析器或 ZLE 运行时
- 不在启动时盲目 source `.zshrc` 或 zsh 插件脚本
- 不引入 winuxcmd FFI/DLL
- 不重复实现 rubash 的解析器/执行器或核心 shell 语义

## 文档

- [快速开始](DOCS/getting-started.md)
- [Zsh 迁移教程](DOCS/zsh-migration-guide.md)
- [Roadmap](DOCS/winuxsh-roadmap.md)
- [架构](DOCS/architecture.md)
- [原生 Zsh Plugin Pack Plan](DOCS/winuxsh-native-zsh-plugin-pack-plan.md)
- [Zsh 兼容计划](DOCS/zsh-compatibility-plan.md)
- [定位与功能地图](DOCS/winuxsh-positioning-and-feature-map.md)

## License

GPL-3.0-or-later，详见 [LICENSE](LICENSE)。
