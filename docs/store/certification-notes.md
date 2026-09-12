# 认证说明（Notes for Certification）

> 提交时粘贴到 Partner Center「提交选项 → 认证说明」。建议直接粘贴英文段；
> 中文段仅作内部对照。

## English

Niubash is a Windows-native, bash-compatible command-line shell. It has **no
graphical user interface by design** — this is expected behavior, not a defect.

How to verify:

1. Open Windows Terminal (or any console host) and run `niu` to enter the
   interactive shell, or run `niu --version` for a quick check.
2. Inside the shell, try `ls`, `grep`, `pwd`, or any standard command —
   commands are provided by the bundled WinuxCmd runtime.
3. The shell reads and writes user-level configuration files under the user
   profile (for example `~/.niubashrc`) and may spawn child processes
   (its own bundled command executables). All of this happens locally on the
   machine; the product does not collect or transmit any personal information.

The package requires the `runFullTrust` restricted capability because Niubash
is a native Win32 desktop application that needs full file-system access and
the ability to create child processes; it cannot function inside an app
container.

The product also runs without issues on Windows 10/11 S mode: all executables
ship inside the package and no system settings are modified.

## 中文对照

Niubash 是 Windows 原生的 bash 兼容命令行 shell，**没有图形界面属于设计预期**，
不是缺陷。

验证方式：打开 Windows Terminal，运行 `niu` 进入交互 shell，或 `niu --version`；
在 shell 内运行 `ls`、`grep`、`pwd` 等命令（命令由内置的 WinuxCmd 提供）。产品
会读写用户目录下的本地配置（如 `~/.niubashrc`）并创建子进程（均为包内自带的
命令可执行文件），全部在本机完成，不收集、不传输任何个人信息。

需要 `runFullTrust` 受限能力的原因：Niubash 是原生 Win32 桌面应用，需要完整
文件系统访问与子进程能力，无法在应用容器中运行。

产品在 Windows 10/11 S 模式下同样可用：所有可执行文件都在包内，不修改任何
系统设置。
