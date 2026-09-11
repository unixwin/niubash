# Planning: locale 默认值与 `${#var}` 计数（future trigger）

状态：**待触发，暂不实施**。记录于 2026-09-11。

## 现状（2026-09-11 核查结论）

- rubash 已合入 locale 子系统（`44ab54d3`，`src/locale.rs`，141 行），但目前是"纯管道"：
  - 真正干活的 `effective_length` / `is_utf8` / `is_printable` 在库内**零调用点**；
  - `init_locale()` → `check_setlocale_warning()` 的 setlocale 警告被临时禁用（作者注释：避免 INTL 测试套件噪音）。
  - 因此对 niubash **当前行为影响为 0**，host 侧不需要传 `LANG`/`LC_*`，也不需要调用任何初始化。
- `locale::locale_name()` 每次动态读**进程环境**（优先级 LC_ALL > LC_CTYPE > LC_MESSAGES > LANG），不缓存、无需初始化。
- rubash 的 export/declare 路径会 `env::set_var` 同步进程环境，所以脚本内 `export LC_ALL=...` 也能被动态读到，host 无需桥接。

## 触发条件

当 rubash 上游把 `effective_length` 真正接进 `${#var}`（UTF-8 locale 下按字符计数，否则按字节）时，本决策生效。届时：

- Windows 上 `LANG`/`LC_ALL` 常常未设置 → `is_utf8()` 返回 false → `${#var}` 对非 ASCII 按**字节**计数（`中文` = 6 而不是 2）。
- `is_printable()` 同样受影响（ASCII locale 下 >= 0xA0 的字符视为不可打印）。

## 决策点

**选项 A（推荐）：默认 UTF-8。** niubash 启动时，仅当进程环境的 `LANG` 与 `LC_ALL` **同时缺失**时，设置 `LANG=C.UTF-8`。

- 选 `C.UTF-8` 而不是 `zh_CN.UTF-8`：我们要的是编码正确性（字符计数、printable 判定），不是地区差异（消息翻译等）。`C.UTF-8` 多语言中性，不强行替用户指定地区。
- 用户显式设置的值天然优先：`locale_name()` 动态读进程环境，脚本内随时可改。
- 实施位置：`crates/niubash-runtime/src/shell.rs` 的环境初始化块（`$BASH`/`SHELL` 设置处附近）。
- 同一 owner 下可顺带评估 rubash 独立二进制的 `main.rs` 是否做同样兜底。

**选项 B：保持不设（等效 C locale）。** 与"无 locale 的 bash"严格一致，`${#var}` 按字节。上游 bash 测试套件（INTL 等）在无 locale 环境下预期如此——若日后要跑 INTL 全绿，注意此项会互相影响。

## 关联代码

- rubash：`src/locale.rs`、`src/executor/export_builtin.rs`（LC_* export 检查）、`src/main.rs`（启动 init）
- niubash：`crates/niubash-runtime/src/shell.rs`（环境初始化块）
