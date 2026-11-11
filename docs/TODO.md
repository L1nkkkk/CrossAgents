# 待办清单

> 来源：进程守护与 Cutex 对照的讨论（2026-10-23）。依据见 [DESIGN.md](DESIGN.md) §2、§4，
> [codex-app-server 实测](research/codex-app-server.md)，[claude-stream-json 实测](research/claude-stream-json.md)，
> [Cutex 前端拆解](research/cutex-gui-teardown.md)。
>
> 状态：`[ ]` 未开始 · `[~]` 进行中 · `[>]` 推迟到后续阶段。
> 已完成（`[x]`）和已决定不做（`[-]`）的条目挪到 [DONE.md](DONE.md)，章节编号相同；做完一条就剪过去。
>
> 最后对照代码核对：2026-11-09（0.2.12，run a916057f：#turn-end、#verify-flow、#budget-idle、#docs）。10-31 之后没走这份清单就做完的事，补记在 DONE.md §二十四。

## 一、进程监督（优先）

已完成 10 条，见 [DONE.md](DONE.md) 同名一节。

目标：hub 或子进程挂了，不再整体停摆；一轮中断有明确状态；能自动 resume。

按 Cutex 的做法改为“agent 进程比 hub 活得久，新 hub 接管”，设计见 [PROCESS-SUPERVISION.md](PROCESS-SUPERVISION.md)。

- [~] 真实 agent 上验证"轮次跑到一半 hub stop → hub start 后轮次继续"（2026-10-23）：codex、codebuddy 已通过（轮次跑完、上下文和进程不变，新版本 hub 也能接管旧版本留下的进程）；claude 本机未装，未验证（协议层已由 mock 自动化测试覆盖，只差真实 Claude Code）。**等环境**：本机没有 `claude` 命令，装上即可补测
- [>] Linux / macOS 上实测宿主（用进程组代替 Job Object，目前未验证）：推迟到 B1 节点阶段，那时驱动本就要拆到节点上跑，一并测。macOS 上的验收见 [tasks/MACOS.md](tasks/MACOS.md) §4「节点与 hub 在 macOS 上」与 §5「建议顺序」的 Mac3

## 二、hub 与 GUI 边界

已完成 17 条，见 [DONE.md](DONE.md) 同名一节。

> 2026-10-23 复核：清单与分级已整理成 [API-AUTH.md](API-AUTH.md)，含全部接口、调用方与三级划分。

权限分级、版本不一致两组已全部完成，见 [DONE.md](DONE.md) 同名一节。

- [~] 验收：旧 hub + 新 GUI、新 hub + 旧 GUI 各跑一次，提示可见、界面不崩、能重置连接。2026-11-07 已在真实旧 hub（0.2.6 @4eade5c）+ 新 GUI 上跑过（`de5bde8`）。**还差「新 hub + 旧 GUI」这一向**，没有实测。

## 三、常驻守护（等第一步稳定后再做）

已完成 7 条，见 [DONE.md](DONE.md) 同名一节。

> §一 完成后，hub 停了 agent 照常跑，下一个 hub 接管。这一节剩下的是让 hub 本身常驻、开机自启。

- [~] 设计 hub 常驻模式：关闭窗口后 hub 继续运行，窗口重开时重新连接并拉取快照。安装版 GUI 起的 hub 是脱离的后台进程，关窗口后照常运行，重开时直接连上；缺的是开机自启（下一条）
- [ ] Windows 自启方案：计划任务或服务，二选一并写明理由（权限、登录前运行、崩溃重拉的节流）；Cutex 用的是按用户的计划任务 + 内置 supervisor
- [ ] 安装包与 exe 代码签名（未签名时 SmartScreen 首次运行会拦）。macOS 的签名与公证见 [tasks/MACOS.md](tasks/MACOS.md) §3「打包与分发」（§二十七 Mac4）

## 四、可观测性

已完成的条目见 [DONE.md](DONE.md) 同名一节。本节暂无待办。

## 五、Claude 运行时（对照 Cutex 的借鉴项）

已完成 1 条，见 [DONE.md](DONE.md) 同名一节。

- [ ] 运行时版本钉住：每个 agent 记录所用的 Claude Code 版本，升级后单独 repin，而不是全局切换
- [ ] 每次升级 Claude Code 后跑 `claude_native` 冒烟测试（DESIGN.md §测试）

## 六、Codex 驱动

已完成 1 条，见 [DONE.md](DONE.md) 同名一节。

- [ ] 单进程线程数上限压测（codex-app-server 研究里标为"未确认"）
- [ ] 空闲线程是否会被服务端自动卸载，确认后更新挂起策略
- [ ] 每个线程的 MCP 子进程开销统计，超过阈值时提示

## 七、多模态输入

已完成 6 条，见 [DONE.md](DONE.md) 同名一节。

- [~] `claude-native`：映射到 stream-json 的图片内容块（PDF 为 `document`，小文本内联），实测图片与文件的支持范围还没做
- [~] `codex-native`：映射到 app-server 的图片输入（`localImage`），实测后确认字段
- [~] 历史展示：时间线渲染图片和文件卡片（已做）；transcript 导出时包含附件引用（未做）
- [ ] 跨 agent 转发：Worker 之间传附件时明确是复制还是引用，避免路径跨工作目录失效

## 八、多节点（固定中心 hub + 节点）

> 来源：多机协同讨论（2026-10-23），设计见 [MULTI-NODE.md](MULTI-NODE.md)。
> 取代原「远期」里的"每台机器一个 hub、GUI 同时连多个 hub"：已比较并放弃，理由见 MULTI-NODE §2。
> 并行方式：不拆仓库，每阶段先在 `api-types` 定接口并生成 bindings，core 与 GUI 两条线各用一个 worktree 和分支，GUI 先对着 MockBridge 开发。

已完成 15 条，见 [DONE.md](DONE.md) 同名一节。

开工前要定：

- [ ] 中心放哪台：先用这台 Windows（A 阶段），B1 之后再考虑迁到云服务器当纯中心（只放个人项目，公司代码先确认合规）
- 连通方式已定：局域网和公网都要，默认打洞（组网工具）（[DONE.md](DONE.md) 同名一节，2026-11-07）。A 阶段远程 GUI 再走 SSH 隧道，hub 继续只听 `127.0.0.1`；非回环一律 TLS 留到下面的 A3（2026-11-08 定：人说按推荐来（run 84848059））。见 [MULTI-NODE.md](MULTI-NODE.md) §14「连通方式」、§10「安全」。
- [ ] 两条线并行时完工监视器怎么提交：按分支提交，还是由人合并

A2 SSH 隧道（代码已完成，见 [DONE.md](DONE.md) 同名一节）：

- [ ] 验收（要实测）：Mac GUI 经 SSH 隧道连上 Windows hub，两端对话一致；隧道断开后重连，按 `Last-Event-ID` 续传。步骤见 [MULTI-NODE.md](MULTI-NODE.md) §14「怎么实测」。代码已写，还没在真机上验证的还有：真实 Electron 的 `safeStorage`（Linux 只有 `basic_text` 时拒绝保存）；托盘、整页重新加载，以及 Windows 上退出时 ssh 进程跟着结束；中心默认 shell 是 PowerShell 时，配对命令前加 `& `；远程模式下终端输出流（[main.cts](../apps/gui/electron/main.cts) `terminalStreamOnce`）和扩展资源（同文件 `serveExtension`）遇到 401 时，把连接标成失效；用 Office 打开远程副本时，会不会因为 `Zone.Identifier` 进入受保护的视图。Mac 上的菜单、快捷键、Dock、托盘和 quarantine 见 §二十七 Mac2。

