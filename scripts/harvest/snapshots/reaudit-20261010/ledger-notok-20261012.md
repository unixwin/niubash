# not-OK 语料重审计台账（2026-10-12）

覆盖 wt92-local-20261005 快照全部 not-OK 非抓取失败桶：**747 条记录 / 675 个唯一资产**（按首次判定分桶：SLOW 350 / GNU-ALSO-FAILS 224 / HANG 101 / SYNTAX-REJECT-RUBASH-ONLY 54 / EXITED 17 / ERROR-STORM 1；FETCH-FAILED 698 不在本审计范围）。同一资产在快照内的多轮记录判定史见各组表内的 `判定史` —— 快照收割期间二进制在漂移，**275 个资产存在互相矛盾的判定**（详见下文[判定不稳](#判定不稳)）。

## 方法与安全

- 纯静态分析：读资产原文判语法归属（zsh/fish/ksh/extglob/stdin 消费者/长跑/网络等待标记扫描 + 逐条人工复核报错行上下文）
- WSL GNU bash 对照：全量 674 个可得资产 `bash -n` 语法复核；HANG 80 个唯一资产加受护栏运行（stdin=/dev/null、timeout 8s、输出截断）
- 现行 rubash `-n`（noexec 纯解析）全量 674 个 + 报错行上下文取证
- **红线遵守**：不可信资产未在本机任何路径执行（eco-test.py 本机路径未触碰）；WSL 内仅 GNU bash 按「公开 dotfiles 对照」任务定义运行；本机执行过的只有本审计自写的最小复现片段（见[复现附录](#复现附录)）
- 资产内容经 `gh api` 重新抓取（raw.githubusercontent 当时不稳）；674/675 成功，1 个源 404（cr1315/.rayrc，标注 UNRESOLVED-NO-SOURCE）

## 判定不稳

快照内 2026-10-04 19:xx/20:xx 与 2026-10-05 02:45 三轮重跑使用了不同构建，275 个资产的判定互相矛盾：210 FETCH-FAILED→OK、29 GNU-ALSO-FAILS→SR、16 EXITED→OK 等。SYNTAX-REJECT 桶被翻转注水（54 条记录里只有一部分是首判 SR）；EXITED 桶 16/17 在重跑中翻成 OK（引擎瞬态）。本台账对所有唯一资产按**当前证据**（静态 + 今日 GNU + 今日 rubash -n）统一定案，每行给出判定史。

## 上一实例（2026-10-10）重放通道伪影

前次会话遗留的 `test-results-20261010.jsonl`（42 条）系统性显示 `startup≈20s + hang:startup`（连原判 SLOW/GNU-ALSO-FAILS 的资产也"挂起"），为宿主级 closed-stdout-pipe 缺陷（rubash#455 家族，niu 在 stdout 管道读端消失后每迭代刷诊断、永不退出），已由 niubash#256（11c1612, src/piped_stdout.rs）修复。该 42 条重放记录全部作废，由本静态审计取代。

## 定案汇总（675 唯一资产）

| 重判类 | 数量 | 说明 |
|---|---|---|
| RECLASSIFIED | 413 | 重分类：SLOW=资产自重（source_wall，GNU 未测）、HANG=交互 stdin 消费/网络等待/prompt 流伪影、EXITED=瞬态翻转、ERROR-STORM=环境依赖、语料污染、zsh 方言等 |
| GNU-ALSO-FAILS-CONFIRMED | 175 | GNU 侧同样失败（rc=124 长跑 116 / rc=127 依赖缺失 / 运行期错误），维持原判 |
| GNU-PARITY-CONFIRMED | 24 | 今日 GNU bash -n 与 rubash -n 拒绝面一致（extglob 解析期依赖 20 / 真坏脚本 1 / 其他 4），非 rubash 缺陷 |
| TRANSIENT-FLIP-TO-OK | 16 | 同资产重跑已 OK（EXITED 桶 16 个） |
| FIXED-SINCE-SNAPSHOT | 14 | 现行 rubash -n 已接受（ble.sh×6、jb-cli #463、odsel 解析超时、zZsh/mirkop/.shortcut、HANG 解析错族×3） |
| ZSH-DIALECT | 13 | zsh 语法资产（ohmyzsh 工具链、zZshFramework、p10k.zsh、fzf-tab zsh 腿等 13+6）——按归因纪律永不归属 rubash |
| RESIDUAL-NEEDS-LIVE-REPLAY | 6 | 静态无阻塞构造、GNU 通过，但重放探针丢失——需在 #256 之后构建上少量复放确认良性（5+1 个） |
| RUBASH-BUG-FIXED-#451A | 5 | ConPTY 输入桥 0x15(^U) 注入（#451 类 A，已关已修） |
| RUBASH-BUG-FIXED-#451B | 4 | source 后首轮回车 continuation 误判（#451 类 B，1b2cd8ee 修） |
| ENV-DEPENDENT | 3 | 目标环境外缺命令/文件（rc=127） |
| RUBASH-DEFECT-LIVE | 1 | **活的 rubash 缺陷**：GNU 接受、现行 rubash 拒绝 → 已开 #505 |
| GNU-ALSO-LONGRUN | 1 | GNU 亦超 8s（真长跑资产） |


## 原判定 SYNTAX-REJECT-RUBASH-ONLY（36 个唯一资产）

| hash | repo/path | 判定史 | 重判类 | 依据 | GNU-n 今日 | rubash-n 今日 |
|---|---|---|---|---|---|---|
| `7d5783e715c0` | GaelGosse/shell_function_n_shortcuts/.shortcut | SR | FIXED-SINCE-SNAPSHOT | current rubash -n accepts (and GNU accepts) | ok | pass |
| `bb7c22c797c8` | akinomyoga/ble.sh/lib/core-syntax.sh | SR,SR | FIXED-SINCE-SNAPSHOT | current rubash -n accepts (and GNU accepts) | ok | pass |
| `8b99971a6065` | akinomyoga/ble.sh/lib/init-term.sh | SR,SR | FIXED-SINCE-SNAPSHOT | current rubash -n accepts (and GNU accepts) | ok | pass |
| `d325949b4be5` | akinomyoga/ble.sh/lib/keymap.vi.sh | SR,SR | FIXED-SINCE-SNAPSHOT | current rubash -n accepts (and GNU accepts) | ok | pass |
| `5900462c1c61` | akinomyoga/ble.sh/src/canvas.sh | SR,SR | FIXED-SINCE-SNAPSHOT | current rubash -n accepts (and GNU accepts) | ok | pass |
| `f4cb220f9017` | akinomyoga/ble.sh/src/color.sh | SR,SR | FIXED-SINCE-SNAPSHOT | current rubash -n accepts (and GNU accepts) | ok | pass |
| `651b15783ae2` | akinomyoga/ble.sh/src/edit.sh | SR,SR | FIXED-SINCE-SNAPSHOT | current rubash -n accepts (and GNU accepts) | ok | pass |
| `389b21559168` | gingerpaledale/zZshFramework/src/files.sh | SR | FIXED-SINCE-SNAPSHOT | current rubash -n accepts (and GNU accepts) | ok | pass |
| `386fd920483d` | h4l/json.bash/examples/jb-cli.sh | SR | FIXED-SINCE-SNAPSHOT | current rubash -n accepts (and GNU accepts) | ok | pass |
| `f8224249a52a` | irrequietus/shellapi/modules/odsel/odsel.shellapi.bash | SR | FIXED-SINCE-SNAPSHOT | current rubash -n accepts (and GNU accepts) | ok | pass |
| `91afb5153901` | klapptnot/dotf-old/.config/bash/mirkop.sh | SR | FIXED-SINCE-SNAPSHOT | current rubash -n accepts (and GNU accepts) | ok | pass |
| `6af2ccdce6c4` | neonsoftware/cmake-bats/doc/sample-project/test-integers.sh | GAF,SR | GNU-PARITY-CONFIRMED | GENUINELY-BROKEN (GNU bash -n also rejects: unbalanced construct; not a parse-time dependency) | fail | fail |
| `3c921d2d8300` | scop/bash-completion/completions-core/apt-get.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `20c563f66c6b` | scop/bash-completion/completions-core/gpgv.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `b8b0cf668817` | scop/bash-completion/completions-core/growisofs.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `90d6bc81eaad` | scop/bash-completion/completions-core/killall.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `c6709cabd93c` | scop/bash-completion/completions-core/lastlog.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `0f2f728582ba` | scop/bash-completion/completions-core/lrzip.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `39d9535f2a0a` | scop/bash-completion/completions-core/lsusb.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `bfc9f0c4a5e4` | scop/bash-completion/completions-core/luseradd.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `93b8c8d135e5` | scop/bash-completion/completions-core/luserdel.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `b875440eadfe` | scop/bash-completion/completions-core/lzip.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `bd957df56e0a` | scop/bash-completion/completions-core/mypy.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `55f8af7966b1` | scop/bash-completion/completions-core/opera.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `2abb09e7e678` | scop/bash-completion/completions-core/pdftoppm.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `a48ed0ba0367` | scop/bash-completion/completions-core/perlcritic.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `ab327bcfd16c` | scop/bash-completion/completions-core/pydocstyle.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `c1e83291558c` | scop/bash-completion/completions-core/pytest.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `ed59f1bf5bfc` | scop/bash-completion/completions-core/ssh-keygen.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `9b22449e5efa` | scop/bash-completion/completions-core/ssh.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `3e41dee4c72a` | scop/bash-completion/completions-core/valgrind.bash | GAF,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `3e87cfa11a9f` | Twilight4/nobility/scripts/web-recon.sh | GAF,SR | RECLASSIFIED | ZSH-DIALECT (GNU bash also rejects; attribution: zsh, not rubash) | fail | fail |
| `3c6dbdbd7d43` | ohmyzsh/ohmyzsh/oh-my-zsh.sh | FF,FF,GAF,SR | RECLASSIFIED | ZSH-DIALECT (GNU bash also rejects; attribution: zsh, not rubash) | fail | fail |
| `df47ce21ffad` | scop/bash-completion/completions/README.md | GAF,SR | RECLASSIFIED | CORPUS-POLLUTION (non-shell asset parsed as shell) | fail | fail |
| `0b2deff5d7ad` | tbrandenburg/shai/scripts/pipeline_essay.sh | GAF,SR | RECLASSIFIED | CORPUS-POLLUTION (prose/essay with fenced snippets) | fail | fail |
| `67dc22ec10ea` | sheldritch/sheldritch/core/args.sh | GAF,SR | RUBASH-DEFECT-LIVE | GNU accepts, current rubash -n rejects | ok | fail |

## 原判定 HANG（80 个唯一资产）

| hash | repo/path | 判定史 | 重判类 | 依据 | GNU-n 今日 | rubash-n 今日 |
|---|---|---|---|---|---|---|
| `8c22d02cf223` | MacRimi/ProxMenux/scripts/menus/share_menu.sh | FF,FF,HANG | ENV-DEPENDENT | GNU rc=127: missing external command in this environment | ok | pass |
| `16c2211044bb` | ducksrfr/mac_admin/scripts/rename_user.sh | FF,FF,HANG | ENV-DEPENDENT | GNU rc=127: missing external command in this environment | ok | pass |
| `21f6def9ff80` | tbrandenburg/shai/scripts/browser_automation.sh | HANG | ENV-DEPENDENT | GNU rc=127: missing external command in this environment | ok | pass |
| `128f3dd500e2` | ohmybash/oh-my-bash/themes/vscode/vscode.theme.sh | HANG,HANG,HANG | FIXED-SINCE-SNAPSHOT | snapshot-era mid-source parse error ("near unexpected token &&") then hang; current rubash -n PASS + GNU PASS (#451 input-bridge/parser fami | ok | pass |
| `13ebf2b2866f` | phil-inc/admiral/.devcontainer/.bashrc | HANG | FIXED-SINCE-SNAPSHOT | snapshot-era mid-source parse error ("near unexpected token &&") then hang; current rubash -n PASS + GNU PASS | ok | pass |
| `41bc43cf5821` | pjh/devcontainer-dotfiles/.bashrc | HANG | FIXED-SINCE-SNAPSHOT | snapshot-era mid-source parse error ("near unexpected token &&") then hang; current rubash -n PASS + GNU PASS | ok | pass |
| `fc0cdd12747b` | Soroushnk/Astro/managed/mtproto.sh | HANG | GNU-ALSO-LONGRUN | GNU bash also exceeds 8s with stdin closed (genuinely long-running asset) | ok | pass |
| `317ba100e859` | Abhinavgupta371/nos_build_make/envsetup.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader; GNU completes with stdin closed) | ok | pass |
| `a0b0c4ca1796` | BashSupport/BashSupport/testData/scripts/mtab.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader; GNU completes with stdin closed) | ok | pass |
| `acc4dbb040a3` | CrackerCat/ZetaSploit/uninstall.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (menu-style uninstall script with prompts; banner file missing) | ok | pass |
| `f6f74dba0620` | JustinChristensen/dotfiles/darwin/.bash_profile | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader+network-wait; GNU completes with stdin closed) | ok | pass |
| `ae55aaaaf744` | MidnightCoding1/mForce/modules/code_security.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader; GNU completes with stdin closed) | ok | pass |
| `45f5ed116eee` | MidnightCoding1/mForce/modules/username_osint.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader; GNU completes with stdin closed) | ok | pass |
| `89fb9e865c46` | NullArray/RootHelper/roothelper.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (network-wait; GNU completes with stdin closed) | ok | pass |
| `cceb7045d2d4` | SlimYAOSP/build/envsetup.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader; GNU completes with stdin closed) | ok | pass |
| `47eb56c84236` | Soroushnk/Astro/managed/cisco_vpn.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader+network-wait+loop/sleep; GNU completes with stdin closed) | ok | pass |
| `7d5d59d50fa8` | Soroushnk/Astro/managed/namaeserver.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader+loop/sleep; GNU completes with stdin closed) | ok | pass |
| `606400d19f7d` | TeamExodus/build/envsetup.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader+network-wait; GNU completes with stdin closed) | ok | pass |
| `83d77b9e15e2` | ZaleHack/phpexploit/test/test-connected.sh | HANG | RECLASSIFIED | ENV-DEPENDENT (uuidgen absent; completes under GNU) | ok | pass |
| `8af33ff37015` | alanhamlett/dotfiles/bashrc | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader; GNU completes with stdin closed) | ok | pass |
| `bcd238337c99` | alanxz/dotfiles/.bashrc | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (loop/sleep; GNU completes with stdin closed) | ok | pass |
| `66c4c143a7ee` | avivleemann/container-template/.bashrc | HANG | RECLASSIFIED | ENV-DEPENDENT (sources /usr/local/bin/_activate_current_env.sh which does not exist here) | ok | pass |
| `b8030ba3c97e` | binpash/pash/evaluation/tests/ann-agg.sh | HANG | RECLASSIFIED | ENV-DEPENDENT (pash 1M-input file absent) + stdin-reading pipeline stage | ok | pass |
| `a3990e1a3653` | binpash/pash/evaluation/tests/bigrams.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (bare stdin-reading pipeline stage; GNU completes with stdin closed) | ok | pass |
| `5699e84d08e6` | binpash/pash/evaluation/tests/grep.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (bare stdin-reading pipeline stage; GNU completes with stdin closed) | ok | pass |
| `017b1c7363d4` | binpash/pash/evaluation/tests/sort.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (bare stdin-reading pipeline stage: sort/head/wc with no file operand; GNU completes with stdin closed) | ok | pass |
| `6825938acf4d` | binpash/pash/evaluation/tests/topn.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (bare stdin-reading pipeline stage; GNU completes with stdin closed) | ok | pass |
| `c0c2c640d35e` | binpash/pash/evaluation/tests/wf.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (bare stdin-reading pipeline stage; GNU completes with stdin closed) | ok | pass |
| `f10440d5a5cc` | bivlked/amneziawg-installer/awg_common.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader+network-wait+loop/sleep; GNU completes with stdin closed) | ok | pass |
| `279ba8e790c9` | bivlked/amneziawg-installer/awg_common_en.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader+network-wait+loop/sleep; GNU completes with stdin closed) | ok | pass |
| `c54818d072aa` | bivlked/amneziawg-installer/install_amneziawg.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader+network-wait+loop/sleep; GNU completes with stdin closed) | ok | pass |
| `73ffed0013d6` | bivlked/amneziawg-installer/install_amneziawg_en.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader+network-wait+loop/sleep; GNU completes with stdin closed) | ok | pass |
| `79becfc54ce0` | cgsdev0/bash-stack/core.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader+loop/sleep; GNU completes with stdin closed) | ok | pass |
| `4c3161a4b9a5` | cyberark/bash-lib/test-utils/bats-support/script/install-bats.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (network-wait; GNU completes with stdin closed) | ok | pass |
| `d5aeef73ab77` | davewalker5/SpectrogramViewer/scripts/make_venv.sh | FF,HANG,SLOW | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (network-wait; GNU completes with stdin closed) | ok | pass |
| `490390557cce` | drj613/obsidian-agent/agents-bootstrap.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (script prompts for input; replay stdin never EOFs) | ok | pass |
| `e77175a7ae80` | ethanbayliss/dotfiles/.bashrc | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (loop/sleep; GNU completes with stdin closed) | ok | pass |
| `47ffc8046cee` | filiperochalopes/dev-notes/.bash_profile | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (network-wait; GNU completes with stdin closed) | ok | pass |
| `0c1f4333a2a8` | fissible/ptyunit/self-tests/hostile/cpu-work.sh | HANG | RECLASSIFIED | CPU-BOUND-COMPLETE (arithmetic loop prints result then exits under GNU; exceeded replay window in snapshot build) | ok | pass |
| `a740316324c5` | freekmurze/dotfiles/config/claude/statusline.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (claude statusline reads stdin JSON via input=$(cat); replay stdin never EOFs) | ok | pass |
| `67b558cc5966` | hbekel/magic/magic | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader; GNU completes with stdin closed) | ok | pass |
| `b11cff734527` | hyperpolymath/valence-shell/setup.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (network-wait; GNU completes with stdin closed) | ok | pass |
| `2648a63b43fc` | jamesmartin/dotfiles/bash_profile | HANG | RECLASSIFIED | ENV-DEPENDENT (spawns gpg-agent daemon; daemon/agent wait in replay) | ok | pass |
| `f69e8e9f3eff` | kevinlekiller/reshade-steam-proton/reshade-linux.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader+network-wait+loop/sleep; GNU completes with stdin closed) | ok | pass |
| `5b9c3c419dcd` | kippesp/home_files/.bash_zsh/bashrc | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (network-wait; GNU completes with stdin closed) | ok | pass |
| `748bdfa6e5f3` | kira-bruneau/nixos-config/home/programs/bash/.bashrc | HANG | RECLASSIFIED | HISTORY-INTERLOCK FAMILY (trap DEBUG history -a + PROMPT_COMMAND history -n; cf. niubash#182 fixed by cce8ad4) | ok | pass |
| `5fcf0660ae13` | konethegreat/github-final-project/simple-interest.sh | HANG,HANG,HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader; GNU completes with stdin closed) | ok | pass |
| `fcb1ccd8f79a` | konethegreat/github-final-project/tests/test-simple-interest.sh | HANG,HANG,HANG | RECLASSIFIED | ENV-DEPENDENT (test harness invokes bash simple-interest.sh which is absent; GNU completes with rc=1) | ok | pass |
| `0d3811df7e5f` | manastria/dotfile/.bashrc | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (loop/sleep; GNU completes with stdin closed) | ok | pass |
| `2210539dad8e` | mbailey/bash-my-aws/test/bash-spec.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (loop/sleep; GNU completes with stdin closed) | ok | pass |
| `6aa4ee160219` | microsoft/shell-intune-samples/macOS/Config/MDATP/runMDATPQuickScan.sh | HANG | RECLASSIFIED | ENV-DEPENDENT (macOS Intune MDATP environment; missing commands/files outside target env) | ok | pass |
| `6803aec9ec6d` | microsoft/shell-intune-samples/macOS/Config/Rosetta2/installRosetta2.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (loop/sleep; GNU completes with stdin closed) | ok | pass |
| `91aa8683b376` | modernish/modernish/share/doc/modernish/utils/mktoc.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader; GNU completes with stdin closed) | ok | pass |
| `72f910ee981d` | muthuishere/mcp-server-bash-sdk/docs/spikes/spike-02-http-listener.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader+network-wait; GNU completes with stdin closed) | ok | pass |
| `0b4b655df726` | pipeseroni/pipes.sh/scripts/gen-man-html.sh | FF,HANG,HANG,HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader; GNU completes with stdin closed) | ok | pass |
| `3ff532761792` | puppylinux-woof-CE/woof-CE/kernel-kit/usrmerge.sh | HANG | RECLASSIFIED | ENV-DEPENDENT (puppy-linux rootfs paths absent; completes under GNU) | ok | pass |
| `2b6644e8a999` | rust-shell-script/rust_cmd_lib/examples/tetris.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader; GNU completes with stdin closed) | ok | pass |
| `6f05fbb6dfc4` | shellspec/shellspec/contrib/bugs.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader; GNU completes with stdin closed) | ok | pass |
| `89a95f76abd9` | shellspec/shellspec/contrib/pretty.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (shellspec pretty filter reads stdin) | ok | pass |
| `13940489fef7` | sloria/doitlive/examples/walkthrough.sh | HANG,HANG,HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (python REPL takeover in interactive replay; GNU completes with stdin closed) | ok | pass |
| `d63453cd3eb8` | sm26449/webterm/agent/shell-integration.sh | HANG | RECLASSIFIED | PROMPT-STREAM-ARTIFACT (shell-integration emits OSC 133 sequences; probe matcher loses the ECO> prompt in the escape stream) | ok | pass |
| `11043df9610b` | softprops/awsnap/awsnap-install.sh | HANG | RECLASSIFIED | PROMPT-STREAM-ARTIFACT (redefines PROMPT_COMMAND via cd/chpwd hook; probe matching disrupted) | ok | pass |
| `3cf7b1f64fd6` | statico/dotfiles/.vim/update.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (network-wait; GNU completes with stdin closed) | ok | pass |
| `fb297bc274f8` | sterrevangeest/dotfiles/.bash_profile | HANG | RECLASSIFIED | NETWORK-WAIT (profile fetches wttr.in; network latency inside source path) | ok | pass |
| `f06e3726d790` | twoerner/myconfig/bashrc | HANG | RECLASSIFIED | ENV-DEPENDENT (lsb_release absent; completes under GNU) | ok | pass |
| `ac3f3de2f5a6` | wagoodman/bashful/example/scripts/compile-something.sh | HANG | RECLASSIFIED | INTERACTIVE-STDIN-CONSUMER (stdin-reader+loop/sleep; GNU completes with stdin closed) | ok | pass |
| `2ed2ffdeffda` | ciaranm/dotfiles/bashrc | HANG | RESIDUAL-NEEDS-LIVE-REPLAY | heavy interactive bashrc (bind/readline settings); completes under GNU; re-check under post-#256 build | ok | pass |
| `e72a0201c0af` | denysdovhan/learnyoubash/test/debugging/valid.bash | HANG | RESIDUAL-NEEDS-LIVE-REPLAY | set -vn test script completes under GNU; probe lost in replay; re-check under post-#256 build | ok | pass |
| `b4e3a68436c1` | ersinkoc/dfmt/hw.sh | HANG | RESIDUAL-NEEDS-LIVE-REPLAY | source /dev/stdin <<< heredoc construct; completes under GNU; re-check under post-#256 build | ok | pass |
| `d3f2ed792705` | h4l/json.bash/hack/trap_bench.bash | HANG | RESIDUAL-NEEDS-LIVE-REPLAY | trap-bench exits rc=1 under GNU quickly; probe lost in replay; re-check under post-#256 build | ok | pass |
| `626d591ddb74` | scop/bash-completion/completions-fallback/nox.bash | HANG | RESIDUAL-NEEDS-LIVE-REPLAY | script completes under GNU, no blocking construct found statically (argcomplete fallback eval); replay probe was never seen after source; re | ok | pass |
| `b9bde77bddd4` | GuidoFe/bash-cat-with-cat/cat.sh | HANG,HANG,HANG | RUBASH-BUG-FIXED-#451A | ConPTY input bridge injected 0x15 (^U) kill-line before probe (issue #451 class A, closed/fixed) | ok | pass |
| `7fbb402c5db7` | Kohei-Wada/dotfiles/.claude/statusline.sh | HANG,HANG,HANG | RUBASH-BUG-FIXED-#451A | ConPTY input bridge control-byte injection family (issue #451 class A pinned evidence; closed/fixed) | ok | pass |
| `68ba2902020d` | ZenterFlow/claude-code-plugin-template/scripts/setup.sh | FF,HANG,HANG,HANG | RUBASH-BUG-FIXED-#451A | ConPTY input bridge injected 0x15 (^U) kill-line before probe (issue #451 class A, closed/fixed) | ok | pass |
| `a6f8abc9dde1` | bats-core/bats-core/libexec/bats-core/bats-format-cat | HANG,HANG,HANG | RUBASH-BUG-FIXED-#451A | ConPTY input bridge injected 0x15 (^U) kill-line before probe (issue #451 class A, closed/fixed) | ok | pass |
| `b53a40909380` | zaventh/lintorama/.claude/hooks/lintorama-stop.sh | HANG,HANG,HANG | RUBASH-BUG-FIXED-#451A | ConPTY input bridge control-byte injection family (issue #451 class A pinned evidence; closed/fixed) | ok | pass |
| `6649a710e3d5` | Kohei-Wada/dotfiles/bin/update-nvim.sh | HANG,HANG,SLOW | RUBASH-BUG-FIXED-#451B | post-source first-enter continuation misjudgment (issue #451 class B, fixed by 1b2cd8ee) | ok | pass |
| `5a183aa8af56` | alexyuriev/zdotfiles/.bash_profile | HANG | RUBASH-BUG-FIXED-#451B | post-source first-enter continuation misjudgment (issue #451 class B, fixed by 1b2cd8ee) | ok | pass |
| `f5a2368698ed` | dreadatour/dotfiles/.bash_profile | HANG | RUBASH-BUG-FIXED-#451B | post-source first-enter continuation misjudgment (issue #451 class B, fixed by 1b2cd8ee) | ok | pass |
| `882c03788523` | pnotz17/artix/git/ble.sh/memo/D1665.bashrc2 | HANG | RUBASH-BUG-FIXED-#451B | post-source first-enter continuation misjudgment (issue #451 class B, fixed by 1b2cd8ee) | ok | pass |

