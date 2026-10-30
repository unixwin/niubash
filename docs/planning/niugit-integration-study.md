# niugit 替代 Git Bash 可行性全面调研（wt49/niugit-study）

状态：**调研完成，未修改任何产品代码，未 push**
日期：2026-10-02/03
调研对象：`unixwin/niu-git`（本地 `D:\repo\niu-git` @ d0e525e，Release v2.55.0.2）
owner 三问：① niugit 作为 release 默认依赖替代 Git Bash；② 凭据"轻松复用"；③ 成熟度是否够格。

---

## 0. 结论速览（TL;DR）

| 问题 | 结论 |
|---|---|
| niugit 是不是真 git | **是**。git-for-windows v2.55.0.windows.2 原版 C 源码，MSVC+CMake+vcpkg 编译（`scripts/build.py`），不是 libgit2 绑定、不是包装器。输出与官方 GfW git **byte-identical**（实测 status/log/branch/remote/ls-files 全同） |
| 凭据能否复用 | **分层成立**。用户级凭据"轻松复用"实证通过（~/.gitconfig 共享、gh CLI 绝对路径 helper、GCM 同二进制即同 Windows Credential Manager 库）；但 **系统级 `credential.helper=manager` 对 niugit 不可见**（在 GfW 自己的系统 gitconfig 里），默认装完会静默降级为终端提示——这是必须产品化解决的坑 |
| 成熟度够不够默认依赖 | **接近但未到**。核心语义 98% 真实通过率、20MB、https 端到端全通；但缺 ARM64、基线落后上游 3 个补丁版（含 wincred CVE 修复）、bundle 零 credential helper、Phase 2 sh 承接验收未做 |
| 建议 | **部分替代现在就可行**（wpm 双条目并存 + 安装器推荐 niugit）；**完全替代设 5 条硬门槛**（见 §5），门槛满足前不动 `git`（MinGit）默认条目 |

---

## 1. niugit 是什么（源码级）

### 1.1 仓库与构建

- GitHub `unixwin/niu-git`（public，2026-09-16 建仓，patch-only：源码永不入库，CI 拉 git-for-windows tag 打 `patches/`）
- 本地 `D:\repo\niu-git`：`scripts/build.py`（231 行）拉取 `v2.55.0.windows.2` tarball → `cmake -S src/contrib/buildsystems -DUSE_VCPKG=OFF -DSKIP_DASHED_BUILT_INS=ON -DBUILD_TESTING=OFF`（build.py:82-92）→ vcpkg classic 安装（`D:/vcpkg/installed/x64-windows`，zlib+curl(schannel)+iconv 三个动态 DLL）
- **确认纯 native MSVC**：产物 `dist/mingw64/bin/git.exe` 无 msys-2.0.dll 依赖；DLL 仅 iconv-2/zlib1/libcurl（build.py:181-186），TLS 走 schannel（系统 crypt32/bcrypt，零 openssl）
- 唯一上游补丁 `patches/0001-http-tolerate-stale-sslBackend...patch`：单后端构建遇到残留 `http.sslBackend=openssl`（GfW 安装器写的，很常见）时 warn+回退而非 die——本机实证该补丁**是刚需**（用户 gitconfig 里就有 openssl）
- 入口：`scripts/entry/launch-git.c` 静态 /MT 启动器，pin `GIT_EXEC_PATH`/`GIT_TEMPLATE_DIR`/PATH 后转发 `mingw64/bin/git.exe`（wpm shim 布局兼容）
- 产物 20.2MB zip（MinGit 38.9MB）；`wpm/niugit.json`（schema 1, layout shim）与 **GitHub Release v2.55.0.2 资产已存在且 sha256 三方一致**（index == release digest == 本地 zip == `fc293ad2...`），下载 13 次
- wpm-source 官方索引（`D:\repo\wpm-source\index.json`，281 包）：`git`=MinGit 2.55.0.5（x64+arm64）、`niugit`=2.55.0.2（**x64 only**）双条目并存
- 砍掉的能力（计划 §3 决策）：NO_PERL（send-email/svn 不可用，实测 `git svn` → "not a git command"）、NO_TCLTK（无 gitk/gui）、NO_EXPAT、NO_GETTEXT、无 PCRE2（git grep -P 丢失）

### 1.2 sh 依赖面（与 niubash 的共生设计）

