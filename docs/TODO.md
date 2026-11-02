# 待办清单

> 来源：进程守护与 Cutex 对照的讨论（2026-10-23）。依据见 [DESIGN.md](DESIGN.md) §2、§4，
> [codex-app-server 实测](research/codex-app-server.md)，[claude-stream-json 实测](research/claude-stream-json.md)，
> [Cutex 前端拆解](research/cutex-gui-teardown.md)。
>
> 状态：`[ ]` 未开始 · `[~]` 进行中 · `[>]` 推迟到后续阶段。
> 已完成（`[x]`）和已决定不做（`[-]`）的条目挪到 [DONE.md](DONE.md)，章节编号相同；做完一条就剪过去。

## 一、进程监督（优先）

已完成 10 条，见 [DONE.md](DONE.md) 同名一节。

目标：hub 或子进程挂了，不再整体停摆；一轮中断有明确状态；能自动 resume。

按 Cutex 的做法改为“agent 进程比 hub 活得久，新 hub 接管”，设计见 [PROCESS-SUPERVISION.md](PROCESS-SUPERVISION.md)。

- [~] 真实 agent 上验证"轮次跑到一半 hub stop → hub start 后轮次继续"（2026-10-23）：codex、codebuddy 已通过（轮次跑完、上下文和进程不变，新版本 hub 也能接管旧版本留下的进程）；claude 本机未装，未验证（协议层已由 mock 自动化测试覆盖，只差真实 Claude Code）。**等环境**：本机没有 `claude` 命令，装上即可补测
- [>] Linux / macOS 上实测宿主（用进程组代替 Job Object，目前未验证）：推迟到 B1 节点阶段，那时驱动本就要拆到节点上跑，一并测

## 二、hub 与 GUI 边界

已完成 15 条，见 [DONE.md](DONE.md) 同名一节。

> 2026-10-23 复核：清单与分级已整理成 [API-AUTH.md](API-AUTH.md)，含全部接口、调用方与三级划分。

权限分级：

- [~] `ErrorCode` 增 `Forbidden`（现在只有 `Unauthorized`，见 [api-types](../crates/api-types/src/lib.rs)）；越权返 403 而不是 401（已做）；GUI 据此置灰入口并说明原因（未做：GUI 总用总 token）

版本不一致：

- [ ] `version` 不等但 `api` 相同 → 允许连接，在侧栏版本号旁标不一致，并隐藏该版本没有的功能入口
- [ ] 验收：旧 hub + 新 GUI、新 hub + 旧 GUI 各跑一次，提示可见、界面不崩、能重置连接

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

- [ ] hooks 默认关闭，需显式开启（参考 cutex 的 "Allow commands" 开关）
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
- [ ] 安全：附件路径不能让 agent 读到工作目录之外的文件，除非用户显式选择

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

已完成 2 条，见 [DONE.md](DONE.md) 同名一节。

> 2026-10-26，任务书见 [tasks/SELF.md](tasks/SELF.md)。按 A → B → C 顺序，A 做完再放开后面。

- [~] A1 run 共享 `node_modules`，GUI 的 `tsc --noEmit` 与 `node --test` 进 `crossagents.toml` checks（2026-10-30 检查已进：`paths = ["apps/gui", "crossagents.toml"]` 依次跑 `npm --prefix apps/gui ci`、`run typecheck`、`test`，冷 worktree 里每次自己装依赖；共享 `node_modules` / 依赖缓存未做）
- [ ] A2 mock 里无对话的 agent 也按插件给出能力
- [ ] A3 worker 克隆合并 / 取消后归档，run 追溯保留它最后几轮
- [ ] B1 压缩合并用 `run_finish` 的 `message`
- [ ] B2 落地提交加 `Co-authored-by` 或可选人的 git 身份
- [ ] B3 run 页只显示本 run 的通知
- [ ] B4 开场说明带本机可用工具链
- [ ] B5 hooks 默认关闭（同 §四）
- [ ] B6 附件路径不能越出工作目录（同 §七；计划由人批）
- [ ] C1 run 复盘：从 trace 产出「现象 / 影响 / 方向」表到 `docs/tasks/RETRO-<run>.md`
- [ ] C2 主导从复盘里挑一条开新 run 去修（计划仍要人批）
- [ ] C3 改 hub 的任务在 `hub sandbox` 里跑冒烟再收尾
- [ ] L1–L3 项目排障记录（L1 完成 2026-10-28）：run 中用 `issue/` 笔记随手记问题与绕法，收尾逐条整理进项目 lessons（按稳定的 project_id 跨节点共享、跨项目隔离），按需搜、失败时附相关条目，不当规则注入（任务书 [tasks/LESSONS.md](tasks/LESSONS.md)）