## 原判定 GNU-ALSO-FAILS（194 个唯一资产）

| hash | repo/path | 判定史 | 重判类 | 依据 | GNU-n 今日 | rubash-n 今日 |
|---|---|---|---|---|---|---|
| `a9a08130163c` | 3xploitGuy/webscrape/webscrape.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `a5ed5535f6e0` | 4o4R/gymnasticonV2/deploy/pi-sdcard/stage-gymnasticon/00-install-gymn | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `351bfe38d631` | AymanLyesri/ArchEclipse/.config/hypr/scripts/change-resolution.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=1 with 9 error lines | ok | pass |
| `2570a92ae1e9` | BashSupport/BashSupport/resources/highlighterDemoText.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `4848a13ae2b3` | BashSupport/BashSupport/testData/scripts/supers.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=2 with 9 error lines | fail | pass |
| `cf12ea4551a3` | BashSupport/BashSupport/testData/scripts/weird.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `2350489ccbab` | ChristianChiarulli/machfiles/waybar/.config/waybar/scripts/spotify.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `5f56c414bee1` | ColdGrub1384/LibTerm/setup.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `eb30fba24174` | CrackerCat/ZetaSploit/install.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `20e26b150721` | CrackerCat/ZetaSploit/update.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=1 with 1 error lines | ok | pass |
| `e3ac638f3356` | Cybersnake223/Hypr/.config/quickshell/dynamic/modules/watchers/master | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `23ed60f5b3e5` | Cybersnake223/Hypr/.config/quickshell/dynamic/scripts/workspaces.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU wall > 20.0s | ok | pass |
| `876e623d2d24` | JV-conseil/ucloud/incl/.bashrc.txt | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `bff8fa19dad4` | Johnng007/Live-Forensicator/Linux/Forensicator.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=1 with 1 error lines | ok | pass |
| `616e9f6d3322` | Johnng007/Live-Forensicator/MacOS/forensicator.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=1 with 1 error lines | ok | pass |
| `da86ec5c12c3` | Kron4ek/Conty/create-conty.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=1 with 1 error lines | ok | pass |
| `5cdc11b63d63` | Kron4ek/Conty/create-utils.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `798b6e17e4eb` | MacRimi/ProxMenux/scripts/emergency_repair.sh | FF,GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `2b0fe48f9f4d` | Nanako0129/coralline/test/test-burn.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=1 with 39 error lines | ok | pass |
| `94950f36d589` | Nanako0129/coralline/test/test-subagent.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=1 with 8 error lines | ok | pass |
| `78d1f93a06df` | OreosLab/checkinpanel/ins_pkg.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `ed2f90a7c8e5` | OreosLab/checkinpanel/ins_selenium.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `62e811059c06` | Ranrar/wgaf/examples/handbrake.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `f24ba1e49339` | Ranrar/wgaf/examples/mouse.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `d3b1a8032c45` | Ranrar/wgaf/examples/widgets.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `d0bf693e6bb6` | RohitKushvaha01/ReTerminal/.github/workflows/verifyDiff.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `038070aa9375` | Sharan-Sundar/Ubuntu_Init/opencv_init.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `1f93cafa3d82` | Sharan-Sundar/Ubuntu_Init/ubuntu_init.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `f0b546f486bf` | Solutingfg/fgtools/fgenv | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `de47c85e8679` | Soroushnk/Astro/Astro-chineese.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `0951a106c78e` | Soroushnk/Astro/Astro-main.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `a031b37efe3a` | Soroushnk/Astro/Astro-russian.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `093eb1aeda4d` | Soroushnk/Astro/managed/add-user.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `3decf37a0ec3` | Soroushnk/Astro/managed/liberta.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `5fc3a14e0843` | Soroushnk/Astro/managed/reallity_ez.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `d06538bd1c84` | Soroushnk/Astro/managed/repository.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `43b08c66e8b0` | Soroushnk/Astro/managed/speedtest.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `4eeeeb481215` | Soroushnk/Astro/managed/tunnel.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `15b65c297951` | Soroushnk/Astro/managed/v2ray_pannel.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `e5ac68955358` | Soroushnk/Astro/managed/wiregaurd.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `f108c5612e08` | Soroushnk/Astro/managed/wordpress.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=11 with 4986 error lines | ok | pass |
| `febfb6c4bb29` | Soroushnk/Astro/managed/xui_manager.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `22cbc625ad8e` | WaleedMeselhy/init_linux_installation/.zshrc | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `dcfeed1a85c3` | Xtended-Tomato/Xtended_build/envsetup.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=1 with 1 error lines | ok | pass |
| `ceda8b7c3c05` | akinomyoga/ble.sh/lib/init-bind.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=127: missing external dependency | ok | pass |
| `07abdc051c03` | akinomyoga/ble.sh/lib/init-cmap.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `60b339232b1e` | akinomyoga/ble.sh/lib/test-bash.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=127: missing external dependency | ok | pass |
| `7eb1b556da44` | akinomyoga/ble.sh/lib/test-canvas.GraphemeClusterTest.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `00c25fa1483b` | akinomyoga/ble.sh/lib/test-canvas.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=127: missing external dependency | ok | pass |
| `9f2ccce7b362` | akinomyoga/ble.sh/lib/test-keymap.vi.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `0b7609e6fa97` | akinomyoga/ble.sh/lib/test-util.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=127: missing external dependency | ok | pass |
| `dd37bd9fde3d` | akinomyoga/ble.sh/lib/vim-airline.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=127: missing external dependency | ok | pass |
| `43d202db1b94` | alexhack235-code/THUNDER/thunder.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `92e170ff74bd` | apify/mcpc/test/e2e/suites/basic/errors.test.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `c5554b04f60a` | apify/mcpc/test/e2e/suites/basic/grep.test.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `73f3158e7d55` | apify/mcpc/test/e2e/suites/basic/max-chars.test.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `820772e8f713` | apify/mcpc/test/e2e/suites/basic/prompts.test.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `29c327570847` | apify/mcpc/test/e2e/suites/basic/skills.test.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `08122bca6858` | artginzburg/sudo-touchid/tests/install-security.test.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=1 with 1 error lines | ok | pass |
| `07ec5434784f` | attogram/bash-screensavers/gallery/alpha/alpha.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `754b2f675ce5` | attogram/bash-screensavers/gallery/life/life.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `2516a35e38b4` | attogram/bash-screensavers/gallery/noise/noise.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `8bcdc75bef25` | attogram/bash-screensavers/gallery/stars/stars.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `afbc589a399b` | atuinsh/atuin/install.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `67791da127cf` | av/harbor/tests/services-integration.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `b2d68cfa2048` | bach-sh/bach/tests/bach-testing-framework.test.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `403567326774` | bach-sh/bach/tests/demo-xxd.test.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `ca4f0537d6ef` | bach-sh/bach/tests/release.test.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `4df9636b22e5` | bharathraj0007/I_Grow_Smart/.bashrc | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=1 with 164 error lines | ok | pass |
| `6f5ae4348e5c` | clu3bot/owt/troubleshoot.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `c1960b6bf3e4` | donggcao18/tessera/evaluation/rust/repo_data/starship-master/src/init | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=127: missing external dependency | ok | pass |
| `9c52ae6967e3` | dwmkerr/effective-shell/samples/scripts/long-operation.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `64f0ac184538` | emielster/tash/tests/tash-tests.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `2299efa96bde` | emrekybs/Pip-Intel/pip-intel.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `d1cdfa0302dc` | engintron/engintron/nginx/utilities/https_vhosts.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `731386d80b02` | fchastanet/shell-command-bookmarker/scripts/run.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `1d1bbb38297c` | fissible/ptyunit/examples/confirm.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `b42448e9a0ae` | fissible/ptyunit/examples/menu.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `9befaac0d65a` | fissible/ptyunit/self-tests/hostile/neverending.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `2de8e106b116` | fissible/ptyunit/self-tests/unit/test-help.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `5e74efcfa235` | fissible/ptyunit/self-tests/unit/test-mock.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `b5a61b91717b` | g-udi/gaudi-shell/setup.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `f7cf33d06a58` | geoschem/geos-chem/run/GCClassic/createRunDir.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `1c2a261bd92f` | geoschem/geos-chem/run/GCHP/createRunDir.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `dd399ce64de2` | gingerpaledale/zZshFramework/src/baseUtils.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `3ef5ba479541` | golegen/linuxfoundation.org/clock-2.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `4f91f1a2781b` | golegen/linuxfoundation.org/clock.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `68a7007c1798` | gruntwork-io/bash-commons/.circleci/shellcheck.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=127: missing external dependency | ok | pass |
| `ecd15e826427` | gruntwork-io/bash-commons/modules/bash-commons/src/dynamic-ubuntu-wait.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `197f61c6a890` | h4l/json.bash/hack/string_sub_bench.bash | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=1 with 1 error lines | ok | pass |
| `00f8444a2dbe` | ibracorp/ibramenu/ibramenu.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `ee09803dbdbe` | iffse/pay-respects/core/templates/eval.bash | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `eb1afdf08d2e` | iffse/pay-respects/core/templates/eval.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `e250c609184b` | iffse/pay-respects/core/templates/init.bash | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `d6da4fcc1dab` | ihack4falafel/OSCP/BASH/CronJobChecker.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `3e0c4cd291b7` | ihack4falafel/OSCP/BASH/LinuxPrivCheck.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `0f3b57807289` | ihack4falafel/OSCP/BASH/SUIDChecker.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `1c4cf9565ec7` | ingydotnet/git-subrepo/note/test.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `390fd300c864` | jacobsalmela/dotfiles/.bash_profile | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `d6f65cd83fc7` | jbpayton/shelldweller/homestead/run.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `dac9097f8b64` | jbpayton/shelldweller/tests/cases/19_http_api_server/probe.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `01261b0f3887` | jbpayton/shelldweller/tests/cases/20_task_inbox/probe.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `def2bcd71701` | jinqians/snell.sh/snell-docker.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `1ced7e3c2046` | joelindra/Hades/modules/injection/local_file_inclusion.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `b1e3d474a757` | joelindra/Hades/modules/injection/mass_sql_injection.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `c697dc9d8965` | joelindra/Hades/modules/injection/mass_xss_testing.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `871ef738feca` | joelindra/Hades/modules/injection/single_sql_injection.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `78a5229db659` | joelindra/Hades/modules/injection/single_xss_testing.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `198ee92befdd` | joelindra/Hades/modules/recon/single_reconnaissance.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `1aa947a4ac95` | joelindra/Hades/modules/special/directory_enumeration.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `c620e04a9fc2` | joelindra/Hades/modules/special/javascript_secret_finder.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `4547d9c54b90` | joelindra/Hades/modules/special/mass_cors_testing.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `594ffe7dff5c` | joelindra/Hades/modules/special/mass_vulnerability_assessment.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `17e756df746c` | joelindra/Hades/modules/special/subdomain_takeover_detection.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `a017f271a903` | kazhala/dotbare/tests/shellcheck.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=127: missing external dependency | ok | pass |
| `0c4fbce2138f` | kevinlekiller/reshade-steam-proton/reshade-steam-proton.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `c218e6a6c4f5` | ko1nksm/getoptions/spec/getoptions_base_spec.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `1e7e7cf292cf` | koanlogic/makl/pkgs/rel-git.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `fe70d63de682` | lastweek/dot-home/.bash_profile | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `8695af7a795a` | lavabit/robox/http/generic.gentoo.docker.x64.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `7818c371fed7` | lavabit/robox/http/generic.gentoo.vagrant.a64.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `3b38303e3011` | lavabit/robox/http/generic.gentoo.vagrant.x64.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `2fc6fded5741` | lavabit/robox/http/magma.gentoo.vagrant.x64.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `dd6989db6f1e` | lavabit/robox/robox.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=2 with 1089 error lines | ok | pass |
| `c20efdcb1c79` | linusbehrens/website/output/git/website/dotfiles/file/.config/bash/.bas | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `5aa2502e418e` | luisadha/alrc-termux/lib/progsche.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `8ac4a8916ecf` | microsoft/shell-intune-samples/macOS/Apps/Defender/installDefender.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `766c6308a2d0` | microsoft/shell-intune-samples/macOS/Apps/Edge/installEdge.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `74091ae385a8` | microsoft/shell-intune-samples/macOS/Apps/FileZilla/installFileZilla.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `399d84e60ecc` | microsoft/shell-intune-samples/macOS/Apps/Firefox/installFirefox.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `600bb515767b` | microsoft/shell-intune-samples/macOS/Apps/Gimp/InstallGimp.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `5fdcc857d35b` | microsoft/shell-intune-samples/macOS/Apps/ImageOptim/installImageOptim.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `ad9fd6c58d7c` | microsoft/shell-intune-samples/macOS/Apps/Python/installPython3.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `bc58eae3af95` | microsoft/shell-intune-samples/macOS/Apps/Slack/installApp3.01-Slack.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `eb219a4694ac` | microsoft/shell-intune-samples/macOS/Apps/WhatsApp/installWhatsApp.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `84e5fc708194` | microsoft/shell-intune-samples/macOS/Apps/Zoom/installZoom.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `d4b2aeb06245` | microsoft/shell-intune-samples/macOS/Apps/Zscaler/installZscaler.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `958257578c80` | microsoft/shell-intune-samples/macOS/Config/DeviceRename/DeviceRename.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=1 with 2 error lines | ok | pass |
| `eb06f32191f1` | microsoft/shell-intune-samples/macOS/Config/DeviceRename/DeviceRename2.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=1 with 3 error lines | ok | pass |
| `18b8157b6e9e` | microsoft/shell-intune-samples/macOS/Config/Dock/dockv5.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `704c54dc6206` | microsoft/shell-intune-samples/macOS/Config/Octory/installOctory.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=1 with 2 error lines | ok | pass |
| `d1502d5ff356` | microsoft/shell-intune-samples/macOS/Config/setTimeZone/setTZfromIP.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=1 with 1 error lines | ok | pass |
| `504a6631ba22` | mitrydoug/dotfiles/.zshrc | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=127: missing external dependency | ok | pass |
| `5ffb5687a2e0` | modernish/modernish/share/doc/modernish/tests/harden-test.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=128 with 16 error lines | ok | pass |
| `e4681933e2a7` | modernish/modernish/share/doc/modernish/tests/local-test.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `2f86bc131ba2` | mohsiur/oh-my-zsh-bash/custom.bash_profile | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `7fcce1a0e25e` | multiverse-os/sh/examples/simple.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `d45fcb1884bd` | multiverse-os/sh/scripts/bashrc.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `f649af7dbe21` | muthuishere/mcp-server-bash-sdk/docs/spikes/spike-01-stdio-viability.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `95a607258971` | nanlabs/aws-glue-etl-boilerplate/.devcontainer/docker/awsglue/start-jupyter.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `e0ca5323a410` | nickexyz/ntfy-shellscripts/ntfy-fail2ban/ntfy-fail2ban.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `c2bc92ae6231` | nuver-labs/vps-audit/vps-audit.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `7d642e3d6a93` | odb/shml/examples/progress_bar.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=127: missing external dependency | ok | pass |
| `289649d088a8` | ohmybash/oh-my-bash/plugins/git/git.plugin.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `aff8315dafd3` | picodotdev/alis/download-sid.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=127: missing external dependency | ok | pass |
| `64d2d16546df` | picodotdev/alis/site/install.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `e0e75bfd022d` | pjmeyer/vscode-demoready/.bash_profile | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `4176c11b9ca3` | r1cksec/cheatsheets/snippet/sh/endlessWhile.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `ac2233b1e2c5` | rcaloras/bashhub-client/install-bashhub.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `056e67668043` | rnnh/bioinfo-notebook/scripts/genome_annotation_SwissProt_CDS.sh | FF,GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `de56929409c9` | ropery/FFcast/src/ffcast.bash | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `a4d8796a4f4e` | ruanbekker/cheatsheets/codedeploy/appspec/versioned/scripts/validate_serv | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `aece3608370d` | s-r-e-e-r-a-j/WebSift/WebSift/websift.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `2ebe94ccd916` | shellspec/shellspec/contrib/check.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=1 with 24 error lines | ok | pass |
| `4eeaa68911e0` | shellspec/shellspec/lib/general.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `27d50ee20a1a` | shellspec/shellspec/spec/general_spec.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `034ad30cc217` | shellspec/shellspec/spec/install_spec.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `41d154198e27` | shellspec/shellspec/spec/libexec_spec.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `1b54d05fbb9d` | spiritLHLS/one-click-installation-script/install_scripts/go.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `405cf76bbbb5` | theahmadov/NIVOS/nivo/arch.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=2 with 37 error lines | ok | pass |
| `525c14558aa8` | theahmadov/NIVOS/nivo/debian.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `c6225609ace1` | webpro/dotfiles/macos/defaults.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU timeout(1) killed it (rc=124): genuinely long-running | ok | pass |
| `e7fcb87e56d3` | whilei/dotfiles/.tofu | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `4c819fbf7b6f` | x4nd3r/dotfiles/.bashrc | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU fails too (runtime error class) | fail | fail |
| `65af005602f6` | zenithbuild/framework/scripts/smoke.sh | GAF | GNU-ALSO-FAILS-CONFIRMED | GNU rc=127: missing external dependency | ok | pass |
| `8e745b429343` | ohmybash/oh-my-bash/lib/cli.bash | GAF,OK,SR,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `53176603fa26` | scop/bash-completion/completions-core/7z.bash | GAF,GAF,SR,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `e9aec3635cb5` | scop/bash-completion/completions-core/cvs.bash | GAF,GAF,SR,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `35e2931f1e9c` | scop/bash-completion/completions-core/ps.bash | GAF,GAF,SR,SR | GNU-PARITY-CONFIRMED | parse-time dependency (GNU bash -n also rejects today) | fail | fail |
| `2466744d0a63` | ohmyzsh/ohmyzsh/tools/changelog.sh | GAF,GAF,SR,SR | RECLASSIFIED | ZSH-DIALECT (GNU bash also rejects; attribution: zsh, not rubash) | fail | fail |
| `83f511863ffe` | ohmyzsh/ohmyzsh/tools/check_for_upgrade.sh | GAF,GAF,SR,SR | RECLASSIFIED | ZSH-DIALECT (GNU bash also rejects; attribution: zsh, not rubash) | fail | fail |
| `e50bcce537f7` | ButterBright/zsh/.zim/modules/powerlevel10k/internal/p10k.zsh | GAF | ZSH-DIALECT | zsh syntax asset; GNU also fails | fail | fail |
| `1193540e6029` | ComradeWave/.dotfiles/config/zsh/ohmyzsh/custom/themes/powerlevel10k/int | GAF | ZSH-DIALECT | zsh syntax asset; GNU also fails | fail | fail |
| `35e73d7c8d9d` | Kqpa/zeesh/.install.sh | GAF | ZSH-DIALECT | zsh syntax asset; GNU also fails | fail | fail |
| `4c5fc77da8dd` | Kqpa/zeesh/.main.sh | GAF | ZSH-DIALECT | zsh syntax asset; GNU also fails | fail | fail |
| `0942c1c10c90` | bonlut/bonlut_dotfiles/oh-my-zsh/.oh-my-zsh/custom/themes/powerlevel10k/i | GAF | ZSH-DIALECT | zsh syntax asset; GNU also fails | fail | fail |
| `42076159c253` | gingerpaledale/zZshFramework/src/android/android.sh | GAF | ZSH-DIALECT | zsh syntax asset; GNU also fails | fail | fail |
| `6f7c47577f8a` | gingerpaledale/zZshFramework/src/git/gitBasic.sh | GAF | ZSH-DIALECT | zsh syntax asset; GNU also fails | fail | fail |
| `15ee256b2fa3` | gingerpaledale/zZshFramework/src/homebrew.sh | GAF | ZSH-DIALECT | zsh syntax asset; GNU also fails | fail | fail |
| `64e08c4777e9` | isaiash/dotfiles/dot_oh-my-zsh/custom/themes/powerlevel10k/internal | GAF | ZSH-DIALECT | zsh syntax asset; GNU also fails | fail | fail |
| `a8662762d59f` | ken-okabe/devcontainer-fsharp/.devcontainer/isolated-zsh-config/homeuser/.zsh/po | GAF | ZSH-DIALECT | zsh syntax asset; GNU also fails | fail | fail |
| `45f4ccb67356` | lincheney/fzf-tab-completion/zsh/fzf-zsh-completion.sh | GAF | ZSH-DIALECT | zsh syntax asset; GNU also fails | fail | fail |
| `22b3ac9a93f3` | ohmyzsh/ohmyzsh/tools/upgrade.sh | GAF | ZSH-DIALECT | zsh syntax asset; GNU also fails | fail | fail |
| `29b0273690ff` | soerface/dotfiles/.zsh/powerlevel10k/internal/p10k.zsh | GAF | ZSH-DIALECT | zsh syntax asset; GNU also fails | fail | fail |