git 需要跑 sh 的部分（hooks、`git-mergetool`/`bisect`/`filter-branch` 等 16 个脚本）由 PATH 上的 `sh` 承接。实测：pre-commit hook 经 PATH 的 sh 执行成功（本机落到 Git Bash 的 sh，`PRE-COMIT-RAN-VIA-MINGW64-sh`）。**注意：Phase 2（niubash sh 下 bisect/mergetool 全流程验收）尚未执行**——这正是 rubash POSIX 覆盖的验收驱动力，还没兑现。

---

## 2. 凭据复用调研（owner 关键问题）

### 2.1 git 凭据机制（源码依据）

凭据 helper 解析在 `src/credential.c` `credential_do()`（v2.55.0 树，~490-510 行）：

```c
if (helper[0] == '!')          strbuf_addstr(&cmd, helper + 1);      // shell 片段，经 sh
else if (is_absolute_path(helper)) strbuf_addstr(&cmd, helper);      // 绝对路径直跑
else                            strbuf_addf(&cmd, "git credential-%s", helper); // dashed 外部命令
```

dashed 形式经 git 子命令分发：先查本 git 的 exec-path（`mingw64/libexec/git-core`），再查 PATH。配置读取顺序：本 git 系统配置（RUNTIME_PREFIX 定位到 **niugit 自己的根** `etc/gitconfig`）→ 用户 `~/.gitconfig` → 仓库 `.git/config`。**前两层与 GfW git 不同源（系统层）、同源（用户层）**。

Windows 上典型凭据存储：
- GCM（`git-credential-manager`，MIT 协议独立产品）：读写 **Windows Credential Manager**（`cmdkey` 域内 `git:https://<host>` 条目）
- `wincred`：同走 Credential Manager（上游 v2.55.0.windows.3 修了它的 heap overflow CVE，GHSA-rxqw-wxqg-g7hw）
- `store`：明文 `~/.git-credentials`
- gh CLI 模式：`!'...gh.exe' auth git-credential`（绝对路径 shell 片段），token 存自己的 keyring
- SSH：`~/.ssh/`（id_*、config、known_hosts），git spawn PATH 上的 `ssh`（`run-command.c`，GIT_TRACE 实证）

### 2.2 实测矩阵（本机，全部可复现探针在 §6）

本机凭据现状：GfW 2.55.0.windows.3 完整安装；系统 gitconfig 有 `credential.helper=manager`（GCM 2.9.0）；用户 `~/.gitconfig` 有 github.com 走 gh CLI 绝对路径 helper + `http.sslBackend=openssl` + 代理；`~/.ssh/id_ed25519` 存在（未注册 GitHub）；System32 OpenSSH 在 PATH。

| # | 场景 | 结果 |
|---|---|---|
| 1 | niugit `credential fill` github.com（用户级 gh helper） | **成功**取得真实凭据（经共享 ~/.gitconfig + 绝对路径 gh.exe） |
| 2 | niugit `clone` 私有仓库 unixwin/demo-repository（https+代理） | **成功**，log/status 全正常；`push --dry-run` 认证+协商 **成功** |
| 3 | niugit `credential fill` 非 github host（example.com） | **无任何 helper**（`config --get-all credential.helper` 为空）→ 终端提示被禁时报 fatal。GfW 同场景会弹 GCM——差异根源：`manager` 配在 GfW 的系统 gitconfig，niugit 不读 |
| 4 | niugit `-c credential.helper=manager`（GCM 在 PATH 时） | **成功调起 GCM 2.9.0**（`git credential-manager --version` 直证 dashed 分发链路；credential fill 到达 GCM 并执行非交互报错，即真实到达） |
| 5 | 模拟"卸载 Git Bash"（PATH 仅 system32+WINDOWS） | 用户级 gh 绝对路径 helper **仍成功**取得凭据（helper 是绝对路径，不依赖 PATH）；但见 §2.4 崩溃注记 |
| 6 | ssh 传输 | niugit spawn 裸 `ssh`（GIT_TRACE 证据）；System32 OpenSSH 连通 github（host key 落 known_hosts；publickey 认证失败是本机 key 未注册，非 niugit 问题）；`~/.ssh` 与 ssh 实现共享 |

### 2.3 凭据复用结论

**"复用用户的凭据"分两条明确成立的路径 + 一条必须产品化的坑：**