## 十二、run 的收尾：再开 Work 与删除

全部完成，见 [DONE.md](DONE.md) 同名一节。

## 十三、借鉴 Multica、magpie、Lancer

已完成 5 条，见 [DONE.md](DONE.md) 同名一节。

> 2026-10-27 整理，依据 [Multica 调研](research/multica.md) 与 [magpie 与 Lancer 调研](research/magpie-lancer.md)（都是 2026-10-26）。
> 顺序按两份调研 §4：先交接卡和选账号，再做 Multica 的三条唤醒规则；心跳与排队超时随 §八 B1 一起做。「不学的」见各自 §3，这里不再列。

第一批（P0）：

- [ ] 选账号（magpie）：`routing.rs` 默认先用最早续期的账号；新克隆的 Worker 到 90% 让号、返工中的到 98% 才换；失败的账号排到队尾不剔除；额度未知的先试一次读出余量（magpie-lancer §2.2）

第二批（P0，agent 间通信里空着的那块）：

- [ ] 唤醒规则（Multica）：agent 挂「等某事」的规则后结束本轮，条件满足再起一轮并说明被什么唤醒；自己的消息不唤醒自己、排队去重、同一规则无人参与连环触发 3 次或 1 小时超过 12 次自动暂停、等待默认最多 7 天；任务结束时规则全关（multica §2.1）
- [ ] 分阶段唤醒（Multica）：子任务带 stage，一批全部结束就唤醒 lead 放行下一批，全部结束再唤醒一次做汇总；内置规则，不用手建（multica §2.2）
- [ ] Planner 唤醒表（Multica）：成员发进展但没 @ 人才唤醒 Planner，@ 了别人不唤醒，Planner 自己的消息不唤醒；派活协议和花名册作为固定段落追加在 Planner 指令后（multica §2.3，对应 DESIGN §4.5、§6）

第三批（P1）：

- [ ] Run 泳道图（Lancer）：一个 Worker 一根线，节点有提交、交接、测试、重试、改派、落地、上下文压缩、模型切换、等你；带聚焦模式；依赖交接卡（magpie-lancer §2.3）
- [ ] 接力链条（Lancer）：Run 页顶部每条任务线一行 `Rex ✓ → Ada → 你`（magpie-lancer §2.4）
- [ ] 额度快作废时提醒 Planner 多派活（magpie）（magpie-lancer §2.5）
- [ ] 「需要你」内嵌在事件流和泳道节点上，选项可行内点选（Lancer）（magpie-lancer §2.6）
- [ ] 工具步骤按产出命名，产物在折叠行下给缩略图（Lancer）（magpie-lancer §2.7）
- [ ] 运行与任务分开：一次运行结束不等于任务完成（Multica）（multica §2.4）
- [ ] 失败分两类，只自动重试基础设施故障（离线、超时、目录准备失败、网络），`agent_error` 直接停并说明原因；会污染会话的故障在原目录开新会话（Multica）（multica §2.5）
- [ ] 本地目录锁：同一目录同时只跑一个运行，后来的显示 `waiting_local_directory`（Multica）（multica §2.6）
- [>] 心跳 15 秒、约 3 分钟判离线；排队超时要「心跳缺席超过宽限期」与「这条运行排了这么久」同时满足（Multica）：随 §八 B1 做（multica §2.7、§2.8）

第四批（P2）：

- [ ] 描述文件加 `min_version`，低于最低版本不注册，GUI 显示「版本过低」（Multica）（multica §2.9）
- [ ] 核对注入 hub MCP 时有没有丢掉用户级 MCP 配置、有没有重复起同一服务器（Claude 的 `--strict-mcp-config`、CodeBuddy）（Multica）（multica §2.10）
- [ ] `run trace` 给出候选账号的排序和理由（magpie）（magpie-lancer §2.8）
- [>] 设备级日 / 周 / 月 token 与成本上限（magpie）：随 §八 A1 建 `devices` 表时加（magpie-lancer §2.9）
- [~] CLI 版本：Agents 页已显示版本与来源（2026-10-26，4425631）；不在已验证范围时标出还没做（magpie）（magpie-lancer §2.10）
- [ ] 从 `~/.codex/auth.json`、`~/.claude/.credentials.json` 导入账号，落到 M5 的独立目录；导入前提示刷新 ChatGPT 令牌会让原工具登出（magpie）（magpie-lancer §2.11）
- [ ] 工作流交接：每一步设交给谁、完成后去哪、打回退到哪，交接前能预览说明（Multica）（multica §2.12）
- [ ] 费用条：每次运行一根，按 agent、模型拆分，与额度管理一起做（Multica）（multica §2.13）
- [ ] 自动化熔断：7 天内至少 50 次运行且失败率 90% 时自动暂停（Multica）（multica §2.14）

