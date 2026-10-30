# oh-my-niu 生态集成与 setup 体验升级 — 设计文档

状态：设计稿（未实现、未提交）
日期：2026-09-26

> **后置裁定（2026-10-28，niubash#647，用户裁定）**：本文的 **trust 体系已全量移除**——
> §12 的信任分级（checksum 档 / local-sign 签名档）、`niu plugin trust` / `source sign` /
> `source verify` 动词、#297 漂移检测与 drifted-sources 账本、#298 first_fetch_verified、
> registry 的哈希/签名字段均已从产品中删除。理由：无行业先例、属多此一举；安装即普通
> git clone + 显式 enable 激活（bash-it 模型）。`--checksum` 仅保留为安装时对外部给定
> 值的钉验（install-time pin），不再落盘为信任状态。§12 的下载/注册表/回滚协议部分
> （12.1 schema 纪律、12.2 两道闸中的 fetch 闸、12.4）继续有效，registry schema 现为
> `@0.4.0`。本文其余 trust 表述按历史设计记录保留，不再代表现行行为。
输入：`crates/niubash-runtime` 现状代码；`D:/repo/rubash/target-ecosys/REPORT.md`（第一轮生态验收）；`D:/repo/rubash/target-ecosys2/REPORT.md`（577 文件 conformance 全扫 + 117 插件冒烟 + 冲突矩阵）。

## 0. 总原则："别人的更好，我们的作为保底"

外部生态（oh-my-bash 主题/插件、bash-completion）是**主选**；niubash 自带的
原生内容（原生 TOML 主题、原生 CommandDef 补全、原生 prompt）是**保底默认**，
只在外部生态缺失或不可用时回落。落到产品语义上就是三条铁律：

1. **优先级永远外部在前**：bundle 里外部资产的名字排在原生资产前面；同名冲突
   时外部胜出（原生仍可通过显式前缀 `native:<name>` 指定）。
2. **保底链永远可用**：外部资产缺席（未装 bundle、引擎缺陷未修、脚本 source
   失败）时，静默回落到原生路径，Tab/提示符不能因适配器故障而坏掉。回落必须
   显式告知（doctor / setup 摘要里注明"当前使用保底层"）。
3. **失败可回滚**：setup 每一步的写入（rc、bundle 版本、适配器安装）都记入
   setup journal，Apply 完成界面打印逐条 undo 命令；bundle 回滚复用现有
   `niu plugin rollback` 通道。

## 1. 现状盘点（file:line 锚点）

### 1.1 插件/bundle 机制（`crates/niubash-runtime/src/plugins/mod.rs`）

| 机制 | 位置 | 说明 |
|---|---|---|
| 官方 bundle 名 | `mod.rs:16-23` | `oh-my-niu`，legacy 名兼容 |
| 版本/协议常量 | `mod.rs:72-78` | `PLUGIN_INDEX_SCHEMA = niubash:plugin-index@0.1.0`、`PLUGIN_API_VERSION`、签名策略 `unsupported` |
| bundle 清单/布局 | `mod.rs:361-422` | `BundleToml` + `BundleLayoutToml`：`packs/aliases/completions/prompts/keybindings/themes` 六个逻辑目录，均可被 bundle.toml 覆盖 |
| inventory 解析链 | `mod.rs:603-647` | env 覆盖 `NIU_PLUGIN_BUNDLE_PATH` → plugin lock（`~/.niubash/plugin-lock.toml`）→ 应用旁 `bundles/` → 编译期 fallback |
| 未信任外部 bundle 跳过 | `mod.rs:652-654` | `skip_untrusted_external_bundle` |
| 外部 bundle 激活 | `mod.rs:659-694` | `activate_external_bundle`（写 lock，保留 `previous_path`） |
| 框架插件（`plugins/<name>/plugin.toml`） | `mod.rs:779-858` | `FrameworkPluginToml`，entry 必须是 plugin 本地相对路径（`mod.rs:859-876`） |
| 加载排序/默认 | `mod.rs:903-920` | `framework_plugin_sort_key`：prompt-core(0)→git(10)→工具(20/30)→hints(40)→theme-minimal(50)→theme-*(60) |
| 编译期兜底 packs | `mod.rs:921-1088` | git/docker/kubectl/npm/zoxide/…/prompts/themes |
| bundle 安装（staging + sha256 + lock + previous） | `mod.rs:2327-2425` | `apply_plugin_bundle_update_from_path` |
| bundle 回滚 | `mod.rs:2426-2462` | `apply_plugin_bundle_rollback` |
| 安装校验（schema/api/pack 契约/index 对账） | `mod.rs:2463-2681` | `validate_bundle_inventory_for_update` 等 |
| source pack 契约 | `mod.rs:2745-2792` | **entry 只允许 `.niu`**（规划期曾拟 `.winux` 扩展名，代码从未落地），须声明 `shell:source` 权限 |
| 补全资产加载 | `mod.rs:3002-3055` | `plugin_completion_defs`：bundle TOML → 编译兜底（`git_completion_def` 在 `mod.rs:3057`） |
| 主题目录/单主题 | `mod.rs:3172-3318` | `plugin_theme_catalog`（user + bundle TOML）、`plugin_theme`、`bundle_asset_path`（`mod.rs:3346-3365`） |
| rc 启停 pack | `mod.rs:3512-3573` | `active_pack_names` / `enable_pack_in_rc` / `disable_pack_in_rc`（编辑 `NIU_PLUGINS=(…)` 行） |

外部 bundle 通道（`plugins/external.rs`）：`add_bundle`（git clone，注册
`~/.niubash/external/registry.toml`，**默认不信任**）`external.rs:115-197`；
`trust_bundle` `external.rs:200-212`；`remove_bundle` `external.rs:216-234`。
CLI 已有：`niu plugin add/trust/use/remove/rollback`（`src/main.rs:1219-1267,
1506-1511`）。

### 1.2 setup 向导（`setup_wizard.rs`，线性流程）

- 常量与预设模型：`WizardConfig` `setup_wizard.rs:158-173`、`Preset`
  `setup_wizard.rs:177-219`（bundle `presets/*.toml` 可扩展，
  `load_presets` `setup_wizard.rs:1596-1624`）。
- 现有流程 `run_wizard_inner` `setup_wizard.rs:488-870`：环境探测 → 字体
  （540-615）→ 预设三选一+custom（617-663）→ starship 引擎（666-722）→
  wpm 工具套餐（724-807）→ WT profile（809-816）→ 摘要+确认（818-830）。
- 主题选择只在 custom 流里（`custom_flow` `setup_wizard.rs:911-1121`），
  数据源是 `theme::list_available_names()`（`setup_wizard.rs:940`），预览用
  `choice_preview`（`WizardIo` `setup_wizard.rs:295-388`，底层
  `interactive_menu::interactive_choice_ex` 已支持高亮跟随预览）。
- 预览渲染 `theme_preview_line` `setup_wizard.rs:1956-1967`（原生 TOML 主题
  专用）；Nerd Font 判定是**硬编码名单** `nerd_font_theme`
  `setup_wizard.rs:1969-1983`。
- rc 生成 `generate_rc` `setup_wizard.rs:1708-1882`：写
  `NIU_THEME/NIU_THEME_PLUGIN/NIU_PLUGINS`（1824-1833），bundle 定位与
  source（1851-1865），备份到 `~/.niubash/backups`（`write_primary_rc`
  `setup_wizard.rs:1918-1947`）。
- 非交互路径直接落 minimal 预设 `setup_wizard.rs:532-537`。

### 1.3 原生 prompt / 主题 / 补全（保底层）

- 主题解析链 `theme.rs:53-71`：user（`~/.niubash/themes/*.toml`，
  `theme.rs:103`）→ bundle（`themes_dir` TOML，经
  `plugins::plugin_theme`）→ 编译默认。`list_available_names`
  `theme.rs:76-99`。
- prompt 双通道：原生模板渲染 + **bash 兼容通道**
  （`prompt.rs:588` 注释："Bash-compatible prompt values rendered from
  PS1/PS2 after the shell has run public Bash prompt hooks such as
  PROMPT_COMMAND"）。starship full 模式已演示"外部接管 PS1、原生禁用"的先例
  （`GitBackend::StarshipFull` `setup_wizard.rs:148-154`）。
- 原生补全栈：`NiubashCompleter` 插件管线（`completion/completer.rs:62-90`
  `load_completion_dirs_with_bundle_and_definitions`：bundle defs → 注入
  defs → 用户目录）；`ExternalCompletionPlugin`
  （`completion/external.rs:269-332`）读 TOML `CommandDef` 且**自动导入
  clap/cobra 形态的 `.bash` 脚本**（`completion/bash_import.rs:29-45`，mtime
  缓存 `<cmd>.parsed.toml`）；`RuntimeCompletionPlugin`
  （`completion/runtime.rs:17-27`）跑外部补全命令。用户目录优先级：用户
  `.toml` > `.bash` 自动导入（`external.rs:334-345` 注释）。
- 引擎侧（rubash，`Cargo.toml:23` + `[patch] Cargo.toml:51-52` 指向
  `../rubash`）已修 `complete -D/-E/-I`（rubash#133 CLOSED）。

### 1.4 fonts / doctor / CLI

- `fonts.rs`：`FONT_OPTIONS` `fonts.rs:30`、`menu_labels` `fonts.rs:63`、
  `nerd_font_installed` `fonts.rs:70`、`install` `fonts.rs:76`、
  `niu font` `fonts.rs:103`。
- `doctor.rs:17-166 run_doctor`：4 项 critical（winuxcmd/command links/rc/
  **plugin bundle**，108-123）+ advisory（字体/终端/语言）。尚无补全与
  生态体检项。
- `src/main.rs:164-170`：`niu setup|font|doctor|plugin` 分发。

### 1.5 生态验收结论（设计输入）

target-ecosys（第一轮）：

- 许可证：oh-my-bash **MIT**（可官方捆绑）；bash-completion
  **GPL-2.0-or-later**（不可 vendor 进 MIT 仓库，只能适配器/用户自装）；
  fzf/starship/bash-preexec 宽松。
- bash-completion 2.18：`complete -D` 动态入口当时被 rubash#133 拒（**已修**）；
  引擎剩余断点：桶#1 引号家族（`bash_completion:188`、`ssh.bash:457`，加载
  即死）、桶#4/#5 `_filedir` 载波崩溃/泄漏、桶#6 `compgen -F` 不执行函数、
  桶#8 补全函数内 cword/cur 空。452 文件中 439 个（97.3%）已可隔离加载。
