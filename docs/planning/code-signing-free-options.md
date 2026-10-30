# 代码签名免费方案提案(AV 影响名单 + 免费签名路线)

状态:**提案,待批准**。2026-10-23,由两起实证触发。

## 1. 问题:无签名让我们在 Windows 生态被按最坏路径对待

两起实证,同一根因(发布物无 Authenticode 签名):

| 案例 | 症状 | 机制 |
| --- | --- | --- |
| niubash#511 | `niu.exe` 被 Defender ML 误杀(`Behavior:Win32/DefenseEvasion.A!ml`),硬链接全族陪葬 | 无签名 exe 无信誉锚点,ML 分类器自由发挥 |
| 用户反馈(2026-10-23) | `git` ~20ms,`ls`/`which` ~0.9s,热启动亦然,仅个别人机器 | 用户杀软对**无签名 exe 每次 spawn 全量扫描**;`git.exe` 有厂商签名直接放行 |

第二例的关键对照:ls/which 不是内建——它们是 `winuxcmd\usr\bin\ls.exe`/`which.exe` 外部进程,每次执行完整 spawn 一次无签名 exe。签名缺失 = 每次执行交一道"信誉税",税额由用户的杀软决定(裸 Defender ~100-300ms,重型第三方可达 ~1s)。

本机不复现的原因:开发机已设 Defender 排除(niubash#511 处置),扫描被跳过。

## 2. 杀软影响名单(无签名 exe 的典型按次开销)

> 定性说明:以下为社区/支持渠道常见报告的行为模式,**具体数值随产品版本、策略与云端信誉状态浮动**,不构成基准测试。机制分三类:A=按次文件扫描,B=云端信誉查询(网络往返),C=未知文件模拟执行。

| 杀软 | 机制 | 无签名 exe 典型表现 |
| --- | --- | --- |
| Windows Defender(Microsoft Defender AV) | A+B | 首见 ~100-300ms(云查询),信誉缓存后接近原生;ML 误报风险(见 #511) |
| 360 安全卫士/360 杀毒 | A+B+C | 未加白的自制 exe 每次启动全扫,重度可到 ~1s;自动上传云鉴定 |
| 腾讯电脑管家 | A+B | 类似 360,按次扫描 + 信誉库 |
| 火绒安全 | A | 相对轻量,但无签名的按次扫描仍在 |
| Avast / AVG | C(CyberCapture) | 未知 exe 可被送隔离模拟执行,**首跑可到数十秒** |
| Bitdefender / Kaspersky | A+B(云重) | 云信誉查询往返,网络慢时显著 |
| ESET / McAfee / Norton | A+B | reputation 机制,无签名 = 云查询路径 |

共性结论:**有签名 → 全部走缓存/直接放行;无签名 → 每家都按最坏路径处理,程度由用户装了什么决定**。这解释了"大部分人无感、个别人 0.9s"的分布。

## 3. 免费/低价签名方案名单(2026-10 查证)

### 首选:SignPath Foundation(免费)

- [signpath.org](https://signpath.org) — 为合格 OSS 项目提供**免费** Authenticode 证书,经 SignPath.io 平台做 CI 集成签名
- 门槛:[OSI 认可的开源许可证](https://signpath.org/terms.html)(我们是 MIT ✓)、无恶意软件、签名产物须从公开源码构建(CI 集成天然满足)
- **无需个人身份证明**——验证的是"二进制来自你的公开源码",不查护照
- 微软官方文档([Windows code signing options](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options))点名推荐的 OSS 免费路线,OV 级别,可避 SmartScreen
- 注意:申请按项目走,niubash 与 WinuxCmd 大概率各需一份申请(两仓都是 MIT ✓)

### 备选:Certum Open Source Code Signing(付费,最便宜的 OV)

- [Certum shop](https://shop.certum.eu/code-signing.html) 的 Open Source 变体,SimplySign 云签名(无硬件卡邮寄)
- 历史价 ~€25-69/年,2026 年转售渠道 Standard 版 ~$90-116/年;个人身份验证
- 用途:SignPath 申请被拒时的兜底

### 排除:Azure Artifact Signing(原 Trusted Signing)

- [定价](https://azure.microsoft.com/en-us/pricing/details/artifact-signing/):Basic $9.99/月(5000 签名/月)
- 致命门槛:**个人身份验证仅限美国/加拿大**;组织验证要求可验证的 3 年税务史。中国区个人/小团队基本不可入
- 结论:排除,留档防止以后再调研一遍

### 明确不可行

- 自签名证书:不入 Windows 根库,零 reputation 价值
- CAcert:同上,不被 Windows 信任

## 4. 落地步骤(批准后)

1. 向 SignPath Foundation 提交 niubash 项目申请(signpath.io;WinuxCmd 视批复结果决定是否单独申请)
2. CI 集成:release 工作流在打包后对 `niu.exe` + `winuxcmd` 全体 applet(硬链接各族)签名
   - **硬链接兼容性**:签名写入文件内容,同一 inode 的所有硬链接名同步看到新内容——签名后硬链接依然成立,`wpm links rebuild` 无需改动
3. 每次发版后:签名 + 记录 SHA256(#511 的哈希指纹流程可退役)
4. 文档:FAQ 加"杀软误报/变慢"自助节(见 §5)

## 5. 用户侧临时缓解(签名落地前的支持话术)

按用户的杀软,把 `%LOCALAPPDATA%\Programs\Niubash` 加入信任区/排除项:

- **Windows Defender**:Windows 安全中心 → 病毒和威胁防护 → 排除项 → 添加文件夹
- **360**:信任区 → 添加目录
- **腾讯电脑管家**:信任设置 → 添加文件/目录
- **火绒**:信任区 → 添加
- **Avast/AVG**:菜单 → 设置 → 保护 → 核心设置 → 排除

加完即时生效,无需重启。

## 6. 关联

- niubash#511(Defender 误杀主票)——本提案是它的系统性修复
- niubash#800(SLOW 分诊清单)——本次性能税案例补入其数据

## 7. 落地进度

**2026-10-25:SignPath Foundation 申请获批(test-certificate 阶段)**

- 2026-10-25 收到 SignPath Foundation(Phillip Deng)批准邮件:niubash 申请通过,当前为 **test-certificate(测试证书)阶段**;通过 setup review 后签发正式证书。申请以 GitHub 仓(https://github.com/unixwin/niubash)与下载页(https://unixwin.github.io/niubash/installer.html)为准。
- 集成骨架已就绪(分支 `ci/signpath-skeleton`),**除凭证外全部完成,凭证到位前完全惰性**(release 路径行为与骨架合入前一致):
  - `.signpath/artifact-configuration/artifact-configuration.xml` — SignPath artifact 声明:`setup.exe`、`niu.exe`、`winuxcmd/**/*.exe`(含 `**/*.dll`,opt\ 载荷在内)全部 Authenticode 签名;条目名由打包脚本归一化,配置与版本号零耦合
  - `scripts/release/build-signpath-artifact-zip.ps1` — 从发布布局组装 SignPath 输入 zip + 回写契约(逐条目预签名 SHA-256)
  - `scripts/release/apply-signpath-signatures.ps1` — 签名字节回写 + 硬链接族重建(wpm shim 布局的硬链接契约,§4.2 的落地答案)
  - `.github/workflows/release.yml` — `signpath-gate`(四个密钥齐备才放行)+ `signpath-sign`(x64/arm64 双腿:组装输入 zip → GitHub artifact → `signpath/github-action-submit-signing-request@v3` 提交并等待 → 回写 → 重打包 zip/tar.gz/7z + 别名 + `.sha256` sidecar → `*-signpath-signed`);`release` 作业优先采用已签名产物,任一签名腿失败即 fail-closed 不发布
- 凭证下发后的启用步骤(仅 repo settings,零代码改动):
  1. SignPath org 内建 niubash project,上传上述 artifact configuration,记下 org id / project slug / signing policy slug
  2. 建 CI user(submitter 权限)并生成 API token
  3. repo secrets 填 `SIGNPATH_ORG_ID` / `SIGNPATH_PROJECT_ID` / `SIGNPATH_SIGNING_POLICY_SLUG` / `SIGNPATH_API_TOKEN`(四者齐备 `signpath-gate` 自动放行,缺任一则保持惰性)
  4. 首次发版验证 test-certificate 签名;通过 setup review 后切正式证书
- 已知待办(正式证书前必须解决,详见 `.signpath/artifact-configuration/README.md`):
  1. **VERSIONINFO 元数据**:Foundation 条款要求签名二进制带版本元数据并在 artifact configuration 施加 metadata 限制(product name = 项目名、版本一致);`niu.exe`/`winuxcmd.exe` 目前无 VERSIONINFO 资源,需先补资源再收紧配置(骨架配置中已留占位注释)
  2. **opt\ 上游载荷是否可签**:Foundation 条款"上游 OSS 二进制不得用本订阅签名",但 usr\bin shim 与 opt\ 载荷是同字节硬链接族,只签一半会撕裂族;已列入 setup review 待决
  3. **审批等待**:Foundation 签名请求需人工审批;工作流等待上限 1 小时,超时 fail-closed,审批链接在 step 输出(`signing-request-web-url`)
  4. **Defender 扫描时序**:#511 的 pre-release 扫描(`scripts/release/defender-scan.ps1`,warn-only)仍在 build-windows 打未签名字节;签名启用后考虑对签名后产物复扫
  5. 下载页/首页按 Foundation 条款补 "Free code signing provided by SignPath.io, certificate by SignPath Foundation" 声明及团队角色/隐私说明(条款要求)
- 备注:与申请同批的 WinuxCmd 是否需要单独申请,以批复邮件为准(§3);本骨架的 `winuxcmd/**/*.exe` 通配可直接覆盖 WinuxCmd 仓自身的发布物,复用成本低。