## 十四、Git 视图（只读）

> 2026-10-26 Pia 整理的需求，原本要作为一个新 Work 交给 DeepSeek 做，因 R1-A 被拒而没开工；R1-A 已修好（45cc895），现在可以开。
> 范围：第一步只读。fetch、切换分支、push 这类会改仓库的操作留到以后。

> 2026-10-30 核对：提交图与单提交 diff 已按 run / 会话的范围做了（`GET /v1/runs/{key}/graph`、`/v1/agents/{key}/graph`、`…/commits/{sha}/diff`，GUI 在右侧工具列 `commitgraph.tsx`，另有只读文件浏览器），没有单独的 `#/git` 页。

- [~] hub：提交图接口：run 与会话各一个、带 `limit`（已做）；`git log --all` 级别的全仓库图未做
- [ ] hub：本地与远程分支列表，含 upstream 和领先 / 落后数量
- [ ] hub：工作区状态，`git status --porcelain=v2`
- [~] hub：单个提交的 diff 已做（对第一个父提交）；提交详情（作者、完整说明等）未单独给
- [ ] hub：只能查 hub 已知的项目目录（agent 的工作目录、run 的仓库），其余路径拒绝；Rust 测试覆盖每个接口和越界拒绝
- [ ] GUI：新增 `#/git` 页：左边本地 / 远程分支，中间带泳道的提交图（分支和 tag 标在提交上，HEAD 高亮），右边提交详情和 diff；顶部显示未提交改动的数量
- [ ] GUI：MockBridge 补一套 git 数据，截图检查
- [ ] 验收：`cargo test --workspace` 与 GUI 构建通过
- [ ] 顺带验证：由 CrossAgents 自己开 Work 来做（主导拆任务交给便宜模型），实测 R1-A 与交接卡

## 十五、会话属于工作区，run 只记录谁参与过

已完成 8 条，见 [DONE.md](DONE.md) 同名一节。

> 2026-10-27，设计见 [DESIGN.md](DESIGN.md) §4.7，方案见 [tasks/S1.md](tasks/S1.md)。起因：run B 承接 run A 时想让 run A 的 planner 接着当主导，并由它决定用新会话还是 run A 用过的会话；现在主导被复制、worker 每个任务克隆一个用完就删、删 run 连带删会话。
> 已定（S1 §5）：派活默认新开执行会话（`fresh: true`），复用要写 `fresh: false`；人在聊的会话可以被派活，但人的请求优先、可中断；归档不自动清理，可恢复、可批量删除。

- [ ] 真实环境补测：Claude Code 会话在 worktree 干完回仓库根目录 `--resume`，对话完整、CLI 的会话列表里找得到（S1 §4）（2026-10-27）在 CLI 层做了：`--resume` 对话完整，删掉 worktree 后仍能恢复；但会话文件留在 worktree 的项目目录，仓库根目录的列表大概率看不到它（S1 §4 末尾）；定了不复制、不链接（S1 §7 第 14 条）；hub 层 `place` 没接真实账号测
- [ ] 兼容性增强：仓库根目录的 Claude 原生会话列表能看到在 worktree 里跑过的会话。先实测交互式 `claude --resume` 列表的行为，再决定做法（不在 `send_home` 里复制或链接会话文件，见 S1 §7 第 14 条）

## 十六、并行跑 run / task 时暴露的四个问题

已完成 2 条，见 [DONE.md](DONE.md) 同名一节。

> 2026-10-27，run `01a17e22`（补齐持续推进的 5 个缺口）里三个 worker 并行跑的时候实测出来的，都已核实到具体原因，均未在那个 run 里修。