A2 留下的（2026-11-08，run 8298b08a）：

- [ ] SSH 口令 / 未知主机密钥转到 GUI 对话框。现在每次 ssh 都带 `BatchMode=yes`，不改用户的 `StrictHostKeyChecking`：要口令，或主机密钥还不认识，就失败，页面原样显示 ssh 的 stderr，并提示先在终端里 `ssh` 一次（[remote.cts](../apps/gui/electron/remote.cts) `COMMON`，[connection.ts](../apps/gui/src/lib/connection.ts) `pairFailure`）。对照 Cutex 的 `sshAskpass.ts`，以后改成在对话框里填口令、确认指纹。
- [ ] **要人定：agent 默认 bypass，write 设备等于能在中心上执行命令。** 重分级已做（settings、repos/init 升 admin，见 [DONE.md](DONE.md) §八）。还要人决定两件事：默认权限是否从 bypass 改成 guard；设备级别是否细分（例如只看加批准、不能驱动 agent）。现在不做字段级检查：缺省就是 bypass，拦住 `config.permission` 挡不住远程 GUI 正常建 agent。若默认改成 guard，下面这些入口要一起评估，该升 admin 的一起升。write 只发给愿意给 shell 的设备；只看用 read。口径见 [API-AUTH.md](API-AUTH.md) §3。

  能放宽自动执行、授予身份或代替人决策（现在仍是 write）：

  - `POST /v1/agents`：`config.permission`、`allow_hooks`、`setting_sources`，以及 cwd / driver。不填 permission 时默认就是 bypass。
  - `POST /v1/agents/{key}/connect`：签发该 agent 的会话 token，旧的作废。拿到会话 token 就能让它在中心上执行命令。
  - `POST /v1/runs`：可关审批（`policy.approvals`）、自动批准计划或落地、扩大招人 / 委派 / 预算。整条保持 write。
  - `POST /v1/runs/{key}/approve`：可整份替换 run 策略（包括把 approvals 改成 off）并批准计划。
  - `POST /v1/runs/{key}/policy`：可扩大自动唤醒；`reset_attempts` 能解除自动暂停。
  - `POST /v1/questions/{id}/answer`：审批题直接批准危险动作；预算题可加限额并恢复；Work 题可接受提议并开 run。

  不改长期 permission，但足以让已经是 bypass 的 agent 执行：给它发消息、开聊、插话、启动（`messages`、`chat`、`send-now`、`start`）；改人设或模型（`profile`、`model`）、压缩（`compact`）；`POST /v1/works/{id}/runs` 继续一轮；评审打回（`review` 的 `rework`）；代人落地（`land`）；把结果写回人的目录（`apply`）；`POST /v1/extensions/{name}/tools/{tool}` 以人的身份调已信任的工具。

- [ ] 评审里提出、这轮没做的：
  - 设备缓存的并发测试（[devices.rs](../crates/hub/tests/devices.rs) `concurrent_first_authentication_and_revoke_never_repopulate_the_cache`）没有专门覆盖这种情况：`Devices::authenticate` 读完库，发现 `generation` 变了，再重读一次（[devices.rs](../crates/hub/src/devices.rs) `authenticate` 里 `cache.generation != generation` 就 `continue`）。要测到，得把 `Store` 包一层来卡住时机。吊销之后不把已经读到的旧设备填回缓存，这条主路径有测试。
  - 中心默认 shell 是 PowerShell 5.1 时，用 `cat` 读 UTF-8 的 `hub.json`，`exe` 路径里的非 ASCII（例如中文用户名）可能变成乱码，配对失败关闭。可以在「连接」页的高级里手填命令。读取方式这次没改，要在真机上确认这个失败和绕过办法。

Tailscale 与设备系统（2026-11-10，run 44148f21）：

内置打洞（2026-11-10，run 44148f21 第二轮；原来「真内置 tsnet」那条改用 iroh 做了，剪到 [DONE.md](DONE.md) §三十）：

- [ ] Mac 真机实测：照 [INSTALL-MAC.md](INSTALL-MAC.md) §4.0 用配对码连 Windows 中心。要测：Mac 上打包时 `cargo build --release -p crossagents-link` 能编过；GUI 按 macOS 系统代理起 crossagents-link（`session.resolveProxy`）；跨网络时经中继能连、能不能打通直连；中心 hub 重启后 Mac 自动重连（中心的 id 不变，[link.rs](../crates/hub/src/link.rs) 的 `<home>/link.key`）。
- [ ] 中继的可用性：这台 Windows 不经代理连不上 n0 的公共中继（实测卡在「连接中继中」）。要不要提供国内自建中继（iroh-relay 部署说明、默认地址），或在 GUI 里填 hub 的代理，待定。
- [ ] UDP 被封、中继也连不上时，没有 TCP 的退路（iroh 的中继本身走 HTTPS，一般能过；纯 UDP 被封的网络能不能用，要实测）。
- [ ] hub 记下设备是经打洞连进来的：设备页现在看不出一台设备走的是 SSH 还是配对码。配对码兑换时没有把方式写进设备记录。
- [ ] Mac 包的另一种架构：`electron:build:mac` 打 arm64 与 x64 两种包，但 crossagents-link 只按本机架构编一次，另一种包里的跑不起来（[INSTALL-MAC.md](INSTALL-MAC.md) §2 写了只装同架构的）。要么只打本机架构，要么两种架构各编一次再分别放进包里。
- [ ] Tailscale 列表真机实测：设置 →「连接」的 Tailscale 一节还没在 Mac 与 Windows 真机上跑过。要测：没装、没登录、没在运行三种提示；同账号多台机器的排序与在线标记；「用这个地址」填进主机后配对能否成功；Linux 服务器作中心时同理（[INSTALL-MAC.md](INSTALL-MAC.md) §4.4）。
- [ ] 老设备 os 要等下次连上才补：迁移 `0018_device_os.sql` 只加 `devices.os` 列，不回填历史数据。一台设备的系统要等它下次带 `x-crossagents-client-os` 请求、且值与库里不同时才写入（[devices.rs](../crates/hub/src/devices.rs)）；在那之前库里它的 `os` 是空的。
- [ ] 设备「在线」可能误判离线：设备页的在线由 [isOnline](../apps/gui/src/lib/devices.ts)（`last_seen_at` 在 5 分钟内，`ONLINE_WITHIN_MS`）决定；而 `last_seen_at` 只在设备带 token 的请求时才写，且每分钟最多写一次（[store-sqlite lib.rs](../crates/store-sqlite/src/lib.rs) 第 2176 行左右的 `UPDATE devices SET last_seen_at`）。GUI 只开着 SSE 长连接、没有别的请求时，设备可能被显示成离线。待定做法二选一：SSE 连着时定期刷新 `last_seen`；或者由 hub 记下当前开着事件流的设备，在线判定看它，不再只看 `last_seen_at`。

A3 开端口直连（以后）：

> 不在 A2 的路径上。A2 走 SSH，hub 仍只听 `127.0.0.1`；这一组给不能 SSH 的客户端，以后做。见 [MULTI-NODE.md](MULTI-NODE.md) §12 A3、§10「安全」。

