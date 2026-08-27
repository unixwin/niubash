# Winuxsh

> **Windows 上的原生 Bash。** 不是 WSL，不是虚拟机，不是 `/mnt/c`，更不是 cmdlet。
> 不是你记忆中的那个 Bash，但原汁原味的程度没两样。这次，参数一个都不会少。

中文 · [English](README.md)

[![Winuxsh CI](https://github.com/unixwin/winuxsh/actions/workflows/ci.yml/badge.svg)](https://github.com/unixwin/winuxsh/actions/workflows/ci.yml)
[![GPL-3.0](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)
[![Stars](https://img.shields.io/github/stars/unixwin/winuxsh)](https://github.com/unixwin/winuxsh/stargazers)

一个原生 Windows 二进制。Bash 语法。Windows 路径。真 Windows 程序。
Unix 命令随包附赠。你的命令和你的工具之间，没有模拟层，也没有翻译官——
你 AI agent 想摔跤都没地方摔。

<img src="assets/demo.gif" alt="Winuxsh 交互会话：git prompt、grep、sed 原地编辑、awk 管道、tree" width="760"/>

就这些。这就是全部卖点。

## 一句话卖点

**你需要的不是 WSL——是 Winuxsh。**

每个 Windows shell 都要你交出点什么。CMD 冻结在 1987 年。PowerShell
不是 Bash——你的 `for`、`grep`、引号直觉，落地即碎。WSL 是你要领养一整个
Linux 发行版才能打印个目录。Git Bash 模拟 Unix 并*猜*你的路径，而 Windows
原生工具根本不说它的方言。

Winuxsh 全部还给你：

- **真·Bash** — `if`、`for`、`case`、`$(...)`、管道、heredoc、函数、数组。引擎是 [rubash](https://github.com/unixwin/rubash)，GNU Bash 官方测试套件 **86/86 全绿**。
- **Windows 路径，原生** — `C:\...`、`C:/...` 原样可用，`/c/...` 输入也听得懂，输出永远是原生。原生工具拿到原生路径，零猜测。
- **Unix 命令随包附赠** — `ls`、`cat`、`grep`、`find`、`test`、`printf`…… 来自 WinuxCmd，什么都不用装。
- **真 Windows 程序，直接调** — `git.exe`、`node.exe`、`python.exe`、`cargo.exe`。你的 PATH 就是你的 PATH。
- **一个你愿意天天看的 prompt** — 27 款主题（agnoster、spaceship、tokyonight、p10 家族……）、会"长牙"的 git 提示、语法高亮、自动建议、vi/emacs 双模式。
- **带权限模型的插件** — 40+ 随包插件（`git`、`docker`、`kubectl`、`npm`、`zoxide`、`fzf`、`thefuck`……），受审阅的 source 插件和 process 适配器都会声明所需的宿主权限。

## AI 原生

每个 AI 编程 agent 都会说 Bash。在 Windows 上，它们大多被锁在 PowerShell
里——就是那个著名的*吃参数*的 shell：

```text
# PowerShell 5.1                              # Winuxsh
> node -e "console.log(JSON.stringify(        ❯ node -e "console.log(JSON.stringify(
    process.argv.slice(1)))" "a b" "" "c\"d"    process.argv.slice(1)))" "a b" "" "c\"d"
    "e\f" "---"                                 "e\f" "---"

ParserError: TerminatorExpectedAtEndOfString   ["a b","","c\"d","e\\f","---"]
```

写了 5 个参数：PowerShell 直接语法报错，Winuxsh 五个全到、一个字节不少。
连 [Codex 在 Windows 上都被锁死 PowerShell](https://github.com/openai/codex/issues/31548)，
用户正在公开投票要求逃生。
完整案卷见 [Why Winuxsh](docs/src/why-winuxsh.md)。

这就是键盘另一头的人经历的日常：

<img src="assets/demo-drama.gif" alt="动画剧情：用户和 codex 对话，PowerShell 吃掉参数，用户崩溃，winuxsh 救场" width="520"/>

`winuxsh -c` 是一份契约，不是边角料：**无 banner、stdout/stderr 稳定、
退出码精确传递。** agent 写什么，进程就收到什么。

```sh
winuxsh -c 'test -f Cargo.toml && echo build' && echo "exit=$?"
winuxsh deploy.sh
```

## 安装

去 [Releases](https://github.com/unixwin/winuxsh/releases) 下载
`winuxsh-v*-win-*-setup.exe`，双击，完事——不需要管理员权限，它会配好
你的 PATH 和 Windows Terminal 配置。嫌重？拿 `.zip` 便携版（首次启动自动
激活 Unix 命令）。源码构建：

```sh
git clone https://github.com/unixwin/winuxsh.git && cd winuxsh
cargo build --release && target\release\winuxsh.exe
```

保持最新：`winuxsh --self-update`。

## 配置

`~/.winuxshrc` 是交互式入口。主题、prompt、插件、环境变量、alias、
函数都放这里：

```sh
WINUXSH_THEME=spaceship
WINUXSH_PLUGINS=(prompt-core git)
[ -f "$WINUXSH/oh-my-winuxsh.winux" ] && . "$WINUXSH/oh-my-winuxsh.winux"
```

## 终端彩蛋

Winuxsh 的终端不只能跑命令——还能打印图片。[terminal-flags](https://github.com/caomengxuan666/terminal-flags)
项目可以把任意图片/GIF 转成独立 ANSI 打印脚本：

```sh
winuxsh flags/taffy.sh         # 照片，直接显示在终端里
winuxsh flags/qiu-dance.sh     # GIF 动画，帧率原样保留
```

真彩色半块像素，运行时不需要 Python 或 Pillow：

<img src="assets/demo-qiu-dance.gif" alt="秋表情动画在 Winuxsh 终端里播放，由生成的 shell 脚本打印" width="560"/>

## 文档

完整文档站：**[docs](https://unixwin.github.io/winuxsh/)** · [快速上手](docs/src/getting-started.md) · [Why Winuxsh](docs/src/why-winuxsh.md) · [高级用法](docs/src/advanced-usage.md) · [架构](docs/src/architecture.md)

底层三件套：[rubash](https://github.com/unixwin/rubash)（Bash 引擎）· WinuxCmd（Unix 命令）· [reedline](https://github.com/nushell/reedline)（行编辑器）

## FAQ

- **"这不就是又一个 Git Bash？"** 不是。Git Bash 在 Windows 上模拟 Unix；Winuxsh 是原生 Windows 进程：原生路径、直接执行 Windows 二进制、Bash 兼容发生在语言引擎里，不在假的文件系统里。
- **"那我还要 WSL 干嘛？"** 各有各的用：真 Linux 内核、Linux Docker、Linux 专用工具链，它依然是把好手。至于剩下的 95%——你需要的不是 WSL，是 Winuxsh。
- **"我的配置在哪？"** `~/.winuxshrc`——纯 Bash。机器状态由 Winuxsh
  自己管理，用户不需要维护第二套配置格式。

---

如果 Winuxsh 让你免了一次"为了跑 grep 先开个 Linux 虚拟机"的体验，
[给仓库点个 Star](https://github.com/unixwin/winuxsh)，顺便告诉一个
还在用 CMD 的朋友。★

## License

GPL-3.0-or-later，详见 [LICENSE](LICENSE)。
