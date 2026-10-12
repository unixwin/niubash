# 工作目录状态契约（Agent cwd-state）

> 出处：x2410516039/xgr-niubash 分支 `one-shot-fastpath`（提交 `5c4ec1d` / `2028f39`），经适配带入上游。
> 面向读者：agent 宿主（DSH / ZCode hook 等）作者。

## 1. 背景与目标

`niu -c` 是 agent/CI 的主力入口，但它是**一次性**的：每条命令一个新进程，起始目录由调用者传入，`cd` 出不了这一次调用。

对 agent 来说这不是小摩擦：为了让第二条命令落在正确目录，模型必须在**每条**命令前面写 `cd <项目根> && …`。代价有三层——

1. 每条命令都多花 token，且 `cd` 让命令的真实意图变模糊；
2. `cd sub && …` 之后，模型无法让"我现在在 sub"这一事实跨调用成立，只能反复重申；
3. 宿主若靠"解析命令文本"来跟踪工作目录（如 Claude Code 系 hook 的 `updatedInput` 包装成 base64），一旦命令被包装成不透明 token，宿主的跟踪就永久失效——声明"工作目录跨调用保持"的工具描述随即变成假话。

目标：给 niu 一个**显式**的目录记忆开关，让"一条命令 = 一次可审计的执行"保持不变，同时让 `cd` 在调用之间成立。

## 2. 契约

```
niu --cwd-state <file> [--cwd-state-verbose] -c '<cmd>'
```

1. **恢复**：启动时读 `<file>`，内容有效目录 → `chdir` 过去；否则沿用调用者传入的 cwd。恢复发生在**一切之前**（先于 niu 自身启动逻辑、`source_non_interactive_env`、相对脚本路径解析）。
2. **记录**：命令结束后把 shell 的最终目录写回 `<file>`。
3. **显式开关**：不带开关时 `niu -c` 的行为逐字节不变；CI 与交互式调用者对"起始目录可预测"的依赖不能被破坏。

状态文件是一个小 JSON 对象，便于宿主读取、展示与重置：

```json
{"cwd":"D:\\project\\git-clone\\niubash","updatedAt":1789641441305}
```

读取时**容忍**手写的裸路径（`echo D:/work > state` 也能用）；写入用 `临时文件 + rename` 覆盖（Windows 上是 `MoveFileEx` + `REPLACE_EXISTING`），rename 被占用时重试 2 次并退化为直接写。

## 3. 刻意保留的设计决策

| 决策 | 原因 |
|---|---|
| 记录在 `finish_with_exit_trap` **之后** | `EXIT` trap 里的 `cd` 也算"shell 最终在哪"；`execute_script` 末尾已把进程 cwd 同步到 shell 的 `PWD`，而 trap 的 `cd` 只更新 `PWD` |
| 记录在**退出码传播之前** | 等价于 `; pwd > state` 而不是 `&& pwd > state`：命令失败也要记录，否则一次失败就丢目录 |
| 取值用 shell 的 `PWD`（`Shell::current_host_cwd()`） | `PWD` 是 shell 自己认账的目录（`pwd` 的输出），比进程 cwd 更贴近语义；`PWD` 不可用时才退进程 cwd |
| 无效状态**静默回退** | 目录被删/文件手改坏/权限不足都不该变成命令失败；下一次记录自动覆盖，状态自愈 |
| 写失败**不改退出码** | 记账失败不是命令失败；但要 `eprintln` 出来，否则"为什么没记住"无从排查 |
| **不做** `NIU_CWD_STATE` 环境变量形态 | `&`/coproc 会让 rubash 自旋出新的 `niu.exe`（`tests/background_spawn_args.rs` 钉死），环境变量会被这些子进程继承，后台 subshell 的 cwd 会污染状态；flag 形态天然只作用于本次调用 |
| **不做** 常驻会话进程 | 一次性执行模型的健壮性（崩溃无残留、无超时/僵尸/输出交错问题）比省一次进程创建值钱 |
| 覆盖范围：`-c` / `--encoded-command` / 脚本文件 / stdin 脚本 | 这四条都是一次性语义；`-C/--repl-command` 与交互式 REPL 只恢复、不记录（REPL 的退出路径不该被宿主开关改写） |
| 选项是**前缀组** | 扫描在遇到第一个非本组 token 时停止，因此 `niu -c 'echo $1' nm --cwd-state` 里的 `--cwd-state` 仍是位置参数，也不会被我方选项干扰 `ShellInvocation::parse` 的 bash 风格回退 |
| 相对 state 路径在启动时**钉死**为绝对路径 | 记录发生在命令跑完之后，而 `execute_script` 此时已经把进程 cwd 换成了 shell 的最终目录；不钉死就会出现"从 A 读、往 B 写" |

## 4. 改动清单（本仓库）