- [ ] core：`[hub] listen` 可配置，默认仍为 `127.0.0.1`；远程访问走设备 token
- [ ] core：非回环一律 TLS（HTTPS / WSS）。hub 自签证书；第一次连接时人比对两边短码（与 [MULTI-NODE.md](MULTI-NODE.md) §4.3「登录与配对」留给 A3 的节点配对码同一流程）后固定指纹。身份认指纹、不认地址：一个 hub 记多个地址（局域网、组网地址、公网地址），GUI 和节点按顺序试（同文 §10、§12 A3、§14「连通方式」）
- [ ] core：`users` 表、`/v1/auth/login`、`crossagents user add`；现有表加 `user_id`，老数据归到默认用户（[MULTI-NODE.md](MULTI-NODE.md) §9「数据模型」、§4.3「登录与配对」、§12 A3）
- [ ] GUI：登录页（账号密码）（[MULTI-NODE.md](MULTI-NODE.md) §4.3「登录与配对」、§12 A3）
- [ ] core：可信设备。新设备登录要在已有的可信设备上确认并通知；开终端、批准扩展、登录 CLI 账号、配对节点只给可信设备，并再验证一次密码（[MULTI-NODE.md](MULTI-NODE.md) §10、§12 A3）
- [ ] core：`/v1/auth/login` 限速（[MULTI-NODE.md](MULTI-NODE.md) §10、§12 A3）
- [ ] core：无效的 `cad_` token 每次都查库（[devices.rs](../crates/hub/src/devices.rs) `Devices::authenticate` 对 `cad_` 前缀未命中缓存就 `device_by_hash`），开端口前要限速（[MULTI-NODE.md](MULTI-NODE.md) §10、§12 A3）

B1 node 代理：

- [ ] core：节点怎么连中心待定（[MULTI-NODE.md](MULTI-NODE.md) §11 第 10 条）；走 A3 开端口时才用 `crossagents node join` 与配对码。hub 与 node 的双向通道（WSS 还是 gRPC 流待定）
- [ ] core：现有表加 `node_id`，老数据指向本机节点（[MULTI-NODE.md](MULTI-NODE.md) §9「数据模型」）
- [ ] core：驱动拆成两半，调度留在 hub，拉起 CLI 与协议翻译挪到 node；单机模式也走内置节点，只保留一套执行路径
- [ ] core：节点上报已装插件、登录状态、可用模型；掉线时事件写本地日志，重连补发（可复用运行时宿主的 seq 编号 + 确认 + 补发机制，见 PROCESS-SUPERVISION §2）
- [ ] core：协作工具 MCP 走 node 转发还是直连中心（待定）
- [ ] core：hub↔node 协议版本 `NODE_PROTOCOL`，`join` 与心跳带 `protocol` / `version` / 能力；不兼容时拒绝上线并回复支持范围与下载地址，hub 同时支持当前版与上一版（MULTI-NODE §13.4）
- [ ] core：远程节点自升级：下载并校验 sha256 → `bin/<版本>/` → 新代理接管运行时宿主 → 清理旧目录；Release 附各平台单独 exe
- [ ] 中心 hub 的部署与升级方式（systemd / Windows 服务 / 容器），GUI 不参与
- [ ] GUI：配对确认弹窗、节点列表与在线状态
- [ ] core：内置终端改在拥有工作区的节点上开（现在 [terminal.rs](../crates/hub/src/terminal.rs) 的 PTY 跑在 hub 那台机器上）（[MULTI-NODE.md](MULTI-NODE.md) §7.1「职责划分」、§12 B1）
- [ ] core：节点启动时用登录 shell 取一次环境（`$SHELL -ilc env`，带超时）。macOS 上 launchd 的 LaunchAgent 只有最小 PATH（[MULTI-NODE.md](MULTI-NODE.md) §7.1「职责划分」、[tasks/MACOS.md](tasks/MACOS.md) §4「节点与 hub 在 macOS 上」）
- [ ] GUI：节点机器上从「管 hub」改成「管 hub 或 node」（[localHub.cts](../apps/gui/electron/localHub.cts) 的 `manageLocalHub` / `run`）。怎么判断本机 node 是不是自己的仍待定（[MULTI-NODE.md](MULTI-NODE.md) §7.1「职责划分」、§13.3 怎么判断是「本机自己的 hub」）
- [ ] core：`runs_on = "node"` 的工具由 hub 按指纹下发已批准快照，节点校验哈希一致才执行并缓存；`CROSSAGENTS_URL` 指向节点本机的转发端点（[EXTENSIONS.md](EXTENSIONS.md) §9「代码在哪跑：`runs_on`」、[MULTI-NODE.md](MULTI-NODE.md) §16「扩展在多节点下」、§12 B1）
- [ ] core：创造模式在远程节点上改为由节点打包扩展目录交给 hub，仍由 hub 生成快照、算指纹（[MULTI-NODE.md](MULTI-NODE.md) §16「扩展在多节点下」、[EXTENSIONS.md](EXTENSIONS.md) §7）

B2 跨节点 Chat：

- [ ] core：模型 id 带节点名（`codex@mac`），不指定节点时按"发起方节点优先、其次空闲"挑选；未装或未登录的插件不提供
- [ ] core：Chat 里跨节点招人；Work run 第一版仍限单节点
- [ ] GUI：`@` 菜单按节点分组，显示「节点离线」

B3 远程开发：

- [ ] core：列目录、读文件、附件索引接口
- [ ] GUI：先选机器再选目录的远程目录浏览框，最近目录记住所在机器
- [ ] GUI：diff 与文件查看器，截图和构建产物附件预览
- [ ] core：读文件接口要给出二进制字节（现在 `FileContentResponse` 只有 `binary: true`、没有字节；A2 另加了原始字节路由，见 [DONE.md](DONE.md) §八，文件在中心磁盘上）。查看器与工作区文件走 GUI → hub → 文件所在节点，节点离线不可看（[EXTENSIONS.md](EXTENSIONS.md) §9「查看器」、[MULTI-NODE.md](MULTI-NODE.md) §16「扩展在多节点下」、§7.5「存储分布与负载」、§12 B3）。文件在节点上时走这个接口

C 单干模式（依赖 A2 的连接页和字节走 hub、B1 的内置节点和终端在节点上开，以及 hub 在 macOS 上的验证，见 [tasks/MACOS.md](tasks/MACOS.md) §4「节点与 hub 在 macOS 上」；[MULTI-NODE.md](MULTI-NODE.md) §12、§15「单干模式」）：

- [ ] GUI：记中心和本机两个 hub。连不上中心就切到本机，界面始终标明当前连的是「本机」还是「中心」（[MULTI-NODE.md](MULTI-NODE.md) §15「单干模式」）
- [ ] 一个 agent 只归一个 hub：中心建的 agent 在中心掉线时照常跑，事件攒在节点上等中心回来补发（[MULTI-NODE.md](MULTI-NODE.md) §7.4「掉线」）；单干时新建的 agent 归本机 hub。第一版对话分开放，「本机」「中心」各看各的。以后 Chat 可合并进中心；单干期间的 Work 先不合（同文 §15「单干模式」、§7.5「存储分布与负载」）
- [ ] hub 要能在 macOS 上跑，和 Windows 同一份代码（[MULTI-NODE.md](MULTI-NODE.md) §15「单干模式」、[tasks/MACOS.md](tasks/MACOS.md) §4「节点与 hub 在 macOS 上」）
- [ ] 验收：断网时 Mac 用本机 hub + 内置节点继续开 Chat，GUI 标明「本机」；中心恢复后，中心建的 agent 补发事件（[MULTI-NODE.md](MULTI-NODE.md) §12）