## 原判定 SLOW（347 个唯一资产）

| hash | repo/path | 判定史 | 重判类 | 依据 | GNU-n 今日 | rubash-n 今日 |
|---|---|---|---|---|---|---|
| `03a587ef4a2f` | 0verread/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2376ms == harness floor ~2300ms; source_wall 7106ms vs OK-median 84ms) | ok | pass |
| `c469a5dc50c0` | 0xacx/chatGPT-shell-cli/internal_dev/debmaker.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2292ms == harness floor ~2300ms; source_wall 6100ms vs OK-median 84ms) | ok | pass |
| `1dbb4152c953` | 1kyc/dev-container-templates/.devcontainer/setup.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2297ms == harness floor ~2300ms; source_wall 9837ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `cbbd44ed4da5` | Andrew565/dotfiles/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2294ms == harness floor ~2300ms; source_wall 7187ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `a5168f626e72` | AprilJack/igc_script_backup/.bashScript | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2325ms == harness floor ~2300ms; source_wall 5010ms vs OK-median 84ms) | ok | pass |
| `40aa93686938` | AyamotoKohei/dotfiles/bash/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2306ms == harness floor ~2300ms; source_wall 2382ms vs OK-median 84ms) | ok | pass |
| `35160cada2de` | AymanLyesri/ArchEclipse/.config/fastfetch/fastfetch.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2307ms == harness floor ~2300ms; source_wall 2235ms vs OK-median 84ms) | ok | pass |
| `d2eaeedfcbee` | AymanLyesri/ArchEclipse/.config/hypr/scripts/compile-run-binaries.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2310ms == harness floor ~2300ms; source_wall 3788ms vs OK-median 84ms) | ok | pass |
| `720722d75734` | BashSupport/BashSupport/testData/scripts/checkfs.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2333ms == harness floor ~2300ms; source_wall 5186ms vs OK-median 84ms) | ok | pass |
| `1d538e79010a` | BradHeff/ArxisOS/branding/skel/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2329ms == harness floor ~2300ms; source_wall 5618ms vs OK-median 84ms) | ok | pass |
| `70450dd65c35` | CosmicSyntax/dotfiles/others/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2298ms == harness floor ~2300ms; source_wall 5344ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `be21f364c1ed` | CpOuyang/custom/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2309ms == harness floor ~2300ms; source_wall 4861ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `a28b62a7f42f` | Cyclenerd/static_status/test.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2295ms == harness floor ~2300ms; source_wall 2311ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `9d3fca9c8bb7` | DmitriyPanteleev/my-server-automation/bash_environtment/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2377ms == harness floor ~2300ms; source_wall 1729ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `c226f72b2b22` | DvdGiessen/smartos-configuration/custom/cfg/root/root/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2307ms == harness floor ~2300ms; source_wall 6158ms vs OK-median 84ms) | ok | pass |
| `142412f01e71` | FNNDSC/scripts/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2347ms == harness floor ~2300ms; source_wall 6163ms vs OK-median 84ms) | ok | pass |
| `0841bed9c50e` | GH05T-HUNTER5/GH05T-INSTA/setup.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 3860ms vs OK-median 84ms) | ok | pass |
| `c4b8848c44cb` | GHolk/loco/bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2297ms == harness floor ~2300ms; source_wall 6102ms vs OK-median 84ms) | ok | pass |
| `b11638d15d73` | Gictorbit/illustratorCClinux/scripts/uninstaller.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 6164ms vs OK-median 84ms) | ok | pass |
| `4fde6af190c2` | Grasseh/dotfiles/links/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 9549ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `5272f46d88b9` | Hackgodybj/Symbian/wget_upload.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2301ms == harness floor ~2300ms; source_wall 10740ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `5401f4914529` | HirbodBehnam/Shadowsocks-Cloak-Installer/Cloak2-Installer.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2382ms == harness floor ~2300ms; source_wall 6119ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `d4e6e5c8fe2b` | HirbodBehnam/Shadowsocks-Cloak-Installer/Shadowsocks-Cloak-Installer.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2382ms == harness floor ~2300ms; source_wall 6089ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `ba424a79c6e8` | Hyde46/hoard/install.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2297ms == harness floor ~2300ms; source_wall 6117ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `88d7e19833ca` | IBM/clai/clai/server/plugins/gitbot/run_rasa.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2319ms == harness floor ~2300ms; source_wall 6083ms vs OK-median 84ms) | ok | pass |
| `7d9008086f8d` | IBM/clai/scripts/clai.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2310ms == harness floor ~2300ms; source_wall 3615ms vs OK-median 84ms) | ok | pass |
| `48bc317e6b78` | IBM/clai/utils/getMakeType.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2379ms == harness floor ~2300ms; source_wall 6084ms vs OK-median 84ms) | ok | pass |
| `1e632d0803a1` | IBM/clai/utils/getVersionInfo.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2301ms == harness floor ~2300ms; source_wall 6097ms vs OK-median 84ms) | ok | pass |
| `360d7ab88287` | IQBE/dotfiles/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2322ms == harness floor ~2300ms; source_wall 2211ms vs OK-median 84ms) | ok | pass |
| `848f1577d263` | IshDeshpa/.dotfiles/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 2212ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `c476fe543a4d` | JackDanger/dotfiles/bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2297ms == harness floor ~2300ms; source_wall 2794ms vs OK-median 84ms) | ok | pass |
| `2c45db0b3452` | K-RD/dotFiles/.bash_profile.bak | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2294ms == harness floor ~2300ms; source_wall 2468ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `38af96ac6259` | Kelsidavis/Demonology/restart-llama.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2308ms == harness floor ~2300ms; source_wall 10077ms vs OK-median 84ms) | ok | pass |
| `efe7269e3e3f` | LordDarkula/dotkula/bash/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2334ms == harness floor ~2300ms; source_wall 5932ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `eb91cc74d65d` | MacRimi/ProxMenux/scripts/oci/oci_manager.sh | FF,FF,SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2311ms == harness floor ~2300ms; source_wall 6097ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `d1c3225e8918` | MarkPolivchuk/configs/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2301ms == harness floor ~2300ms; source_wall 4871ms vs OK-median 84ms) | ok | pass |
| `69d1d5998684` | MatheuZSecurity/D3m0n1z3dShell/install.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2297ms == harness floor ~2300ms; source_wall 5263ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `60ef4c78edfa` | Melvillian/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2303ms == harness floor ~2300ms; source_wall 6156ms vs OK-median 84ms) | ok | pass |
| `a1fce8789e90` | MichelfrancisBustillos/ansible_pull/files/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2299ms == harness floor ~2300ms; source_wall 2066ms vs OK-median 84ms) | ok | pass |
| `49f4b7278d0a` | MidnightCoding1/mForce/modules/config_leak_detector.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 6090ms vs OK-median 84ms) | ok | pass |
| `5b2121a2e862` | MidnightCoding1/mForce/modules/domain_osint.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2299ms == harness floor ~2300ms; source_wall 6089ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `ee7b487c6830` | MidnightCoding1/mForce/modules/ip_osint.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2305ms == harness floor ~2300ms; source_wall 6114ms vs OK-median 84ms) | ok | pass |
| `44bcf9b2ba6a` | MidnightCoding1/mForce/modules/password_generator.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2312ms == harness floor ~2300ms; source_wall 9687ms vs OK-median 84ms) | ok | pass |
| `d6f48d1a95b8` | MidnightCoding1/mForce/modules/public_exposure.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2304ms == harness floor ~2300ms; source_wall 6164ms vs OK-median 84ms) | ok | pass |
| `b1de4c5fe1bb` | MidnightCoding1/mForce/modules/subdomain_enum.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2336ms == harness floor ~2300ms; source_wall 6093ms vs OK-median 84ms) | ok | pass |
| `3e1c73cc62ef` | Minituff/nautical-backup/dev/create_test_backups_container_date.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2311ms == harness floor ~2300ms; source_wall 2388ms vs OK-median 84ms) | ok | pass |
| `9aa2e78bcf20` | Minituff/nautical-backup/dev/create_test_backups_date_container.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2310ms == harness floor ~2300ms; source_wall 2544ms vs OK-median 84ms) | ok | pass |
| `18ceb78dc727` | Mr-MIBonk/M.I.B._More-Incredible-Bash/esd/scripts/gem.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2319ms == harness floor ~2300ms; source_wall 3063ms vs OK-median 84ms) | ok | pass |
| `59daf6afd158` | Mr-MIBonk/M.I.B._More-Incredible-Bash/esd/scripts/rnand.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2309ms == harness floor ~2300ms; source_wall 3133ms vs OK-median 84ms) | ok | pass |
| `85f862332e8a` | Mr-MIBonk/M.I.B._More-Incredible-Bash/esd/scripts/rsdb.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 3052ms vs OK-median 84ms) | ok | pass |
| `49a25f27001d` | Mr-MIBonk/M.I.B._More-Incredible-Bash/esd/scripts/sub2.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2311ms == harness floor ~2300ms; source_wall 3127ms vs OK-median 84ms) | ok | pass |
| `01f4913144a4` | Mr-MIBonk/M.I.B._More-Incredible-Bash/esd/scripts/sub4.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2310ms == harness floor ~2300ms; source_wall 3127ms vs OK-median 84ms) | ok | pass |
| `3f7ebb57c22c` | Mr-MIBonk/M.I.B._More-Incredible-Bash/esd/scripts/sub6.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2297ms == harness floor ~2300ms; source_wall 3051ms vs OK-median 84ms) | ok | pass |
| `3014f5860636` | Mr-MIBonk/M.I.B._More-Incredible-Bash/esd/scripts/sub8.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2301ms == harness floor ~2300ms; source_wall 3117ms vs OK-median 84ms) | ok | pass |
| `11d225c04a71` | Mr-MIBonk/M.I.B._More-Incredible-Bash/tests/test_lock_files.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2381ms == harness floor ~2300ms; source_wall 6193ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `68671d4faa4a` | Nanako0129/coralline/configure.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2299ms == harness floor ~2300ms; source_wall 6166ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `e5fedf6b41d9` | Nanako0129/coralline/statusline.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2380ms == harness floor ~2300ms; source_wall 9705ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `67096c31cb1a` | Nanako0129/coralline/test/test-float-companion.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2391ms == harness floor ~2300ms; source_wall 6108ms vs OK-median 84ms) | ok | pass |
| `bb1c30a535d6` | Nanako0129/coralline/test/test-upgrade.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2308ms == harness floor ~2300ms; source_wall 3109ms vs OK-median 84ms) | ok | pass |
| `92e3290cd7a0` | NeonMan/shellUnit/doc/mkdoc.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2335ms == harness floor ~2300ms; source_wall 5058ms vs OK-median 84ms) | ok | pass |
| `90c09179f369` | OSGeo/grass-legacy/lib/init/init.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2296ms == harness floor ~2300ms; source_wall 6101ms vs OK-median 84ms) | ok | pass |
| `690291d9c7a8` | P-manBrown/tascon-backend-mirror/.devcontainer/postCreateCommand.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2311ms == harness floor ~2300ms; source_wall 2209ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `eee03cf2eeed` | PhilippFeO/dotfiles/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 6091ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `01d94a148987` | Pvag/config_files/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2294ms == harness floor ~2300ms; source_wall 4775ms vs OK-median 84ms) | ok | pass |
| `00178e7c603f` | RomanWlm/Env-n-Tips/env/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2301ms == harness floor ~2300ms; source_wall 5012ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `a784dcaa5be9` | RoversX/ThinkTerm/ci/source-archive.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2319ms == harness floor ~2300ms; source_wall 6090ms vs OK-median 84ms) | ok | pass |
| `bedddf64b03a` | SolarDrill/contabilidad_api/contabilidad_api/.devcontainer/bashrc.override.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2309ms == harness floor ~2300ms; source_wall 6088ms vs OK-median 84ms) | ok | pass |
| `7f949bc24659` | Soroushnk/Astro/managed/IPsec.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2311ms == harness floor ~2300ms; source_wall 9716ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `bf1e2afbf232` | Soroushnk/Astro/managed/ssh_panel.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2392ms == harness floor ~2300ms; source_wall 6087ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `c5cd8b3bf55e` | Soroushnk/Astro/managed/system-usage.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2298ms == harness floor ~2300ms; source_wall 6105ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `7194db679a38` | StannisZhou/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2379ms == harness floor ~2300ms; source_wall 9400ms vs OK-median 84ms) | ok | pass |
| `9637f71993e3` | TermuxArch/TermuxArch/pullTermuxArchMods.bash | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2297ms == harness floor ~2300ms; source_wall 6155ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `7dc9dffd6ac4` | ThomasBush/dev/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2314ms == harness floor ~2300ms; source_wall 5785ms vs OK-median 84ms) | ok | pass |
| `bb2871aa5dd0` | Twilight4/nobility/scripts/cve.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2483ms == harness floor ~2300ms; source_wall 6159ms vs OK-median 84ms) | ok | pass |
| `f0ee5a621158` | TypedDevs/bashunit/install.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2312ms == harness floor ~2300ms; source_wall 5188ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `cedd3907421d` | XuehaiPan/Dev-Setup/my-dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2317ms == harness floor ~2300ms; source_wall 10448ms vs OK-median 84ms) | ok | pass |
| `18479b9f5e83` | ZaleHack/phpexploit/deps/PySocks-1.4.2-61-g805d716/test/test.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2305ms == harness floor ~2300ms; source_wall 2142ms vs OK-median 84ms) | ok | pass |
| `a6e305e5c65b` | abs-lang/abs/docs/src/.vuepress/dist/installer.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2295ms == harness floor ~2300ms; source_wall 6132ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `47ed15cd6f13` | abstractmachines/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2299ms == harness floor ~2300ms; source_wall 7132ms vs OK-median 84ms) | ok | pass |
| `19c35fe4d7a1` | acoustep/dotfiles/home/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2321ms == harness floor ~2300ms; source_wall 5937ms vs OK-median 84ms) | ok | pass |
| `45bfe88d0a48` | adrianp/dotfiles/bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2299ms == harness floor ~2300ms; source_wall 2798ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `40f433c207cb` | aerialboundaries/dotfiles/bashrc/ubuntu/general/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 7465ms vs OK-median 84ms) | ok | pass |
| `f86fc559df18` | ahalfpen727/.dotfiles/.bash_profile_example | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2316ms == harness floor ~2300ms; source_wall 2586ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `da30b84843f9` | ajohnstone/dot-files/bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2292ms == harness floor ~2300ms; source_wall 6156ms vs OK-median 84ms) | ok | pass |
| `d03af0df1ac3` | akinomyoga/ble.sh/src/canvas.c2w.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2323ms == harness floor ~2300ms; source_wall 2457ms vs OK-median 84ms) | ok | pass |
| `df005b756623` | alan/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2307ms == harness floor ~2300ms; source_wall 6093ms vs OK-median 84ms) | ok | pass |
| `53cc0db99de5` | alanb1501/dotconfig/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2296ms == harness floor ~2300ms; source_wall 6997ms vs OK-median 84ms) | ok | pass |
| `92f782d5a8ba` | anonimitoraf/.dotfiles/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2294ms == harness floor ~2300ms; source_wall 6084ms vs OK-median 84ms) | ok | pass |
| `8b6bd8bc6760` | antonyo-turco/dotfiles/linux/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 2222ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `8b715d17f6da` | aorumbayev/autogpt4all/autogpt4all.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 10432ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `51508ec94f2f` | apify/mcpc/test/e2e/suites/basic/insecure.test.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2304ms == harness floor ~2300ms; source_wall 2141ms vs OK-median 84ms) | ok | pass |
| `0a7089cf0432` | apify/mcpc/test/e2e/suites/sessions/proxy.test.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2386ms == harness floor ~2300ms; source_wall 2218ms vs OK-median 84ms) | ok | pass |
| `3d62e5a9d00b` | artginzburg/sudo-touchid/install.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2304ms == harness floor ~2300ms; source_wall 2377ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `9dc89c9dc417` | atoulme/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 5184ms vs OK-median 84ms) | ok | pass |
| `9d3c8216e3bc` | atrexel/custom_config/MacConfigs/mac/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2299ms == harness floor ~2300ms; source_wall 4762ms vs OK-median 84ms) | ok | pass |
| `cc4f5ea5ebac` | attogram/bash-screensavers/jury/assemble-the-jury.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2309ms == harness floor ~2300ms; source_wall 6108ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `cb25cd7a669e` | av/harbor/.scripts/terser.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2299ms == harness floor ~2300ms; source_wall 6163ms vs OK-median 84ms) | ok | pass |
| `ac53c2cd59b5` | av/harbor/.scripts/test-requirements-macos.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2308ms == harness floor ~2300ms; source_wall 2132ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `1cbf0bff86e7` | azagramac/bashrc-for-debian-macos/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2319ms == harness floor ~2300ms; source_wall 6103ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `76ec69759352` | b2cc/shellscripts/bash/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2316ms == harness floor ~2300ms; source_wall 2805ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `4a196cc0663e` | basherpm/basher/install.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2319ms == harness floor ~2300ms; source_wall 5174ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `8cf4bcaf3671` | bats-core/bats-core/libexec/bats-core/bats-exec-suite | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2294ms == harness floor ~2300ms; source_wall 4688ms vs OK-median 84ms) | ok | pass |
| `f5b1727b46d2` | bats-core/bats-core/libexec/bats-core/bats-format-pretty | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2309ms == harness floor ~2300ms; source_wall 3689ms vs OK-median 84ms) | ok | pass |
| `458feff6c266` | bats-core/bats-core/libexec/bats-core/bats-preprocess | FF,SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2539ms == harness floor ~2300ms; source_wall 4685ms vs OK-median 84ms) | ok | pass |
| `a1cfebd08f1c` | bcherry/bcherry/configs/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2296ms == harness floor ~2300ms; source_wall 7001ms vs OK-median 84ms) | ok | pass |
| `c5773f01bf2d` | bensk/SE8_p5js/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2378ms == harness floor ~2300ms; source_wall 5188ms vs OK-median 84ms) | ok | pass |
| `dd3d709a4103` | binpash/pash/scripts/up.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2305ms == harness floor ~2300ms; source_wall 5517ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `0a043501ebf8` | bjpirt/dotfiles/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2381ms == harness floor ~2300ms; source_wall 4860ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `607a72cb9299` | bnekolny/utilities/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2303ms == harness floor ~2300ms; source_wall 247ms vs OK-median 84ms) | ok | pass |
| `002915b4865a` | brunofjesus/dotfiles/bash/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 6105ms vs OK-median 84ms) | ok | pass |
| `7731e503e89c` | calbach/dotfiles/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 5027ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `c6838b708e65` | carlthuringer/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2294ms == harness floor ~2300ms; source_wall 6097ms vs OK-median 84ms) | ok | pass |
| `145c4d5f6c16` | centminmod/centminmod/installer-edge.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 6159ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `166fcd98cfbc` | centminmod/centminmod/installer-latest.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2381ms == harness floor ~2300ms; source_wall 6084ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `0c94f2ca9d47` | centminmod/centminmod/installer.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2296ms == harness floor ~2300ms; source_wall 6159ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `66423457ee65` | centminmod/centminmod/installer55-gitlab.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2301ms == harness floor ~2300ms; source_wall 6096ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `b652c8b43110` | centminmod/centminmod/installer55.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2295ms == harness floor ~2300ms; source_wall 6110ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `719d95f9b3d2` | centminmod/centminmod/installer56-gitlab.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2386ms == harness floor ~2300ms; source_wall 6119ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `28c1a62d0ecc` | centminmod/centminmod/installer56.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2379ms == harness floor ~2300ms; source_wall 6102ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `6054bedae9ba` | centminmod/centminmod/installer7.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2296ms == harness floor ~2300ms; source_wall 6161ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `a14941dcd89b` | centminmod/centminmod/installer71.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 6109ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `7306d5ad205a` | centminmod/centminmod/installer72.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2305ms == harness floor ~2300ms; source_wall 6153ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `e91c826af979` | centminmod/centminmod/installer73.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2299ms == harness floor ~2300ms; source_wall 6088ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `3fcdc1c446d3` | centminmod/centminmod/installer74.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2303ms == harness floor ~2300ms; source_wall 6164ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `ee2ca40e2c61` | centminmod/centminmod/installer80.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 6101ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `b7350e8bdf55` | centminmod/centminmod/installer81.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2315ms == harness floor ~2300ms; source_wall 6162ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `43810e8ac097` | centminmod/centminmod/installer82.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2299ms == harness floor ~2300ms; source_wall 6120ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `017b6a609c17` | centminmod/centminmod/installer83.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2301ms == harness floor ~2300ms; source_wall 6157ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `0ea6cf58dd0d` | centminmod/centminmod/installer84.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 6137ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `f66c3c967615` | centminmod/centminmod/installer85.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2303ms == harness floor ~2300ms; source_wall 6103ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `bff856c2d5b9` | cervebar/useful-garbage/bash/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2294ms == harness floor ~2300ms; source_wall 5193ms vs OK-median 84ms) | ok | pass |
| `3a371d7dd18e` | cgsdev0/bash-stack/examples/bg-task/task.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2310ms == harness floor ~2300ms; source_wall 5097ms vs OK-median 84ms) | ok | pass |
| `2db256a22e8b` | chevalmuscle/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2296ms == harness floor ~2300ms; source_wall 4916ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `4b267243718e` | chriswarren/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2295ms == harness floor ~2300ms; source_wall 2793ms vs OK-median 84ms) | ok | pass |
| `c8b81984f650` | chyld/devops/ARCHIVE/2024-JAN/dotfiles/macos/current/.bash_prof | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2314ms == harness floor ~2300ms; source_wall 2703ms vs OK-median 84ms) | ok | pass |
| `fd26ddddd7e9` | chyld/devops/ARCHIVE/2024-JAN/dotfiles/macos/mac-osx-2016-07/.b | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2314ms == harness floor ~2300ms; source_wall 2542ms vs OK-median 84ms) | ok | pass |
| `4eaa952b2a3e` | chyld/devops/ARCHIVE/2024-JAN/dotfiles/vagrant/old/.bash_profil | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2319ms == harness floor ~2300ms; source_wall 2130ms vs OK-median 84ms) | ok | pass |
| `3afc318869b2` | cipher387/linux-for-OSINT-21-day/Day_2/conditional.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2389ms == harness floor ~2300ms; source_wall 6137ms vs OK-median 84ms) | ok | pass |
| `fa08f74098c4` | claudiosecco/bash-conf/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2380ms == harness floor ~2300ms; source_wall 6162ms vs OK-median 84ms) | ok | pass |
| `408e58d0c26e` | cloudfoundry/loggregator-dotfiles/bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2293ms == harness floor ~2300ms; source_wall 6117ms vs OK-median 84ms) | ok | pass |
| `06d02cc1ac3b` | clu3bot/owt/owt.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2312ms == harness floor ~2300ms; source_wall 6086ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `70622ba18101` | codebend3r/che/install.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2376ms == harness floor ~2300ms; source_wall 5501ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `7e7e6b7c7116` | coondog0630/configfiles/bash/.bash_profile.macosx | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2389ms == harness floor ~2300ms; source_wall 5258ms vs OK-median 84ms) | ok | pass |
| `59a2b3248562` | coreyti/osx-setup/conf/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 7056ms vs OK-median 84ms) | ok | pass |
| `5ddb997aa6af` | coursehero/slacktee/test/test.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2325ms == harness floor ~2300ms; source_wall 6103ms vs OK-median 84ms) | ok | pass |
| `dcaa86ecb708` | cr1315/.rayrc/bash/10_bash/main.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2298ms == harness floor ~2300ms; source_wall 6090ms vs OK-median 84ms) | None | fail |
| `0b10e8546ead` | cubieboard/linux-sdk-card-products/ct/ct-linaro-server/overlay/root/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2382ms == harness floor ~2300ms; source_wall 6083ms vs OK-median 84ms) | ok | pass |
| `fb6820f376e9` | cufta22/dotfiles/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2315ms == harness floor ~2300ms; source_wall 2143ms vs OK-median 84ms) | ok | pass |
| `bed704c4957f` | dan-relvas/dotfiles/bash-mac/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 4842ms vs OK-median 84ms) | ok | pass |
| `bdf87f9b35d0` | darenjacobs/dotfiles/.bak/.bash_profile.mac | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2312ms == harness floor ~2300ms; source_wall 10487ms vs OK-median 84ms) | ok | pass |
| `89def6b2b0aa` | davidsonfellipe/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2315ms == harness floor ~2300ms; source_wall 2481ms vs OK-median 84ms) | ok | pass |
| `c6ad36acd03c` | deifante/misc-scripts/benevity.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 4771ms vs OK-median 84ms) | ok | pass |
| `7ca7f54e63e2` | dgurns/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2378ms == harness floor ~2300ms; source_wall 11488ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `95ae2accff24` | dhollenbeck/cheat-sheets/mac/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2295ms == harness floor ~2300ms; source_wall 5672ms vs OK-median 84ms) | ok | pass |
| `a74cc98f7f2d` | digitalxs/dxsbash/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2336ms == harness floor ~2300ms; source_wall 2558ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `6ff02dc3a375` | dim-an/cod/wc.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2312ms == harness floor ~2300ms; source_wall 6163ms vs OK-median 84ms) | ok | pass |
| `d8b4753cdb2b` | dpflann/dots/bash_profile/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2312ms == harness floor ~2300ms; source_wall 2548ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `52655d425746` | dragonis41/dotfiles/config/Manjaro/root/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2298ms == harness floor ~2300ms; source_wall 6090ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `f8f244d14211` | dshramko/help/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2379ms == harness floor ~2300ms; source_wall 4946ms vs OK-median 84ms) | ok | pass |
| `2368ec2dccd8` | ducksrfr/mac_admin/scripts/install-latest-ms-office.sh | FF,SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2303ms == harness floor ~2300ms; source_wall 3452ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `1e018d985bd7` | ducksrfr/mac_admin/scripts/upload_logs.sh | FF,SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2301ms == harness floor ~2300ms; source_wall 2539ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `12d76b0333a3` | duducp/susa/uninstall.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2297ms == harness floor ~2300ms; source_wall 6090ms vs OK-median 84ms) | ok | pass |
| `1728f0bffc4f` | dweaver/dotvim/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2297ms == harness floor ~2300ms; source_wall 7041ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `735aa67140db` | dwmkerr/effective-shell/samples/scripts/phonetic.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2303ms == harness floor ~2300ms; source_wall 6090ms vs OK-median 84ms) | ok | pass |
| `bb4f8f76b808` | dwmkerr/effective-shell/samples/templates/advanced.template.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2305ms == harness floor ~2300ms; source_wall 6950ms vs OK-median 84ms) | fail | pass |
| `ef8f433461f6` | egurapha/Shunpo/install.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2298ms == harness floor ~2300ms; source_wall 6164ms vs OK-median 84ms) | ok | pass |
| `af0e87e23cbd` | emlys/config/configs/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2311ms == harness floor ~2300ms; source_wall 2402ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `768dfaa0b300` | engintron/engintron/installers/install_apcu_in_cpanel.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2303ms == harness floor ~2300ms; source_wall 6096ms vs OK-median 84ms) | ok | pass |
| `138ebf9c69c4` | erwanjegouzo/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2309ms == harness floor ~2300ms; source_wall 2624ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `c5050288f8e2` | falenn/dotfiles/bash/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2436ms == harness floor ~2300ms; source_wall 6106ms vs OK-median 84ms) | ok | pass |
| `9b41e78d7f0e` | fgonzal/.dotfiles/hosts/macbook/bash/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2301ms == harness floor ~2300ms; source_wall 4759ms vs OK-median 84ms) | ok | pass |
| `0123a6cdaa89` | fissible/ptyunit/self-tests/unit/test-assert.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2296ms == harness floor ~2300ms; source_wall 6141ms vs OK-median 84ms) | ok | pass |
| `c16cf7d32d79` | fissible/ptyunit/self-tests/unit/test-describe.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2379ms == harness floor ~2300ms; source_wall 6097ms vs OK-median 84ms) | ok | pass |
| `b2a4b654f4dd` | fissible/ptyunit/self-tests/unit/test-params.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2326ms == harness floor ~2300ms; source_wall 6144ms vs OK-median 84ms) | ok | pass |
| `5c6590c45e52` | fissible/ptyunit/self-tests/unit/test-skip-test.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2386ms == harness floor ~2300ms; source_wall 6163ms vs OK-median 84ms) | ok | pass |
| `7e26334f8be4` | flatiron-school/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2380ms == harness floor ~2300ms; source_wall 7183ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `704f8fe01be2` | flurdy/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2297ms == harness floor ~2300ms; source_wall 2552ms vs OK-median 84ms) | ok | pass |
| `b3f0bccb399a` | freekmurze/dotfiles/macos/set-defaults.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2318ms == harness floor ~2300ms; source_wall 6158ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `0d229329f265` | g-udi/gaudi-shell/gaudi_shell.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2298ms == harness floor ~2300ms; source_wall 4848ms vs OK-median 84ms) | ok | pass |
| `b708b5b7c8af` | g-udi/gaudi-shell/install.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2297ms == harness floor ~2300ms; source_wall 5182ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `e4fd2af11d12` | golegen/linuxfoundation.org/test.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2305ms == harness floor ~2300ms; source_wall 6111ms vs OK-median 84ms) | ok | pass |
| `65c7d96bc9f4` | gruntwork-io/pre-commit/hooks/packer-validate.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2306ms == harness floor ~2300ms; source_wall 6162ms vs OK-median 84ms) | ok | pass |
| `b145ace3d03d` | gruntwork-io/pre-commit/hooks/terraform-validate.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 6092ms vs OK-median 84ms) | ok | pass |
| `1ccc2e622179` | gruntwork-io/pre-commit/hooks/tofu-validate.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2315ms == harness floor ~2300ms; source_wall 6111ms vs OK-median 84ms) | ok | pass |
| `2cfc24478304` | gwanza16/tfe/artifacts/home/forensicrs/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 4870ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `8ba81f948d30` | h1-the-swan/github_system_files/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2310ms == harness floor ~2300ms; source_wall 7333ms vs OK-median 84ms) | ok | pass |
| `f64f45ba5476` | heussd/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2458ms == harness floor ~2300ms; source_wall 3301ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `cd82199b433e` | hiruna/wifi-txpower-unlocker/arch-linux-region-BO-wifi-txpower-unlocker.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 3936ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `79d0adf93c7c` | hoangqt/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2296ms == harness floor ~2300ms; source_wall 8865ms vs OK-median 84ms) | ok | pass |
| `4bb44edfbdce` | hornos/shpak/lib/k.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2298ms == harness floor ~2300ms; source_wall 4704ms vs OK-median 84ms) | ok | pass |
| `dbe9d6416aa7` | howlingsails/ubuntu-bash-profile/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 6111ms vs OK-median 84ms) | ok | pass |
| `5f4b0ef59996` | hrs/dotfiles/install_python_packages.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 5912ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `964230f1d1b1` | hrs/dotfiles/shell/dot-bashrc.d/path.bash | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2298ms == harness floor ~2300ms; source_wall 2551ms vs OK-median 84ms) | ok | pass |
| `cbf7d811a441` | hrs/dotfiles/x-windows/.config/polybar/scripts/openweathermap-s | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 3524ms vs OK-median 84ms) | ok | pass |
| `fbaa8befd1bd` | hyperpolymath/valence-shell/impl/rust-cli/tests/manual_sigint_test.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2305ms == harness floor ~2300ms; source_wall 3133ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `a4a09dcd730d` | ianmalott/dotfiles/bash/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2305ms == harness floor ~2300ms; source_wall 2474ms vs OK-median 84ms) | ok | pass |
| `9c60095333f6` | ibracorp/ibramenu/ibrainit.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2306ms == harness floor ~2300ms; source_wall 9725ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `23f9abe05cd5` | illyadz77/.bash_alias/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 6086ms vs OK-median 84ms) | ok | pass |
| `889bbc54a40f` | imsudoer/bashboard/.sutd/menu.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 6082ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `f1929e2e35a4` | imsudoer/bashboard/.sutd/tui.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2297ms == harness floor ~2300ms; source_wall 10681ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `154518fe77c6` | imsudoer/bashboard/install.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2297ms == harness floor ~2300ms; source_wall 6158ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `9521b0ae19bc` | irmec/vagrant/ansible/inventories/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 4790ms vs OK-median 84ms) | ok | pass |
| `db5e935910c7` | jasonfellows/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 4929ms vs OK-median 84ms) | ok | pass |
| `83b7203e8bdd` | jaykilleen/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2316ms == harness floor ~2300ms; source_wall 6180ms vs OK-median 84ms) | ok | pass |
| `fbe4c16f7517` | jcandlin/archive/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2476ms == harness floor ~2300ms; source_wall 4677ms vs OK-median 84ms) | ok | pass |
| `ce249c2f3ce7` | jerodsanto/dotfiles/bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2304ms == harness floor ~2300ms; source_wall 2715ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `2547ad5848de` | jgeewax/dotfiles/.bash-powerline.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2321ms == harness floor ~2300ms; source_wall 2621ms vs OK-median 84ms) | ok | pass |
| `7f5e8a3676b1` | jkamenik/dotfiles/files/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 6094ms vs OK-median 84ms) | ok | pass |
| `b8839a68dfeb` | jobin-b/dotfiles/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2296ms == harness floor ~2300ms; source_wall 2388ms vs OK-median 84ms) | ok | pass |
| `14373c543935` | joelindra/Hades/modules/recon/port_scanning.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 6091ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `e365ee316f5e` | joelindra/Hades/modules/system/install_dependencies.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2298ms == harness floor ~2300ms; source_wall 3315ms vs OK-median 84ms) | ok | pass |
| `03845b8b67aa` | jonahkagan/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2296ms == harness floor ~2300ms; source_wall 4943ms vs OK-median 84ms) | ok | pass |
| `4cd471128883` | jwerle/config/bash | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2379ms == harness floor ~2300ms; source_wall 4114ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `ad1658ffcdab` | kayvonr/dotfiles/deprecated/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2308ms == harness floor ~2300ms; source_wall 2459ms vs OK-median 84ms) | ok | pass |
| `268905779ec7` | kenperkins/osx-shell/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2327ms == harness floor ~2300ms; source_wall 2381ms vs OK-median 84ms) | ok | pass |
| `cf5d25e5952d` | kiarafbickers/.bash_profile/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2381ms == harness floor ~2300ms; source_wall 4355ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `f926487165bc` | kjmatson/.dotfiles/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2293ms == harness floor ~2300ms; source_wall 2546ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `caa948ff5c29` | kmorrison/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2331ms == harness floor ~2300ms; source_wall 2388ms vs OK-median 84ms) | ok | pass |
| `c8f603cecc4c` | krupeshf/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2299ms == harness floor ~2300ms; source_wall 5019ms vs OK-median 84ms; network-in-source-path+loop/sleep) | ok | pass |
| `e67b996c4015` | kurogai/100-redteam-projects/Projects/69_Process_monitor/start.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2296ms == harness floor ~2300ms; source_wall 5086ms vs OK-median 84ms) | ok | pass |
| `920a8085b6ae` | lans97/dotfiles/.zshrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2301ms == harness floor ~2300ms; source_wall 2871ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `3f2e38afa095` | lasantosr/intelli-shell/install.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2376ms == harness floor ~2300ms; source_wall 5261ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `2e4fd6f8bd53` | lavabit/robox/http/generic.arch.docker.chroot.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2299ms == harness floor ~2300ms; source_wall 9759ms vs OK-median 84ms) | ok | pass |
| `d212f4348b4e` | lavabit/robox/http/generic.arch.vagrant.chroot.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2303ms == harness floor ~2300ms; source_wall 9691ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `a5dbb0db5b60` | lavabit/robox/http/magma.arch.vagrant.chroot.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2297ms == harness floor ~2300ms; source_wall 9717ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `49a9bde43c92` | learnbyexample/cli-computing/example_files/shell_scripting/search.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2306ms == harness floor ~2300ms; source_wall 6095ms vs OK-median 84ms) | ok | pass |
| `806960c2caeb` | learnbyexample/cli-computing/example_files/shell_scripting/user_input.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2319ms == harness floor ~2300ms; source_wall 6102ms vs OK-median 84ms) | ok | pass |
| `509adbf4e495` | liuchengxu/dotfiles/bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2307ms == harness floor ~2300ms; source_wall 9774ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `e17c3cfa7eb8` | llaski/bash/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2296ms == harness floor ~2300ms; source_wall 2463ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `3bbef4c14b79` | luisadha/alrc-termux/install.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2306ms == harness floor ~2300ms; source_wall 10775ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `825318f90c4c` | luisadha/alrc-termux/lib/alfetch.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2312ms == harness floor ~2300ms; source_wall 2135ms vs OK-median 84ms) | ok | pass |
| `4e48da46d1e6` | luisadha/alrc-termux/lib/plugin_handler.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2379ms == harness floor ~2300ms; source_wall 6152ms vs OK-median 84ms) | ok | pass |
| `31250940f00d` | luisadha/alrc-termux/plugins/boxes_motd_custom.alrc.plugin.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2292ms == harness floor ~2300ms; source_wall 2540ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `ca4d0b2c2841` | m-vdb/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2304ms == harness floor ~2300ms; source_wall 2458ms vs OK-median 84ms) | ok | pass |
| `8381490c0652` | magnetikonline/dotfiles-macos/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2390ms == harness floor ~2300ms; source_wall 4762ms vs OK-median 84ms) | ok | pass |
| `ac75798a6aba` | martijndeb/fish-bowl/utils/install.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2297ms == harness floor ~2300ms; source_wall 6095ms vs OK-median 84ms) | ok | pass |
| `addbc96e5c8f` | mattben/dot/mac/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2311ms == harness floor ~2300ms; source_wall 4763ms vs OK-median 84ms) | ok | pass |
| `a297864110ca` | mauriciojost/dotfiles/modules/screen/start-screen-session.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2312ms == harness floor ~2300ms; source_wall 9120ms vs OK-median 84ms) | ok | pass |
| `e8b35c77af01` | memphis-tools/dummy_fastapi_flask_blog_clients/api_clients/bash/dummy-ops_api_client.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2320ms == harness floor ~2300ms; source_wall 6152ms vs OK-median 84ms) | ok | pass |
| `c1c26a047884` | meowrch/meowrch/home/.local/bin/resetxdgportal.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2299ms == harness floor ~2300ms; source_wall 4114ms vs OK-median 84ms) | ok | pass |
| `d47c4344bc03` | meowrch/meowrch/install.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2311ms == harness floor ~2300ms; source_wall 2229ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `cf93a4c330aa` | mgree/ffs/tests/ffs.auto.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2301ms == harness floor ~2300ms; source_wall 6082ms vs OK-median 84ms) | ok | pass |
| `401465bc89bb` | mgree/ffs/tests/ffs.new.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2316ms == harness floor ~2300ms; source_wall 6082ms vs OK-median 84ms) | ok | pass |
| `80c6f5ae976b` | mikelax/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2301ms == harness floor ~2300ms; source_wall 3057ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `c10ffe8a532a` | mitgit/scripts/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2305ms == harness floor ~2300ms; source_wall 6153ms vs OK-median 84ms) | ok | pass |
| `1dfcc428142e` | modernish/modernish/share/doc/modernish/examples/get-firefox.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2294ms == harness floor ~2300ms; source_wall 2298ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `77eb1d88a120` | modernish/modernish/share/doc/modernish/tests/die-bg-test.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2379ms == harness floor ~2300ms; source_wall 10365ms vs OK-median 84ms; loop/sleep) | fail | pass |
| `515fefa8d4e2` | mproffitt/bashprofile/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2315ms == harness floor ~2300ms; source_wall 6091ms vs OK-median 84ms) | ok | pass |
| `fe556b6ec8f7` | muthuishere/mcp-server-bash-sdk/test_mcpserver_core.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2305ms == harness floor ~2300ms; source_wall 3451ms vs OK-median 84ms) | ok | pass |
| `db3ad5ff2290` | mydiemho/dev-env/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2318ms == harness floor ~2300ms; source_wall 2457ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `2b0cda281d7d` | nanlabs/aws-glue-etl-boilerplate/scripts/clean-localstack.sh | FF,SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2295ms == harness floor ~2300ms; source_wall 6157ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `62d0f367f896` | nanlabs/aws-glue-etl-boilerplate/scripts/validate-env.sh | FF,SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2295ms == harness floor ~2300ms; source_wall 2545ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `809689fe8401` | naoeoxyz/dotfiles/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2310ms == harness floor ~2300ms; source_wall 2052ms vs OK-median 84ms) | ok | pass |
| `fb1ebb4836a2` | ncarlier/webhookd/pkg/hook/test/test_timeout.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2320ms == harness floor ~2300ms; source_wall 2662ms vs OK-median 84ms) | ok | pass |
| `2104b814ef6e` | ndparker/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2293ms == harness floor ~2300ms; source_wall 10336ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `a274ce96e00d` | neninja/dotfiles/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2304ms == harness floor ~2300ms; source_wall 2636ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `8b321e7d7601` | neuralxploit/cyber_c2/start_c5.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2314ms == harness floor ~2300ms; source_wall 6252ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `5cdb45d89ef9` | nickexyz/ntfy-shellscripts/arp-scan/arp-scan.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2376ms == harness floor ~2300ms; source_wall 7628ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `f91d4ae7cfab` | nickexyz/ntfy-shellscripts/fortinet_fortiguard.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2318ms == harness floor ~2300ms; source_wall 2797ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `7505453f8742` | nicolasbrouard/scripts/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2307ms == harness floor ~2300ms; source_wall 4972ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `024b299af09f` | odb/shunt/scripts/update_shml.sh | FF,SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2295ms == harness floor ~2300ms; source_wall 5257ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `23ce11fc0065` | ogios/dots/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2305ms == harness floor ~2300ms; source_wall 2022ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `ad6947e52b7c` | ohmysh/ohmysh/install.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 6176ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `6e3df7acac57` | ohmyzsh/ohmyzsh/tools/uninstall.sh | FF,SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 6144ms vs OK-median 84ms) | ok | pass |
| `9c7c9dbfd2bb` | paradite/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2277ms == harness floor ~2300ms; source_wall 2482ms vs OK-median 84ms) | ok | pass |
| `10034970174d` | pddg/dotfiles/.bashrc.osx | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2330ms == harness floor ~2300ms; source_wall 7002ms vs OK-median 84ms) | ok | pass |
| `ad249d04a50d` | perryr16/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2294ms == harness floor ~2300ms; source_wall 6021ms vs OK-median 84ms) | ok | pass |
| `5ccd46d78923` | peterhellberg/dotfiles/archive/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2339ms == harness floor ~2300ms; source_wall 2964ms vs OK-median 84ms) | ok | pass |
| `0bd5442951f1` | peterhellberg/dotfiles/linux/blade/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2320ms == harness floor ~2300ms; source_wall 3208ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `f87326da5226` | picodotdev/alis/download.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2383ms == harness floor ~2300ms; source_wall 3214ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `e057e759282b` | prasannashrestha011/dotfiles/bash/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2306ms == harness floor ~2300ms; source_wall 4859ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `a1f34e443bb7` | prdai/dotfiles/bash/bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2313ms == harness floor ~2300ms; source_wall 2213ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `cdd87be1a692` | ptarjan/dotfiles/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2319ms == harness floor ~2300ms; source_wall 6132ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `b6d1bdf74c97` | puppylinux-woof-CE/woof-CE/kernel-kit/firmware_extra.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2298ms == harness floor ~2300ms; source_wall 8701ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `08d41d29bbdc` | puppylinux-woof-CE/woof-CE/kernel-kit/get_sof.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2298ms == harness floor ~2300ms; source_wall 2390ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `85696e6f8f87` | puppylinux-woof-CE/woof-CE/woof-code/support/bdrv.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2297ms == harness floor ~2300ms; source_wall 6162ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `a27d26e48c06` | puppylinux-woof-CE/woof-CE/woof-code/support/pc_image.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 6156ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `defea2bbb8ad` | puppylinux-woof-CE/woof-CE/zwn/builders/deb-build.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2324ms == harness floor ~2300ms; source_wall 10199ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `7a5c8846b6a8` | puppylinux-woof-CE/woof-CE/zwn/builders/slack-build.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2309ms == harness floor ~2300ms; source_wall 6925ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `a5dd6f1e0bb1` | puritys/dotfiles/basic/bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2312ms == harness floor ~2300ms; source_wall 4874ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `430cf5980486` | r1cksec/cheatsheets/snippet/sh/readUserInput.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 6104ms vs OK-median 84ms) | ok | pass |
| `d546f498b1f1` | rafaelcaricio/dotfiles_old/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2295ms == harness floor ~2300ms; source_wall 4528ms vs OK-median 84ms) | ok | pass |
| `427db9a04b20` | rahulsmehta/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2383ms == harness floor ~2300ms; source_wall 2482ms vs OK-median 84ms) | ok | pass |
| `506b89e58d23` | rebeccahongsf/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2310ms == harness floor ~2300ms; source_wall 2482ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `ddc13eae576e` | rewinfrey/Dotfiles/bash_files/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2295ms == harness floor ~2300ms; source_wall 9517ms vs OK-median 84ms) | ok | pass |
| `7fe58ea2270d` | rnnh/bioinfo-notebook/scripts/linux_setup.sh | FF,SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2309ms == harness floor ~2300ms; source_wall 4114ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `412a688c659b` | ruanbekker/cheatsheets/pushgateway/scripts/bash_memory_exporter.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2307ms == harness floor ~2300ms; source_wall 2711ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `42351d19eaf5` | rudexi/dotfiles/bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2298ms == harness floor ~2300ms; source_wall 2794ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `9419b4491bff` | ryanpcmcquen/gists/_bash_profile_for_mac_os/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2325ms == harness floor ~2300ms; source_wall 4862ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `c19bdce37497` | rydnr/dry-wit/contrib/audit.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2316ms == harness floor ~2300ms; source_wall 83ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `080c96faea7d` | sbulav/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2296ms == harness floor ~2300ms; source_wall 6115ms vs OK-median 84ms) | ok | pass |
| `3e98f5bab4fc` | scouredimage/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2326ms == harness floor ~2300ms; source_wall 4927ms vs OK-median 84ms) | ok | pass |
| `df9650aa5119` | seddy/scripts/bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2400ms == harness floor ~2300ms; source_wall 5022ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `d895c0bff34d` | sethkrasnianski/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2298ms == harness floor ~2300ms; source_wall 2550ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `28af03ebef61` | shellspec/shellspec/contrib/release.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2309ms == harness floor ~2300ms; source_wall 6125ms vs OK-median 84ms) | ok | pass |
| `580d33342efc` | shellspec/shellspec/examples/count_cpus.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2303ms == harness floor ~2300ms; source_wall 6093ms vs OK-median 84ms) | ok | pass |
| `63a23bd3170c` | shubhamkulkarni01/CSFiles/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2304ms == harness floor ~2300ms; source_wall 6157ms vs OK-median 84ms; loop/sleep) | ok | pass |
| `ff8dc9c9c45b` | slomkowski/bash-full-of-colors/bash_logout.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2309ms == harness floor ~2300ms; source_wall 6157ms vs OK-median 84ms) | ok | pass |
| `f5393092b1b2` | smathy/.dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2380ms == harness floor ~2300ms; source_wall 8306ms vs OK-median 84ms) | ok | pass |
| `eb7c27080916` | smtlaissezfaire/shunit2/source/2.0/bin/docbookPrep.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 2954ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `9efafcec4ce7` | solarmist/config_stuff/bash/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 4856ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `0edfd5cc31b4` | sonofagl1tch/OSHardening/OSX/OSX_ElCapitan_desktop/CIS_MacOSX_1011_Hardenin | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2319ms == harness floor ~2300ms; source_wall 3928ms vs OK-median 84ms) | ok | pass |
| `ab41f79971f9` | soymadip/KireiSakura-Kit/core/_updater.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2318ms == harness floor ~2300ms; source_wall 7115ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `35e4dd20893f` | spark-match/spark-match-01-devops/.github/actions/bats-runner/install.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2295ms == harness floor ~2300ms; source_wall 6086ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `5244cb922414` | spiritLHLS/one-click-installation-script/install_scripts/R.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2295ms == harness floor ~2300ms; source_wall 6092ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `3727830d4ac6` | spiritLHLS/one-click-installation-script/install_scripts/cplusplus.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2313ms == harness floor ~2300ms; source_wall 6087ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `1e7b312be466` | spiritLHLS/one-click-installation-script/install_scripts/filebrowser.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2309ms == harness floor ~2300ms; source_wall 6152ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `ac91de3da168` | spiritLHLS/one-click-installation-script/install_scripts/fossbilling.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2305ms == harness floor ~2300ms; source_wall 6087ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `9305ffeed5bc` | spiritLHLS/one-click-installation-script/install_scripts/golang.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2299ms == harness floor ~2300ms; source_wall 2459ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `48ae572a972f` | spiritLHLS/one-click-installation-script/install_scripts/jupyter.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2313ms == harness floor ~2300ms; source_wall 6084ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `a829ef485ca2` | spiritLHLS/one-click-installation-script/install_scripts/rust.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2304ms == harness floor ~2300ms; source_wall 6089ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `cebba2cd2818` | spiritLHLS/one-click-installation-script/install_scripts/todebian11.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2296ms == harness floor ~2300ms; source_wall 6153ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `a63ea2566f32` | spiritLHLS/one-click-installation-script/install_scripts/toubuntu22.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2307ms == harness floor ~2300ms; source_wall 6092ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `09662b54a7a6` | spiritLHLS/one-click-installation-script/install_scripts/vnstat.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2298ms == harness floor ~2300ms; source_wall 6084ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `51b82343d84a` | spiritLHLS/one-click-installation-script/install_scripts/zipline.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2303ms == harness floor ~2300ms; source_wall 6094ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `d644339d92ad` | spiritLHLS/one-click-installation-script/repair_scripts/check_sudo.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2319ms == harness floor ~2300ms; source_wall 6165ms vs OK-median 84ms) | ok | pass |
| `c1cd0c447d97` | spiritLHLS/one-click-installation-script/repair_scripts/network.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2301ms == harness floor ~2300ms; source_wall 6083ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `a07e190a1f06` | spiritLHLS/one-click-installation-script/repair_scripts/package.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2303ms == harness floor ~2300ms; source_wall 6108ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `5108ba27d8d3` | spiritLHLS/one-click-installation-script/repair_scripts/resize_journal.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2302ms == harness floor ~2300ms; source_wall 6089ms vs OK-median 84ms) | ok | pass |
| `a7d29aec9ce2` | stephenmoloney/ci-actions-copyleft/test.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2297ms == harness floor ~2300ms; source_wall 5425ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `6828161bfe47` | stevenle/arch-dotfiles/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2317ms == harness floor ~2300ms; source_wall 6137ms vs OK-median 84ms) | ok | pass |
| `f8cb033b0916` | strukto-ai/mirage/integ/state_store.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2301ms == harness floor ~2300ms; source_wall 3211ms vs OK-median 84ms) | ok | pass |
| `12f243ec237a` | supl/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2324ms == harness floor ~2300ms; source_wall 657ms vs OK-median 84ms) | ok | pass |
| `008ac612a273` | tarsisazevedo/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2298ms == harness floor ~2300ms; source_wall 2950ms vs OK-median 84ms) | ok | pass |
| `1e80f6892cca` | tbrandenburg/shai/scripts/agent_conversation.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 3729ms == harness floor ~2300ms; source_wall 6648ms vs OK-median 84ms) | ok | pass |
| `8909fed989ec` | techarkit/shell-scripting-tutorial/add.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2318ms == harness floor ~2300ms; source_wall 9724ms vs OK-median 84ms) | ok | pass |
| `0a670933d416` | techarkit/shell-scripting-tutorial/agtb.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2336ms == harness floor ~2300ms; source_wall 9734ms vs OK-median 84ms) | ok | pass |
| `ee5694e97961` | techarkit/shell-scripting-tutorial/continue.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2300ms == harness floor ~2300ms; source_wall 9685ms vs OK-median 84ms) | ok | pass |
| `f4378dc0e20d` | techarkit/shell-scripting-tutorial/extract.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2325ms == harness floor ~2300ms; source_wall 2261ms vs OK-median 84ms) | ok | pass |
| `caac61a4fe16` | techarkit/shell-scripting-tutorial/info.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2301ms == harness floor ~2300ms; source_wall 6088ms vs OK-median 84ms) | ok | pass |
| `722e9344c1be` | techarkit/shell-scripting-tutorial/uadd.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2298ms == harness floor ~2300ms; source_wall 6160ms vs OK-median 84ms) | ok | pass |
| `7e0c040e347b` | theGreatWhiteShark/configurations-and-scripts/bash/.bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2297ms == harness floor ~2300ms; source_wall 3278ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `3369ddbd94fc` | theahmadov/NIVOS/nivo/NIVO_FUSR.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2304ms == harness floor ~2300ms; source_wall 6115ms vs OK-median 84ms) | ok | pass |
| `52a039efe6f1` | thomvaill/tads-boilerplate/scripts/tests/shunit2.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2382ms == harness floor ~2300ms; source_wall 6092ms vs OK-median 84ms) | ok | pass |
| `19663d281091` | tony-rasskazov/configs/bash/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2308ms == harness floor ~2300ms; source_wall 5343ms vs OK-median 84ms) | ok | pass |
| `1097d9a52452` | vaughnwalters/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2310ms == harness floor ~2300ms; source_wall 2390ms vs OK-median 84ms) | ok | pass |
| `f6dfc87932e1` | vscode-shellcheck/vscode-shellcheck/.github/scripts/semantic-release-with-vsce-retry.s | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2304ms == harness floor ~2300ms; source_wall 6157ms vs OK-median 84ms) | ok | pass |
| `1eef3ac6af15` | wagoebv/wagoe/scripts/configure-security.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2963ms == harness floor ~2300ms; source_wall 3862ms vs OK-median 84ms) | ok | pass |
| `66e024cb049e` | wagoodman/bashful/example/scripts/random-error.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2321ms == harness floor ~2300ms; source_wall 6101ms vs OK-median 84ms) | ok | pass |
| `671c2ec85a56` | webfella/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2295ms == harness floor ~2300ms; source_wall 9649ms vs OK-median 84ms) | ok | pass |
| `5ae7b6558eed` | webpro/dotfiles/remote-install.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2295ms == harness floor ~2300ms; source_wall 5179ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `0615cc2c3234` | wiwiamlam/machine_setup/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2336ms == harness floor ~2300ms; source_wall 2395ms vs OK-median 84ms) | ok | pass |
| `9c9aad80109b` | wrengr/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2298ms == harness floor ~2300ms; source_wall 6245ms vs OK-median 84ms) | ok | pass |
| `f87615a4e131` | wrightbradley/dotfiles/home/dot_bashrc | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2317ms == harness floor ~2300ms; source_wall 5093ms vs OK-median 84ms) | ok | pass |
| `02f3551506d9` | xooooooooox/radp-bash-framework/install.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2295ms == harness floor ~2300ms; source_wall 6316ms vs OK-median 84ms; network-in-source-path) | ok | pass |
| `4e649ca563f1` | xvoland/Extract/install_extract_func.sh | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2311ms == harness floor ~2300ms; source_wall 2710ms vs OK-median 84ms) | ok | pass |
| `aa7fa8926510` | yuchan/dotfiles/.bash_profile | SLOW | RECLASSIFIED | ASSET-SOURCING-HEAVY (startup 2298ms == harness floor ~2300ms; source_wall 9781ms vs OK-median 84ms; network-in-source-path) | ok | pass |