1. **用户级凭据：天然复用，实证成立（零配置）。** `~/.gitconfig` 是所有 git 实现共享的；gh CLI / 绝对路径 / `store` 明文文件全部原样工作。凡是用 gh 的用户（本机即此配置）换 niugit 凭据无缝。
2. **GCM/Windows Credential Manager：同二进制即同库，路径成立但需可达性。** GCM 把凭据存 Windows Credential Manager，凭据库与"哪个 git"无关——只要 niugit 能调起**同一个** `git-credential-manager.exe`（PATH 上可达即可，实测 v2.9.0 可调起），存量凭据全部直接复用。可达性三个方案：装 GCM 独立发行版（加 PATH）；bundle GCM 进 niugit 包（MIT 可再分发，代价 ~+10-15MB，需要 owner 拍板体积）；或保留 Git for Windows 安装的 GCM（不可靠，目标就是摆脱它）。
3. **坑：系统级 `credential.helper=manager` 静默丢失。** GfW 安装器把 manager 写进 `C:\Program Files\Git\etc\gitconfig`（系统层）。niugit 有自己的根（etc/gitconfig 仅 `http.sslBackend=schannel`），**不读 GfW 的系统层** → 大量"装了 Git for Windows 一直没输过密码"的用户换 niugit 后第一次 clone 会被终端提示要密码（甚至误以为凭据丢了）。**产品对策**：niugit 的 `etc/gitconfig` 写入默认 `credential.helper=manager`（GCM 不可达时 git 自动落到终端提示，行为不劣化）+ 安装器/wizard 首跑做一次 GCM 可达性检测并给出一条 `wpm install` 或提示文案。计划文档 §8 写的"`git-credential-wincred`（构建自带产出）**未兑现**——dist 里 grep 不到任何 credential helper（见 §3 缺口 #3）。

### 2.4 附带发现：helper spawn 失败时的 0xC0000005（非 niugit 独有）

最小 PATH（无 sh、helper 不可达）下 `credential fill` 崩溃（rc=-1073741819），干净 cmd.exe 环境复现；**官方 GfW git 同场景逐字节同样崩溃**（4 组对照全部同崩同成：sh 在 PATH 且 helper 可达 → 两边都成功）。定性：上游 git-for-windows 共有行为/本无头会话工件，**不是 niugit 回归**；目标部署形态（niubash 在 PATH 提供 sh）恰是工作正常的那一支。记录在案供上游跟踪，不阻塞本决策。

---

## 3. 成熟度评估（"他有没有资格"）

### 3.1 测试基线（`docs/test-baseline.md` + `testlog-run10/SUMMARY.md`）

- **testrun6（2026-09-17，干净全量 t[01]\* 150 文件，JOBS=4）**：ok=9540 / notok=355，其中 162 是上游 `# TODO known breakage` → **真实失败 193 ≈ 98.0% 通过率**。已知失败家族：promisor/sparse/submodule/filter/hook-stdio 等 8 类（均有 triage 记录）
- **testrun10（2026-09-26，重启后 36 文件子集 JOBS=2）**：并行 aggregate 158 notok 中，单跑复核后**真实失败仅 3**（t0602 symlink-symref fsck ×2、t1461 `%(raw) --perl` ×1），全落已知候选家族，无新家族；~141 为负载敏感环境工件（test-tool DLL 解析瞬时失败，非产品缺陷）
- 9/17-9/18 的 ref 丢失"机器异常"已用**官方 GfW 2.55.0.windows.3 复现** → 判定非 niugit 缺陷，且重启后探针全绿
- harness 侧已修 3 个（t-perl-shim 引号、POSIX 路径、BASH_ENV）——测试工程是认真的
- **WinuxCmd differential 套件无 niugit 覆盖**（它是独立 git 发行，验收集就是上游 t/ 套件，职责上正确，但意味着 wpm 侧没有自己的 smoke 门禁）

### 3.2 基本面实测（本机，dist 直跑）

子命令矩阵全过：init/add/commit/log/status/branch/remote/fetch(--dry-run)/stash/tag/reflog/worktree/config/describe/clean/blame/show/diff/rebase(usage 同 GfW)。https 端到端 clone/ls-remote/push--dry-run 全通。输出格式与 GfW 逐字节一致（含 `--decorate`、porcelain 分支头）。

### 3.3 版本滞后与缺口清单（默认依赖的硬账）