存储与负载：

- [ ] 附件（截图、产物、日志、大 diff）留在节点，hub 只存引用（节点、路径、大小、哈希），按需拉取
- [ ] 节点保留自己 agent 的追加事件日志，兼作 hub 灾备重建来源
- [ ] 流式字块事件量大：一轮结束后合并或定期清理旧字块
- [ ] 节点离线缓存的上限与过期策略
- [ ] 云服务器部署：hub 设内存上限、只经 HTTPS/WSS 并限制来源，附件中转受公网带宽限制

## 九、自举（用 CrossAgents 开发 CrossAgents）

已完成 26 条，见 [DONE.md](DONE.md) 同名一节。

> 来源：2026-10-24 自举清单，设计与取舍见 [tasks/DOGFOOD.md](tasks/DOGFOOD.md)，每项一个提交。

- [ ] 2026-10-27 trace run `01a17c78…dccf42e8` 发现的问题（未修）
  - [ ] 批准时建的模板会话（如 Orion、Zara）没有用到，一直留在名单里（未修，原属上一条的后半句，归 §十八 / §十九 的会话归属一起处理）
- [ ] plan_approval=required 时主导可以不提计划就动手（2026-10-28 记，未修，要不要拦由人定）：run `43af3611` 的 `plan_approval` 是 `required`，主导一跳过 `plan_submit`，直接在 `planning` 状态下改代码、`git commit` 并调用 `run_finish`，hub 没有拦。现在只在「有计划待批」时卡住流程，主导自己不提计划就绕过了审批门禁；可选做法是 `planning` + `required` 时拒绝改文件的工具与 `run_finish`（或先要求 `plan_submit`），但这会改变主导能干的事，先记下来等决定

## 十、agent 描述文件

已完成 6 条，见 [DONE.md](DONE.md) 同名一节。

> 2026-10-26 第一轮：内置三家的硬编码迁到描述文件，hub 按描述扫描本机、Agents 页显示结果。格式与优先级见 [DESIGN.md](DESIGN.md) §9.3。

- [ ] 从 CLI 读模型列表：Codex 仍读 `models_cache.json`，其余家各自有列表接口后接进来
- [ ] 描述文件里的账号段：登录方式、配置目录与环境变量也写进描述，加一家账号不用改 `*_account.rs`
- [ ] 版本钉住与自下载 CLI（DESIGN §9.4 的"已验证范围"要靠它）

## 十一、交给 CrossAgents 自己改自己

已完成 13 条，见 [DONE.md](DONE.md) 同名一节。

> 2026-10-26，任务书见 [tasks/SELF.md](tasks/SELF.md)。按 A → B → C 顺序，A 做完再放开后面。

- [~] A1 run 共享 `node_modules`，GUI 的 `tsc --noEmit` 与 `node --test` 进 `crossagents.toml` checks（检查已进；2026-11-06 加了 worktree 准备命令，冷 worktree 自己装依赖；跨 worktree 共享 `node_modules` / 依赖缓存仍未做）
- [~] B1 压缩合并用 `run_finish` 的 `message`：`run_land` 已能传 `message` 作落地提交说明；`run_finish` 的 `message` 仍只用于提交主导自己的改动，没有传到落地（不传时默认 `crossagents: <run 标题>`）
- [ ] C2 主导从复盘里挑一条开新 run 去修（计划仍要人批）

## 十二、run 的收尾：再开 Work 与删除

全部完成，见 [DONE.md](DONE.md) 同名一节。

## 十三、借鉴 Multica、magpie、Lancer

已完成 24 条，见 [DONE.md](DONE.md) 同名一节。剩下两条都随别的阶段做。

> 2026-10-27 整理，依据 [Multica 调研](research/multica.md) 与 [magpie 与 Lancer 调研](research/magpie-lancer.md)（都是 2026-10-26）。
> 顺序按两份调研 §4：先交接卡和选账号，再做 Multica 的三条唤醒规则；心跳与排队超时随 §八 B1 一起做。「不学的」见各自 §3，这里不再列。

- [>] 心跳 15 秒、约 3 分钟判离线；排队超时要「心跳缺席超过宽限期」与「这条运行排了这么久」同时满足（Multica）：随 §八 B1 做（multica §2.7、§2.8）
- [>] 设备级日 / 周 / 月 token 与成本上限（magpie）：`devices` 表已经建了（[DONE.md](DONE.md) §八），上限仍然推后，没跟 A1 一起做（magpie-lancer §2.9）

## 十四、Git 视图（只读）

> 2026-10-26 Pia 整理的需求，原本要作为一个新 Work 交给 DeepSeek 做，因 R1-A 被拒而没开工；R1-A 已修好（45cc895），现在可以开。
> 范围：第一步只读。fetch、切换分支、push 这类会改仓库的操作留到以后。

> 2026-10-30 核对：提交图与单提交 diff 已按 run / 会话的范围做了（`GET /v1/runs/{key}/graph`、`/v1/agents/{key}/graph`、`…/commits/{sha}/diff`，GUI 在右侧工具列 `commitgraph.tsx`，另有只读文件浏览器），没有单独的 `#/git` 页。

> 2026-11-06 run `a40444a0`（#git-api、#git-page）做完了分支列表、status、提交详情、路径白名单和 `#/git` 页（含 MockBridge 数据）。

- [~] hub：`git log --all` 级别的全仓库提交图未做
- [ ] 顺带验证：由 CrossAgents 自己开 Work 来做（主导拆任务交给便宜模型），实测 R1-A 与交接卡

## 十五、会话属于工作区，run 只记录谁参与过

已完成 8 条，见 [DONE.md](DONE.md) 同名一节。

> 2026-10-27，设计见 [DESIGN.md](DESIGN.md) §4.7，方案见 [tasks/S1.md](tasks/S1.md)。起因：run B 承接 run A 时想让 run A 的 planner 接着当主导，并由它决定用新会话还是 run A 用过的会话；现在主导被复制、worker 每个任务克隆一个用完就删、删 run 连带删会话。
> 已定（S1 §5）：派活默认新开执行会话（`fresh: true`），复用要写 `fresh: false`；人在聊的会话可以被派活，但人的请求优先、可中断；归档不自动清理，可恢复、可批量删除。

- [ ] 真实环境补测：Claude Code 会话在 worktree 干完回仓库根目录 `--resume`，对话完整、CLI 的会话列表里找得到（S1 §4）（2026-10-27）在 CLI 层做了：`--resume` 对话完整，删掉 worktree 后仍能恢复；但会话文件留在 worktree 的项目目录，仓库根目录的列表大概率看不到它（S1 §4 末尾）；定了不复制、不链接（S1 §7 第 14 条）；hub 层 `place` 没接真实账号测
- [ ] 兼容性增强：仓库根目录的 Claude 原生会话列表能看到在 worktree 里跑过的会话。先实测交互式 `claude --resume` 列表的行为，再决定做法（不在 `send_home` 里复制或链接会话文件，见 S1 §7 第 14 条）

## 十六、并行跑 run / task 时暴露的四个问题

全部完成，见 [DONE.md](DONE.md) 同名一节。

> 2026-10-27，run `01a17e22`（补齐持续推进的 5 个缺口）里三个 worker 并行跑的时候实测出来的，都已核实到具体原因，均未在那个 run 里修。

## 十七、本地资源锁与通用执行器

已完成 5 条，见 [DONE.md](DONE.md) 同名一节。