## 原判定 EXITED（17 个唯一资产）

| hash | repo/path | 判定史 | 重判类 | 依据 | GNU-n 今日 | rubash-n 今日 |
|---|---|---|---|---|---|---|
| `a07e14e8b435` | bats-core/bats-core/libexec/bats-core/bats-gather-tests | EXITED,SLOW | RESIDUAL-NEEDS-LIVE-REPLAY | shell exited during source of official ohmybash completion (no exit statement in file); current rubash -n + GNU parse clean; needs live repl | ok | pass |
| `02c2dc07bbd3` | ohmybash/oh-my-bash/completions/dirs.completion.sh | EXITED,OK | TRANSIENT-FLIP-TO-OK | same asset passed on the in-snapshot rerun (verdict set ['EXITED', 'OK']); engine-side transient in the snapshot build, not reproducible sta | ok | pass |
| `29a527075647` | ohmybash/oh-my-bash/completions/django.completion.sh | EXITED,OK | TRANSIENT-FLIP-TO-OK | same asset passed on the in-snapshot rerun (verdict set ['EXITED', 'OK']); engine-side transient in the snapshot build, not reproducible sta | ok | pass |
| `e5d4488a7515` | ohmybash/oh-my-bash/completions/docker-compose.completion.sh | EXITED,OK | TRANSIENT-FLIP-TO-OK | same asset passed on the in-snapshot rerun (verdict set ['EXITED', 'OK']); engine-side transient in the snapshot build, not reproducible sta | ok | pass |
| `7db915674a5c` | ohmybash/oh-my-bash/completions/docker-machine.completion.sh | EXITED,OK | TRANSIENT-FLIP-TO-OK | same asset passed on the in-snapshot rerun (verdict set ['EXITED', 'OK']); engine-side transient in the snapshot build, not reproducible sta | ok | pass |
| `ae02819bf6cb` | ohmybash/oh-my-bash/completions/docker.completion.sh | EXITED,OK | TRANSIENT-FLIP-TO-OK | same asset passed on the in-snapshot rerun (verdict set ['EXITED', 'OK']); engine-side transient in the snapshot build, not reproducible sta | ok | pass |
| `8487af5c40bc` | ohmybash/oh-my-bash/completions/drush.completion.sh | EXITED,OK | TRANSIENT-FLIP-TO-OK | same asset passed on the in-snapshot rerun (verdict set ['EXITED', 'OK']); engine-side transient in the snapshot build, not reproducible sta | ok | pass |
| `ee4378d5e0e1` | ohmybash/oh-my-bash/completions/gem.completion.sh | EXITED,OK | TRANSIENT-FLIP-TO-OK | same asset passed on the in-snapshot rerun (verdict set ['EXITED', 'OK']); engine-side transient in the snapshot build, not reproducible sta | ok | pass |
| `dcc1b8530039` | ohmybash/oh-my-bash/completions/gh.completion.sh | EXITED,OK | TRANSIENT-FLIP-TO-OK | same asset passed on the in-snapshot rerun (verdict set ['EXITED', 'OK']); engine-side transient in the snapshot build, not reproducible sta | ok | pass |
| `17ece8ef0b2e` | ohmybash/oh-my-bash/completions/npm.completion.sh | EXITED,OK | TRANSIENT-FLIP-TO-OK | same asset passed on the in-snapshot rerun (verdict set ['EXITED', 'OK']); engine-side transient in the snapshot build, not reproducible sta | ok | pass |
| `f962d0f75f7c` | ohmybash/oh-my-bash/completions/nvm.completion.sh | EXITED,OK | TRANSIENT-FLIP-TO-OK | same asset passed on the in-snapshot rerun (verdict set ['EXITED', 'OK']); engine-side transient in the snapshot build, not reproducible sta | ok | pass |
| `d852d00fc60b` | ohmybash/oh-my-bash/completions/oc.completion.sh | EXITED,OK | TRANSIENT-FLIP-TO-OK | same asset passed on the in-snapshot rerun (verdict set ['EXITED', 'OK']); engine-side transient in the snapshot build, not reproducible sta | ok | pass |
| `d2ac8e05435d` | ohmybash/oh-my-bash/completions/packer.completion.sh | EXITED,OK | TRANSIENT-FLIP-TO-OK | same asset passed on the in-snapshot rerun (verdict set ['EXITED', 'OK']); engine-side transient in the snapshot build, not reproducible sta | ok | pass |
| `8d3d0cf00029` | ohmybash/oh-my-bash/completions/pip.completion.sh | EXITED,OK | TRANSIENT-FLIP-TO-OK | same asset passed on the in-snapshot rerun (verdict set ['EXITED', 'OK']); engine-side transient in the snapshot build, not reproducible sta | ok | pass |
| `f861db300661` | ohmybash/oh-my-bash/completions/pip3.completion.sh | EXITED,OK | TRANSIENT-FLIP-TO-OK | same asset passed on the in-snapshot rerun (verdict set ['EXITED', 'OK']); engine-side transient in the snapshot build, not reproducible sta | ok | pass |
| `aa5b29b49873` | ohmybash/oh-my-bash/completions/projects.completion.sh | EXITED,OK | TRANSIENT-FLIP-TO-OK | same asset passed on the in-snapshot rerun (verdict set ['EXITED', 'OK']); engine-side transient in the snapshot build, not reproducible sta | ok | pass |
| `98ad0249a310` | ohmybash/oh-my-bash/completions/rake.completion.sh | EXITED,OK | TRANSIENT-FLIP-TO-OK | same asset passed on the in-snapshot rerun (verdict set ['EXITED', 'OK']); engine-side transient in the snapshot build, not reproducible sta | ok | pass |

