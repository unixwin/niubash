# reaudit-20261010 — wt92 not-OK 语料重审计（2026-10-12 完成账）

本目录是对 `../wt92-local-20261005/` 快照 not-OK 桶（747 条记录 / 675 个唯一资产）的重验重分类交付。
由 eco/reaudit 代理在 niubash 工作树 `D:/repo/niubash-wt-ecoaudit`（分支 `eco/reaudit-20261010`，基于 master 11c1612）完成。

## 交付物

| 文件 | 内容 |
|---|---|
| `ledger-notok-20261012.md` | **主台账**：方法、判定不稳、定案汇总、675 个唯一资产逐条证据表、复现附录 |
| `reclassified-records-20261012.jsonl` | 747 条 not-OK 记录逐条重判（含原始证据字段，机器可读） |
| `classified-uniques.json` | 675 个唯一资产完整证据（标记扫描、今日 GNU `bash -n`、今日 rubash `-n`、HANG 受护栏运行输出、判定史） |
| `test-results-20261010.jsonl` | （前次实例遗留）重放记录 42 条——**全部作废**，见下 |
| `repro-c5-multiline-bracket-fn.sh` | rubash#505 最小复现（本审计自写） |
| `repro-r8-extglob-loader-parity.sh` | extglob loader 模式 GNU 平价演示（本审计自写） |

## 的一句话结论

not-OK 语料经静态 + WSL GNU + 现行 rubash `-n` 三方重验：**唯一仍活的 rubash 缺陷是 1 个**（sheldritch args.sh，函数体内跨行 `[[` + `||` 续行 → rubash#505，附最小复现）；54 条 SR 桶中 20 个 GNU 平价、11 个快照后已修、6 个 zsh 方言、2 个语料污染、其余归 zsh/坏脚本；101 条 HANG 桶中 9 个对应已关已修的 #451A/B，45+ 个是交互 stdin 消费类伪影，其余为环境依赖/网络/prompt 流伪影，仅 6 个留下需在 #256 后构建上少量复放确认。上一实例（10-10）的 42 条重放记录因宿主级 closed-stdout-pipe 伪影（niubash#256，已修）全部作废。

## 纪律遵守

- 不可信资产未在本机裸执行；eco-test.py 本机路径未触碰
- WSL GNU bash 仅按任务定义跑公开 dotfiles 对照（`bash -n` 全量 + HANG 受护栏运行 stdin=/dev/null）
- 本机执行过的只有自写的最小复现片段（`rubash -n`/`rubash` 于自写 6-8 行脚本）
- zsh/fish 语法资产一律标注方言归属，不计入 rubash（`for hash rhash in ${(kv)...}` 教训在本数据中得到复现并落实）
- git 全程 niugit 完整路径 + D:/ 形态；未 stash；未触碰其他工作树；临时文件在 %TEMP%/ecoaudit2

## 后续建议（超出本审计范围）

1. RESIDUAL-NEEDS-LIVE-REPLAY 6 个：在 #256 之后构建上做点状复放即可闭账
2. harvest 侧：非脚本扩展名（.md 等）入语料前过滤；判定写入时附构建指纹（git hash），避免再出现 275 个跨构建矛盾判定
3. SLOW 桶深水（nvm.sh 等）循 rubash#241 推进