> 2026-10-29，run `27bd5f0e` 的 `run_finish` 检查撞上主导自己的另一轮 `cargo test`（LNK1104）。设计见 [tasks/LOCKS.md](tasks/LOCKS.md)。

- [ ] L4：run 页显示资源持有者；强制释放（先杀树再放锁）
- [ ] 跨节点共享资源（数据库、部署环境）随多节点 B1 设计，v1 遇到 `scope = "shared"` 直接报错

## 十八、工作组：持续的工作空间，run 是其中一轮

> 2026-10-29，设计见 [tasks/G1.md](tasks/G1.md)。起因：一轮做完想接着干，只能让主导再开一个 Work、跳到新页面，历史被切成几段。
> 与 §十九 有冲突，以 [tasks/LEAD.md](tasks/LEAD.md) §9 为准：一轮结束不再把主导 `send_home`、不删集成工作区，同组下一轮在同一个集成 worktree 里切新分支；原地模式没有 run 分支，不适用 G1 §3.3 的分支表。G1-2 与 §十九 P1 一起做。

全部完成（G1-1 至 G1-5），见 [DONE.md](DONE.md) 同名一节。

## 十九、Lead 执行模式与工作区保留

> 2026-10-29，设计见 [tasks/LEAD.md](tasks/LEAD.md)。起因：修个小 bug 也要建 worktree，主导搬家丢上下文、再冷编译一遍；run 一结束主导被送回、检出被删，没法接着干。Lead 可选原地或 worktree，Worker 始终独立 worktree 可并行；Run 完成、会话结束、工作区清理拆成三件事。
> 顺序：§九 剩下的小 bug → G1-1 → P1 + G1-2 → G1-3 / G1-4 / G1-5 → P2 → P3。P3 开工前先定 LEAD §10 第 1 条（原地模式要不要后台集成 worktree）。

已完成 3 条（P1、P2、P3），见 [DONE.md](DONE.md) 同名一节。

- [ ] **要人定：LEAD §10 第 3 条——完成后保留 / 清理是工作组级设置还是每轮可改**（2026-10-30 记）。§9 的表里说该做成工作组的设置，但 §10 第 3 条把它列为待定、只写了「倾向工作组级」。P1 按**无条件保留 + 显式清理入口**做，不加任何策略字段：那个字段会被序列化进 `works.data` 成为兼容负担，而现在没有任何地方能改它（没 GUI、没 CLI 子命令），永远等于默认值。定下来之后加一个 `#[serde(default)]` 字段是向后兼容的，不会因为现在不做而变难

## 二十、发起时选会话（已做）与它暴露的缺口

> 2026-10-30，run `01a18a4a`。人的原话：「我发起 work/chat 的时候，可以选择新的 session 或者已有 session」。
> 已完成 4 条，见 [DONE.md](DONE.md) 同名一节。

## 二十一、自举暴露的 hub 缺陷（2026-10-30）

> 已完成 5 条，见 [DONE.md](DONE.md) 同名一节。

## 二十二、仓库卫生（2026-10-30）

> 同一个 run 里顺手修掉的三件，记下缘由免得再犯。

全部完成，见 [DONE.md](DONE.md) 同名一节。

## 二十三、发布

全部完成，见 [DONE.md](DONE.md) 同名一节。

## 二十四、10-31 之后的工作留下的尾巴

> 2026-11-06 核对时从设计稿和提交记录里摘出来的，做完的部分见 [DONE.md](DONE.md) 同名一节。

- [ ] **MCP 连接关闭（`-32099`）根因未查明**：`mcp-diag` 只让子进程退出的原因（stderr、退出状态）变得可见，没有定位问题。下次撞上先看这些输出
- [ ] **要人定：内置事件流面板删不删**：试点 `extensions/debug/` 已完成，内置面板暂时保留（[EXTENSIONS-MIGRATION.md](EXTENSIONS-MIGRATION.md)）
- [>] 减少额度消耗阶段 3：暂缓（[USAGE-TRIM.md](USAGE-TRIM.md)）
- [~] **已下线的模型仍在名单里显示为可用**（2026-11-07 记，run `01a1b1f2`）：已做的部分见 [DONE.md](DONE.md) 同名一节（回合失败后记下、派活前拒绝、名单标出）。还差：刷新账号的模型列表时，不会把列表里没有、但仍在用的模型标为不可用，要等某个回合因「模型不可用」失败一次，那一次照样失败；Hazel 4 模板仍指向 `deepseek-v4.1-flash-sg`，只是被标为不可用，没换模型
- [ ] **应用到原目录后的复跑同步阻塞，且不占目录的构建资源**（2026-11-07 记，run `01a1b1f2`）：`crates/orchestrator/src/apply/mod.rs` 的 `rerun` 在 apply 请求里同步执行检查，`cargo test --workspace` 这类要几分钟，请求和 GUI 按钮都得等；它用 `ShellVerifier::default()`，不持任何资源锁，可能与人在同一目录里跑的 cargo 构建撞车（§十七）。应改为后台执行、结果经事件报告，并占用该目录的构建资源。
- [ ] **其余「先读后写」的 SQLite 事务还没逐个检查**（2026-11-07 记，run `01a1b1f2`）：只有 `update_lesson`、用量 outcome 更新、数据迁移三处改成了 `BEGIN IMMEDIATE`（`e0298fa`）。store 里其他先读后写的 deferred 事务没有查过，在 WAL 下同样可能立刻报 SQLITE_BUSY（不等 busy_timeout）。

## 二十五、借鉴 oh-my-pi 与 Cutex 0.2.1