## 原判定 ERROR-STORM（1 个唯一资产）

| hash | repo/path | 判定史 | 重判类 | 依据 | GNU-n 今日 | rubash-n 今日 |
|---|---|---|---|---|---|---|
| `8f166829fd34` | luisadha/alrc-termux/lib/music.sh | ERROR-STORM | RECLASSIFIED | ENV-DEPENDENT (loops on a command absent outside its target env: termux-media-player) | ok | pass |

## 复现附录

### #505 活缺陷（函数体内跨行 [[ + || 续行）

```bash
f() {
if [[
  ( -z "$ExactMatch" && "$ArityMin" = "$ArityMax" )
  || ((${#_ARGS_BOUNDS[@]} == 0))
]]; then
  :
fi
}
```

GNU bash -n / 实跑通过；rubash 1.4.0 master `-n` 与实跑皆拒绝（`conditional binary operator expected`）。
要素矩阵：无函数包裹 PASS、单行 PASS、跨行 FAIL（注释/缩进无关）。

### extglob loader 模式平价演示（两行文件，真实运行）

```bash
shopt -s extglob
case plugin::disable in plugin::@(disable|enable|load)) echo MATCHED ;; esac
```

GNU 与 rubash 实跑均输出 MATCHED（逐命令增量解析都正确）；`-n` 整文件解析两者同样拒绝 —— GNU 平价，非缺陷。

## 关联 issue

- rubash#505（本审计新开，唯一活缺陷）
- rubash#452（评论纠偏：集群 1 GNU 平价、集群 2/3 zsh 归因、11 个已修、2 个语料污染）
- rubash#451（HANG 引擎子集，已关已修）、rubash#463/#460、rubash#241（SLOW 深水：nvm.sh 解析吞吐）
- niubash#256（上一实例重放通道伪影的宿主修复）、niubash#182（history-interlock，cce8ad4）

## 机器可读

- `reclassified-records-20261012.jsonl`：747 条记录逐条重判（含原始证据字段）
- `classified-uniques.json`：675 个唯一资产完整证据（标记、今日 GNU/rubash -n、受护栏运行输出、判定史）
