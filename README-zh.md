# niubash

<p align="center">
  <img src="assets/niubash-icon-256.png" alt="niubash" width="120"/>
</p>

> **Bash 原生登陆 Windows——牛来了。**
> 不用 WSL，不开虚拟机，没有 `/mnt/c`，没有 cmdlet 方言。
> 两个名字一个 shell——`niu` 或 `niubash`：你手指肌肉记得的那个，也是你 AI agent 天生会说的那个。

中文 · [English](README.md)

[![niubash CI](https://github.com/unixwin/niubash/actions/workflows/ci.yml/badge.svg)](https://github.com/unixwin/niubash/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/unixwin/niubash)](https://github.com/unixwin/niubash/releases)
[![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11%20x64-blue)](https://github.com/unixwin/niubash)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange)](https://github.com/unixwin/niubash)
[![MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Stars](https://img.shields.io/github/stars/unixwin/niubash)](https://github.com/unixwin/niubash/stargazers)

一个原生 Windows shell（同时以 `niu` 和 `niubash` 两个名字安装），v1.0.0 正式版。Bash 语法。Windows 路径。真 Windows 程序。
Unix 命令随包附赠。你的命令和你的工具之间，没有模拟层，也没有翻译官——
你 AI agent 想摔跤都没地方摔。

<img src="assets/demo.gif" alt="niubash 交互会话：git prompt、grep、sed 原地编辑、awk 管道、tree" width="760"/>

就这些。这就是全部卖点。

## 为什么不用 WSL

为了跑个 `grep` 先开一台 Linux 虚拟机，等于为了喝牛奶买下一整座牧场。
牛确实是好牛，但日子不必这么过。

每个 Windows shell 都要你交出点什么。CMD 冻结在 1987 年。PowerShell
不是 Bash——你的 `for`、`grep`、引号直觉，落地即碎。WSL 是你要领养一整个
Linux 发行版才能打印个目录。Git Bash 模拟 Unix 并*猜*你的路径，而 Windows
原生工具根本不说它的方言。

niubash 全部还给你：

- **真·Bash** — `if`、`for`、`case`、`$(...)`、管道、heredoc、函数、数组。引擎是 [rubash](https://github.com/unixwin/rubash)，GNU Bash 官方测试套件 **86/86 全绿**。兼容不是嘴上说说，是上游把卷子替我们考了。
- **Windows 路径，原生进原生出** — `C:\...`、`C:/...` 原样可用，`/c/...` 输入也听得懂，输出永远是原生。原生工具拿到原生路径，零猜测，零路径轮盘赌。
- **Unix 命令随包附赠** — `ls`、`cat`、`grep`、`find`、`test`、`printf`…… 来自 winuxcmd 的真二进制（不是脚本模拟），PATH 命令链接注入，什么都不用装。
- **真 Windows 程序，直接调** — `git.exe`、`node.exe`、`python.exe`、`cargo.exe`。你的 PATH 就是你的 PATH。

## 30 秒上手

牛市来得比这还快。去 [Releases](https://github.com/unixwin/niubash/releases)
下载 `niubash-v*-win-*-setup.exe`（当前 v1.0.0），双击，完事——不需要管理员
权限，它会配好你的 PATH 和 Windows Terminal 配置。嫌重？拿 `.zip` 便携版
（首次启动自动激活 Unix 命令）。源码构建：

```sh
git clone https://github.com/unixwin/niubash.git && cd niubash
cargo build --release && target\release\niu.exe   # niubash.exe 同样可用——一个 shell，两个名字
```

然后：

```sh
niu -c 'ls'                                   # 就这么直接
niu -c 'for f in *.md; do wc -l "$f"; done'   # 真·bash 循环，引号直觉全程在线
niu -c 'git log --oneline -5'                 # 你的 git.exe，原样调用
niu deploy.sh                                 # 脚本执行，安静、确定、退出码精确
niu                                           # 交互式 REPL
```

配置只有一份：`~/.niubashrc`，纯 Bash 语法。主题、prompt、插件、环境变量、
alias、函数都放这里：

```bash
NIU_THEME=p10-classic
NIU_THEME_PLUGIN=theme-p10-classic
NIU_PLUGINS=(prompt-core git common-aliases)
export NIU_THEME NIU_THEME_PLUGIN

# 官方插件发行版 oh-my-niu
[ -f "$NIUBASH/oh-my-niu.winux" ] && . "$NIUBASH/oh-my-niu.winux"

alias ll='ls -la'
alias gst='git status'
hello() { echo "hello from niu"; }
```

多 shell 共享历史？`NIU_HISTORY_MODE` 三档可选：`shared`（默认）、
`session`、`private`。

从 winuxsh 升级来的？首次启动会把旧 `~/.winuxshrc` **一次性自动迁移**为
`~/.niubashrc`（`NIU_*` 前缀自动改写，原文件原样保留，静默且幂等）；
`~/.winshrc` 继续作为兼容回退（仅在 `~/.niubashrc` 不存在时读取）。

保持最新：`niu --self-update`（shell 内也可用 `self-update`）。

## 特性

- **真·Bash 语义** — [rubash](https://github.com/unixwin/rubash) 引擎（同版本 1.0.0），GNU Bash 上游测试套件 86/86。
- **原生路径契约** — 任何方言进，Windows 原生出。MSYS 式的路径转换抽风，这里不存在。
- **Unix 命令真二进制** — winuxcmd（1.0.0）通过 PATH 命令链接注入，`ls`/`grep` 是真 Windows 进程，不是嵌在 shell 里的模拟。
- **一个愿意天天看的 prompt** — 27 款主题（agnoster、spaceship、tokyonight、p10 家族……）、会"长牙"的 git 状态提示（staged / modified / untracked / ↑↓ / stash / 冲突）、语法高亮、自动建议、vi/emacs 双模式、Ctrl+R 历史搜索。
- **带权限模型的插件系统** — 40+ 官方 pack（`git`、`docker`、`kubectl`、`npm`、`zoxide`、`direnv`、`fzf`、`thefuck`……），manifest 统一声明宿主权限，受审阅的 source pack 只能加载 bundle 内声明过的脚本。
- **补全系统** — shell 定义 + bash 补全脚本自动导入 + `cmd -h` 描述抓取 + 三级缓存。
- **三种执行模式** — 交互 REPL；`niu -c`（安静确定性，不加载 rc 和插件）；`niu -C`（一次性 REPL 命令，加载完整启动状态后退出）。
- **自更新** — shell、命令层（`wpm update winuxcmd`）、插件包三条更新线各自独立。

## 架构

```
niu.exe / niubash.exe
├── niubash 宿主层（Rust）       reedline 行编辑 · 主题 · 补全 · 插件 · Ctrl+C
├── rubash 语言引擎（lib，Rust）  lexer / parser / executor / builtins
└── winuxcmd.exe 命令层（C++）   Unix coreutils 真二进制，PATH 命令链接注入
```

- **rubash 是引擎，也是唯一权威** — niubash 不自己实现 shell 语言，rubash
  作为 Rust crate 直接链接。解析、执行、内建命令、变量展开、重定向、管道、
  作业控制，全部在上游。修语义 bug 去 [rubash](https://github.com/unixwin/rubash)
  上游修，Windows 上每一个 bash 用户一起受益。
- **winuxcmd 是命令层，不是 DLL** — 没有 FFI、没有路由表魔法。它就是普通
  Windows 进程，rubash 通过正常 PATH 找到 `ls`、`grep` 这些命令链接。
- **oh-my-niu 是官方插件发行版** — 随 niubash 发行，manifest 声明权限，
  审阅过的 source pack + process 适配器两种形态。
- 非目标：Linux/macOS 原生 shell 产品。rubash 可跨平台复用，但 niubash
  的目标就是 Windows——把一件事做牛。

## 对 AI agent 友好

每个 AI 编程 agent 都会说 Bash——模型是在 Bash 语料上训练的。在 Windows
上，它们大多被锁在 PowerShell 里，就是那个著名的*吃参数*的 shell：

```text
# PowerShell 5.1                              # niubash
> node -e "console.log(JSON.stringify(        ❯ node -e "console.log(JSON.stringify(
    process.argv.slice(1)))" "a b" "" "c\"d"    process.argv.slice(1)))" "a b" "" "c\"d"
    "e\f" "---"                                 "e\f" "---"

ParserError: TerminatorExpectedAtEndOfString   ["a b","","c\"d","e\\f","---"]
```

写了 5 个参数：PowerShell 直接语法报错，niubash 五个全到、一个字节不少。
连 [Codex 在 Windows 上都被锁死 PowerShell](https://github.com/openai/codex/issues/31548)，
用户正在公开投票要求逃生。完整案卷见 [Why niubash](docs/src/why-niubash.md)。

`niu -c` 是一份契约，不是边角料：

- **无 banner**、stdout/stderr 稳定、**退出码精确传递**——agent 写什么，进程就收到什么。
- `niu -c` **不加载 rc、不加载插件、不跑交互钩子**，今天跑和明天跑一个样。
- **路径零转换**：Bash 语感直接可用，没有 MSYS 式的参数改写轮盘赌。
- Bash 训练出来的模型，在 niubash 里第一次不用"入乡随俗"。

这就是键盘另一头的人经历的日常：

<img src="assets/demo-drama.gif" alt="动画剧情：用户和 codex 对话，PowerShell 吃掉参数，用户崩溃，niubash 救场" width="520"/>

## 这头牛

每个 niubash 安装都自带一头会说话的牛。在 `~/.niubashrc` 里引入官方
[oh-my-niu](https://github.com/unixwin/oh-my-niu) 之后：

```text
$ niu_moo "Bash, 原生登陆 Windows。"
 _________________________
< Bash, 原生登陆 Windows。 >
 -------------------------
        \   ^___^
         \  (oo)\_______
            (__)\       )\/                ||----w |
                ||     ||
```

`NIU_BANNER=1` 打开启动时的块字符大字横幅；`niu_moo` 随便喂一句话。
oh-my-niu 的主题、prompt 和 git 状态段都是这头牛在驱动；
[logo](assets/niubash-icon-256.png) 上那对角，就是它头上那对。

## 对比

| | niubash | WSL | Git Bash | PowerShell | CMD |
|---|---|---|---|---|---|
| Bash 语法 | ✅ | ✅ | ✅ | ❌ | ❌ |
| 原生 Windows 路径（无 `/mnt/c`） | ✅ | ❌ | ⚠️ 转换抽风 | ✅ | ✅ |
| 直接调用 `git.exe` / `node.exe` | ✅ | ⚠️ 经 `/mnt/c` | ⚠️ 路径翻译 | ✅ | ✅ |
| 自带 Unix 命令（`ls`、`grep`、`find`） | ✅ | ✅ | ✅ | ❌ | ❌ |
| agent 写的 Bash 直接能跑 | ✅ | ✅ | ⚠️ 参数改写 | ❌ | ❌ |
| 冷启动到提示符 | **~170 ms** | 秒级 | ~1 s | ~280 ms | — |
| 不装额外 OS、不开 VM | ✅ | ❌ | ✅ | ✅ | ✅ |
| 主题 / git 提示 / 插件 | ✅ | — | ✅ | ⚠️ | ❌ |

一个二进制。一个进程。没有发行版要打补丁，没有模拟层要哄。

## FAQ

- **"这不就是又一个 Git Bash？"** 不是。Git Bash 在 Windows 上模拟 Unix：
  翻译路径、猜参数。niubash 是原生 Windows 进程，Bash 兼容发生在语言引擎
  （rubash）里，不在假文件系统里。
- **"那我还要 WSL 干嘛？"** 各有各的用：真 Linux 内核、Linux Docker、
  Linux 专用工具链，它依然是把好手。至于剩下的 95%——你需要的不是 WSL，
  是 niubash。
- **"我的配置在哪？旧 winuxsh 的 `~/.winuxshrc` 还能用吗？"**
  `~/.niubashrc`，纯 Bash。首次启动会一次性自动迁移旧 `~/.winuxshrc`
  （原文件保留不动）；`~/.winshrc` 仅在 `~/.niubashrc` 缺席时作为回退读取。
  机器状态由 niubash 自己管理，用户不需要维护第二套配置格式。
- **"为什么叫 niu？"** niu = 牛。短、好打、不粘键盘油。项目叫 niubash，
  二进制叫 `niu`，环境变量前缀 `NIU_`。Windows 上最"牛"的 bash，名字得对得起产品。
- **"是在黑 PowerShell 吗？"** 不是。PowerShell 是强大的自动化语言，只是它
  不是 Bash。模型在 Bash 语料上训练，在 Windows 上却被迫说 cmdlet 方言——
  问题出在错配，不在于谁写得烂。

## 文档

完整文档站：**[docs](https://unixwin.github.io/niubash/)** · [快速上手](docs/src/getting-started.md) · [Why niubash](docs/src/why-niubash.md) · [高级用法](docs/src/advanced-usage.md) · [架构](docs/src/architecture.md)

---

如果 niubash 帮你省下了"为跑 grep 先开虚拟机"的仪式感，
[给仓库点个 Star](https://github.com/unixwin/niubash)，把牛市分享给下一个
还在 CMD 里挣扎的朋友。★

## License

MIT，详见 [LICENSE](LICENSE)。