> 2026-11-06 读源码和设计稿整理，人选了这 7 条。只读过，没运行。依据固定到核对时的提交：
> oh-my-pi [`b7221d9`](https://github.com/can1357/oh-my-pi/tree/b7221d994869ed36a81b44e60827786f572498ed)（单 agent 编码 harness，从 Pi fork 而来；它在工具层的亮点，如按内容哈希编辑、LSP、调试器、进程内 bash，我们驱动现成 CLI 用不上，不列）；
> Cutex desktop 0.2.1 [`9fbae59`](https://github.com/senxiu-official/cutex-desktop/tree/9fbae59caa37c09eaa89d127a233f83d49601203)（上次核对是 0.2.0 `6216e67`）。
> 建议顺序：3 → 2 → 1（1 先单独开 Work 出设计），其余随后。

- [ ] **1. 跨厂商顾问**（oh-my-pi [advisor-watchdog.md](https://github.com/can1357/oh-my-pi/blob/b7221d994869ed36a81b44e60827786f572498ed/docs/advisor-watchdog.md)）：给一个 agent 配一个别家模型当顾问，每轮读新增的转录，用 `advise` 工具插回意见。三级：`nit` 攒到回合边界、`concern` 可打断、`blocker` 一定唤醒。要照着学的规矩：
  - 去重，以及过滤没内容的话（lgtm、done 一类）；每轮最多 4 条非 blocker；
  - 打断之后冷却 3 轮；
  - 人主动按停时不自动续跑，意见留成卡片，下次人发消息时一起带上；
  - 压缩上下文后顾问状态重置；顾问自己的意见不再回喂给它。

  我们有插话和中断，缺的是逐轮看转录的入口和这套投递规则。现有的 `review_request` 只能按任务审一次。先出设计：顾问是普通会话还是专门角色、额度怎么算、run 里和 run 外各怎么用
  - 2026-11-06 设计稿已写好：[tasks/ADVISOR.md](tasks/ADVISOR.md)。等人看过再决定实现范围：稿子自己的最小可用版，还是主导建议的缩小版（状态放内存、预算只按次数、只做工具和卡片）。§8 有 5 个问题要人定
- [~] **2 的尾巴**（第 2 条本身已完成，见 [DONE.md](DONE.md) 同名一节）：快照里的排队原因与撤回、Codex 插话超时改为 `outcome_unknown` 已做（2026-11-07，`265434e`）。还差：
  - Electron 的超时、断线映射没在打包环境里实测过
- [~] **4 的尾巴**（2026-11-07，`8e22440`）：驱动压缩事件、`inject` 不再阻塞事件泵、扩展的规则源已接（见 DONE.md 第 4 条）。还差：真实 Claude / Codex 的自动压缩信号与插话都没有实测，目前只有 mock 验证

## 二十六、扩展 api 2

> 来源：扩展 api 2 的设计（2026-11-07）。只有设计，W1–W4 未开工。见 [EXTENSIONS.md](EXTENSIONS.md) §9，各条验收见该节「分期」。
> 这里的 W1–W4 是扩展 api 2 的实现阶段，和 [tasks/W1.md](tasks/W1.md) 的轻量 Work 不是一回事。

- [ ] **W1** webview、`[capabilities]`、`api = 2`、批准界面。验收：three.js 页面带 WASM 正常渲染；webview 里写死循环，主界面仍能用；停用后 webview 消失（[EXTENSIONS.md](EXTENSIONS.md) §9「分期」）
- [ ] **W2** 查看器，以及限定范围的文件访问。hub 的原始字节路由已有（[DONE.md](DONE.md) §八，[EXTENSIONS.md](EXTENSIONS.md) §9「查看器」）；页内查看器和目录句柄还没做。验收：点对话里的 `out/car.gltf` 打开 3D，相对 `.bin` 和贴图能加载；范围外的路径被拒（[EXTENSIONS.md](EXTENSIONS.md) §9「分期」）
- [ ] **W3** 浏览器视图。验收：登录 GitHub 后重启仍是登录状态；禁止被嵌的网站能打开；菜单不被盖住（[EXTENSIONS.md](EXTENSIONS.md) §9「分期」）。macOS 上摄像头 / 麦克风要在打包时声明用途，见 [tasks/MACOS.md](tasks/MACOS.md) §3「打包与分发」
- [ ] **W4** GUI 侧工具。验收：agent 能打开网页、读成文本、点击、截图，人看得到、能接手；没有 GUI 时明确报错（[EXTENSIONS.md](EXTENSIONS.md) §9「分期」）

## 二十七、macOS

> 来源：2026-11-07 讨论。任务书见 [tasks/MACOS.md](tasks/MACOS.md)。
> 2026-11-08（run 8298b08a）：Mac2 的 GUI 代码、Mac3 的终端 `-l` 与 `[locate.paths]` 已改，都要在 Mac 上实测。
> 2026-11-08（run e28910b3）：不带 hub 的 ad-hoc 包配置已做（`c529e55`、`8e90dac`、`6e4392a`），要在 Mac 上实测。Developer ID、公证、自动更新暂不做（人有 Mac、没有 Apple Developer 账号）。CI、登录 shell 取 PATH 还没做。
> 阶段照该文 §5「建议顺序」（Mac1–Mac4；验收是建议，原话只给了顺序）。待定项照 §6「待定」。在 Mac 上打包与安装见该文 §8。
> Mac1–Mac4 与 `docs/tasks/` 里早期里程碑 M0–M5、以及 §二十六 的 W1–W4（扩展 api 2；[tasks/W1.md](tasks/W1.md) 是轻量 Work）不是一回事。

已完成 3 条，见 [DONE.md](DONE.md) 同名一节。

- [ ] **Mac1 macOS CI 跑测试**。依赖：无。验收（建议）：在 macOS runner 上 `cargo test --workspace`、`npm --prefix apps/gui test`、`npm --prefix apps/gui run typecheck` 通过；跑不过的用例逐条记下。见 [tasks/MACOS.md](tasks/MACOS.md) §5「建议顺序」
- [~] **Mac2 GUI 小修 + 不带 hub 的 ad-hoc 包手测**。§2 的代码已改（2026-11-08，`455741e` 菜单、Dock、托盘、全屏收起、主进程快捷键；`3c49369` 界面上按平台显示 ⌘ 或 Ctrl；`aeb3e8d` 把 `.terminal`、`.fileloc` 算进打开即执行；`6e4392a`「在旁边打开」在 Mac 上写「⌘+单击或中键：在旁边打开」），要在 Mac 上实测：系统菜单；⌘ 快捷键和各处提示，包括用 ⌘/ 打开「键盘快捷键」对话框；点 Dock 图标恢复窗口；点通知；模板图标在浅色、深色和 Retina 下的显示；全屏时先退出全屏再藏进托盘；托盘点击；远程副本带上 quarantine 属性。不带 hub 的 ad-hoc 包配置已做，要实测（2026-11-08，`c529e55` 打包、`8e90dac` 主进程、`6e4392a` 连接页）：dmg 与 zip，各 arm64、x64，`identity` 为 `"-"`，包里没有 hub，托盘 png 在 `mac.extraResources`。这台 Windows 打不出 mac 包。步骤见 [tasks/MACOS.md](tasks/MACOS.md) §8「在 Mac 上打包与安装（不带 hub、ad-hoc 签名）」，给人看的安装说明见 [INSTALL-MAC.md](INSTALL-MAC.md)（`8f3b0ea`：打包、安装、连 Windows 上的 hub、更新与卸载）。Developer ID、公证和自动更新是 Mac4，暂不做。依赖：Mac1 至少 typecheck 与 test 绿；dmg 只能在 macOS 上打。验收（建议）：输入框里 ⌘C / ⌘V / ⌘X / ⌘A / ⌘Z 都能用；⌘Q 退出、⌘M 最小化；点 Dock 图标窗口回来；菜单栏图标在浅色、深色和 Retina 下都看得清；按 [tasks/MACOS.md](tasks/MACOS.md) §8（或 [INSTALL-MAC.md](INSTALL-MAC.md)）装上后首次打开能启动，`codesign --verify --deep --strict` 通过，并按 [MULTI-NODE.md](MULTI-NODE.md) §14「怎么实测」连上 Windows hub。见 [tasks/MACOS.md](tasks/MACOS.md) §5「建议顺序」
- [~] **Mac3 随 A2 / B1 做环境、终端、字节走 hub**。run 8298b08a 已改、要实测的两条：内置终端在 macOS 上加 `-l`（`c5355fd`，[terminal.rs](../crates/hub/src/terminal.rs) `unix_shell`）；描述文件 `[locate.paths].macos` 补了 `/opt/homebrew/bin` 与 `/usr/local/bin`（同一提交；claude、codex、cursor、codebuddy、with）。还没做：用登录 shell 取 PATH（仍随 B1）、Unix 宿主与锁在 Mac 上实测（§一那条）。字节走 hub 的接口已做（[DONE.md](DONE.md) §八），Mac GUI 连上远程 hub 之后要实测能打开扩展资源和副本。依赖：[MULTI-NODE.md](MULTI-NODE.md) §12 的 A2（代码已完成，验收见上面「要实测」）与 B1；同文 §15「单干模式」定下本机 hub 加内置节点之后，PATH 在节点启动时取一次。浏览器视图要不要把摄像头 / 麦克风写进 `Info.plist` 也挂在这里（[EXTENSIONS.md](EXTENSIONS.md) §9「浏览器视图」）。验收（建议）：从 Dock 启动的 GUI 起的 hub 能找到 Homebrew 和 npm 装的 CLI；内置终端里 `brew` 可用；agent 会话里 `cargo` / `npm` 可用；宿主用进程组管住整棵树；Mac GUI 连远程 hub 时能打开扩展静态资源与工作区文件。见 [tasks/MACOS.md](tasks/MACOS.md) §5「建议顺序」
- [>] **Mac4 签名、公证、自动更新**。2026-11-08：人有 Mac、没有 Apple Developer 账号，暂不做。范围仍是该文 §3「打包与分发」的 Developer ID、hardened runtime、公证，以及 `electron-updater` 走通。本轮的包不带 hub，没有要一起签的 hub 二进制。依赖：Mac2 的包能在 Mac 上装起来，以及一个 Apple Developer 账号（没有账号做不了，见该文 §6「待定」）。验收（建议）：首次打开不再需要去「隐私与安全性」放行；装 N 版后发布 N+1，GUI 自动下载并在退出时安装。见 [tasks/MACOS.md](tasks/MACOS.md) §5「建议顺序」

待定（[tasks/MACOS.md](tasks/MACOS.md) §6「待定」）：

- 手边有 Mac（2026-11-08 定）。Mac2 起的打包和界面要在这台 Mac 上实测。这台是 Windows，mac 包在这里打不出来
- [>] 有没有 Apple Developer 账号（Mac4 的硬依赖）。2026-11-08：没有，Mac4 暂不做。现在停在 ad-hoc 签名的包，不自动更新。见 [tasks/MACOS.md](tasks/MACOS.md) §8
- [ ] macOS CI 的费用。私有仓库的 macOS runner 要计费；要不要只在 `main` 和发版前跑，等有账号时再定

## 二十八、省协调成本（统计与通知分类、评审闭环、消息直传与会话交接）

> 来源：2026-11-09 run `c917a9ab`（标题「省协调成本：统计与通知分类、评审闭环、消息直传与会话交接」）。设计、实测结果与逐条说明见 [tasks/COORDINATION.md](tasks/COORDINATION.md)；**完成清单见 [DONE.md](DONE.md) 同名一节**（9 个任务全部合并，已从本清单剪过去）。
> **已随 0.2.12 生效**：2026-11-09 发版（`cdfe86b`），本机 hub 已在跑 0.2.12。历史：那一轮自己是在 hub 0.2.11 上跑的，仍按 0.2.11 的流程走（每条通知立刻叫醒、lead 手动开评审、手动接受）。人当时在 Mac 上测远程 GUI，连的就是这台机器上正在跑的 0.2.11，那一轮没有停、重启或升级它。
> 标记：**【实测】** 还要在真实 agent / 真机上量；**【下一轮】** 实现或改动留到后续；**【待评估 / 待定】** 判定或做法未定；**【已知】** 已知行为，本轮不改。状态沿用 `[ ]` 未开始 · `[~]` 进行中 · `[>]` 推迟。
> 下面是 `lead/leftovers`（2026-11-09 03:03 更新版）的**全部**条目，逐条落到这里和设计稿 §11「遗留」；已修、不再是遗留的两条在各自小节里写明一句。

### 28.1 Cursor 驱动（#cursor-driver，已合并 `585d81d`）

- [>] 【下一轮 / 低优先】同一天的版本目录用目录名降序打平（`crates/drivers/src/argv.rs:52`）。`YYYY.MM.DD-<hex>` 与 `YYYY.MM.DD-HH-MM-SS-<hex>` 混在同一天时更晚的构建可能排到后面；`cursor-agent.ps1` 对同一天也不固定顺序
- [>] 【下一轮 / 低优先】`displayName` 写死 `crossagents`、命名空间固定 `plugin-crossagents-crossagents`（`crates/drivers/src/cursor_native/mod.rs:496`）；同进程再加载同名插件会撞名。本机已装的 stripe、outlook、gmail、x 不撞
- 已修、不再是遗留（#rounds `07875a2`）：`*.new` 版本先走脚本升级、下次再直接启 `node`；提示词长度按引号转义后的 UTF-16 长度算。设计稿 §6 写了一句

### 28.2 通知（#notices，已合并 `ee04aad`）

- [~] 【已知 / 故意】已归档 agent 的待带通知不投递、也不清理，行留在 agent id 上（恢复后还能看到）；删除 agent 时才清

### 28.3 回合记录（#rounds，已合并 `07875a2`）

- [ ] 【下一轮】hub 重启前已在排队的消息，重启后跑出的回合没有 `sources` 和 `notices`；recover 补发的 `TurnCompleted` 也产出没有来源的回合。要修得在启动时把未完成消息的事件灌回 `rounds`，或 `requeue` 时补发输入事件
- [ ] 【已知 / 原有缺口】stream rule 排队注入（`crates/hub/src/runtime.rs` 的 `inject` 直接 `inbox.push`、不写 `agent.input`）跑出的回合没有来源；回合中插话那条路径记为通知（`Notice`）
- [ ] 【下一轮】叫醒消息的 `notice.carried` 在投递成功后才发，回合先结束则这一回合缺这些通知、事件随后丢弃；外部 agent 取消息很快时也可能碰上
- [ ] 【已知】外部 agent（`take_external`）的回合错后一条消息；用量不报（`null`）
- [ ] 【下一轮】`usage --by round --run` 只认编排器发出的消息；人发给该 run 成员的消息开始的回合不算进去
- [ ] 【实测】Cursor / With 发 `PromptViaFile` 没有单元测试；没在真实 agent 上跑过 `usage --by round` 的端到端输出
- [ ] 【实测】Codex 恢复会话的用量修正只在单元测试里验证，没在真实 Codex 会话上跑过（恢复时若先重发一条旧用量通知，上一次调用会被多算）；恢复或分叉出来的 Codex 会话请求次数仍记为未知（`null`）

### 28.4 评审闭环（#review-loop，已合并 `f94bee7`）

- [ ] 【实测】同一个评审 agent 跨轮（`reviewer_fresh=false`）在 mock 里跑不起来（驱动不能在另一个目录恢复会话、计划被拒），只有单元测试覆盖
- [ ] 【实测】hub 重启后恢复 `Reviewing` / `Passed` 的任务只有单元测试（`crates/orchestrator/src/tests_verification.rs`），没有 mock 重启测试
- [ ] 【实测】原 worker 会话已不在、退回时换新会话的路径没有 mock 测试
- 已修、不再是遗留（#task-budget `2201c36`）：`review_submit` 的 `reviewed_sha` 原来要求完整相等、拒绝信息却给 8 位短提交号；现在去空白、不分大小写、至少 7 位前缀算同一个提交，拒绝信息给完整提交号。设计稿 §4 写了一句
- 【生效情况，非遗留】新评审闭环已随 0.2.12 生效（本机 hub 已在跑 0.2.12）；run `c917a9ab` 那一轮自己是按 0.2.11 的流程走的（lead 手动开评审、手动接受）

### 28.5 每轮新会话（#lead-session，已合并 `f95fe4b`）

- [ ] 【下一轮 / 单开任务】已归档 lead 通过 `/v1/works/{id}/runs` 开新一轮：轮次建起来、会话重启，但开轮消息送不到、run 一直不结束。`create_work_round` 传 `source=None`（`crates/orchestrator/src/work.rs:275`），不走续会话和交接，`send_envelope` 对已归档 agent 返回 Conflict。原有问题
- [~] 【已知】排队中的消息只在内存里：续会话后、新会话打开前 hub 重启，去向和以前一样（本轮没有新增保护）
- [ ] 【实测】「空闲才关」和旧会话取下一条消息之间的竞争只有 Inbox 的单元测试，没有端到端测试
- [~] 【已知】lead 一直忙、队列一直有消息时，换会话拖到 `round_wait_limit`（默认 120 秒），之后这一轮在旧会话里开（日志里有警告）

### 28.6 任务预算（#task-budget，已合并 `2201c36`）

- [~] 【已知】预算都没配时整套检查跳过、**默认不起作用**；下面几条只在配了预算时会碰上
- [ ] 【实测】hub 的预算检查（`crates/hub/src/budget.rs` 的 `check_tasks`，按 tick 跑）没有端到端测试，只测了纯函数和编排器发的通知；`crates/mock-agent/tests/task_budget.rs` 没写；`crossagents usage --by task` 没在真实数据上跑过、没有输出样例
- [ ] 【待评估】任务账本不落表，hub 启动时重放 `events` 表重建（`Store` trait 在 `crates/core/src/store.rs`，不在本轮范围）；重启不丢，但事件越多启动越慢；`usage` 表仍然没人写
- [~] 【已知】范围外的改动：`crates/cli/src/cmd/events.rs` 给三个新事件（`task.budget`、`task.reviewer`、`task.check_timed`）加了 match 分支；`bindings/TaskReport.ts` 重新生成；`TaskSpec` 和 `PlannedTask` 去掉了 `Eq`（`TaskBudget` 含 `f64`）

### 28.7 换档（#phase-model，只交报告）

- [>] 【下一轮】实测数据、方案和下一轮要先测的几条见 [tasks/COORDINATION.md](tasks/COORDINATION.md) §9 与 run 笔记 `lead/phase-model`；**实现留到下一轮**。下一轮先测（【实测】）：换回低档那一步的缓存；真实 lead 会话中途换档后长对话的缓存命中；缓存能留多久；协调阶段用低档的输出占比和质量

### 28.8 测试（偶发失败，本轮未改测试与辅助函数）

- [~] 【已知】`crates/mock-agent/tests/continuation.rs` 的 `a_task_that_moves_within_the_threshold_is_not_reported` 偶发失败（期望 0 次续跑叫醒，实际 1 次）；#notices 的 `ee04aad` 第一次验收挂在这条，代码不动用 `task_verify` 重跑四条全过。后面验收再碰上就 `task_verify` 重跑
- [ ] 【待评估】`crates/mock-agent/tests/run_policy.rs` 的 `the_stuck_report_is_set_kept_and_turned_off` 偶发失败过一次（`run_policy.rs:162`，设 `stuck_minutes=15` 后回复里是 None），疑是与其他写 run 的路径并发，原因没查；另外 cargo 在第一个失败文件就停，验收命令可考虑加 `--no-fail-fast`
- [~] 【已知】`crossagents-core` 的 `exec_tree` 测试 `exec_waits_for_a_live_predecessor_root_process`（断言 `elapsed >= 1s`）全量跑时偶发失败，单独重跑通过
- [~] 【已知 / 疑似时序】`crates/mock-agent/tests/stream_rules.rs` 的 `a_rule_goes_once_in_twenty_turns`、`compaction_lets_the_rule_go_again`、`the_agents_own_compaction_lets_the_rule_go_again` 整组跑时失败过一次，单独重跑通过

### 28.9 hub 平台

- [ ] 【待定，问过人】任务验收每条命令的超时写死 600 秒（`crates/orchestrator/src/verify.rs:57`），`crossagents.toml` 的 `timeout_secs` 只管落地前的检查；这一轮靠把验收拆成多条命令绕过。是否做成可配：待定
- [ ] 【下一轮】验收通过后 worker 又交一次会让检查重跑（#notices 发生了两次，每次约 12 分钟）
- [~] 【下一轮 / 剩一半】验收没过时任务状态仍叫 `passed`（只表示验收跑完了）。run a916057f 的 #verify-flow（`b2bb2d4`）已让 `task_list` / `task_get` 带 `checks_ok`、GUI 在 `passed` 旁标「验收未过」，见 [DONE.md](DONE.md) §二十八。还差：状态本身不区分（没有单独的「验收未过」状态），只看 `status` 的调用方（CLI、外部脚本、通知文字）仍看不出；要不要拆状态待定
- [ ] 【待评估】`usage_get` 显示 Cursor 账号 `limit_reached=true`，但月度窗口只用了 22%，且 #phase-model 在 Cursor 账号上跑了 **9 个真实回合都没碰到限额错误**：这个判定很可能不对，依据待评估。因此 #rounds、#review-loop、#lead-session、#task-budget、#phase-model 都被改派给 Cleo 2（Claude Haiku）
- [~] 【已知】任务的 mock-agent 测试不在验收里时（如 #rounds），改了共用类型的编译问题要等下一个跑 mock 的任务才暴露；主导靠人工查构造处兜底
- [~] 【已知】两个任务各自在分支上验收、先后合并后的组合没有人自动验证：#review-loop 与 #lead-session 合并后，主导在 run 分支 `5b1a6d1` 上手动跑了 `crossagents-orchestrator` 和两边相关的 8 个 mock 测试文件，全过
- [ ] 【下一轮】end_turn 没交结果的剩余部分（主体已由 run a916057f 的 #turn-end `c7875f7` 做完，见 [DONE.md](DONE.md) §二十八）：评审空手结束时，通知 lead 的那条还没附上评审的回复文字（#replay 那次意见写在回复里，lead 仍要自己去翻）；评审「已提醒过」记在内存（`Orchestrator.review_reminders`），hub 重启后同一轮评审会再提醒一次。经验库 `dc8dc64a`

## 二十九、CodeBuddy 记账与返工换会话（2026-11-11）

> 设计与实测见 [USAGE-TRIM.md](USAGE-TRIM.md) §7、§8。CodeBuddy 记账修正 `eeb7b09`；返工换会话随后一次提交。

- [ ] 【实测】返工换会话没在真实 worker 上跑过，只有单元测试；等有 `task.rework_session` 事件后，按 USAGE-TRIM §8.4 量次数和前后用量，再决定 150000 要不要调
- [ ] 【待评估】阈值只能经 API 的 run `policy.rework_new_session_tokens` 设，CLI / GUI 没有入口
- [~] 【已知】上下文读数在编排器内存里：hub 重启后、worker 再报用量之前退回，读不到，沿用原会话
- [~] 【已知】worker 回合一直不结束时，换会话等到 `round_wait_limit`（默认 120 秒）后放弃，退回原会话
- [ ] 【待评估】0.2.19 及以前 CodeBuddy 的历史用量偏大（USAGE-TRIM §7.3）；重放时按 `(Dₖ − Dₖ₋₁)/2` 修正能修掉大部分，但每个进程第一个回合修不准，没做
- [ ] 【待评估】CodeBuddy 上跑 Claude 模型时 `cache_creation_input_tokens` 是不是也只是 miss 没验证；现在一律不记写缓存
- [ ] 【待评估】CodeBuddy 子代理（Agent 工具）的请求：流式帧里被过滤，回合用量取 `result.usage` 的差分时会算进来，但进程第一个回合用流式请求之和，这一回合的子代理用量会漏；没实测
