# 待办清单

> 来源：进程守护与 Cutex 对照的讨论（2026-10-23）。依据见 [DESIGN.md](DESIGN.md) §2、§4，
> [codex-app-server 实测](research/codex-app-server.md)，[claude-stream-json 实测](research/claude-stream-json.md)，
> [Cutex 前端拆解](research/cutex-gui-teardown.md)。
>
> 状态：`[ ]` 未开始 · `[~]` 进行中 · `[>]` 推迟到后续阶段。
> 已完成（`[x]`）和已决定不做（`[-]`）的条目挪到 [DONE.md](DONE.md)，章节编号相同；做完一条就剪过去。
>
> 最后对照代码核对：2026-11-07（0.2.6）。10-31 之后没走这份清单就做完的事，补记在 DONE.md §二十四。

## 一、进程监督（优先）

已完成 10 条，见 [DONE.md](DONE.md) 同名一节。

目标：hub 或子进程挂了，不再整体停摆；一轮中断有明确状态；能自动 resume。

按 Cutex 的做法改为“agent 进程比 hub 活得久，新 hub 接管”，设计见 [PROCESS-SUPERVISION.md](PROCESS-SUPERVISION.md)。

- [~] 真实 agent 上验证"轮次跑到一半 hub stop → hub start 后轮次继续"（2026-10-23）：codex、codebuddy 已通过（轮次跑完、上下文和进程不变，新版本 hub 也能接管旧版本留下的进程）；claude 本机未装，未验证（协议层已由 mock 自动化测试覆盖，只差真实 Claude Code）。**等环境**：本机没有 `claude` 命令，装上即可补测
- [>] Linux / macOS 上实测宿主（用进程组代替 Job Object，目前未验证）：推迟到 B1 节点阶段，那时驱动本就要拆到节点上跑，一并测

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
- [ ] 安装包与 exe 代码签名（未签名时 SmartScreen 首次运行会拦）

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

开工前要定：

- [ ] 中心放哪台：先用这台 Windows（A 阶段），B1 之后再考虑迁到云服务器当纯中心（只放个人项目，公司代码先确认合规）
- [ ] 连通方式：局域网 / Tailscale / 公司内网，决定第一版是否自己做 TLS
- [ ] 两条线并行时完工监视器怎么提交：按分支提交，还是由人合并

A1 账号与设备：

- [ ] core：`users`、`devices` 表（client / node 两类，token 只存哈希，可吊销），现有表加 `user_id`、`node_id`，老数据迁到默认用户和本机节点
- [ ] core：`/v1/auth/login`、`crossagents user add`、设备列表与吊销接口
- [ ] GUI：登录页、设备管理页

A2 远程 GUI：

- [ ] core：`[hub] listen` 可配置，默认仍为 `127.0.0.1`；远程访问走设备 token
- [ ] GUI：Electron 主进程改读连接配置（本机 / 远程地址 + token），设置加「连接」页、断线提示，连不上中心时可切回本机。连远程时关闭本机 hub 的启动与升级，切回本机时再检查一次（MULTI-NODE §13.3）
- [ ] 验收：Mac GUI 连上 Windows hub，两端对话一致，断线后按 `Last-Event-ID` 续传

B1 node 代理：

- [ ] core：`crossagents node join`、配对码、hub 与 node 的双向通道（WSS 还是 gRPC 流待定）
- [ ] core：驱动拆成两半，调度留在 hub，拉起 CLI 与协议翻译挪到 node；单机模式也走内置节点，只保留一套执行路径
- [ ] core：节点上报已装插件、登录状态、可用模型；掉线时事件写本地日志，重连补发（可复用运行时宿主的 seq 编号 + 确认 + 补发机制，见 PROCESS-SUPERVISION §2）
- [ ] core：协作工具 MCP 走 node 转发还是直连中心（待定）
- [ ] core：hub↔node 协议版本 `NODE_PROTOCOL`，`join` 与心跳带 `protocol` / `version` / 能力；不兼容时拒绝上线并回复支持范围与下载地址，hub 同时支持当前版与上一版（MULTI-NODE §13.4）
- [ ] core：远程节点自升级：下载并校验 sha256 → `bin/<版本>/` → 新代理接管运行时宿主 → 清理旧目录；Release 附各平台单独 exe
- [ ] 中心 hub 的部署与升级方式（systemd / Windows 服务 / 容器），GUI 不参与
- [ ] GUI：配对确认弹窗、节点列表与在线状态

B2 跨节点 Chat：

- [ ] core：模型 id 带节点名（`codex@mac`），不指定节点时按"发起方节点优先、其次空闲"挑选；未装或未登录的插件不提供
- [ ] core：Chat 里跨节点招人；Work run 第一版仍限单节点
- [ ] GUI：`@` 菜单按节点分组，显示「节点离线」

B3 远程开发：

- [ ] core：列目录、读文件、附件索引接口
- [ ] GUI：先选机器再选目录的远程目录浏览框，最近目录记住所在机器
- [ ] GUI：diff 与文件查看器，截图和构建产物附件预览

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
- [>] 设备级日 / 周 / 月 token 与成本上限（magpie）：随 §八 A1 建 `devices` 表时加（magpie-lancer §2.9）

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