- oh-my-bash 主题：5/5 主题 PS1 全空，唯一根因 **rubash#148**（`eval
  arr=(glob)` 在 nullglob 下不做路径展开，级联杀死 `_omb_module_require`
  lib 层；issue 已建，OPEN，修复在途）+ C-2（`-i` 管道 stdin 时 `$-` 缺
  `i/s`）。PROMPT_COMMAND 钩子值双侧已一致（与 `prompt.rs:588` 通道吻合）。

target-ecosys2（全量 conformance）：

- 语料 577 文件，`-n` 全量 A 级分歧仅 2（其中 A-2：zsh 风格 `${(...)` 使
  **git-completion.bash 整体不可 source**，P0）。
- 117 插件冒烟：88 PASS-IDENTICAL / 28 FAIL-CONSISTENT（宿主框架函数缺失，
  **非引擎问题**）/ 1 环境差异。**5 个 no-op 垫片函数即可让 28 个框架插件
  转绿**：OMB 侧 `_omb_module_require`、`_omb_util_command_exists`、
  `_omb_util_print`；bash-it 侧 `cite`、`about-plugin`。
- 符号冲突全生态仅 3 个：alias `g`（bashmarks × git）、alias `d`
  （bash-it dirs × bashmarks）、func `set_xterm_title`（bash-it xterm ×
  OMB xterm）。
- 精选 bundle 候选（双侧一致、零外部依赖、无冲突）：OMB `git`、`bashmarks`、
  `golang`、`npm`、`dotnet`、`ansible`；bash-it `colors`、`base`、`proxy`、
  `pack`、`dirs`。
- 测量纪律：oh-my-bash checkout 必须 `core.autocrlf=false`（无
  .gitattributes）；WSL 基线须裁剪 PATH。

## 2. 分层设计与保底链

```
主题层    主选：bundle 内 oh-my-bash 主题（ven/oh-my-bash，MIT）
          保底：bundle 原生 TOML 主题（themes/）→ 编译默认 classic
          通道：OMB 主题走 bash 兼容 PS1/PROMPT_COMMAND（prompt.rs:588），
                原生主题走现有模板渲染

补全层    主选：bash-completion（GPL，用户侧获取，适配器探测/装/source）
                + git-completion.bash / kubectl(cobra) 等单脚本接入
          保底：原生 CommandDef（bundle completions/*.toml + 编译
                git/docker/kubectl/npm）+ path/command/variables 补全器
          入口：complete -D 动态注册（rubash#133 已修）

插件层    主选：官方 bundle 精选 OMB/bash-it 插件 + omb-compat 垫片包
          保底：编译期原生 packs（git/docker/kubectl/npm/…）
          治理：3 组符号冲突检测；外部 bundle 走 add→trust→use
```

每一层都是一个"探测 → 主选可用即用 → 不可用回落保底并告知"的状态机，
状态收敛到 doctor 与 setup 摘要。

## 3. 主题层设计

### 3.1 bundle 资产：vendor oh-my-bash

- 官方 bundle 新增目录 `ven/oh-my-bash/`：`LICENSE.md` + `lib/`（22 文件）+
  `tools/`（含 git-completion.bash，供补全层复用）+ `themes/`
  （82 个 `*.theme.sh`）。**不打包** `oh-my-bash.sh` 之外的安装器逻辑；
  加载走 niubash 自己的入口（见 3.3）。bundle 构建管线负责：固定 commit、
  LF 规范化（OMB 无 .gitattributes，CRLF 会杀死 source）、生成主题清单。
- `BundleLayoutToml`（`plugins/mod.rs:378-392`）新增
  `omb_themes_dir`（默认 `"ven/oh-my-bash/themes"`）与
  `omb_root_dir`（默认 `"ven/oh-my-bash"`）；`bundle_asset_path`
  （`mod.rs:3346-3365`）映射两个新逻辑目录。
- OMB 主题不走 `PluginExportsRecord.themes` 逐个注册成 pack（82 个 pack 会
  淹没 `niu plugin list`）。改为**目录级清单**：`ven/oh-my-bash/
  themes.index.toml`（构建期生成：name、文件、是否需要 nerd font、一句话
 描述——数据来源为验收矩阵与主题头部注释），由新函数
  `omb_theme_entries()` 一次性加载。

### 3.2 主题解析链与 OMB_THEME 映射

`theme.rs:53 by_name` 现链 user→bundle-native→default。新链：

1. `~/.niubash/themes/<name>.toml`（用户覆盖，永远最高）
2. OMB 主题（`omb_theme_entries()` 命中）→ **主选**
3. bundle 原生 TOML（现有 `plugin_theme` `mod.rs:3293`）
4. 编译默认（classic）

规则：

- `NIU_THEME=<name>` 平铺命名，OMB 与原生同名时（如 `agnoster` 两侧都有）
  **OMB 胜**（原则 1）；显式 `native:<name>` 可选原生版。
- **OMB_THEME 兼容**：rc 里出现 `OMB_THEME=<name>` 且 `NIU_THEME` 未设时，
  等价映射为 NIU_THEME（omb 侧命名即平铺名，无需改名表；仅记录
  `NIU_THEME_SOURCE=omb`）。反向：选中 OMB 主题时 loader 同时导出
  `OMB_THEME`/`OSH`/`OSH_THEME`，让依赖这些变量的 OMB 插件（bashmarks 等）
  正常工作。
- 选中 OMB 主题时 rc 不写 `NIU_THEME_PLUGIN=`（那是原生 theme-* pack 通道），
  改写 `NIU_THEME_SOURCE=omb`；摘要在主题行标注来源。

### 3.3 加载通道与 canary 保底

- OMB 主题依赖 OMB lib 层（`_omb_util_*`、`_omb_module_require`），其根因
  `eval arr=(glob)` 正是 **rubash#148**。因此加载入口设计为：bundle loader
  （`oh-my-niu.niu`）在 `NIU_THEME_SOURCE=omb` 时，设
  `OSH="$NIUBASH/ven/oh-my-bash"`、`OSH_THEME=<name>` 后 source OMB 的
  `oh-my-bash.sh` 主体（保留其官方加载路径，不自造子集）。主题随后设置
  PS1/PROMPT_COMMAND，由 bash 兼容通道（`prompt.rs:588`）渲染——验收已证
  PROMPT_COMMAND 钩子值双侧一致。
- **canary 保底**（对应原则 2）：`niu setup` 选 OMB 主题时与每次 doctor，
  在子 shell 里跑 canary（source 主题 → 检查 `PS1` 非空且
  `PROMPT_COMMAND` 含 `_omb_util_prompt_command_hook`）。canary 失败
  （#148 未修/引擎回归）则：setup 页面把该主题标灰并注明
  "等待引擎 rubash#148"，默认选择回落原生对应主题；已有 rc 则启动时告警一
  次并回落 `native:<同名或 classic>`。原生映射表（agnoster→原生 agnoster、
  robbyrussell→p10-lean、brainy→multiline classic 等）放清单文件，可运营
  期补齐。
- 原生主题标注为**保底默认**：bundle 未安装或 canary 失败时的默认链不变
  （`theme.rs` 编译默认 + bundle 原生主题）。

### 3.4 setup 主题页：全量画廊

升级 `custom_flow` 主题块（`setup_wizard.rs:939-1066`）：

- 数据源换成新函数 `theme_gallery_entries()`：user + OMB（82）+ 原生 bundle
  主题 + 编译原生，合并去重（同名 OMB 优先），每条带
  `source: user|oh-my-bash|native(保底)|compiled(保底)` 徽标、nerd-font
  标记（改硬编码名单 `nerd_font_theme` `setup_wizard.rs:1969` 为清单字段）、
  可用性（canary 结果）。
- 交互沿用 `choice_preview`（`setup_wizard.rs:324`）逐条高亮预览：
  - 原生主题：现有 `theme_preview_line`（`setup_wizard.rs:1956`）。
  - OMB 主题：新 `omb_theme_preview()` —— 子进程
    `niu -i` source 主题后捕获渲染后的 PS1 样串（走 prompt.rs:588 同一
    渲染路径的文本化输出），**按主题名做进程内缓存**（向导一次会话内每主
    题只跑一次；82 主题仅在高亮停留时逐个触发）。canary 失败的主题预览
    显示"等待 rubash#148"占位而非空白。
- 默认选中项：bundle 在位且 canary 通过 → 上次选择或 `robbyrussell`
  （OMB 最常用纯文本主题）；否则原生 `p10-classic`/`classic`（现逻辑
  `setup_wizard.rs:953-957` 保留为保底分支）。

## 4. 补全层设计（适配器架构）

### 4.1 探测 → source → 注册

新模块 `crates/niubash-runtime/src/completion/bash_completion.rs`：

- `probe() -> BashCompletionCandidate { path, origin, version }`，探测顺序：
  1. `NIU_BASH_COMPLETION_PATH`（显式覆盖，含 rc 数组形式）
  2. niu 托管安装：`~/.niubash/external/bash-completion/`（见下）
  3. 系统共享：Git-for-Windows
     `C:/Program Files/Git/usr/share/bash-completion/bash_completion`、
     MSYS2 同路径变体
  4. 包管理器落点：scoop `~scoop/apps/.../persist/...`、winget 用户包目录
     下 `bash-completion/`（存在则用；两者都不是首选安装通道）
  5. 用户 rc 声明路径
- `niu completion install`：`git clone --depth 1` 固定 tag（2.18+）到
  `~/.niubash/external/bash-completion`，登记进现有 external registry
  （复用 `plugins/external.rs` 的注册表结构；GPL 内容只落在用户目录，
  MIT 仓库零 vendor）。`niu completion uninstall/status` 对应 remove/查询。
- **source 与注册**：setup 生成的 rc 在 bundle loader 之后追加：

  ```sh
  # bash-completion (external, primary); native stays as fallback
  if [ -r "${NIU_BASH_COMPLETION_PATH:-$HOME/.niubash/external/bash-completion/bash_completion}" ]; then
    . "$_"
  fi
  ```

  bash-completion 自身经 `complete -D -F _comp_complete_load` 完成动态注册
  （引擎 rubash#133 已修，**此前提已满足**）。`complete -p` 注册数与
  `-D` 条目在 doctor 里可见。
- **激活门槛**：桶#1 引号家族（`bash_completion:188`、`ssh.bash:457`）未修
  前整文件 source 即死。适配器必须先做**引擎就绪探测**（子 shell source
  最小探针，或按引擎版本门控），不就绪时 `niu completion status` 报
  "installed, engine parser not ready（等待 rubash 引号家族修复）"，rc 不
  写 source 行，保底层继续服务。

### 4.2 引擎桥：函数补全优先、原生保底

Tab 管线（`NiubashCompleter`）新增插件 `BashScriptCompletionPlugin`，插在
`ExternalCompletionPlugin` **之前**（改 `completer.rs:62-90` 装配序）：

1. 查询引擎 `complete` 注册表（rubash 引擎 API：按命令名取
   `-F funcname/-W wordlist/-C command` 注册）；
2. 命中 `-F`：在引擎内以 COMP_WORDS/COMP_CWORD/COMP_LINE 执行函数、收集
   COMPREPLY（依赖引擎桶#6/#8 修复）；命中 `-W/-C/-G` 直接产生候选；
3. 未命中或执行失败 → 落回原生命令/外部 TOML/路径补全（**保底，原则 2**）。

bash-completion 注册过的命令（git、ssh、tar…）由引擎函数补全主选；
未注册命令继续走原生 `CommandDef`。适配器任何失败不阻断 Tab。

### 4.3 单脚本接入点（git / kubectl 等）

| 工具 | 主选路径 | 保底路径 | 引擎依赖 |
|---|---|---|---|
| git | bash-completion `completions/git` 或 bundle `ven/oh-my-bash/tools/git-completion.bash`（挂到动态加载目录） | bundle `completions/git.toml` + 编译 `git_completion_def`（`mod.rs:3057`） | A-2：zsh `${(...)` 解析（target-ecosys2 Issue 1，P0）修前 git-completion.bash 整体不可 source |
| kubectl | `kubectl completion bash` 输出入 `~/.niubash/completions/`，经引擎 `complete -F`（`__start_kubectl` 是函数型，**不走** bash_import） | 原生 simple def（`mod.rs:3052`） | 桶#6/#8 |
| clap/cobra `opts=` 形态 | 已有：`ExternalCompletionPlugin::load_dir` 自动导入 `.bash`（`bash_import.rs`） | — | 无（已工作） |
| 通用 | bash-completion 452 文件（439 已可加载） | 原生 + 路径/命令补全 | 桶#1（两个文件级断点） |

新增 CLI：`niu completion status|install|uninstall|register <cmd> [--script <path>]`
（`src/main.rs:164-170` 分发处扩展），`register` 负责把单脚本放入
`~/.niubash/completions/` 并按形态选择引擎函数路径或 bash_import 路径。

## 5. 插件层设计

### 5.1 精选生态 packs 进官方 bundle

- 目录形态沿用框架插件：`plugins/<name>/<name>.plugin.sh` + `plugin.toml`
  （`FrameworkPluginToml` `mod.rs:444-465`）。精选集（target-ecosys2 建议，
  全部 PASS-IDENTICAL、零外部依赖）：
  `omb-bashmarks`、`omb-git`、`omb-golang`、`omb-npm`、`omb-dotnet`、
  `omb-ansible`、`bit-colors`、`bit-base`、`bit-proxy`、`bit-pack`、
  `bit-dirs`。前缀消歧：`omb-`/`bit-` 避免与原生 `git`/`npm` pack 撞名。
- **契约放宽**：`validate_source_pack_contract`（`mod.rs:2745-2792`）现要求
  entry 以 `.niu` 结尾（规划期拟定的 `.winux` 从未落地）。新增允许 `.sh`（生态
  pack 原文），仍强制 `shell:source` 权限与 plugin 本地相对路径检查；同步
  更新 `framework_plugin_source_entry`（`mod.rs:859-876`）无需变（仅拼路径）
  与安装校验测试。
- `framework_plugin_sort_key`（`mod.rs:909-920`）给生态 pack 排 55（在
  theme-* 之前、工具之后），`framework_plugin_default`（`mod.rs:903`）保持
  false（保底默认仍是原生 pack，生态 pack 显式 opt-in）。
- 每个 vendor 目录带来源 LICENSE（OMB MIT / bash-it MIT）与 commit 记录，
  进 `index.toml` 的新 `[provenance]` 段（安装校验 `mod.rs:2520` 顺带对账）。

### 5.2 omb-compat 垫片包（5 函数解 28 插件）

- 新 pack `plugins/omb-compat/`，sort key 5（`prompt-core` 之后、`git` 之
  前——垫片必须先于一切生态插件加载），entry 为 `.niu` 垫片脚本，定义：

  - `_omb_module_require`（登记已加载、返回 0；不做真实加载——主题不走此
    路径，见 3.3，插件包按验收结论自包含）
  - `_omb_util_command_exists`、`_omb_util_print`
  - `cite`、`about-plugin`（bash-it 侧 no-op）

  可选追加 `_omb_util_print_prompt`、`_omb_deprecate_defun_print/put`、
  `group`、`url`（验收 err 里出现过的次级缺失，装上更稳）。
- 效果：外部 bundle 用户（`niu plugin add oh-my-bash` 原仓库）与官方精选
  packs 共享同一垫片；28 个 FAIL-CONSISTENT 插件转绿。

### 5.3 符号冲突检测（g / d / set_xterm_title）

- bundle 新增 `conflicts.toml`（进入 index 对账）：

  ```toml
  [[conflicts]]
  symbol = "alias:g"   packs = ["omb-bashmarks", "omb-git"]
  [[conflicts]]
  symbol = "alias:d"   packs = ["omb-bashmarks", "bit-dirs"]
  [[conflicts]]
  symbol = "func:set_xterm_title" packs = ["omb-xterm", "bit-xterm"]
  ```

- 检测实现放 `plugins/mod.rs`（近 `active_pack_names` `mod.rs:3512`）：
  `detect_symbol_conflicts(inventory, active) -> Vec<ConflictWarning>`。
  除 curated 表外做**通用 alias 交集**：对启用中的
  `exports.aliases=true` pack 逐个 `load_bundle_aliases_from_path`
  （`mod.rs:2945`）取别名集合，两两求交（`g`/`d` 可被通用算法自然发现；
  `set_xterm_title` 类函数冲突靠 curated 表）。
- 接入两个点：
  1. `niu plugin enable`（CLI 路径 `main.rs` → `enable_pack_in_rc`
     `mod.rs:3537`）：冲突时列出双方与符号，要求
     `--force` 或改选（bashmarks 与 git 二选一等提示）。
  2. 启动加载器：仅告警一次（后加载者胜出，bash 语义本就如此），把"谁覆
     盖了谁"写进 doctor。

### 5.4 `plugin trust` 在 setup 里的引导

- setup 插件节扫描 external registry（`external.rs:59 read_registry`）：
  - 存在 registered-but-untrusted：显示 bundle 路径与
    `plugin_permission_review_text`（`mod.rs:2163`）摘要，选项：
    [查看权限] [执行 `niu plugin trust <name>`] [跳过]。确认信任后可选
    `niu plugin use <name>`（调 `activate_external_bundle` `mod.rs:659`，
    lock 自动保留 previous_path，可 `niu plugin rollback`）。
  - 未信任前 resolver 本就跳过其 packs（`mod.rs:652`），无需新增安全语义。
- 摘要与 doctor 里呈现 trust 状态。

## 6. setup 体验：分节向导

### 6.1 结构

`run_wizard_inner`（`setup_wizard.rs:488`）从线性流程改为**分节向导**：

```
niu setup
 ├─ 0 环境探测（EnvProbe 不变，setup_wizard.rs:222-293）
 ├─ 1 预设快车道（首次运行：recommended/poweruser/minimal/custom）→ 命中预设
 │     则按预设填节默认值，仍进节目录让用户微调
 ├─ 2 节目录（可乱序进入，Esc 快进语义沿用 WizardIo setup_wizard.rs:295）
 │    ├─ 主题 Theme     （全量画廊，§3.4）
 │    ├─ 补全 Completion（§4 探测/install/单脚本接入，缺省=保底说明）
 │    ├─ 插件 Plugins   （精选 packs 多选 + 冲突检测 + trust 引导，§5）
 │    ├─ 字体 Font      （现字体节原样搬入 setup_wizard.rs:540-615）
 │    ├─ 工具 Tools     （wpm 套餐，setup_wizard.rs:724-807）
 │    └─ 终端 Terminal  （WT profile，setup_wizard.rs:809-816）
 ├─ 3 摘要 + Apply/Cancel（print_config_summary 扩展，setup_wizard.rs:1124）
 └─ 4 Apply：写 rc（备份不变）+ setup journal + 打印 undo 命令
```

- 单节重跑：`niu setup --section theme|completion|plugins|font|tools|terminal`
  （`main.rs:164` 分发加参数；等价 presets 通道 `apply_preset`
  `setup_wizard.rs:873` 的节级版本）。非交互仍走 minimal 预设
  （`setup_wizard.rs:532-537` 不变）。
- 插件节需要**多选菜单**：`interactive_menu` 现为单选；新增
  `interactive_multi_choice`（空格切换、回车确认）或退化为逐项 yn——作为
  WP-S1 的一部分。
- 每节文案进 `zh()` 表（`setup_wizard.rs:1989`）。

### 6.2 预览与信任提示

- 主题节：高亮实时预览（原生 + OMB 子进程缓存，§3.4）。
- 补全节：探测结果即时显示（找到 bash-completion：路径/版本/将注册的
  complete -D；未找到：提供 install 或"使用保底补全"两选）。
- 插件节：pack 行内显示来源徽标（oh-my-bash/bash-it/原生保底）、权限高亮
  （复用 `permission_detail` `mod.rs:2263` 的 risk 着色）、冲突即时警告。

### 6.3 回滚（setup journal）

- `write_rc_and_mark_done`（`setup_wizard.rs:1182`）扩展：Apply 时同步写
  `~/.niubash/setup-journal.toml`：节→变更记录（rc 备份路径、选择的主题/
  packs、bundle 版本 before/after（读 `PluginLockToml` `mod.rs:499`）、
  适配器安装记录、信任的 external bundle 名）。
- 完成界面打印逐条 undo：

  ```
  niu plugin rollback oh-my-niu        # bundle 回到 <prev-version>
  niu completion uninstall             # 移除 bash-completion 克隆
  niu plugin remove <external-name>    # 撤销信任/使用的外部 bundle
  cp ~/.niubash/backups/.niubashrc.<stamp>.bak ~/.niubashrc
  ```

  bundle 回滚直接复用 `apply_plugin_bundle_rollback`（`mod.rs:2426`）与
  `niu plugin rollback`（`main.rs:1506`），不新造机制。

## 7. doctor 生态体检项

`doctor.rs:17 run_doctor` 新增（advisory 为主，不提高 critical 门槛）：

| 行 | 内容 | 判定 |
|---|---|---|
| bash-completion | `probe()` 结果：路径 + 版本 + "engine ready/等待引擎修复"；未装则 "native fallback active（保底层在岗）" | advisory |
| complete -D | 子 shell `complete -p` 含 `-D` 注册条数（>0 = 动态加载入口活） | advisory |
| bundle 完整性 | 现有 plugin bundle 行（`doctor.rs:108-123`）升级：inventory source/trust_source、lock `checksum_sha256`（archive 安装）或 git HEAD（克隆安装）对账，失配告警 | 升级现有 critical |
| OMB 主题 canary | 抽当前 OMB 主题跑 canary（§3.3）；失败提示 rubash#148 状态与保底回落 | advisory |
| 符号冲突 | `detect_symbol_conflicts` 结果（§5.3） | advisory |

## 8. 工作包拆解（按依赖排序）

> 尺寸为估算的净代码量（Rust + bundle 资产管线 + 测试）。每个 WP 独立可合
> 并、可回滚；无引擎依赖的先行。

### 阶段 A（无引擎依赖，立即可做）

**WP-A1 `omb-compat` 垫片包** — ~100 行（bundle 资产 + 测试）
`plugins/omb-compat/`（新 pack）+ `mod.rs:909` sort key 5。
验收口径：target-ecosys2 阶段 3 的 28 个 FAIL-CONSISTENT 插件 source 转绿。

**WP-A2 source pack 允许 `.sh` entry + 精选生态 packs** — ~300 行
`mod.rs:2768`（扩展名允许集 + `shell:source` 仍强制）、`mod.rs:903-920`
（排序/默认）、vendor 管线（commit 固定 + LF 规范化 + LICENSE +
`index.toml` 对账 `mod.rs:2520` 扩展）。

**WP-A3 符号冲突检测** — ~200 行
新 `detect_symbol_conflicts`（`mod.rs` 近 3512）+ `conflicts.toml` 校验 +
`niu plugin enable` 告警/`--force`（`main.rs` enable 路径）。

**WP-A4 完成层探测与 CLI** — ~350 行
新 `completion/bash_completion.rs`（probe/install/uninstall/status，registry
复用 `external.rs` 结构）+ `main.rs:164-170` 分发 + rc source 行生成
（带引擎就绪门控）。激活默认关闭直至引擎就绪。

### 阶段 B（bundle/主题资产，#148 修复合入前可做，激活靠 canary 门控）

**WP-B1 vendor oh-my-bash 主题 + 布局扩展** — ~250 行（不含资产本体）
`BundleLayoutToml`（`mod.rs:378-392`）+ `omb_theme_entries()` 清单加载 +
`bundle_asset_path`（`mod.rs:3346`）映射 + `theme.rs:53` 解析链插入 OMB 层
+ `OMB_THEME` 映射与 `native:` 前缀 + canary（`niu theme canary <name>`
子命令）。
依赖：**激活**依赖 rubash#148（OPEN，在途）；vendor/清单/解析链无依赖。

**WP-B2 主题全量画廊（setup 主题节）** — ~350 行
`theme_gallery_entries()` + `omb_theme_preview()`（子进程缓存）+
`custom_flow` 主题块替换（`setup_wizard.rs:939-1066`）+ 徽标/i18n
（`zh()` `setup_wizard.rs:1989`）。原生部分即时可用；OMB 预览随 #148 点亮。

### 阶段 C（setup 重构 + doctor）

**WP-C1 分节向导** — ~500 行（重构为主）
`run_wizard_inner`（`setup_wizard.rs:488`）分节化 + 节目录菜单 +
`--section` 参数（`main.rs:164`）+ `interactive_multi_choice`。预设快车道
与非交互路径行为不变（回归保护：现有 wizard 测试
`setup_wizard.rs:2190` 起的用例全绿）。

**WP-C2 setup journal + undo** — ~200 行
`write_rc_and_mark_done`（`setup_wizard.rs:1182`）journal 写入 + 完成界面
undo 命令；复用 `apply_plugin_bundle_rollback`（`mod.rs:2426`）。

**WP-C3 doctor 生态体检** — ~200 行
`doctor.rs:17` 新行（§7 表）；复用 WP-A4/B1 的 probe/canary/冲突检测。

### 阶段 D（引擎依赖，排在引擎修复之后）

**WP-D1 引擎函数补全桥** — ~300 行
`BashScriptCompletionPlugin`（`completion/` 新文件）+ `completer.rs:62-90`
装配序 + 引擎 complete 注册表查询桥（rubash 侧 API 配合）。
依赖：引擎桶#6（compgen -F 执行函数）、桶#8（COMP_WORDS/cword）、桶#4/#5
（`_filedir` 载波）。

**WP-D2 单脚本接入点** — ~150 行
`niu completion register <cmd>` + kubectl/git 挂载（§4.3 表）。
依赖：git 主选路径依赖引擎 A-2（zsh `${(...)` 解析，target-ecosys2
Issue 1）；kubectl 依赖 WP-D1。

**WP-D3 bash-completion 激活开关** — ~80 行
WP-A4 的门控翻转：引擎桶#1（`bash_completion:188`/`ssh.bash:457` 引号
家族）修复后默认写 rc source 行，doctor 从 "not ready" 转 "ready"。

### 引擎侧（rubash 仓库）依赖清单

| issue/桶 | 状态 | 门控的 WP |
|---|---|---|
| rubash#133 `complete -D` | **CLOSED（已修）** | 前提已满足（WP-A4 设计基础） |
| rubash#148 `eval arr=(glob)` nullglob 路径展开 | **OPEN（在途）** | WP-B1 激活、WP-B2 OMB 预览 |
| 桶#1 `\'` 引号家族（bash_completion:188、ssh.bash:457） | 待修/待建 issue | WP-D3（bash-completion 整体 source） |
| 桶#4/#5 `_filedir` CTLESC 崩溃/字面泄漏 | 待修 | WP-D1 |
| 桶#6 `compgen -F` 不执行函数 | 待修 | WP-D1 |
| 桶#8 补全函数内 COMP_WORDS/cword 空 | 待修 | WP-D1 |
| A-2 zsh `${(...)` 解析（git-completion.bash P0） | 待修 | WP-D2 git 主选 |
| C-2 `-i` 时 `$-` 缺 `i/s` | 待核实（真交互路径） | WP-B1 主题门（`case $-` 判定） |

## 9. 许可证与合规

- **oh-my-bash / bash-it：MIT** — 可进官方 bundle，逐目录带 LICENSE 与
  版权头（主题文件多自带 MIT 头，vendor 管线不得剥离）。
- **bash-completion：GPL-2.0-or-later** — 只走"用户侧获取 + niu 出 MIT
  适配器"通道（`niu completion install` 克隆到用户目录）；niubash 仓库与
  官方 bundle **零 vendor**。doctor/setup 文案注明来源与许可。
- **测量纪律继承**（target-ecosys2 §固化建议 6）：vendor 管线强制
  `core.autocrlf=false` 检出 + LF 规范化（OMB 无 .gitattributes）；
  WSL 基线一律裁剪 PATH 后再比对。

## 10. 风险与未决

1. **OMB 主题加载深度耦合 #148**：不修则主题层只剩 vendor+画廊（预览灰显）
   ——按 canary 门控分阶段交付，不阻塞其余 WP。
2. **垫片 `_omb_module_require` 的 no-op 语义**：对自包含插件成立（验收结
   论），但若某插件真的依赖 lib 模块函数，垫片会静默缺函数。验收冒烟矩阵
   （88 PASS 侧）作为回归基线；`niu plugin doctor` 对 sourced pack 增加
   "加载后 undefined function 引用"抽检（后续项，不在本期 WP 内）。
3. **主题预览子进程成本**：82 主题逐个子 shell 预览，单次 ~200-500ms；靠
   高亮触发 + 会话内缓存控制在可感知范围内；若仍慢，退化为清单文件里的
   静态 PS1 摘录（构建期采集）。
4. **`niu completion install` 依赖 git**：与 `niu plugin add` 同前提
   （`external.rs:159-167` 已按 git 存在处理），无 git 时给出手工路径指引。
5. **scoop/winget 落点探测的稳定性**：非官方安装通道，路径漂移风险高；
   定位为 best-effort（探测顺序第 4 位），status 输出始终显示最终命中的
   origin。

## 9. 引擎依赖门控——状态更新（2026-09-26 深夜，captain 复核）

本文档第 8 节门控表按生态验收时点的引擎状态撰写。当日修复轮之后：

| 引擎依赖 | 设计文档时点 | 当前状态 |
|---|---|---|
| rubash#133（complete -D/-E/-I） | 已修 | ✅ 已关（e63f2d43） |
| A-2 / zsh `${(M)...}`（D2 的 git P0） | OPEN | ✅ 已关（242509e1）——git-completion.bash 3557 行双侧 source rc 0 |
| 桶#1 引号家族（D3 的激活开关） | OPEN | ✅ 已关（5aed4af9）——bash-completion 全量加载 rc 0、注册 131=机制面齐平 |
| 桶#4/#5 _filedir panic/泄漏（D1） | OPEN | ✅ 已关（37124dca） |
| 桶#6 compgen -F/COMP_*（D1） | OPEN | ✅ 已关（7c3e27c3）——git 补全候选 3=3 |
| rubash#148（eval glob，B1/B2 主题激活） | OPEN | ⏳ 修复在途（Q4 车道） |
| 桶#8/OSTYPE 政策项（验收矩阵环境项） | 环境 | 📋 升格为 rubash#154（uname/arch 内置 + RUBASH_IDENTITY 人设，待 owner 拍板默认人设） |

即：**A1–A4、C1–C3、D2、D3 的引擎依赖已全部就绪**；唯一硬门控剩 #148（B1/B2 主题激活，canary 分阶段方案不受阻）；#154 决定 452 矩阵最后 5 个环境项的归零方式。

## 10. 身份人设披露条款（owner 拍板 2026-09-26 深夜，rubash#154）

默认人设 = MSYS2 兼容（生态分流依赖），但**必须显著披露**：`niu --help` 身份节、doctor 常显当前 persona（含 uname -s/OSTYPE 生效值与切换方法）、README"身份与兼容"小节、仓库根 SKILL.md（AI 消费方同样需要知道）。补全冲突的分层事实（已核实）：引擎裸启动 `complete -p` 为空——外部脚本注册进引擎表后按 GNU 后注册者胜语义天然接管；宿主原生补全为保底层，Tab 管线维持"引擎注册表优先、原生 CommandDef 兜底、`native:<name>` 显式可达"；实现验收须含"宿主原生不反向覆盖引擎注册"的顺序测试。

## 11. 设计附录 A：外部插件管理器作为一等来源（2026-09-27，captain 布置的两个设计缺口之一）

§5.1 只定义了两条资产通道：精选 packs 进官方 bundle，以及 `niu plugin add
<git-url>` 的外部 **bundle** 通道（要求 `bundle.toml`，`external.rs:115-197`）。
本附录把外部插件**管理器**——oh-my-bash 自带 loader、bash-it、bpkg——定义为一等
来源（source）：不经过精选、不要求 bundle.toml、保留管理器原生布局，由 source
adapter 统一探测/安装/更新/卸载/枚举。

### 11.1 三通道分层（互不替代）

| 通道 | 格式要求 | 信任模型 | 谁的东西 |
|---|---|---|---|
| 官方 bundle（oh-my-niu） | `bundle.toml` + `index.toml` 逐字段对账（`mod.rs:2520-2681`） | 编译兜底/安装校验即信 | 官方精选 packs（§5.1） |
| 外部 bundle（`plugins/external.rs`） | `bundle.toml`（`external.rs:176-183` 强制） | add → trust → use | 第三方 **niubash 格式** bundle |
| 外部 source（本附录，新 `plugins/sources.rs`） | 管理器原生布局（无 bundle.toml） | add → trust → load | oh-my-bash / bash-it / bpkg **原生树** |

目录上同样三分：source 落在 `~/.niubash/sources/<adapter-id>/`（登记表
`~/.niubash/sources/registry.toml`；`NIU_PLUGIN_SOURCES_ROOT` 可覆盖，测试/便携
场景用）。不复用 `~/.niubash/external/`：那条通道的校验链要求 bundle.toml，管理
器树没有；混用会把两种信任语义和两种卸载语义搅在一起。

### 11.2 Source-adapter 接口

```rust
pub trait PluginSourceAdapter: Sync + Send {
    fn id(&self) -> &'static str;            // "oh-my-bash"
    fn display_name(&self) -> &'static str;
    fn license(&self) -> &'static str;       // 11.6 许可锚点，进 registry 与 trust 界面
    fn detect(&self, root: &Path) -> bool;   // 布局指纹，clone 后判定归属
    fn installed_version(&self, root: &Path) -> String;
    fn list_assets(&self, root: &Path) -> Vec<SourceAsset>;  // themes/plugins/aliases
    fn loader_snippet(&self, record: &SourceRecord) -> String; // 带守卫的 rc 片段
}
```

- **detect 是布局指纹**，不是文件名黑名单：oh-my-bash = 根下 `oh-my-bash.sh`
  （corpus `target-ecosys/repos/oh-my-bash` 实测布局：`oh-my-bash.sh` +
  `themes/<name>/<name>.theme.sh`（82 个全为目录形态）+ `plugins/<name>/
  <name>.plugin.sh` + `aliases/*.aliases.sh|bash`）；bash-it = `lib/composure.bash`
  + `aliases/`（WP-S2）；bpkg = `package.json`（WP-S3，仅本地安装）。
- **install/update/uninstall/list 是模块级协议函数**（`add_source` /
  `update_source` / `rollback_source` / `remove_source` / `list_sources`），
  对任何 adapter 共用：git `clone --depth 1 -c core.autocrlf=false`（OMB 无
  .gitattributes，CRLF 会杀死 source——§9 测量纪律继承）或本地目录**快照复制**
  → staging → checksum 校验 → promote → 注册 **untrusted**。本地路径源是快照不是
  引用：源目录事后漂移不影响已装版本，checksum 才有意义。
- 每个管理器每机一份（source id = adapter id）；要换发行版先 remove 再 add。

### 11.3 冲突规则：外部在前，内置保底（§0 落到 source 层）

1. source 资产名平铺进现有命名空间（theme 名 = 主题目录名，如 `agnoster`）。
2. 解析优先级沿用 §3.2 链并插入 source 层：
   `user TOML` > **external source（本附录）** > bundle 原生 > 编译默认；
   `native:<name>` 前缀跳过 source 层直取内置。
3. 注入点：`plugin_theme_catalog`（`mod.rs:3251`）在 user 条目之后、bundle 条目
   之前插入 `source = "external_source"` 条目（dedup 先到先得 → user 永远最高、
   external 压 bundle，同名时 OMB `agnoster` 赢原生 `agnoster`，与 §3.2 一致）。
   `plugin_theme`（`mod.rs:3293`）识别并剥掉 `native:` 前缀。
4. OMB 主题渲染走 bash 兼容 PS1/PROMPT_COMMAND 通道（`prompt.rs:588`，§3.3），
   **不**转换成 TOML Theme；source 层在 catalog/画廊/loader 提供条目与入口，
   渲染激活仍受 §3.3 canary 门控（依赖 rubash#148 家族 → 现 #251）。
5. §5.3 符号冲突检测的未来扩展：source 的 alias 资产集合（`aliases/*.aliases.sh`
   可静态解析 alias 名）参与两两求交；0.1.0 先只做清单展示不做拦截。

### 11.4 离线/降级行为

- `loader_snippet` 永远带存在性守卫（`if [ -r "${NIU_PLUGIN_SOURCES_ROOT:-…}/
  oh-my-bash/oh-my-bash.sh" ]`）：目录缺席 → 静默回落原生主题/packs（原则 2），
  Tab/提示符不因 source 缺席而坏。
- **untrusted source 不贡献任何资产**：`source_theme_entries()` 只枚举
  trusted && 目录在位的记录（与 `mod.rs:652` 跳过未信任 bundle 同型）。
- `niu plugin source list` 的状态机：`untrusted` / `ready` / `degraded
  (native fallback active)`（登记在册但目录缺失/校验失配）。doctor 生态体检
  （§7）加一行 source 汇总。
- 离线安装：`--path <dir>` 本地树安装，零 git 依赖（与 §10 风险 4 的 git 前提
  处理对齐）。

### 11.5 引擎证据锚（corpus waves 结论，防止"设计先于可行性"）

- **bash-it 与 oh-my-bash 整仓在 rubash 引擎的脚本面加载是干净的**：
  `D:/repo/rubash/docs/CORPUS-COVERAGE.md` §1（ecosys1–3）：117 插件冒烟
  88 PASS-IDENTICAL / 28 FAIL-CONSISTENT（宿主框架函数缺失，非引擎问题）/
  1 环境差异；5 个 no-op 垫片（§5.2 omb-compat）解全部 28 个。
- **OMB 主题 PS1 渲染 shape 字节级 pin**：rubash
  `tests/regression/fixtures/eco-omb-theme-ps1.sh` + golden（`.gnu.out`），
  lib→theme→`${PS1@P}` 全链 byte-green——source 层的加载路径有引擎侧回归保护。
- **交互面已知缺口**：oh-my-bash `-i` 加载 13/326 函数（rubash#251 OPEN，
  `eval arr=(glob)` #148 家族在交互 source 路径的残余）。因此交互激活被 §3.3
  canary 门控分阶段交付；source 的安装/枚举/信任/回滚协议面与脚本面不受阻。
- 许可：OMB / bash-it 均 MIT（target-ecosys §A）——adapter 克隆进用户目录、
  niubash 仓库零 vendor，与 bash-completion 的 GPL 处理（§9，仅用户侧获取）
  互不冲突；`license()` 进 registry 与 trust 界面。

### 11.6 CLI

```
niu plugin source list [--json]                 # 状态机 + 资产计数
niu plugin source add <id|url|path> [--ref R] [--checksum <sha256>] [--path <dir>]
niu plugin source trust <id>                    # review 摘要 + 翻信任位
niu plugin source remove <id>                   # 删树 + 删记录（信任与否无关）
niu plugin source update <id> [--ref R] [--checksum <sha256>]
niu plugin source rollback <id>                 # 回 previous（附录 B 12.4）
niu plugin source verify <id>                   # tree checksum 复算对账
```

`add` 第一参数可为 adapter id、git URL 或本地路径；URL/路径先落 staging 再
detect，识别不出时报错并列出受支持管理器。与既有 `niu plugin add`（外部
bundle）并存，动词不冲突。

## 12. 设计附录 B：plugin-index@0.1.0 信任与下载协议（2026-09-27）

现状：`PLUGIN_INDEX_SCHEMA = "niubash:plugin-index@0.1.0"`（`mod.rs:75`）只服务
官方 bundle 的 `index.toml` 对账（schema/bundle/version/bundle_api/min_niubash/
release{artifact,checksum,checksum_algorithm,checksum_required,signature}/packs
逐字段，`mod.rs:2520-2681`）；签名策略常量 `unsupported`（`mod.rs:76`）。本附录
把同一 schema 前缀扩展为**可下载 source 的目录条目 + 信任边界 + 校验 + 缓存/
回滚**协议。

**层次归属（owner 决策）**：这是 oh-my-niu/niubash 插件层自身的能力，**不是
wpm**——wpm 是 Windows-only 二进制包管理器（`setup_wizard.rs:724-807` 工具套
餐通道）。本协议只分发"source 进 shell 的资产"（主题/插件脚本/别名/补全脚
本），永不装二进制；wpm 永不写 rc source 行。跨层需求（包既带二进制又带插
件）不在 0.1.0。

### 12.1 Index 条目 schema（TOML）

```toml
schema = "niubash:plugin-index@0.1.0"

[[entries]]
name = "oh-my-bash"                    # source 名 = adapter id
version = "master@8d3f2c1"             # 人类可读 pin 描述
source-url = "https://github.com/ohmybash/oh-my-bash.git"
ref = "master"                         # tag/branch/commit
checksum = "sha256:<hex>"              # 确定性 tree checksum（12.3），必填
license = "MIT"                        # 必填
```

约束：`checksum` 非空且算法固定 sha256（沿用 `checksum_required = true` 语义，
`mod.rs:2612-2613`）；`license` 必填——GPL 资产（bash-completion）**不得**进
默认 index，只能用户侧直装（§9）；`signature` 0.1.0 仍 `unsupported`，引入
签名时 bump schema 版本号，不做静默兼容。官方 index 只收录 MIT/宽松许可且
corpus 冒烟过的管理器（首期仅 oh-my-bash）。

### 12.2 `niu plugin source add` 信任边界（两道闸）

1. **下载闸（fetch gate）**：解析 index 条目或显式 `--url/--path/--ref/
   --checksum`。CLI 打印信任边界告示（name/version/origin/license/checksum、
   "第三方 shell 代码"风险行）后执行 fetch。**fetch 不执行任何被下载代码**
   ——clone/复制到 staging 是纯数据操作，且产物注册为 untrusted，所以在无 TTY
   的脚本环境下省略交互确认不降低安全性（AGENTS.md 的确定性非交互约束保持）。
   交互式确认 UI 属于 setup 向导插件节（§5.4 同型引导），后续 WP。
2. **执行闸（trust gate）**：untrusted source 的资产零激活
   （11.4）。`niu plugin source trust <id>` 打印 review 摘要（license、路径、
   资产计数、checksum verify 结果、loader 将 source 什么）后翻信任位；翻位前
   `source_theme_entries()` 不含它，rc loader 片段不生成。

### 12.3 Checksum：确定性 tree digest

git clone 没有内建树校验和 → 定义 `tree_sha256(root)`：递归枚举（**跳过
`.git/`**），按 POSIX 相对路径排序，逐文件 `update(path \0 len \0 content)`。
安装时必算必记（registry `checksum_sha256`）；`niu plugin source verify <id>`
复算对账（与 bundle lock 的 `checksum_sha256` 对账同型，`mod.rs:2462-` /
doctor §7"bundle 完整性"行同型）。显式 `--checksum` 提供时在 staging 上强制
比对，失配 → 删 staging、原安装不动、报错（对齐 archive 安装
`mod.rs:2342-2354` 的既有语义）。verify 失配 → `niu plugin source list` 状态
`degraded`，资产退出枚举（原则 2：保底层在岗）。

### 12.4 缓存与回滚

- **缓存 = staging 目录**：`sources/<id>/.staging-*` 生命周期与 bundle update
  staging 一致（成功 promote 即删、失败即删，`mod.rs:2421-2423`）；不做常驻
  下载缓存（0.1.0 规模不需要，git 浅克隆本身即"缓存重建"）。
- **回滚 = registry `previous` 记录**：`update_source` 成功后把旧状态
  `{ref, version, checksum_sha256}` 写进 `previous`；`niu plugin source
  rollback <id>` 按 previous.ref 重新 fetch、按 previous.checksum 校验、恢复
  旧记录（复用 `apply_plugin_bundle_rollback`（`mod.rs:2426-2462`）的 lock
  previous_path 思路，但 source 是内容寻址而非路径寻址——目录原地替换，无
  previous_path 可指）。git 源浅克隆无旧对象时直接重 clone previous ref；本地
  路径源无法重建旧树 → rollback 报错并指引重装。
- **update 的信任语义**：origin URL 不变 → trusted 保留（checksum 变化经
  verify/doctor 可见）；origin 变了（换镜像/换发行）→ trusted 重置为 false，
  重新过执行闸。

### 12.5 工作包拆解

| WP | 内容 | 状态 |
|---|---|---|
| WP-S1 | source-adapter 框架 + oh-my-bash adapter + `niu plugin source` 七动词 + 信任/checksum/回滚协议 + 单测 + 二进制级集成冒烟（本附录 + §11 的实现切片） | 本文档同批落地 |
| WP-S2 | bash-it adapter（detect `lib/composure.bash`；assets `plugins/*.plugin.bash` + `aliases/*.aliases.bash`；loader 走 `bash-it.sh`，垫片复用 §5.2） | 待做 |
| WP-S3 | bpkg adapter（`package.json` 形态；仅 `--path` 本地安装，下载由 bpkg 本体负责，niu 只接管加载） | 待做 |
| WP-S4 | federated index（plugin-ecosystem-vs-zsh.md P1）：官方 index 默认 + 用户追加 git/URL index；12.1 schema 多 index 化 + `niu plugin source search` | 待做 |

### 12.6 与 §8 门控表的关系

WP-S1 不依赖任何引擎修复（安装/枚举/信任/回滚全是 Rust 层协议）；source 的
**交互激活**依赖 rubash#251（13/326 函数，#148 eval-glob 家族残余）与 §3.3
canary。即：S1 现在可做、现在可测；OMB 主题点亮时点由 #251 决定。

## 13. 设计附录 C：首跑向导 zsh 化 + 生态 surfacing（2026-09-27，owner 拍板）

owner 决策覆盖 §6.1 的首跑形态：**简单向导胜过功能齐全的向导**（参考
oh-my-zsh 安装器的"一两个问题 + 一屏'以后怎么改'即退场"）。首跑不再是分节
向导/预设问卷，而是：主题画廊一问（§0 分层：外部 source 主题在前，内置
主题隔条后置、诚实标注"保底"，Skip 恒等可选）+ 至多两三个默认关的
opt-in 问题（补全包、niu-git 一次性询问）。字体/预设菜单/starship/工具套
餐/WT 注册问题全部退出交互流程（保留 `niu font`、`niu setup --preset`、
`niu --install-wt-profile` 等显式命令）。配套新增只读的 `niu plugin
discover`（含 source 状态 ready/untrusted 与可装管理器提示），主题目录
（`plugin_theme_catalog`/`niu plugin themes`）按 §0 排序并带"built-in
fallback 保底"标记。决策与细节记录于 `docs/planning/wizard-redesign.md`；
§6 的分节设计保留为后续"单节重跑"（`niu setup --section …`）的素材。

## 14. 设计附录 D：惯例对齐（vim-plug / lazy.nvim / zinit）与实施状态（2026-10-02，wt42/niu-plugin）

### 14.1 Owner 三次裁定（2026-10-02，最终）

1. **OMB/bash-it 一等公民**："我说的是bash那一套插件系统融入我们"——现成 bash
   生态是插件系统的主体；niu 安装/启用/管理这些真实资产（主题/插件/补全），
   原生只做保底。重写兼容不在范围（历史战役已验收）。
2. **不 vendor，外部下载**："我的意思是我们这个不支持外部下载么 这样不是能绕过
   许可证吗"——oh-my-bash / bash-it / bash-completion 以 curated catalog（名字
   + 官方 URL + 简介 + 许可证标注）进产品；`niu plugin add <id>` 按用户指令
   clone。niubash 不是再分发者，GPL（bash-completion）只落在用户目录。
3. **惯例对齐 lazy.nvim/vim-plug**："参考 vim 那类插件管理器"——git-clone 安装
   正统、GitHub 简写、lockfile 钉 commit、update/sync/clean/restore 动词集。

### 14.2 落地形状（本附录的实现）

前置事实：内置插件/主题栈已退役（niubash#145，2026-09-28），WP-S1 source
协议与附录 C 向导已在 master。因此"三通道"收敛为单通道——**外部 source 即
主体**，保底 = 产品自身默认（无主题/内置补全），§0 铁律语义不变。

| 惯例 | lazy.nvim/vim-plug | niu 实现 |
|---|---|---|
| 简写安装 | `owner/repo` | `niu plugin add owner/repo` → `https://github.com/owner/repo.git`（`normalize_origin`） |
| 预置清单 | LazyVim dist specs | curated catalog（`plugins/catalog.rs`）：oh-my-bash / bash-it / bash-completion，许可证如实标注 |
| lockfile | lazy-lock.json | `sources/registry.toml`（schema `@0.2.0`）钉 `commit_sha` + `checksum_sha256` |
| restore | `:Lazy restore` | `niu plugin restore [id]`：按 lock 钉重取（fetch-by-sha，失配即拒绝） |
| update/sync | `:Lazy sync`/`:PlugUpdate` | `niu plugin update <id>` / `niu plugin sync`（全部刷到 ref tip；local 快照跳过并说明） |
| clean | `:PlugClean` | `niu plugin clean`：清 `.staging-*` 与无主孤儿树（只删 detect 命中的管理器形状目录） |
| 声明式启用清单 | plugin spec | OMB：rc 托管块里的 `plugins=(…)`/`aliases=(…)`/`completions=(…)` + `OSH_THEME`（oh-my-bash.sh 原生消费）；bash-it：`enabled/<prio>---<file>`（reloader 原生消费） |
| 触发型懒加载 | `event=`/`cmd=` | **不做**（远期项：shell 等价物 = 按命令名/补全触发延迟 source；需引擎 complete -D 运行时桥，另立 WP） |

信任协议分级（§12 落地）：`checksum`（hash 锁，add/trust/verify 全程强制，
trust 拒绝篡改树）→ `local-sign`（`niu plugin source sign <id>`，机器本地
ed25519 钥匙签树摘要；签名后任何换树 update 一律重新过执行闸，直到复审重签）。
registry schema 从 `@0.1.0` 升 `@0.2.0`（§12.1 的 bump 纪律），旧档可读、
策略字段缺省 checksum。

资产级管理（本附录核心）：`niu plugin list`（真实资产 + 启用态 + 信任档位）、
`niu plugin enable|disable <id|asset|id/asset>`（同名冲突要求限定形式）。
rc 托管块用 `# >>> niu source <id> … >>>` 标记对包裹，块外内容永不改写。

### 14.3 实施状态（wt42/niu-plugin，2026-10-02）

- ✅ 资产级一等管理（`plugins/assets.rs` + `niu plugin list/enable/disable`）
- ✅ bash-it 适配器（WP-S2：detect `bash_it.sh`+`lib/composure.bash`；
  available/enabled 目录模型；enabled 条目按 `# BASH_IT_LOAD_PRIORITY` 定序，
  副本而非符号链接——Windows 符号链接需特权，reloader 两者等价）
- ✅ bash-completion 适配器（§4.1 收编为 source：整源激活模型）
- ✅ curated catalog + GitHub 简写（`plugins/catalog.rs`）
- ✅ 信任分级 + 本地 ed25519 签名（`plugins/trust.rs`，schema 0.2.0）
- ✅ lockfile 钉 commit + `restore`/`sync`/`clean`
- ✅ setup journal + 逐条 undo（`~/.niubash/setup-journal.toml`；完成屏打印
  cp 备份/`niu plugin disable`/`niu plugin source remove` 三类命令）
- ✅ doctor 生态行（sources 汇总 + degraded 行显式"fallback active + restore
  指引"——铁律 2 的显式告知面）
- ⏳ 引擎依赖项：OMB **交互**激活（`oh-my-bash.sh` 整链 source + PS1 接管）
  仍由 rubash#251（13/326 交互函数）门控；脚本面（lib/theme/plugin 单文件
  source）已绿并有引擎级测试钉住。canary 门控（§3.3）保留为 #251 修复后的
  点亮开关，未在本批实现。
- 未做：bpkg 适配器（WP-S3）、federated index（WP-S4）、触发型懒加载（远期）。
### 14.4 裁定（2026-10-03 owner）：omb-compat 垫片取消

框架类资产（oh-my-bash/bash-it）**只经其原生 loader 启用**（OMB rc 数组 / bash-it
enabled 文件）——不存在也不允许"绕过 loader 单独 source 框架插件"的产品路径；独立单文件
插件直接 source（与 GNU bash 下手动 source 行为一致，含缺框架函数时的报错保真）。
**不重实现任何上游 lib 函数**：垫片只在为错误路径（绕 loader）造补丁时才需要，
方向作废。取而代之：启用路径 loader 保真守卫（含负向测试：无绕 loader 的启用路径）。

### 14.5 加载保真守卫落地（wt44/niu365，2026-10-03）

14.4 裁定的实施记录。wt42 车道原始发现：真实 OMB 插件（如 bashmarks）脱离 loader
单独 source 时，其废弃接口块（`_omb_deprecate_declare 20000 SDIRS …`、
`_omb_deprecate_function 20000 _echo_usage …`、`_omb_util_print` 等）依赖框架
lib 链，裸 source 报 `command not found`。按裁定**不造垫片**，落地为：

1. **审计结论**：产品内不存在任何绕过 loader 直接 source 框架插件文件的代码
   路径——`niu plugin enable` 只写框架自身的选择状态（OMB：rc 托管块的
   `plugins=(…)`/`aliases=(…)`/`completions=(…)`/`OSH_THEME`，由守卫块内
   `. "$OSH/oh-my-bash.sh"` 整链消费，框架 lib 由框架 loader 自带；bash-it：
   `enabled/<prio>---<file>` 条目；bash-completion：整源激活）；doctor /
   discover 均只读；无 preview/try 旁路通道。
2. **守卫测试**（`tests/plugin_assets.rs`）：
   `omb_framework_assets_enable_only_through_the_native_loader`——正向：单插件
   bashmarks 经 `niu plugin enable` → rc 数组 → 守卫 loader source 后，交互会话
   中其函数可用（dispatcher + save/list 往返，stderr 零 command not found）；
   负向：托管块内不得出现资产文件直 source（`plugins/bashmarks` 路径）、不得
   出现任何 `_omb_` 垫片定义。
3. **独立路径与保真**：`independent_assets_source_directly_and_framework_deps_
   stay_honest`——无 lib 依赖资产直 source 成功（明示支持的独立路径）；带框架
   依赖的文件被手动 source 时报与 GNU bash 5.3.0 相同的缺失错误（WSL 实测：
   `_omb_deprecate_declare: command not found` 等报错后继续执行、后续函数照常
   定义、rc 0）——不造兼容层，报错即保真。
4. **夹具**：fixture loader 按 corpus `oh-my-bash.sh` 消费
   `plugins=/aliases=/completions=` 数组；新增 fixture bashmarks 插件只镜像
   真实插件的无 lib 面（废弃接口块属框架 lib，仅由真实 loader 提供，刻意不复现）。

操作含义：要让 bashmarks 这类带框架依赖的插件工作，永远走
`niu plugin enable oh-my-bash/bashmarks`（或整源 enable）经 loader 加载；裸
source 一个框架插件文件得到的报错，与在 GNU bash 里做同样事情的报错一字不差
——那是正确行为，不是缺口。
### 14.5 裁定（2026-10-03 owner）：默认值是地板，不是身份（defaults-as-floor）

产品自己的 prompt/主题/补全默认值降到**最低优先级**：任何已启用的第三方框架
（oh-my-bash/bash-it/starship）都能轻松覆盖，且产品绝不反向打架。本节是可执行的
政策定义，实现于 niubash `crates/niubash-runtime/src/shell.rs`（槽位认领模型）、
`tests/defaults_floor.rs`（二进制级冲突实证）与 `tests/interactive.rs` Matrix 3b
（ConPTY 级渲染实证）。

**判定序（prompt 槽）**：

```
1. PS1 认领令牌：executor 环境里非空 PS1 —— 谁最后设置谁赢
   （用户 rc 显式赋值 = 已启用外部框架的主题 = 运行时命令，同一令牌，
   GNU parse.y:6150 prompt_again() 每次渲染现读 PS1，同语义）
2. 产品裸默认（地板）：reedline 原生模板 prompt——只在"无人认领"时渲染
```

- **认领=PS1，不是 PROMPT_COMMAND**。PROMPT_COMMAND 是 pre-prompt 钩子（引擎每
  prompt 执行，与渲染无关）；仅注册钩子的框架（OMB `history` 插件的 `history -a`、
  标题器）不得借占位 `\$ ` 顶掉地板。要拥有 prompt 的钩子自己写 PS1——starship
  的 `starship_precmd`（src/init/starship.bash:80）正是这样做的，而宿主的
  `run_precmd_hooks` 先跑钩子后同步后端，所以 starship 在**第一个** prompt 即完成
  认领。
- **释放=恢复地板**。PS1 被 unset/置空（`niu plugin disable`、主题关闭、裸
  `unset PS1`）后，下一 prompt 恢复产品地板（`floor_prompt` 字段持有配置派生的
  原生后端）。GNU 在此场景渲染空 prompt（parse.y:6150 把缺 PS1 映射为 ""）；
  niubash 的契约更强——地板就是地板（owner 裁定），且地板从不写 PS1，故不可能
  与活跃认领打架（无双重渲染：认领时 `self.prompt` 整体换为 bash 兼容通道）。
- **框架识别走正确路径**：registry/trust 态 + rc 托管块（`# >>> niu source <id>`
  标记对）是框架启停的唯一事实源；`is_foreign_git_bash_ps1`（#117）这类内容检测
  **只保留给"继承 env"这一个场景**，且只在 enter_interactive 于任何 rc 之前跑一
  次——主动安装的框架在 rc 里设置的 PS1（含恰好含 Git Bash 会话标记的文本）永不
  被丢弃（钉死测试 `framework_ps1_set_after_interactive_entry_is_never_discarded`）。
  不得为框架识别扩展内容黑名单（2026-10-03 打地鼠禁令）。

**rc 顺序契约**：

1. niu 缺省块（`NIU_PROMPT_CWD_STYLE`/`NIU_COMPLETION_STYLE` 等地板 knob）在
   **早期**；这些 knob 只作用于地板渲染，bash 兼容通道激活时不参与，天然不与外
   部框架抢槽。
2. 外部框架启用块（`# >>> niu source <id> … >>>` 标记对）在**后**（generate_rc
   置于地板 knob 之后；`niu plugin enable` 对既有 rc 追加到末尾）。后加载者胜，
   bash 语义本就如此。
3. **互不覆写**：托管块只改标记对之间的行，块外用户内容永不动（既有纪律，二进
   制级钉死于 `managed_block_lands_after_floor_config_and_user_lines`）。

**实证矩阵（全部为测试）**：

| 场景 | 证据 | 结果 |
|---|---|---|
| 真装 OMB（fixture 树，经 `niu plugin add --path`+trust+enable 全链）后 boot | `omb_theme_claims_the_prompt_slot_end_to_end` | 主题 PS1 存活进 shell，地板未覆盖 |
| OMB 认领下的 reedline 实渲染（ConPTY） | `omb_theme_claim_renders_and_floor_returns_on_release` | 渲染主题脸非地板；`unset PS1` 后地板回归；再认领再赢 |
| `niu plugin disable` 后 fresh boot | `disabling_the_source_releases_the_prompt_slot` | 主题 PS1 消失（piped `-i` 路径引擎自设 GNU 默认 PS1 `\s-\v\$ `，reedline 地板恢复由单测钉死） |
| 无人认领时的地板渲染（ConPTY） | `floor_prompt_renders_when_ps1_unclaimed` | user@host 地板脸渲染 |
| starship 认领形状（PROMPT_COMMAND 钩子写 PS1） | `starship_style_precmd_hook_claims_via_ps1_same_cycle` | 同一渲染周期完成认领 |
| 仅钩子不认领（OMB history 形状） | `hook_only_prompt_command_keeps_the_floor` | 地板保持，钩子照跑 |
| 真 OMB 仓库（82 主题全 lib 链）boot | `real_ohmybash_chain_keeps_claim_or_floor_invariant` | rc 0，认领或地板二选一成立（当前 #251 门控下=地板在岗，铁律 2） |
| #117 继承 PS1 丢弃不误伤框架 | `framework_ps1_set_after_interactive_entry_is_never_discarded` | 交互进入后设置的 PS1（含 #117 标记文本）永不丢弃 |

**字段清单纪律（2026-10-03 ground truth）**：shipped 产物中退役栈字段清零——
`.niubashrc.example` 已重写为迁移后形态（地板 knob + HOME 归一 + 托管块示例 + 用
户型），`example_rc_sells_no_retired_stack_fields` 断言
`NIU_THEME`/`NIU_THEME_PLUGIN`/`NIU_PLUGINS`/`NIU_BANNER`/`NIU_PROMPT_SYMBOL`/
`NIUBASH`/`oh-my-niu` 均不出现且文件引擎可解析（#157 闭合形式守护）。generate_rc
字段三分类：退役残留=无；活的地板配置=`NIU_PROMPT_CWD_STYLE`/`NIU_COMPLETION_STYLE`
（`prompt.rs:219`/`config.rs:174` 在读）；身份占位=无（地板从不写 PS1）。已知无害
残留：interactive 测试驱动 fixture 里的 `NIU_DISABLE_DEFAULT_PLUGINS=1`（无读点，
仅测试内部，未随产品分发）。

**字段清单扩展面（2026-10-03 wt45/floor 续作清点）**：退役栈发卖不止 example
rc 一处，用户面文档同罪，已一并清零——`README.md`（Configuration 块 + Features 三
条卖点：27 themes / 40+ official packs / plugin bundles 更新面）、
`docs/src/getting-started.md`（§6 rc 模板、§6b 主题插件章、§7 oh-my-niu bundle
章整体改写为地板+外部生态形态；§What next 死链
`oh-my-niu-bundle-plan.md`→`oh-my-niu-ecosystem.md`）、`docs/src/advanced-usage.md`
（Startup And Config、Prompt And Themes、Plugin Workflow、Third-Party
Bundles→Sources、Updating 五节；退役动词 `search`/`themes`/`info`/`review`/
`doctor`/`use` 现在硬 bail，见 `src/main.rs` run_plugin_command 退役分支）。运行时
env 出版通道分类：`NIU_PROMPT_SYMBOL` 由 `shell.rs:271` 从活配置
`shell.prompt_symbol`（TOML 语义默认 `%`）单向写入 env——引擎无读点（cf68607 已删
读端），仅供用户脚本引用地板符号，属**活地板的值出版**而非退役残留；rc 侧不设
该 knob（地板读配置不读 env，见 example rc 注释）。

**越界发现（本车道不动，报船长转归属 owner）**：`docs/src/hooks.md` 整篇描述的
hook 契约已死——注册助手（`niubash_add_*_hook`）随 oh-my-niu 框架退役，而
`config.rs load()` 只返回 `HookConfig::default()`（全空），`shell.rs` 各
`run_*_hooks` runner 从用户空间不可达（无任何运行时注册路径）；hooks.md 需 hook
子系统 owner 裁定（退役 runner 或重开注册内置命令），不宜由地板车道代笔。
`docs/src/niubash-roadmap.md` 的 "oh-my-niu is the official bundled plugin
distribution" 叙述同为历史态，属 roadmap 文档 owner 的改写面。另有一处**活的矛
盾**：发布打包 `scripts/package-release.ps1` 仍强制随包 staging oh-my-niu bundle
（`bundle.toml` 候选链，`-SkipOhMyNiubashBundle` 才跳过），`docs/src/installer.md`
与 `docs/src/architecture.md` 对此的描述与打包现状一致——但产品侧插件动词已对
bundle 体系硬 bail（niubash#145），"发布包仍在发退役栈" 需发布 lane owner 裁定
（stop staging 或恢复消费路径），不属地板车道。

### 14.6 任意插件面 + 管理器描述符化 + 声明式 spec（2026-10-03，wt46/anyplug）

Owner 裁定（本节两条输入合并落地）：

1. "可以 source 任意的插件，任意一个 bash 插件或者是管理器；注意不要破坏规则和
   打地鼠"——插件面从"三个已知管理器"开放为**任意可 source 的 bash**；
2. 声明式 spec（lazy.vim/vim-plug/zinit 家族）："添加插件应该用配置文件，不只是
   CLI"——`~/.niubash/plugins.toml` 成为唯一事实源，CLI 降级为糖。

#### 14.6.1 三层模型

| 层 | 内容 | 不变量 |
|---|---|---|
| 基础层 | 任意野生 repo/单文件/gist 风 `.bash` 经 `niu plugin add <owner/repo\|url\|path>` 安装为 **file source** | 资产枚举**诚实呈现**：递归列出全部 `*.sh`/`*.bash` 候选（相对路径即资产名，`install.sh`/tests 等仅打标签不隐藏），**不猜唯一入口**；启用=逐文件守卫 source（`. <path>`），与 GNU bash 手动 source 逐字节保真（含缺函数报错保真，14.4 裁定同族）；无候选（纯 README repo）→ add 失败并如实说明 |
| 管理层 | oh-my-bash / bash-it / bash-completion / **bpkg**（附录 A WP-S3 补齐） | catalog 只是预填简写，**不是准入白名单**——add 对任意 URL/路径开放；bpkg 按"驱动 bpkg 自己的 CLI"实现（`default_origin = None`：下载由 `bpkg install` 完成，niu 仅经 `--path` 接管已装树，指纹 `bpkg.json`/scripts-数组的 `package.json`）；管理器全部**描述符化**（14.6.2） |
| 红线 | 引擎（rubash）零插件特判；无垫片（14.4） | 引擎源码不出现插件名/管理器名硬编码（审计只读）；框架资产只经原生 loader 启用，niu 不提供任何上游 lib 函数替身 |

#### 14.6.2 描述符化（防打地鼠的正确不变量）

现状审计结论：三个适配器是**手写 struct**（`sources.rs` OhMyBashAdapter 等），
分发虽非 if-else 链（已是 trait 注册表），但新增管理器仍要写一整个 struct 的
detect/list_assets/loader_snippet——每多一个管理器就多一份手写码，正是
rubash#117 禁止的形状在管理器维度的重演。

重构：**一张静态描述符表 + 一个解释器**。`ManagerDescriptor`（数据）声明：

- 身份：id / display / license / origin（`None`=本地安装型）/ summary / install_note
- 布局指纹 `LayoutFingerprint`：`Files{all, any}`（OMB=`oh-my-bash.sh`；
  bash-it=`bash_it.sh`+`lib/composure.bash`；bash-completion=`bash_completion`）
  或 `JsonScriptsArray`（bpkg 清单，npm 的 scripts 对象形状天然排除）
- 资产枚举 `AssetPattern`：`DirNamed`（`themes/<n>/<n>.theme.sh`）与
  `Flat{dir, in_available, suffixes, kind}`（aliases/available 等）——OMB、
  bash-it、bash-completion 三家现有枚举全部可由这两条模式表达
- 加载 `LoaderSpec`：root 变量 + 入口文件 + 附加导出（OMB 的
  `NIU_THEME_SOURCE=omb` 即一行数据）
- 选择 `SelectionSpec`：`LoaderArrays` / `EnabledDir` / `WholeSource` /
  **`DirectFiles`**（新增：file source 与 bpkg 的逐文件启用）

**新增管理器 = 表里加一行数据，零解释器代码。** bpkg 即以一行落表验证该不变量。
loader 片段统一为一个模板（root 变量在存在性守卫内赋值+分隔符规范化+导出+source
入口），OMB/bash-it 的旧模板语义等价（bash-it 原来在守卫外赋值 BASH_IT；统一后
树缺失时变量不赋值，净效果相同——加载本就不发生）。

file source（野生）与 bpkg 共用 `FileSource` 形状但**每安装一个 id**（管理器是
一机一份，file source 一插件一份）：id = 显式 `--id`，否则 origin 尾段（repo/
目录名）净化。`detect_manager_adapter`（仅管理器指纹，供 clean 判孤儿树）与
`detect_adapter_for_install`（管理器→野生兜底，供 add）分开——**野生兜底绝不
进 clean 的删除判定**，否则任何含 .sh 的用户目录都会被当孤儿删掉。

#### 14.6.3 声明式 spec（`~/.niubash/plugins.toml`）

```toml
schema = "niubash:plugin-spec@0.1.0"

[[sources]]
target = "oh-my-bash"            # catalog id | owner/repo | git url | 本地路径
enable = ["git", "npm"]          # 启用资产（OMB→plugins=() 数组；bash-it→enabled/）
theme  = "agnoster"              # 可选主题（OSH_THEME / BASH_IT_THEME）

[[sources]]
target = "rcrowley/bash-preexec" # 野生 file source：GitHub 简写
enable = ["bash-preexec.sh"]     # 资产名=树内相对路径
```

字段补全（实现定稿）：`id`（管理器=管理器 id；野生/bpkg 首次 sync 由 origin
尾段派生并写回）；`kind`（适配器钉子：`niu plugin add bpkg --path <dir>` 采纳
的树在后续 sync 强制走 bpkg 指纹，不匹配即拒绝，不静默降级为野生）；`ref`
（仅首次拉取）。带 `--path`/`--url` 的 add 把显式 origin 原样存为 target——
存 catalog id 会在 sync 时重解析成官方 GitHub URL 而不是用户的本地树。

- **spec + registry 二件套**（= lazy-lock.json 同构）：spec 声明"要什么"，registry
  （schema 升 `@0.3.0`，增 `spec_enabled`/`spec_theme` 记录上次同步物化态）钉
  "装的是什么"（commit_sha + tree sha256）。事实源永远在 spec。
- **`niu plugin sync` 重定义为对账物化**（:Lazy sync）：声明未装的 → add_source
  流（fetch 闸自动走，**trust 闸永远不自动**——新装落地 untrusted 并打印
  `niu plugin trust <id>` 指引）；已装已信的 → rc 托管块由 spec **幂等再生成**；
  已装未信的 → 报"awaiting trust"；registry 里有而 spec 没声明的 → **只提示清理
  不自动删**（`niu plugin sync --prune` 才删）。旧"全部刷到 ref tip"语义移交给
  `niu plugin update`（无参=全部）。
- **合并语义（OMB 数组 vs 用户手工项）**：记 `prev` = 上次同步**spec 自身**物化
  的选择集（`spec_enabled` 只记 spec 声明过的已知资产，不含手工项——否则手工项
  一次 sync 后被吸进 prev，之后的任何 spec 变更都会把它当"spec 删掉的"清掉，这是
  实现期发现并钉死的存活语义 bug），`hand_added` = 当前块内条目 − prev。新物化集
  = `next(spec) ∪ hand_added`；prev − next（spec 里删掉的）被移出块。主题：spec
  声明则生效，未声明保持现状（手工选择不覆写）。bash-it 的 enabled/ 目录用同一
  规则。**物化幂等**：spec 不变 → 块字节不变（钉死测试）。
- **CLI 降级为糖**：`add` = 追加 spec 条目 + sync；`enable/disable <资产>` =
  改 spec 的 enable/theme + sync；`disable <source>` = 删除 spec 条目 + sync
  （托管块随之下线，树不动，tree 删除仍走 `source remove` 指引）。无 spec 时首次
  CLI 操作自动建档；存量（registry-only）状态下 sync 只提示"未声明"并给出可粘贴
  的 TOML 片段，不接管不清理。
- **首启 bootstrap**：rc 一行引导 `niu plugin sync --bootstrap`（setup 向导写入；
  `NIU_PLUGIN_BOOTSTRAP=off` 可关）。--bootstrap = 同一对账的安静形态：干净时零
  输出即退，有缺装则装（trust 仍待人工）并打一行提示；doctor 加 spec 对账行
  （reconciled / N missing / N undeclared / no spec）。

#### 14.6.4 验收口径（实证集，全部真 clone）

| 形状 | 期望 |
|---|---|
| 单文件野生插件（纯 bash） | add→trust→enable（挑文件）→引擎 source 生效；候选清单诚实（不猜入口） |
| bash-preexec | 同上；preexec/precmd 钩子变量行为与 GNU bash 手动 source 一致 |
| bpkg 包 | `bpkg install` 形态（deps/<name>）经 `--path` 接管；scripts 逐文件启用 |
| bashmarks（依赖框架 lib） | 直接 source：缺 `_omb_module_require` 报错与 GNU bash 手动 source 同错同措辞（保真，不垫片）；经 OMB loader 数组启用：正常 |
| 未知形状 repo（README+install.sh） | add 呈现候选（install.sh 带标签）绝不自动 source；纯无可 source 文件 → 明确失败 |

验收落地（2026-10-02，wt46/anyplug）：五形状全部钉在
`tests/plugin_anyplug.rs`（离线 fixture 复现各 repo 形状）+
`tests/plugin_spec_sync.rs`（幂等再生成/合并语义/CLI-spec 一致性/清理提示）。
bashmarks 负向保真为**字节级对比**：GNU bash（Git Bash）与 niubash 引擎对裸
source 输出同一行诊断（`line N: _omb_module_require: command not found`）且同
退出码（0——`.` 返回文件内最后一条命令的状态，即函数定义；这一点两壳一致，
测试把退出码也钉死）。npm 形状（package.json scripts 为对象）加 kind 钉子时
被拒（"does not look like 'bpkg'"），不带钉子时落入野生 file 层。

门禁：cargo test --workspace --locked 全绿 + fmt；同步交付 `niu plugin --help`
新动词、docs/plugins-guide.md（spec 入门/三种添加方式对照/trust 流程/排障）与
AI 速查（docs/plugins-quickref.md）；引擎树零改动（审计只读：引擎源码对插件/
管理器名仅出现在注释出处，无字符串字面量特判，2026-10-02 复核）。

### 14.7 lazystudy 三 commit 并入 anyplug 主线（2026-10-02，wt48/release161）

wt47/lazystudy 的 `4506a0b`（纯 Rust 下载驱动 ureq/flate2/tar）、`074eb88`
（合集 distros + 向导 Q2.5 + tool 通道）、`3e013a0`（`niu plugin ui` 菜单 +
用户 guide）手工并入 master。seed/研究文档已先行 cherry-pick（master
`3ac1163`/`660e507`），但 recipes.rs 当时**未接线**（无 `mod recipes` 声明，
依赖的 `entry` 字段与 tool PATH 块函数不在 master 上）——本次合并补齐接线。
两个合并裁定（均保持两边的不变量，非新增语义）：

1. **`niu plugin add <id>` 的 recipe 路由：catalog 优先**。bare word 先查
   catalog（三管理器，一机一份）：命中 → anyplug 的 spec 声明流（spec 仍是
   它们的事实源）；未命中而命中 recipe → recipe 驱动（generic file source
   带 entry / download 直装 / manager-asset 链）。recipe 通道落在 registry
   （untrusted），`niu plugin sync` 按"未声明"提示，`--prune` 才删——与
   14.6.3 的存量语义一致。recipe id 均为带前缀平名（`omb-theme-*`），
   与 `<source>/<asset>` 限定名无歧义。
2. **wt47 的 `generic` 单入口适配器并入 anyplug 开放 file source（"file"）**。
   14.6.1 的"诚实枚举、不猜唯一入口"不变量保留：树的全部 `*.sh/*.bash`
   候选照列；recipe 的 `entry` 是**数据**（recipe 声明的推荐文件），落在
   `SourceInstallRequest.entry`——staging 阶段校验存在（过期 recipe 在装时
   如实报错，即 wt47 `detect_entry` 契约、单点表达于
   `fetch_source_to_staging`），并在 next-verb 里点名
   `niu plugin enable <id>/<entry>`。`enable/disable` 路由先查 tool 注册表
   （PATH 块），否则走资产层——与 wt47 相同。

hooks.md 死契约处置（niubash#161 顺带裁定）：**改写为"活面 + 待实作"而非
删除**。理由：`run_precmd_hooks` 等并非纯存根——它们承载 §14.5 的活的
PROMPT_COMMAND 执行、PS1 认领/释放同步、PS0 渲染与标题解析（defaults-floor
测试依赖），删除 = 拆活机器。死的只是用户注册面（`niubash_add_*_hook` 随
框架退役，无运行时注册路径）。文档如实记录两件事：今天的 bash 原生钩子面
（PROMPT_COMMAND/PS0/trap/PS1 认领），与"niu 原生命名钩子待外部生态模型
owner 裁定"。

### 14.8 管理器 bundle 裁定 + wpm 学习 + 镜像管道（2026-10-03，wt49/bmirror）

Owner 四条输入（前三条 2026-10-03 布置，第四条同日追加调整）：

1. "用 bundle 的形式提供 这样就不用下载那个插件管理器本身"——插件
   管理器（含下载驱动）随产品 bundle 走；
2. "wpm 有什么可以学习的地方"；
3. "源/proxy 怎么让中国用户用的舒服 能不能复用别人的各种国内源的
   下载渠道"；
4. 追加："wpm 的源不稳定……我不知道怎么弄了反正"——**镜像源稳定性
   不可依赖，做管道不做源**：用户自己知道去哪找能用的镜像 URL，
   我们的工作是让贴进来就能用。

#### 14.8.1 裁定 1 落地：管理器=产品 bundle 的一部分（现状即目标形态）

现状审计：niubash 没有、也不需要"可下载的插件管理器"这个产物——
管理器是编译进 `niubash-runtime` 的 `plugins` 模块（sources/download/
recipes/spec/trust/mirrors/…），随 niu 安装包分发，用户零额外安装、
零额外更新通道。**下载驱动（ureq/rustls 纯 Rust，owner 裁定
2026-10-03）与管理器本体已经是 bundle 内置**，裁定与现状一致；本节
做的是把它写成规范。

与 niubash#145 退役的 bundle 的区别（防混淆）：#145 退役的是
oh-my-niu 内置主题/插件**内容栈**（bundle 里装的是插件数据）；本裁定
说的是**管理器本身**（bundle 里装的是管理代码）。前者退役、后者内置，
两层并行不悖：内容一律走外部 GitHub 生态（§14.1 裁定 2），管理面是
产品的一部分。

载体控制（owner 问"用什么样的载体控制"）——控制面三层，版本随产品：

| 层 | 载体 | 角色 |
|---|---|---|
| CLI 动词 | `niu plugin <verb>`（add/enable/sync/trust/mirror/…） | 糖 |
| 声明式 spec | `~/.niubash/plugins.toml`（§14.6.3） | 唯一事实源 |
| 锁 | `sources/registry.toml`（schema `@0.3.0`）+ `tools/registry.toml` | 装了什么（commit+checksum 钉） |

产品版本=管理器版本：不设独立版本号、不设管理器自身的更新器（自更
新走产品 `niu update` 通道，self_update.rs）。

#### 14.8.2 wpm 学习点对照（读源码 `wpm.cpp`，4231 行，WinuxCmd 0.4.0）

| wpm 机制 | 源码锚点 | niu 的采纳 |
|---|---|---|
| 四内置源（GitHub canonical / jsdelivr CDN / release-pinned / China mirror），region+priority 排序，`user_sources` 合并 | `kBuiltinIndex`（wpm.cpp:67）、`merged_sources`（:318） | **学习架构思想，不照搬实现**。wpm 必须选源是因为它拉自己的包索引（index.json 本身要选镜像）；niu 的生态源在 GitHub（owner/repo 直达），**没有"索引源"要选**，镜像纯粹是网络层优化。落地为镜像管道（14.8.3） |
| auto 源选择：3s probe 最高优先源 → `stable_partition` cn 源前置；**排序不淘汰**（每个源失败仍被依次尝试） | `try_fetch_index`（wpm.cpp:2975-3034） | 学习"probe 只重排、永不拦截"原则；niu 形态=probe 只建议不自动切（owner 追加裁定） |
| China mirror 实战：`gh.caomengxuan666.com` nginx cache proxy；artifact `urls` 数组 mirror 在前、GitHub origins 兜底 | `kBuiltinIndex` official-cn（:104-113）、`artifact_urls`（:1939） | 学习 URL 形状：prefix 式 `<mirror>/<完整 GitHub URL>` 是 ghproxy 类服务的通用形态；niu 落地为用户自填 prefix（`niu plugin mirror set <url>`），fallback 语义由"set none 一键回直连"承担 |
| 双传输栈：WinHttp 主 + `URLDownloadToFileW`(urlmon) 兜底，进度条 | `http_get` / `with_urlmon_fallback`（wpm.cpp:811-876） | **不需要**：niu 下载驱动是纯 Rust ureq/rustls，跨平台无 Win32 兜底需求 |
| `--proxy` + 归一化（剥 scheme/auth/path）+ env 链（WPM_\*_PROXY→HTTPS_PROXY→ALL_PROXY）+ NO_PROXY bypass + IE/WPAD 自动代理检测 | `normalize_proxy_server` 等（:1052-1230） | **不需要（现状够用）**：ureq 走标准 env 代理；显式 --proxy/WPAD 是 WinHttp 时代的包袱。未来用户报需求再议 |
| `--from` manifest（文件/URL → `[packages]` 批装） | `install_from_manifest`（:2770） | **已有等价物且语义更纯**：`plugins.toml` 声明式多条目 + `sync`（spec 是事实源），加 `distro` 合集（LazyVim extras 形态） |
| `--json` 机器可读输出 | 各 list 命令 | **部分已有**：`niu plugin list --json`；mirror 面单值配置、无脚本消费场景，暂不加 |
| index 本地缓存（index.json + `last_success_source`）+ artifact 缓存（sha256 键命中免下） | `load_index` / `cached_artifact_is_valid`（:1663） | **已有更强等价**：recipes.toml 编译进二进制（零网络、零陈旧）；重装一致性由 checksum 校验承担 |
| `probe_url` 轻量可达性：GET 收到响应头即断、3s、只影响排序 | `probe_url`（wpm.cpp:1516） | **采纳**：`niu plugin mirror test` 与 doctor 行（HEAD、3s；4xx/5xx 也算可达——服务器应答即证明网络路径，wpm 同义） |

#### 14.8.3 镜像管道（核心实现）

裁定链：§14.1-2"不 vendor，外部下载"（所有内容来自 GitHub）+
"中国用户用的舒服"（复用国内镜像渠道）+"源不稳定"（管道化）。

**模型**——`~/.niubash/mirrors.toml`（`NIU_MIRRORS` 可覆盖，测试/
便携场景；schema `niubash:mirrors@0.1.0`）：

```toml
schema = "niubash:mirrors@0.1.0"
active = "custom"          # "none"（默认直连）| "custom"

[github]
prefix = "https://your-mirror.example.com/"     # 前缀式（ghproxy 类）：
                                                # 请求发 <prefix><完整 GitHub URL>
git_instead_of = "https://your-git-mirror/"     # git 专用（gitclone 类）：
                                                # git -c url.<base>.insteadOf=https://github.com/

[github.releases]
prefix = "https://your-release-mirror/"         # release 资产专用前缀（优先于 prefix）
```

**CLI**：`niu plugin mirror list | set <url|none> | test`。`set <url>`
校验 https/http 前缀并归一化尾部斜杠（"贴进来就能用"）；`set none`
回直连但保留 `[github]` 段（一行翻回）。社区镜像示例只以**注释**存在
于生成的配置文件（"UNSUPPORTED and may disappear at any time —
verify before use"），预置源数量为零。

**管道接线（重写全部发生在传输层）**：

| 通道 | 接线点 | 记录面（不动） |
|---|---|---|
| 工具/配方下载 | `plugins/download.rs http_get_bytes` 内部 rewrite | `ToolRecord.url` = canonical |
| git clone / fetch-by-sha | `plugins/sources.rs git_clone_arg_list` + `git_fetch_commit_to` 注入 `-c url.<base>.insteadOf=…` | origin 参数与 registry = canonical |
| 字体 | `fonts.rs download()`（curl 通道）rewrite | `NERD_FONTS_RELEASE` 常量 = canonical |
| 自更新 | `self_update.rs download_asset` rewrite | `asset.browser_download_url` = canonical |
| 探测 | doctor 行（active=none 时 probe GitHub 3s）+ `mirror test` | —— |

**铁律不变量**：

1. **重写只在传输层**：spec/registry/ToolRecord/recipes 里的 URL 一律
   canonical GitHub——锁文件跨机跨网可移植；镜像装出的树与直连装出的
   树逐字节等价（checksum 校验照常，**镜像不是信任信号**，trust 协议
   不受影响）。
2. **只重写 `https://github.com/` 前缀**（http、其他 host、本地路径、
   `git@` SSH 直通）——镜像不得静默截获它不代理的流量。
3. **降级纪律**：配置缺失/坏 TOML/未知 active → 传输层一律退直连
   （镜像配置问题永不弄坏下载）；`mirror list` 与 doctor 显式报告
   该状况。
4. **无 auto-select**：probe 只建议（wpm 教训的正向面——它的 auto
   也只重排不拦截），切不切、贴哪个 URL 是用户的决定。
5. **预置源=0**（owner 追加裁定）：社区镜像服务的死活不是产品责任。

**验收（全部为测试）**：`plugins/mirrors.rs` 单测 13 条（重写矩阵/
release 覆盖/insteadOf 只对 GitHub origin/URL 归一化/probe 目标/
缺档-坏档-未知名降级/set 往返与 custom 段保留）；`sources.rs`
`git_clone_args_carry_instead_of_mirror_for_github_origins_only`
（clone 参数携带 insteadOf 对、origin 仍 canonical、本地 origin 不
镜像、无配置时零 insteadOf）；`tests/plugin_mirrors.rs` 二进制级 3
条（set 归一化往返+list 报告+set none 保留段落/垃圾 URL 拒绝且不落
盘/`mirror --help` 与 `plugin --help` 面）。真网络冒烟（设镜像后
`niu font`/`plugin add` 走镜像 URL）按门禁为可选——传输层重写已被
上述测试钉死，端到端网络属于环境验证。
### 14.8 wpm 收缩到命令层（2026-10-03 owner 裁定，wt49/wpmretract）

裁定：**插件下载驱动（`plugins/download.rs`，纯 Rust ureq/rustls + zip/tar
+ sha256）成为唯一的可执行工具安装入口**（跨平台一致）；**wpm 只管 niubash
的 Unix 命令层**（sed/cp/grep/ls 等随 winuxcmd 树走的工具链，及
awk/jq/7z/zstd/wget/aria2/rclone/busybox 一类经典 Unix 工具箱），**不再安装
或推荐 starship/fzf/eza 等应用工具**。落地方式遵守打地鼠禁令（rubash#117
同族）：**修路径而非造桥接**——不新增"检测 wpm 装过什么"的桥接层，直接把
推荐面与安装面改道到插件驱动。

**职责表**：

| 平面 | 安装入口 | 管什么 | 不变量 |
|---|---|---|---|
| Unix 命令层 | wpm（仅 Windows） | winuxcmd 树与经典 Unix 工具箱；`wpm links rebuild` 命令链接初始化（安装器/包内树）；`wpm update winuxcmd` 命令层更新；`alias apt=wpm`（已装用户的命令层入口别名，按 wpm 在位条件写入） | 不装应用工具；wpm 字符串/命令/探针零出现在非 Windows 构建（编译期 cfg 门控，非运行时） |
| 应用工具层 | 插件下载驱动 `niu plugin add <recipe-id>` | 二进制应用工具（starship/fzf/eza/bat/fd/ripgrep/zoxide/dust/duf/erdtree/direnv/niugit…，recipes.toml download 行，sha256 逐平台钉死） | 跨平台一致；transport 纯 Rust；冲突门：`~/.niubash/tools/` 已有同 id 则拒绝并指向 `niu plugin tool list`；**不**探测 wpm 的安装状态（那是桥接） |

实施记录（wt49/wpmretract）：

1. **setup 向导**：niu-git 问题的安装分支从 `wpm install niugit` 改道
   `niu plugin add niugit`（niugit 落为 download recipe，digest 取自
   niu-git 仓库 `wpm/niugit.json` 官方索引条目并下载实测）；向导保留的
   wpm 面全部是命令层角色：`wpm links rebuild` 提示、PATH 探针
   （环境摘要）、`apt` 别名（收紧为 wpm 在位才写 + cfg(windows) 编译门）。
2. **command-not-found 提示拆双表**（`shell.rs`）：
   `plugin_recipe_for_command`（应用工具 → `niu plugin add <id>`，跨平台，
   与 recipes.toml 的 download 行同源对齐）+ `wpm_package_for_command`
   （命令层 → `wpm install <pkg>`，cfg(windows) 编译门）。应用工具的
   wpm 提示臂全部删除（delta/sd/kubectl/helix 等尚未有 recipe 的应用
   工具不再出 wpm 提示，也不出插件提示——不指向死动词）。
3. **冲突门**：`install_executable` 前置拒绝同 id（`tool_installed`：
   目录或注册表记录任一命中），错误信息点名 `niu plugin tool list` 与
   `niu plugin tool remove <id>`；recipe 层把该拒绝渲染为"already
   installed"健康报告（collection 重放不因此报失败）。
4. **recipe 索引**：PROBED_TOOLS 应用类补齐 download 行（见下）；
   erdtree/direnv 上游只发裸可执行文件，驱动补 `ArchiveKind::Raw`
   （字节写 `bins[0]` + Unix chmod 755）——这是驱动能力补全（同一代码
   路径服务所有裸 exe 资产），不是逐工具行为码。thefuck 无二进制发行
   （Python 应用），落 info-only 行如实说明。
5. **平台红线收口**：doctor / winuxcmd 激活 / 向导的 `wpm links rebuild`
   提示全部改编译期 cfg 门控；easter_egg 的 `wpm`（typing 子命令别名，
   words-per-minute）与产品无关，保留。
6. **字体下载迁纯 Rust**（同日 owner 追加军令）：`fonts.rs` 的
   `download()` 从系统 curl 子进程改调 `plugins::download::http_get_bytes`
   + `write_download`——下载逻辑独立纯 Rust 在字体面的补全。

**PROBED_TOOLS 分类**（向导探针 → 各平面）：

| 工具 | 类别 | 安装改道 |
|---|---|---|
| starship/fzf/eza/bat/fd/rg(ripgrep)/zoxide/dust/duf/erd(erdtree)/direnv | 应用工具 | `niu plugin add <id>`（download recipe） |
| thefuck | 应用工具（无二进制发行） | info-only recipe 行（pip/包管理器自理） |
| git/niu-git | 应用工具 | 向导问题改道 `niu plugin add niugit`（Windows-only 问题保留） |
| kubectl/docker/npm | 运行时探针（环境摘要） | 不由向导安装，无推荐入口 |
| wpm（Windows-only 探针） | 命令层 | 探针保留；`apt` 别名入口保留 |

遗留（报船长/owner 裁定，不在本车道动）：
`docs/src/advanced-usage.md` Elevated Commands 节的 gsudo 仍指向
`wpm install gsudo`——gsudo 属应用工具，改道需要一个 gsudo download
recipe（涉及 UAC/提权平面归属），待裁定；命令层与应用层的分界线
（awk/jq/7z 归命令层）如 owner 意在更严格的"仅随产品走"口径，则
`wpm_package_for_command` 需再收缩。

### 14.9 下载全面收缩（2026-10-04 owner 最终裁定，wt50/dlretract）

裁定：**niu 是纯 bash 生态插件管理器，shell 本体承担零网络/HTTP 下载职责。**
全部 HTTP 依赖移除；可执行工具的安装归属真实包管理器（Windows 上 wpm
为第一顺位——owner 更正 2026-10-03：shell 停止下载 ≠ wpm 被降级，
winget/scoop 只作为 wpm 不携带物（GUI 应用、字体）的补充；其他平台
apt/dnf/yum/brew 原生管理器，非 Windows 构建零 wpm 字符串，688f224
红线不变）；插件驱动 `niu plugin add <git-url>` 是**唯一**扩展安装入口
且只用 `git clone`；字体收缩为检测+推荐。

**删除面**（本节为存档裁定记录，下面的 14.8 两节描述的 download 驱动、
镜像 prefix 通道、`niu plugin tool` 动词均已被本节取代）：

1. `plugins/download.rs` 整模块（ureq/rustls 传输、zip/tar.gz 解包、
   `~/.niubash/tools/registry.toml` 工具注册表、PATH 块激活通道）。
   Cargo.toml 移除 `ureq`/`flate2`/`tar`/`zip`（flate2 仅作为 png 的
   传递依赖留在锁文件）。
2. 镜像 HTTP prefix 通道（`rewrite_download_url`、`[github] prefix`、
   `[github.releases]`）与可达性探测（`probe_reachability`、
   `niu plugin mirror test`）。镜像收缩为 **git-only insteadOf**：
   `niu plugin mirror set <url>` 写 `git_instead_of`；旧 prefix 字段
   兼容解析、静默忽略。
3. `niu plugin tool list/remove` 动词（报错指向包管理器流程）；
   download-era 安装残留（`~/.niubash/tools/<id>`）手动删除。
4. `fonts.rs` 下载/解压/注册路径：保留 NerdFont 检测（目录+注册表
   扫描），`niu font` 改为非交互的检测+推荐（winget `DEVCOM.
   JetBrainsMonoNerdFont` 等 / scoop nerd-fonts bucket / brew cask /
   nerdfonts.com 兜底；wpm 不携带字体，属正常分工而非降级）。
5. recipe 索引（509 行）保留为**目录元数据**：`driver = "download"`
   行照常解析（平台 URL 为纯数据），`niu plugin add fzf` 打印包管理器
   推荐命令（Windows 首选 `wpm install fzf`，winget/scoop 次之；非
   Windows 仅 apt/dnf/brew）+ 上游 release URL，永不 fetch。
   command-not-found 提示同名推荐。自更新（self_update，WinHTTP）
   是产品自身更新通道，不属于扩展安装，保留直连（不再走镜像）。

**教育口径**（docs 同步用）：两个系统共存——**wpm = Windows 的 Unix
命令层**（含应用工具，第一顺位），**`niu plugin` = bash 生态扩展驱动**
（git 源、主题、补全，任意平台）。收缩叙事是"niu 不再下载"，不是
"wpm 被 winget 取代"。

**验收**：runtime 单测 304/0（原 71 失败经删除消除，非 #[ignore] 隐藏）；
`RUSTFLAGS="-D warnings" cargo check --all-targets` 干净；冒烟套件
B 段改为收缩面审计（Cargo.toml 无下载依赖、无 download.rs、锁文件无
ureq、`plugin add fzf` 离线出推荐、`plugin tool` 报退役、`niu font`
离线推荐），全离线通过。