| 文件 | 内容 |
|---|---|
| `src/main.rs` | `CwdStateOption` + 前缀式选项扫描 + 恢复 + 记录 + `finish_one_shot()`（9 个退出点统一收口）+ `print_usage` |
| `crates/niubash-runtime/src/shell.rs` | `Shell::current_host_cwd()`（`PWD` → 进程 cwd 的兜底链） |
| `tests/cwd_state.rs` | 11 个契约用例（往返、失败仍记录、EXIT trap、子 shell 不泄漏、失效回退、裸路径、自动建目录、显式开关、位置参数、相对路径固定、缺参报错） |

记录点全部收口到 `finish_one_shot(cwd_state, &shell, code)`，避免在每个 `std::process::exit` 前手写一遍。

## 5. 验证

- `cargo test --workspace --locked`：**420 passed / 0 failed**（13 个测试目标；20 ignored 为仓库原有的 winuxcmd 命令链接门禁），其中新增 `tests/cwd_state.rs` 11 项。
- `cargo fmt --check -p niubash` 通过（顺带把分支上既有的 `base64_decodes_*` 测试格式化到 rustfmt 标准——该门禁在本次改动前就是红的）。
- 手工双调用：`niu --cwd-state F -c 'cd <dir>; pwd'` → `niu --cwd-state F -c pwd` 命中 `<dir>`；失败命令（退出码 1）仍然记录；`( cd x )` 不泄漏；`--encoded-command` 同样有效。
- 性能（20 轮，`echo hi` 重定向到 null）：不带开关 0.45s，带 `--cwd-state` 0.46s（≈0.5ms/次，含一次 JSON 原子写）；同机安装版 1.1.2 为 0.84s（本分支的 one-shot 快路径另有约 2× 收益）。

## 6. 配套改造（本仓库之外）：DSH `my-bash` 预设

`C:\Users\xianguanrong\.dsh\.agent-presets\my-bash\niubash-env.mjs`

- **state 与预设同目录、按会话工作区分组**：`state/<末两段路径>-<工作区哈希>.json`，一个工作区一个 JSON，里面按会话分条：

  ```json
  { "version": 1, "workspace": "D:\\project\\git-clone",
    "conversations": { "<会话id>": { "cwd": "D:\\project\\git-clone\\niubash", "updatedAt": … } } }
  ```

  同一项目下的所有会话共用一个文件（好读、好 diff、好删），各会话仍各有自己的目录；>14 天未动的文件在挂载时清理。
- **niu 侧的交接文件**：`%TEMP%\dsh-niubash-cwd\<会话id>.json`，每次调用前由宿主写入"本次起始目录"，命令结束后 niu 把最终目录写回，宿主再折进工作区 JSON。分组是宿主才知道的概念，所以不进 niu 的 flag。
- **会话开始播种**：`agent/created` 时写入该会话的工作区目录；另有首次工具调用时的 lazy 播种兜底。已存在的条目不覆盖，host 重启后继续原目录。
- **调用**：`start = workdir ?? 该会话条目 ?? 会话工作区`；`workdir` 存在或本会话尚无条目时先写条目（这样 niu 的恢复只会与宿主 spawn 的目录一致），argv 为 `[niu, --cwd-state, <交接文件>, -c, <cmd>]`。
- **可见性**：会话首次调用、或某次调用真的移动了目录时，结果末尾追加 `[cwd: <目录>]`；静默漂移正是"记住目录"带来的新失败模式。
- **能力探测（两步）**：挂载时先跑 `niu --help` 找 `--cwd-state`；spawn 失败或超时则**改为读二进制里是否有该字面量**（usage 文本与选项解析器同在一个二进制，所以"字面量在"与"开关可用"是同一事实）。判定为不支持时不传开关、warn、工具描述与提示词改为如实的"一次性"契约——旧二进制硬失败（`niu: unknown argument '--cwd-state'`，exit 1），所以这个闸门与它的兜底都必须可靠。判定结果会记录**用的是哪条判据**。
- **并发**：同一工作区的多个会话各自合并自己的条目并整文件 rename，last rename wins（窗口是"读→写"的微秒级，且损坏不可能，丢的条目会被该会话下一次调用重新记录）；同一会话内的并行调用仍共享一份目录，契约文案要求并行时各自显式 `workdir`。

## 7. 已知未做

- **环境变量与 shell 函数**不持久化（只有 cwd）。若将来要做，建议另开 `--env-state`，并沿用"显式开关 + 自愈回退"的形状。
- **ZCode hook**（`~/.zcode/hooks/`）本次未接：其 `niu --encoded-command` 包装只需在 argv 前加 `--cwd-state <按工作区根哈希命名>.txt`，即可拿到同样的跨调用目录，并天然按项目隔离。
- **并行调用的目录竞态**：同一 agent 两个并发调用各自 `cd`，终值 last-writer-wins；文件写是原子的，不会写坏。需要确定性时可按 owner 串行化（上游 `dsh-tool-bash-persistent` 的做法）。

## 8. 复现

```bash
cargo fmt --check -p niubash
cargo build --locked && cargo test --workspace --locked
# 双调用验收
F=D:/tmp/niu-cwd.json
target/release/niu.exe --cwd-state "$F" -c 'cd D:/project/git-clone/niubash; pwd'
target/release/niu.exe --cwd-state "$F" -c 'pwd'      # 必须输出 niubash 目录
```