- [ ] **GUI 里卡片刚出现就点「开始」会撞上同一个 409**（2026-10-30 记，未修）：上面那条测试竞态按测试侧修掉了（等提议者那一轮真的结束），但产品侧的问题是另一笔——Work 卡片在提议者还在应答时就出现在界面上，人这时点「开始」会拿到 `409 busy: … is answering a person's message`。要么卡片在那一轮结束前显示为「还不能开始」，要么答卡片时自动等它结束。
- [ ] **持续推进「唤醒投递失败」那条 Notice 没有集成测试**：实现和代码审查都做了（首次失败发一条、投递成功清标记、再失败再发一条），但真实 hub 下让 `host.notify` 稳定失败做不到——store 只有 INSERT 没有改插件的接口，删 agent 又会先被 `continuation_step` 的 `get_agent` 挡住。要覆盖得加一个可注入失败的 Host 测试替身。

## 十七、本地资源锁与通用执行器

已完成 5 条，见 [DONE.md](DONE.md) 同名一节。

> 2026-10-29，run `27bd5f0e` 的 `run_finish` 检查撞上主导自己的另一轮 `cargo test`（LNK1104）。设计见 [tasks/LOCKS.md](tasks/LOCKS.md)。

- [ ] L4：run 页显示资源持有者；强制释放（先杀树再放锁）
- [ ] 跨节点共享资源（数据库、部署环境）随多节点 B1 设计，v1 遇到 `scope = "shared"` 直接报错

## 十八、工作组：持续的工作空间，run 是其中一轮

> 2026-10-29，设计见 [tasks/G1.md](tasks/G1.md)。起因：一轮做完想接着干，只能让主导再开一个 Work、跳到新页面，历史被切成几段。
> 与 §十九 有冲突，以 [tasks/LEAD.md](tasks/LEAD.md) §9 为准：一轮结束不再把主导 `send_home`、不删集成工作区，同组下一轮在同一个集成 worktree 里切新分支；原地模式没有 run 分支，不适用 G1 §3.3 的分支表。G1-2 与 §十九 P1 一起做。

已完成 3 条（G1-1、G1-2、G1-3），见 [DONE.md](DONE.md) 同名一节。

- [ ] G1-4：GUI `#/work/<id>`、对话里的轮次分隔、右侧「各轮」窗格、侧栏一组一行
- [ ] G1-5：CLI `work ls/show`

## 十九、Lead 执行模式与工作区保留

> 2026-10-29，设计见 [tasks/LEAD.md](tasks/LEAD.md)。起因：修个小 bug 也要建 worktree，主导搬家丢上下文、再冷编译一遍；run 一结束主导被送回、检出被删，没法接着干。Lead 可选原地或 worktree，Worker 始终独立 worktree 可并行；Run 完成、会话结束、工作区清理拆成三件事。
> 顺序：§九 剩下的小 bug → G1-1 → P1 + G1-2 → G1-3 / G1-4 / G1-5 → P2 → P3。P3 开工前先定 LEAD §10 第 1 条（原地模式要不要后台集成 worktree）。

已完成 1 条（P1），见 [DONE.md](DONE.md) 同名一节。

- [ ] **要人定：LEAD §10 第 3 条——完成后保留 / 清理是工作组级设置还是每轮可改**（2026-10-30 记）。§9 的表里说该做成工作组的设置，但 §10 第 3 条把它列为待定、只写了「倾向工作组级」。P1 按**无条件保留 + 显式清理入口**做，不加任何策略字段：那个字段会被序列化进 `works.data` 成为兼容负担，而现在没有任何地方能改它（没 GUI、没 CLI 子命令），永远等于默认值。定下来之后加一个 `#[serde(default)]` 字段是向后兼容的，不会因为现在不做而变难
- [ ] P2 Lead 原地：拆开 `run.in_place` 的几层含义（Lead 模式 / 是否受版本控制 / 归属 / 保留策略）；原地主导直接认领不走 `lead_in`；按 checkout 的目录占用（落地也要遵守、Run 完成即释放）；主导自己改动的原地验收，结果绑定内容快照。不做 worker 进用户目录（LEAD §4、§7、§11）
- [ ] P3 混合派工：独立 index + 专用 ref 的工作区快照（不推送、大小上限、密钥文件提示）；worker 从快照并行开工；结果三方应用回原目录，全部算完再写、可恢复、不重复应用、保留人的暂存区（LEAD §5、§6）

## 二十、发起时选会话（已做）与它暴露的缺口

> 2026-10-30，run `01a18a4a`。人的原话：「我发起 work/chat 的时候，可以选择新的 session 或者已有 session」。
> 已完成 4 条，见 [DONE.md](DONE.md) 同名一节。

## 二十一、自举暴露的 hub 缺陷（2026-10-30）

> 已完成 5 条，见 [DONE.md](DONE.md) 同名一节。

## 二十二、仓库卫生（2026-10-30）

