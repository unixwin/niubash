# 贡献指南

感谢你对 niubash 项目的兴趣！我们欢迎各种形式的贡献，包括但不限于代码、文档、测试、插件和反馈。

## 行为准则

请保持友善与专业。我们期望所有社区互动都基于相互尊重；骚扰、歧视或攻击性言论不被接受。

## 如何贡献

### 报告问题

发现 bug 或有功能请求时，请通过 GitHub Issues 提交：

1. 搜索现有问题，避免重复
2. 提供复现步骤和预期行为
3. 包含你的环境信息：操作系统、`niu --version`、终端类型（ConPTY / Windows Terminal / 其他）

### 先想清楚改动属于哪一层（必读）

niubash 与 [rubash](https://github.com/unixwin/rubash) 有明确的职责边界，提交前请先定位你的改动属于哪一层：

- **shell 语义**（解析器、执行器、内建命令、函数、重定向、管道、作业控制）→ 上游 `unixwin/rubash`，niubash 只是内嵌它的宿主。发现语义问题请直接到 rubash 仓库修复，不要在宿主侧做长期语义绕行。
- **交互 UX**（行编辑、提示符渲染、补全面向用户的部分）→ niubash / reedline。
- **插件系统 / 发行集成**（`niu setup`、`niu plugin`、oh-my-niu、WinuxCmd 命令链接、打包）→ niubash。

不确定属于哪一层时，开一个 issue 描述现象即可，维护者会帮你定位。

### 运行与调试

- 项目命令使用 Windows 原生命令和正常 Windows 路径；不要用 PowerShell 作为项目命令语言，也不要引入 MSYS2/Git Bash/Cygwin/WSL 的路径假设。
- 从外部进程启动 niubash 用 `niu -c '<command>'`；已在 niubash 会话内则直接执行命令。
- 用户配置入口是 `~/.niubashrc`；插件清单（`plugins.json` + registry）驱动生成的加载器 `~/.niubash/generated/plugins.niu`。

## 开发流程

#### 1. Fork 并克隆仓库

```bash
git clone https://github.com/YOUR_USERNAME/niubash.git
cd niubash
```

#### 2. 创建分支

```bash
git checkout -b fix/your-fix-name
# 或 feature/your-feature-name
```

#### 3. 开发并验证

推荐测试驱动开发：先写失败测试，再实现，再跑绿。

快速验证回路（提交前请至少跑这一组）：

```bash
cargo fmt --check -p niubash
cargo build --locked
cargo test --workspace --locked
```

重命名品牌门（改名残留校验，必须保持通过）：

```bash
sh scripts/check-rename-clean.sh
```

涉及运行时库的改动：

```bash
cargo test -p niubash-runtime --lib --locked
```

宿主契约与兼容套件需要 WinuxCmd 命令链接在 `PATH` 中（用 Windows 分隔符 `;`）：

```bash
PATH="C:/path/to/WinuxCmd/build-vs-release;$PATH" \
WINUXCMD_PATH="C:/path/to/WinuxCmd/build-vs-release/winuxcmd.exe" \
cargo test --test host_contract --locked -- --ignored
```

#### 4. 提交更改

```bash
git add .
git commit -m "fix(area): 简短描述"
```

提交信息格式：

```
<类型>(<范围>): <简短描述>

<详细描述（可选）>

<相关 issue（可选，如 Refs #123 / Closes #123）>
```

类型：`feat` / `fix` / `docs` / `test` / `refactor` / `perf` / `ci` / `chore`

#### 5. 推送并创建 PR

```bash
git push origin fix/your-fix-name
```

然后在 GitHub 上创建 Pull Request。CI 全绿是合入门槛。

## 代码规范

- 使用 `rustfmt` 格式化代码（`cargo fmt`），遵循现有模块结构与命名约定
- 所有新功能必须有测试；修复 bug 时添加回归测试
- 保持非交互行为安静、确定：无横幅、稳定 stdout/stderr、精确透传退出码
- 不往用户启动文件写东西：`niu plugin` 管理不写 `~/.niubashrc`，插件状态走 manifest

## 许可证

贡献的代码将以项目的 [MIT 许可证](LICENSE) 发布。

## 感谢

感谢所有贡献者的努力！