| # | 缺口 | 严重度 | 说明 |
|---|---|---|---|
| 1 | **无 ARM64 产物** | 阻断双架构默认 | wpm `git`(MinGit) 有 arm64；build.py `--arch arm64` 代码在但 VS 组件未装。niubash 是双架构发布线 |
| 2 | **基线落后 3 个补丁版**（windows.2 vs .5） | 高 | .3 修 `git-credential-wincred` heap overflow（GHSA-rxqw-wxqg-g7hw）。niugit 不带 wincred 故不直接中招，但安全跟进节奏是默认依赖的信誉指标；.4/.5 主要是 MSYS2 侧（与 niugit 无关） |
| 3 | **bundle 零 credential helper** | 高（凭据体验） | dist 无 git-credential-store/wincred/manager 任何之一；计划 §8 的 wincred 承诺未兑现。若 bundle wincred 必须先把基线升到 ≥.3 |
| 4 | **Phase 2 sh 承接验收未做** | 高（核心卖点未验证） | bisect/mergetool/filter-branch/16 脚本在 niubash sh 下的全流程验收是"shell 即平台"叙事的验收驱动力，至今只验证过 Git Bash 的 sh |
| 5 | `--version` 无发行标识 | 低 | 打印 `git version 2.55.0`（无 `.windows.2`/品牌后缀）；计划 §8.1 想要 `2.55.0.windows.N.niubash` 串未实现；版本探测工具会误判 |
| 6 | 全量干净基线过期 | 中 | testrun6 是 9/17 的（150 文件）；其后落了 3 个 harness 修复 + 机器恢复，需重跑一次全量确认 98% 仍成立 |
| 7 | `git svn`/`send-email` 永久缺失（NO_PERL） | 低（已决策） | 文档化即可 |
| 8 | 无 wpm 侧 smoke 门禁 | 中 | 建议给 wpm 索引加最小 clone/commit/credential 探针，随发版跑 |

### 3.4 niubash/oh-my-niu/rubash 侧依赖点（换 git 实现的爆炸半径）

- niubash 运行时调 git 只有一处模式：`Command::new("git")` 纯 PATH 发现（`crates/niubash-runtime/src/plugins/sources.rs:531,560` git clone/pull；错误文案"is git.exe on PATH?"）——**无路径硬编码、无 Git-Bash 专属发现逻辑、无输出格式解析耦合**（git prompt 内建已退役，改由主题负责）
- oh-my-niu git 插件（`lib/git.niu`）只用 `branch --show-current`/`rev-parse --short HEAD`/`rev-parse --is-inside-work-tree`/`diff --quiet`/`diff --cached --quiet`/`ls-files --others --exclude-standard`——全部实测 niugit 输出与 GfW 一致
- rubash 引擎对 git.exe 零依赖（grep 仅见注释与 MSYS 路径转换测试）
- setup_wizard 已内建 niu-git 引导（`wpm install niugit`，Windows-only 编译门控，setup_wizard.rs:158-176, 1040+），并有探测 PATH git 后"现有 git 不受影响"的文案——**产品接线已经就位**
- niubash release workflow 只用 git 克隆 rubash 源码，不捆绑 git——"替代 Git Bash 进 release"落地形态就是：wpm 索引默认项/安装器组件指向 niugit

---

## 4. 替代建议

### 建议：分两步走，现在部分替代，门槛后完全替代

**立即可做（低风险）**：
1. wpm 双条目维持现状（`git`=MinGit 保持默认，`niugit` 并存）——现状即此，无需改动
2. niubash setup_wizard 把 niugit 选项的推荐位次提升（代码已支持，纯文案/排序改动）
3. niugit `etc/gitconfig` 增加 `[credential] helper = manager` 默认值（GCM 不可达时行为不劣化），消除 §2.3 的坑 3

**完全替代（升为 release 默认依赖、动 `git` 条目）的 5 条硬门槛**：
1. ARM64 产物并入 Release 与 wpm 条目（对齐 MinGit 双架构）
2. 基线升级到 ≥ v2.55.0.windows.5（安全跟进信誉；顺带解决 #5 版本串，加发行标识后缀）
3. 凭据方案定案：bundle GCM 或（基线升级后）bundle wincred，并附安装器 GCM 可达性检测
4. Phase 2 sh 承接验收完成（bisect/mergetool/hooks 在 niubash sh 下全绿）——这是产品叙事的兑现点
5. 重跑一次全量 t[01]\* 干净基线（≥ testrun6 口径）+ wpm 侧最小 smoke 门禁建立