> 同一个 run 里顺手修掉的三件，记下缘由免得再犯。

- [ ] **`EventKind` 的 `large_enum_variant` 用 `#[allow]` 压住了，真修法是装箱**：变体装的是整条记录，`RunCreated { run: Run }` 约 592 字节，其余多为几十字节。装箱会牵动每一处构造与匹配，所以暂时加了带理由的 `#[allow]`，换来 `clippy --workspace -- -D warnings` 能对其余一切设闸。
- [ ] **ts-rs 解析不了 `deserialize_with`**：`cargo test` 每次都打 `failed to parse serde attribute | deserialize_with =`。目前用显式 `#[ts(type = ...)]` 绕开（`ContinuationUpdate` 的三态字段），但下次有人加 `deserialize_with` 而忘了标类型，bindings 会静默生成错的类型。
- [ ] **`cargo test --workspace` 在冷构建那一轮偶发失败过一次，没查到是哪条**（2026-10-30 记）：落 main 前的第一轮全量测试退出 101、有一条 `test result` 带非零失败，我当时只统计了数量没留下名字，之后连跑三轮全绿、零失败。与下面两条「与并发负载抢锁」的既有偶发是同一类条件（刚合并、整机在编译），但没有证据指向具体哪条。下次撞上要**先把输出存盘再分析**。2026-10-31 run `6d85f696` 撞上一次并留下了名字：`crates/mock-agent/tests/continuation.rs` 的 `settling_the_checkpoint_at_idle_does_not_swallow_a_stuck_report`（19 项里只它一条失败，当时同一台机器上另一个任务在全量编译；单独重跑该测试与同文件 19 项全绿）。它等的是「空闲时结算检查点」的时序，和下面两条一样属于与并发负载抢时间的一类，应当标成串行或独占资源。
- [ ] **`crates/core/tests/lock_kill_race.rs` 与并发负载抢锁，偶发失败**：和别的 crate 的测试一起跑时失败过一次，单独跑与重跑组合命令都 4/4。它测的就是抢锁，跟整个 workspace 的测试并行本就不可靠；应当标成串行或独占资源。

- [ ] **`switch_round` 直接 shell 调 `git checkout -b`，绕过 `WorkspaceProvider` trait**（2026-10-31 run `bb28e32e` 记）：`crates/orchestrator/src/engine.rs` 里同组续轮切分支那一处不走 `WorkspaceProvider`，而是自己起一个 git 进程。是分层瑕疵不是 bug——行为正确，测试也过——但它绕开了唯一该知道工作区怎么操作的那一层：换别的 provider（非 git、或远端检出）时这一处会静默失效，而且它拿不到 trait 那边的分支名占用检查（见 §二十二 上面那条 `-2`/`-3` 另起名字的规矩）。真修法是给 trait 加一个开分支的方法，让这一处调它。
- [ ] **`lead_self` 里 `continuing` 跳过 `serving_person` 检查，把 §十六 第一条那个 409 换了个位置**（2026-10-31 run `bb28e32e` 记）：`run_next` 必须跳过这个检查——主导正是在应答人的时候调它，不跳过就永远 409。但 `continuing` 这个口子同时也放过了 HTTP `POST /v1/works/{id}/runs` 那条路，于是「人正在和主导说话时不能给它派活」这道保护在 API 侧也没了，等于 §十六 第一条那个问题从工具侧搬到了 API 侧。真修法是把「这是续轮」和「免除 serving_person」分开表达，只让工具那条路免除。
- [ ] **`AgentActivity::push` 开头的 `retain` 会让「超过 2 小时没有任何中间事件的工具调用」把计数归零**（2026-10-31 run `bb28e32e` 记，审阅时 Leo 发现）：`crates/orchestrator/src/running.rs` 的 `push` 在 match 之前先清过期条目。某个工具调用连续两小时没发出任何中间事件（没有 `AgentToolDetail` / `AgentToolOutput` / `AgentTurnCompleted`），等它终于发事件时条目已被清，match 里 `get_mut` 拿不到，下一个事件便用 `TurnActivity::new` 重建，`started` 归零、`files_touched` 清零——主导会看到「本轮刚开始、改过 0 个文件」。和同文件 `RunningCommands` 的 `STALE_AFTER_HOURS` 是同一种取舍，方向安全（宁可少报不多报），所以没在本轮改。真修法是让过期判断只影响 `of()` 的读，清理走单独一条按 `AgentTurnStarted` 触发的路，而不是每个事件都先清。