**不建议**：直接把 niugit 设为唯一默认并撤掉 MinGit 条目——ARM64 缺失会让 ARM 用户直接无 git，凭据静默降级会伤"开箱即用"口碑。

**回退路线**（计划 §10 已定）：任何时候退回 MinGit（wpm `git` 条目），集成层完全一致，无沉没成本。

---

## 5. 与"Git Bash"的关系澄清

owner 语境的"替代 Git Bash"实际是三件事的解绑，niugit 只覆盖其一：
- **git 本体** → niugit（本调研，已实证可替代）
- **sh/ssh** → sh 由 niubash/rubash 自任（Phase 2 验收未完）；ssh 由 System32 OpenSSH 承接（Win10+ 自带，实测连通；计划 §3 同判断）
- **MSYS2 运行时/交互 Bash** → 本来就不进 niubash 发行

---

## 6. 探针与原始输出（可复现）

工作目录 `/tmp/niugit-probe/`（脚本 A/B/cred-input/segtest*.cmd 保留）。凭据输出中的 token 已脱敏（`password=<REDACTED-OK>`），未落盘任何明文凭据。

```text
# P1 用户级凭据复用（成功）
$ printf 'protocol=https\nhost=github.com\n\n' | D:/repo/niu-git/dist/git.exe credential fill
protocol=https
host=github.com
username=caomengxuan666
password=<REDACTED-OK>

# P2 私有仓库端到端（成功，含 schannel 回退警告=补丁#1 生效）
$ D:/repo/niu-git/dist/git.exe clone https://github.com/unixwin/demo-repository.git
warning: Ignoring unavailable SSL backend 'openssl'; using 'schannel' instead
$ git push --dry-run origin main → Everything up-to-date

# P3 系统级 helper 不可见（坑）
$ D:/repo/niu-git/dist/git.exe config --show-origin --get-all credential.helper
(空, rc=1)   # GfW 同命令 → file:C:/Program Files/Git/etc/gitconfig manager

# P4 GCM 经 PATH 复用（成功调起）
$ D:/repo/niu-git/dist/git.exe credential-manager --version
2.9.0+194ba290ce533465310d50f811684ab180536ae7

# P5 输出格式逐字节一致
diff <(niugit status -sb)    <(gfw status -sb)    → IDENTICAL
diff <(niugit log --decorate) <(gfw log --decorate) → IDENTICAL
(branch -a -v / remote -v / ls-files 同)

# P6 hook 经 PATH sh 执行（成功，本机为 MINGW64 sh）
PRE-COMIT-RAN-VIA-MINGW64-sh

# P7 ssh 分发链路（GIT_TRACE）
trace: run_command: ssh -o SendEnv=GIT_PROTOCOL git@github.com 'git-upload-pack ...'

# P8 模拟卸载 Git Bash（PATH=system32+WINDOWS）：
#   gh 绝对路径 helper → 仍成功取得凭据（P1 输出同）
#   helper 不可达/需 sh → niugit 与官方 GfW 同场景同样 0xC0000005（cmd.exe 干净环境复核）

# P9 版本与完整性
niugit: git version 2.55.0（无 .windows 后缀）
sha256(本地zip) = sha256(release asset) = wpm index = fc293ad2daed66da...e356
```

## 7. 引用

- 构建/产物：`D:\repo\niu-git\scripts\build.py`（configure 82-92、DLL 181-186）、`scripts/entry/launch-git.c`、`dist/etc/gitconfig`、`wpm/niugit.json`
- 计划与基线：`D:\repo\niubash\docs\planning\niubash-git.md`（§3 依赖边界、§8 集成、§10 阶段）、`D:\repo\niu-git\docs\test-baseline.md`、`D:\repo\niu-git\testlog-run10\SUMMARY.md`
- 凭据语义：`D:\repo\niu-git\src\credential.c` `credential_do()`（helper 三分支）、`run_credential_helper()`（use_shell）
- 生态接线：`D:\repo\niubash\crates\niubash-runtime\src\setup_wizard.rs`（NIUGIT_INSTALL_COMMAND）、`plugins/sources.rs:531`（Command::new("git")）、`D:\repo\oh-my-niu\lib\git.niu`、`D:\repo\wpm-source\index.json`（git/niugit 条目）
- 上游：git-for-windows v2.55.0.windows.3 release notes（wincred CVE GHSA-rxqw-wxqg-g7hw）、windows.5（MSYS2 runtime 更新）
