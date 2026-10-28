# 待办清单

> 来源：进程守护与 Cutex 对照的讨论（2026-10-23）。依据见 [DESIGN.md](DESIGN.md) §2、§4，
> [codex-app-server 实测](research/codex-app-server.md)，[claude-stream-json 实测](research/claude-stream-json.md)，
> [Cutex 前端拆解](research/cutex-gui-teardown.md)。
>
> 状态：`[ ]` 未开始 · `[~]` 进行中 · `[x]` 完成 · `[-]` 不做（已决策，理由留在条目里） · `[>]` 推迟到后续阶段

## 一、进程监督（优先）

目标：hub 或子进程挂了，不再整体停摆；一轮中断有明确状态；能自动 resume。

按 Cutex 的做法改为“agent 进程比 hub 活得久，新 hub 接管”，设计见 [PROCESS-SUPERVISION.md](PROCESS-SUPERVISION.md)。

- [x] 子进程退出时记录原因（exited / crashed / killed / lost），`Exited` 带 `cause` 与退出码，另发 `agent.exited` 事件（超时类原因未单独区分）
- [-] 子进程崩溃后不自动拉起（2026-10-23 已决定）：中断的那一轮不自动重放（可能重复执行命令、重复改文件），只标 `interrupted` 交给人或 Planner
- [x] 中断的那一轮给出明确状态：hub 重启时无法接管的轮次记为 `stop_reason: "interrupted"` 并附说明；能接管的轮次照常跑完
- [x] 运行时宿主登记在 `<home>/runtimes/`，hub 启动时握手确认后接管，不按 pid 杀进程（防 pid 复用误杀）；无人认领的宿主经连接停掉
- [x] Windows 上每个 agent 放进独立 Job Object（KILL_ON_JOB_CLOSE），宿主一退整棵树清掉；`acp-generic` 的 job 由 hub 持有
- [x] hub 退出默认把宿主里的会话留给下一个 hub（不 suspend，也就不会留下半截会话文件）；`hub stop --agents` 才停掉全部 agent
- [~] 真实 agent 上验证"轮次跑到一半 hub stop → hub start 后轮次继续"（2026-10-23）：codex、codebuddy 已通过（轮次跑完、上下文和进程不变，新版本 hub 也能接管旧版本留下的进程）；claude 本机未装，未验证（协议层已由 mock 自动化测试覆盖，只差真实 Claude Code）。**等环境**：本机没有 `claude` 命令，装上即可补测
- [x] 修复：`codebuddy-native` 包装层没转发 `adopt`，导致 CodeBuddy 无法接管（实测发现，mock 测试只覆盖 codex 所以没测出）
- [x] 给 claude-native / codebuddy-native 补接管的自动化测试：mock 新增 `stream-json` 模式（`crates/mock-agent/src/stream_json.rs`），`tests/adopt.rs` 对 codex、claude、codebuddy 各跑一遍"轮次跑到一半 hub 停 → 新 hub 接管 → 轮次跑完、进程不变"；已验证能抓到 codebuddy 漏转发 `adopt` 那类 bug
- [x] 宿主会继承拉起者的可继承句柄（Windows）：hub 拉起宿主前清掉自身 std 句柄的继承标志（`drivers::process::stop_inheriting_std_handles`，`hub start` 复用同一函数），`hub run | …` 在 hub 退出时立即结束；回归测试 `tests/pipe.rs`。hub 直接拉起的子进程（acp-generic）之间的管道句柄泄漏未处理，它们随 hub 一起结束，影响不大
- [-] 接管 codex 时，如果正在跑的那一轮的 id 一直没出现，就没法向 Codex 发 interrupt，只能本地放弃等待（已知边界：只影响中断是否及时，不影响接管正确性，该轮仍会按 `interrupted` 收尾）
- [>] Linux / macOS 上实测宿主（用进程组代替 Job Object，目前未验证）：推迟到 B1 节点阶段，那时驱动本就要拆到节点上跑，一并测

## 二、hub 与 GUI 边界

> 2026-10-23 复核：清单与分级已整理成 [API-AUTH.md](API-AUTH.md)，含全部接口、调用方与三级划分。

接口清单：

- [x] 产出 [API-AUTH.md](API-AUTH.md)：从路由表导出全部接口，按读 / 写 / 管理三级分类，作为后续改造的对照表
- [x] 记清现有三档入口：`/v1/health` 免鉴权；`authed` 组共用总 token；`/v1/me`、`/v1/me/tools*` 只认每会话 agent token
- [x] GUI 侧对账：`apps/gui/src/api.ts` 的方法逐条对过路由表，没找到孤儿接口，无需删除；调用方已记入 API-AUTH.md
- [x] CLI 侧对账：`crates/cli/src/client.rs` 与 `cmd/*.rs`，`hub status` 读免鉴权 `/v1/health` 加鉴权 `/v1/info`、`hub stop` 打 `/v1/shutdown`

权限分级：

- [x] 把校验从 `require_token` 抽成可替换的策略点，分 read / write / admin 三级；A1 接 users/devices 时换实现，不动路由（`crates/hub/src/access.rs`；总 token = admin，新增 `<home>/token-read` = read，会话 token 只到 `/v1/me*`；DOGFOOD §7）
- [x] 管理级：`/v1/shutdown`；账号增删与登录（`/v1/accounts*` 的写接口）。插件模型表归写级：只影响界面显示哪些模型
- [x] 写级：agent 增删启停取消压缩、改名改话题归档改档案连接、发消息与撤回；run 的 approve / reject / cancel / land / finish；question 的 answer / dismiss；插件模型表
- [x] 读级：`/v1/info`、agents 列表与详情、transcript、turn diff、plugins、usage、quota、accounts 读接口、runs 读接口、events、notes
- [~] `ErrorCode` 增 `Forbidden`（现在只有 `Unauthorized`，见 [api-types](../crates/api-types/src/lib.rs)）；越权返 403 而不是 401（已做）；GUI 据此置灰入口并说明原因（未做：GUI 总用总 token）
- [x] 验收：只读凭据打写接口得 403，会话 token 打管理接口得 403，错误信息能看出缺哪一级（`crates/hub/tests/api.rs` `credentials_have_levels`）

令牌与免鉴权面：

- [x] Electron 主进程加「方法 + 路径」白名单：只放行 GUI 实际调用的接口，页面无法触达 `/v1/shutdown` 与 `/v1/me*`；新增调用要同步改 `ALLOWED`（[main.cts](../apps/gui/electron/main.cts)），拒绝信息会点明这一点
- [x] 收敛免鉴权面：`/v1/health` 只回 `status` 与 `api`，`version`、`uptime_secs` 收进需鉴权的 `/v1/info`；CLI `hub status` 改读 `/v1/info`（A2 放开 `listen` 后这些就是白送的情报）
- [x] 令牌轮换确认：GUI 每次调用重读 `token`；CLI 每次构造 `Client` 时重读（`for_hub` 内 `read_token`），都是短命进程，hub 重启换 token 不需要重启客户端
- [-] token 文件 Windows ACL：`write_private` 只在 Unix 分支设 `0o600`，Windows 是裸 `fs::write`。核查后不加：`~/.crossagents` 不在云同步目录内，NTFS 默认 ACL 已限制到本用户、SYSTEM 与 Administrators，同机其他用户读不到。home 若被移到同步目录再处理

版本不一致：

- [x] `api` 不等（GUI 期望 v2、hub 还是 v1）→ 拒绝连接，提示 hub 与 GUI 版本不匹配、请一起更新，不静默降级。事件流连上后先查 `/v1/info` 的 `api`（[hub.ts](../apps/gui/src/hub.ts) `API_VERSION`，升版时要和 api-types 一起改）；不等则丢弃事件、拒绝其余请求、整页换成说明，hub 重启后自动重查。mock 场景「版本不匹配」可看效果
- [ ] `version` 不等但 `api` 相同 → 允许连接，在侧栏版本号旁标不一致，并隐藏该版本没有的功能入口
- [x] 给 `API_VERSION` 定升版规则：写进 [API-AUTH.md](API-AUTH.md) §5，分「必须升版」（删字段、改语义或单位、改路径方法、新增必填、收紧校验）与「不必升版」（新增可选字段、新接口、新错误码取值）两类
- [ ] 验收：旧 hub + 新 GUI、新 hub + 旧 GUI 各跑一次，提示可见、界面不崩、能重置连接

## 三、常驻守护（等第一步稳定后再做）

> §一 完成后，hub 停了 agent 照常跑，下一个 hub 接管。这一节剩下的是让 hub 本身常驻、开机自启。

- [~] 设计 hub 常驻模式：关闭窗口后 hub 继续运行，窗口重开时重新连接并拉取快照。安装版 GUI 起的 hub 是脱离的后台进程，关窗口后照常运行，重开时直接连上；缺的是开机自启（下一条）
- [ ] Windows 自启方案：计划任务或服务，二选一并写明理由（权限、登录前运行、崩溃重拉的节流）；Cutex 用的是按用户的计划任务 + 内置 supervisor
- [x] hub 的停止命令：`hub stop` 只停 hub，agent 留给下一个 hub；`hub stop --agents` 停掉全部 agent（取代原来的"等待会话 suspend 后再退出"）
- [x] 安装版 GUI 自带 hub：安装包带同版本 `crossagents.exe`，启动时复制到 `<home>/bin/<版本>/` 并从那份启动（运行时宿主、`mcp` 都从 `current_exe()` 拉起，按版本分目录才不锁住安装目录）；没有 hub 就起，自己的 hub 版本旧就 `hub stop` 再起新版，agent 被接管；手动起的 dev hub、远程 hub 不碰。判断依据：`hub.json` 的 `exe` 在 `<home>/bin/` 下。旧版本目录在无进程使用后清理。设计见 [MULTI-NODE.md](MULTI-NODE.md) §13，手测见 [tasks/U1.md](tasks/U1.md)
- [x] GUI 自动更新：electron-builder 改出 NSIS 安装包（按用户安装），electron-updater 从 GitHub Releases 后台下载、退出时安装；打包前检查 GUI 与 Cargo 版本一致
- [x] 建公开的发布仓库 `L1nkkkk/CrossAgents-releases`（主仓库私有，见 MULTI-NODE §13.5），发布 v0.1.0（2026-10-25：`electron:build` 打包后用 `gh release create` 上传，资产名与 `latest.yml` 一致）
- [x] 发 0.1.1 时验证真实的在线更新：装着 0.1.0 的 GUI 能在后台下载并在退出时装上新版（2026-10-28：启动 3 秒发现 0.1.1，按 blockmap 差量下载 8.4 MB / 116 MB；托盘退出后静默安装约 6 秒；重开后起 `bin/0.1.1` 的 hub 并清掉 `bin/0.1.0`）
- [ ] 旧 hub 遇到新库时说清楚：本机跑过 dev hub 后 `state.db` 已有 0.1.0 不认识的迁移（`migration 9 was previously applied but is missing`），0.1.0 的 hub 直接退出，GUI 只显示「cannot start the local hub」，hub.log 里没有原因。至少把这句错误带进 GUI 和 hub.log，提示「数据库来自更新的版本，请升级」
- [ ] 安装包与 exe 代码签名（未签名时 SmartScreen 首次运行会拦）
- [x] 无窗口时的额度与预算上限（2026-10-24 决定，见 [tasks/DOGFOOD.md](tasks/DOGFOOD.md) §5）：hub 不在时只记账、hub 回来时结算——宿主缓存并补发用量事件，新 hub 启动时先读账并立即检查一次预算，再恢复调度；不在宿主里强制。Chat 默认上限仍未做。原记录：暂缓（2026-10-23 决定）。现状：Work run 的 `policy.budget` 只在 hub 在时检查（`crates/hub/src/budget.rs`），hub 不在时宿主里的 agent 不受限，Chat 会话没有上限。开工前要定：hub 不在时超限是宿主停在轮次之间，还是只记账等 hub 回来处理；Chat 要不要默认上限

## 四、可观测性

- [x] `cli` 增加状态命令：`hub status` 除后台 agent 进程（pid、启动时间、隔离方式）外，汇总各会话——总数、归档数、各状态计数、正在干活的名单；处于 error 的会话逐个读 transcript 的 `error`，截成一行显示（最多 20 个）。`--json` 同步给出 `sessions` 与 `errors`
- [x] 事件日志落盘：每条事件一行 JSON 写进 `logs/events-<date>.jsonl`，与 `GET /v1/events` 同格式；独立线程无缓冲写入（读方立即可见，`Get-Content -Wait` 可跟），队列满时丢弃并计数、不拖慢 hub；按天滚动，启动时删 14 天前的文件（只认 `events-*.jsonl`，不动别人的文件）。可关：`[hub] event_log = false`。读：`crossagents events --log [--since N]`，hub 没在跑也能读
- [x] GUI 显示"hub 离线"时的诊断信息：侧栏未连接时显示 `offline: <原因>`，原因来自 Electron 主进程且带修复指引（如 `hub is not running (…); start it with crossagents hub start`）

## 五、Claude 运行时（对照 Cutex 的借鉴项）

- [x] 设置源开关（2026-10-24，DOGFOOD §6：编辑队友面板三档预设，`POST /v1/agents/{key}/settings`）：Profile 级别选择是否读取项目的 CLAUDE.md、`.claude/` 规则、hooks、权限。驱动层已能读——`setting_sources` 从 agent config 取，默认 `project,local`（`crates/drivers/src/claude_native/mod.rs`，透传给 `--setting-sources`）；缺的是 GUI 开关与几个预设组合
- [ ] hooks 默认关闭，需显式开启（参考 cutex 的 "Allow commands" 开关）
- [ ] 运行时版本钉住：每个 agent 记录所用的 Claude Code 版本，升级后单独 repin，而不是全局切换
- [ ] 每次升级 Claude Code 后跑 `claude_native` 冒烟测试（DESIGN.md §测试）

## 六、Codex 驱动

- [ ] 单进程线程数上限压测（codex-app-server 研究里标为"未确认"）
- [ ] 空闲线程是否会被服务端自动卸载，确认后更新挂起策略
- [ ] 每个线程的 MCP 子进程开销统计，超过阈值时提示
- [x] code-mode 模型（模型目录里 `tool_mode: code_mode_only`，如 gpt-6-luna）只看得到 `exec`，MCP 工具藏在 `tools.mcp__crossagents__<tool>` 里：只被告知「调用 task_complete」的 worker 找不到它，每轮都不上报、任务停住（2026-10-25 真实 run 里发现）。Codex agent 的开场说明写明工具名与 `exec` 里的调法；派给 worker 的任务末尾都提醒用 `task_complete` 收尾

## 七、多模态输入

现状：`SendMessageRequest` 只有 `text` 字段（`bindings/SendMessageRequest.ts`），driver 层也没有附件类型，发不了图片和文件。

- [x] 消息结构从纯文本改为内容块（text / image / file），`SendMessageRequest` 增加 `attachments`，重新生成 ts-rs 绑定（收件箱里是 `Prompt { text, attachments }`，`DriverSession::prompt_with`；见 [tasks/DOGFOOD.md](tasks/DOGFOOD.md) §3）
- [x] 附件落盘到 hub 数据目录，按会话归档并记录 sha256，不把 base64 直接存进 SQLite（`<home>/attachments/<agent id>/<sha256>`）
- [x] 大小与类型限制：单文件 10 MB、每条 8 个、合计 20 MB，图片 / PDF / JSON / 文本；hub 返回 400，GUI 和 CLI（`crossagents send --file <path>`，可给多次）发送前同规则检查
- [x] GUI：拖拽、粘贴截图、选择文件；输入框显示缩略图，发送前可删除
- [~] `claude-native`：映射到 stream-json 的图片内容块（PDF 为 `document`，小文本内联），实测图片与文件的支持范围还没做
- [~] `codex-native`：映射到 app-server 的图片输入（`localImage`），实测后确认字段
- [x] `acp-generic`：读取 agent 声明的 prompt 能力，未声明图片则不允许发图（`initialize` 的 `agentCapabilities.promptCapabilities` 记在会话上，图片按 ACP image block 发送；没声明就由 hub 返回 400，不再退化成路径引用）
- [x] 按能力降级：驱动不支持的附件类型，GUI 禁用入口并说明原因，不静默丢弃（`Capabilities.attachments`：claude-native / codebuddy-native 收图片、PDF、文本，codex-native 不收 PDF，acp-generic 启动前为 `null`、会话启动后按声明；transcript 把它给 GUI，`POST /v1/agents/{key}/messages` 遇到不收的类型返回 400 并说明原因；GUI 收窄文件选择器的 accept，粘贴/拖入不支持的类型时报出原因）
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

> 来源：2026-10-24 自举清单，设计与取舍见 [tasks/DOGFOOD.md](tasks/DOGFOOD.md)，每项一个提交。

- [x] P0-1 自举隔离：长寿进程（运行时宿主、`mcp`）与 `hub start` 起的后台 hub 都从 `<home>/shadow/<hash>/` 的副本跑，`cargo build` 不再撞上被锁的 `crossagents.exe`；worktree 里的 agent 各有绝对路径的 `CARGO_TARGET_DIR`；`hub sandbox start|stop|status` 起临时 dev hub（独立 home、随机端口）；agent 的 shell 继承 `CROSSAGENTS_HUB_HOME`，`hub stop` 停自己所在的 hub 时拒绝（`--force` 例外）
- [x] P0-2 GUI 审查闭环：`GET /v1/runs/{key}/tasks/{task}/diff`（任务全量 diff）、`GET /v1/runs/{key}/diff`（run 对 base 的总 diff）、`POST /v1/runs/{key}/tasks/{task}/review`（逐行 / 逐文件意见以人的身份进 worker 收件箱，`rework` 时打回）；任务详情「审查」节带行号可评论，落地卡片「查看全部改动」；mock 场景「审查任务」「落地前总 diff」
- [x] P0-3 agent 自己验证：消息带附件（hub 落盘、事件与 transcript 带元数据、claude / codex 原生映射、其余驱动写成引用）；GUI 粘贴 / 拖入 / 选文件、时间线缩略图；`npm --prefix apps/gui run screenshot` 截 mock 预览，协作工具 `gui_screenshot` 把 PNG 作为 MCP 图片返回；mock 场景「消息带附件」
- [x] P0-4 main 动过之后的落地：落地前在集成 worktree 里 `git merge <base>`（选 merge 不选 rebase，理由见 DOGFOOD §4）；冲突时建 `sync-<base>` 任务交给 run 的默认 worker、写 `run.note`、通知 lead，任务合并后再落地；`land` 返回 `LandOutcome`；mock 场景「落地等冲突解决」
- [x] P1-5 看额度派活：`AgentHost::route` 按账号健康度（窗口 ≥ 90% 快用完、100% / `reached` / credits ≤ 0 已用完）在派活时换同厂商账号或换别家队友；worker 撞限时由新克隆在同一 worktree 接手；任务卡片进度行、`task.rerouted` 事件、lead 通知三处说明；预算在 hub 回来时先结算再调度；mock 场景「额度改派」
- [x] P1-6 统一项目规则：`CROSSAGENTS.md` → `AGENTS.md` → `CLAUDE.md`（从工作目录找到仓库根），注入给不会自己读该文件的 agent 的开场说明，各驱动按自家格式（Claude `--append-system-prompt`、Codex `developerInstructions`、ACP 首条消息）；Claude 设置源三档开关；mock 场景「Claude 设置源」
- [x] P1-7 分级审批与权限：`PermissionPolicy::Guard`（Claude 走 `can_use_tool`、Codex `untrusted`），hub 判断危险动作（push、发布、改 CI、`rm -rf` 到工作目录外等）后以 `HumanAction::Approval` 问人，超时 / `human_ask=deny` 时拒绝；Work worker 默认守卫（run 策略 `approvals`），Chat 可在 `config.permission` 打开；API 三级权限与 403 `forbidden`；mock 场景「危险操作待批准」
- [x] P1-8 可追溯的失败：`trace.rs` 从事件折叠出 run 的追溯（按 lead / 任务：轮次与出错、命令与退出码和输出尾巴、`workspace_run`、验收、改派、用量、首个失败）；`GET /v1/runs/{key}/trace`；`crossagents run trace <id> [--log]`（`--log` 读磁盘事件日志，hub 不在也能查）；run 页「追溯」节；mock 场景「失败追溯」
- [x] 验收与落地的四处漏洞（2026-10-27，每条一个提交）
  - Windows 下带引号的检查命令假通过：`cmd /C` 经 `arg` 传入时引号被转成 `\"`，`powershell -Command "exit 7"` 返回 0。改为 `raw_arg` 拼 `/D /S /C "<命令>"`；`workspace_run` 同一路径一起修好（`verify.rs::shell`）
  - 检查超时后孙进程还在改工作区：`Job` 从 drivers 挪到 `core::process`，新增 `spawn_tree`——Windows 以 `CREATE_SUSPENDED` 启动、放进带 KILL_ON_JOB_CLOSE 的 Job 后再恢复线程，没有逃出 Job 的窗口；Unix 用独立进程组、结束时 `killpg`。超时、出错、命令结束都整棵树一起结束
  - 主导落地的代码没经过检查：`run_land` 改为「同步基线 → 跑检查 → 合入」（`landing.rs::ready_to_land` / `merge_run`）。检查在 run 锁外跑，合入前在锁内比对集成工作区的 HEAD，变了就重新同步、重新检查，三次仍在变就拒绝。人从 Run 页 / API 落地仍不跑检查
  - 同一张 Work 卡片能建出两个 run：`Orchestrator::lock_card` 按卡片 id 加锁，`answer_human` / `dismiss_human` 在锁里重读状态，后到的返回 409 `<id> is already answered`；锁表在最后一个持有者释放时移除该项
- [ ] 2026-10-27 trace run `01a17c78…dccf42e8` 发现的问题（未修）
  - [x] 检查没跑完就被合并（2026-10-28 修）：`task_complete` 不再直接置 `Passed`，改为 `Running|WaitingAnswer|Stalled → Verifying`，并给这次验收生成一个独立 UUID 写在 `report.verification` 上。hub 在后台 `report_done` 里提交、跑 `spec.acceptance`、把这次检查对应的 commit SHA 记进 `report.verified_sha`，然后才 `Verifying → Passed` 并把 worker 总结转给主导；回写前校验「任务仍是 Verifying 且标识一致」，不一致（返工、新一轮验收、run 被取消）就丢弃。现在 `Passed` 只表示「报告已就绪，等 issuer 决定」，检查失败、超时、执行异常都如实记在 `report.checks` 里，不新增状态。`task_accept` 的门禁：（1）任务在 `Verifying` 时一律拒绝，`force` 也不绕过；（2）普通 accept 要求检查全通过且 worktree 相对已验证 SHA 无新改动（未提交文件也算），只合并这个 SHA，不再 `commit_all`；（3）检查失败 / `unchecked`（`spec.acceptance` 为空）/ 中断 / 接收 Stalled / 验证后有新改动，都要 `force: true` 加非空 `reason`，原因写进 `report.forced` 与任务进度行留审计；（4）`task_drop`、`task_cancel`、删除 run 在 `Verifying` 时同样拒绝（都不持锁等验收，避免与 `report_done` 死锁）。重启后遗留的 `Verifying` 转 `Passed` 并标 `report.interrupted`（按失败处理），需要时用新工具 `task_verify` 生成新 UUID 重验。`let_go` 的归档 / 送回失败不再只记日志，而是一路返回到 `release_agents` 的调用方，`cleanup_worktree` 在无法确认 agent 已停时跳过删除、保留 checkout。测试见 `mock-agent/tests/verify_gate.rs`
  - [x] 构建目录按 worktree 隔离与回收（2026-10-28 修，同 §十六第一条）：`CARGO_TARGET_DIR` 从「同一仓库所有 run 共用一个 `<root>/targets/<repo>-<hash>`」改为 `<root>/<run>/targets/<tree>`，`<tree>` 是 worktree 的目录名（`integration` 或 task key），worker 与 hub 验收在同一 tree 里共用，`CARGO_HOME` 仍共享。`workspace::worktree_env` 不再依赖 `repo_key`；cwd 落在 `<run>/targets` 下、正好是 `<run>` 本身、或旧的 `<run>/target` 时返回 None（不设变量）。`targets` 是保留名：`plan::validate` 拒绝同名 task key，`keys_in_use` 跳过它。回收：`cleanup_worktree` 在 worktree 删掉后回收该 tree 的 target；`remove_integration` 只回收 `targets/integration`，不再递归删整个 `targets`；`<run>/targets` 与 `<run>` 空了才 `remove_dir`。删除前先改名 `.trash-*`，改名失败就留到下次重试（Unix 上改名不会失败，正确性靠「checkout 已不在、任务不在 Verifying」这条规则）。启动时与每次回收时扫描所有 `<run>/targets/*`，清掉 checkout 已不在且不在 `Verifying` 的孤儿目录和残留 `.trash-*`。旧布局 `<root>/targets/<repo-key>` 与 `<run>/target` 不再写入，沿用原来的 `keys_in_use` 保护逻辑、阈值改为 0（没人用就删）。`build_cache_limit`（`[hub] build_cache_gb`）改为「整个 worktrees 根目录下构建产物总量」的软预算：超出只回收上面允许删的目录，回收后仍超就记 warn 并发 `hub.build_cache` 事件，不动正在用的目录，不做硬上限、不做派工准入；默认值 30 GB → 120 GB（每个 worktree 一份 target，默认并行度 3 的 run 最多占 4 份）。不接 sccache。冷编译实测（Windows 本机，全新 target）：`cargo check --workspace` 32 秒、646 MB；`cargo test --workspace --no-run` 68 秒、10.0 GB（含 debug info 与全部测试二进制）——预算按后一个量级算，四份就是 ~40 GB，120 GB 约等于三个 run 同时在跑
  - [x] 门禁与回收的补修（2026-10-28，同一 trace 的后续）：① `report_done` 任何一步出错（`commit_all`、worktree 缺失、`base_of`、写库）都不再让任务停在 `Verifying`——错误写进 `report.failure`、清掉 `verified_sha`，在锁内确认「仍是 Verifying 且标识一致」后转 `Passed`，并把「什么都没验证」连同原因转给 issuer；`complete` 与 `task_verify` 两处 spawn 走同一条路。② `engine.rs::resume` 的 `Verifying` 不再受「continuation 开着且 worker 忙」那段 `continue` 影响：验收属于 hub 不属于 worker，一律标 `interrupted` 转 `Passed`。③ `cancel`（取消整个 run）在有任务 `Verifying` 时返回 Conflict，写明等结果再取消；检查放在 run 锁外，不持锁等验收。④ `lead_left` 不再调递归删整个 `targets` 的入口，改为 `prune_run_targets` + 新的 `run_targets_in_use`：只回收 checkout 已不在的 tree，`Verifying` 的保留；删除 run 仍一次清空整份（`remove_run_build_dirs`）。⑤ `task_accept` 强制合并时 `merged_sha` 返回真正合入的提交（worktree 当时的 HEAD），不再回填 `verified_sha`。⑥ 释放失败的测试：AgentHost 替身让 `send_home` / `archive_agent` 报错，断言 checkout 与 `targets/<tree>` 都留着，正常释放时照样删。测试都在 `crates/orchestrator/src/tests_verification.rs`（替身 host / workspace / verifier + 内存 SQLite）
  - 派工通知写的是模板名：`engine.rs::approve_locked` 的 "Who does each task" 用的是 worker 模板（如 Dara），`task.handed_off` 的 `to_name` 也一样，实际干活的是克隆出来的 `w-<run>-<task>`。主导照着通知给 Dara 发消息，Dara 不在这个 run 里（`you are in no run`），白跑了一轮。改法：通知和 handoff 写实际执行会话的名字，同时注明来自哪个模板
  - 克隆把原会话的 `config.spawned_by_run` 也带过来了，应该去掉；批准时建的模板会话（如 Orion、Zara）没有用到，一直留在名单里
  - GUI：编辑类工具失败（with-cli 报 "模型返回的待替换代码块未匹配到整行"）时，工具行仍按模型打算改的 diff 显示 `+2 -0`，`detail.error` 也不显示（`Session.tsx` 只有部分工具类型会读它）。失败的编辑不该显示增减行数，原因要能点开看到
- [x] 2026-10-28 三处让 run 收尾失败、`cargo test --workspace` 在 main 上红的问题（与业务改动无关，逐条修好）
  - with 驱动的 MCP 超时按秒写、with-cli 按毫秒读（2026-10-28 修）：现象是主导用 with 插件调 hub 的 `run_finish` 时，每次都在 1.8 秒后报「请求超时, 超时时间:1800 ms」。`run_finish` 要同步跑 `cargo test --workspace`（几分钟），主导永远拿不到检查结果，run 停在 `planning`，只能靠人去看 `run.checks`。实例 run `01a18178…43af3611` 连调 3 次次次超时。原因是 `crates/drivers/src/with_native/mod.rs` 的 `MCP_TIMEOUT_SECS = 1800` 被原样写进 `mcpServers.<name>.timeout`，而 with-cli 把这个字段当毫秒（它自己的报错文案就是 `请求超时, 超时时间:%d ms`，可从 `with-cli.exe` 里读到）。改为 `MCP_TIMEOUT_MS = 30 * 60 * 1000`，配置写 `1800000`，注释写明单位与依据，单元测试断言「30 分钟换算成的毫秒数」。其余驱动一起核过：codex 不写超时（用默认）、claude / codebuddy 的 `--mcp-config` 不写超时、acp 协议没有这个字段，单位都与各自客户端一致，未改。`run_finish` 仍是同步的，检查时间没缩短
  - `delete_run.rs` 两条测试卡在归档时机（2026-10-28 修，改测试）：`a_landed_run_goes_with_its_worktrees_branches_and_lead`（`:170`）与 `a_dirty_checkout_asks_for_force`（`:268`）在 `run_finish` / `POST /v1/runs/{id}/finish` 返回后立刻断言主导已归档、检出已删。6df4957 之后这是错的时序：复制出来的主导要等「本轮结束且没有排队消息」才归档（`AgentHost::archive_agent_after_turn` + `runtime.archiving`），检出由 `orchestrator.leaving → lead_left` 在那之后清。先核过行为本身：两条路径（主导自己 `run_finish`、人点结束）下，主导空闲就当场归档、正在忙就挂进 `archiving` 等回合结束，都会归档并删检出，没有永远不归档的路径，所以只改测试——新增 `until_lead_left` 轮询「已归档 + 检出没了」（100ms × 300 次，带 panic 文案），断言放到等待之后，注释从「run 结束时归档」改为「结束这一轮后归档」。同一类时序假设在 `lead.rs`（归档后恢复检出、分支没了重建、删 agent 清检出）、`work_start.rs`（结束后再开 Work）、`session_reuse.rs`（`pinned_run` 建的两条 `a_copy_that_resumes_only_where_it_ran_*`）里也一起改了：等归档与检出删除、等恢复后检出重建、并先等关单那一轮真正结束（否则恢复后的等待会吃到上一轮的 `turn_completed`，把旧输出当成回答）。`lead.rs` 那条 landed run 的恢复不再依赖「`lead_left` 删没删分支」的竞态，测试自己先删分支，让检出固定从 base 重建。另外 `tests/common/mod.rs` 的 `Watcher::until` 从 15 秒放宽到 30 秒：整套 `cargo test --workspace` 并发时 15 秒不够（e7b18db 已记为既有 flake），等的是一个本来就会到的事实，不该提前判失败
  - 持续推进的检查点停在 `working`，重启前后不一致（2026-10-28 修）：`continuation::reserved_due_time_survives_restart_and_default_runs_stay_unchanged`（`:502`）唤醒后读到 `phase="working"`、`next_at=None`，重启同一个 run 变成 `phase="continue"`、`next_at=last_attempt_at+1h`，`attempts` 与 `last_attempt_at` 相同。根因：检查点只在 hub 时钟每秒一次的 `continue_runs` 里写，主导回合结束时没有任何人重算它，于是「主导正在这一轮里」那个观测（`working`、没有预留）被持久化下来；重启后的第一次推导直接是 `continue` 并重新预留，两边自然不同。引入提交 `3d8f6e5`（`feat: add opt-in bounded run continuation`，`git log -S "The lead has a turn or queued messages"` 只命中它，此后 `continue_run` 的调用方一直只有时钟）。修法：`continue_run` 加 `deliver` 开关，新增 `Orchestrator::settle_continuation(lead)`，在 `AgentStatusChanged → Idle`（回合已发布结束、且队列为空时才置 Idle）时按同样的事实重写该 run 的检查点；`deliver=false` 保证不计数、不投递唤醒，已经到期的 `next_at` 留给时钟下一次投递。测试在重启前先 `phase(..., "continue")` 等检查点落定，并断言 `attempts == 1`、`next_at - last_attempt_at == 60 分钟`
- [ ] plan_approval=required 时主导可以不提计划就动手（2026-10-28 记，未修，要不要拦由人定）：run `43af3611` 的 `plan_approval` 是 `required`，主导一跳过 `plan_submit`，直接在 `planning` 状态下改代码、`git commit` 并调用 `run_finish`，hub 没有拦。现在只在「有计划待批」时卡住流程，主导自己不提计划就绕过了审批门禁；可选做法是 `planning` + `required` 时拒绝改文件的工具与 `run_finish`（或先要求 `plan_submit`），但这会改变主导能干的事，先记下来等决定
- [ ] `settle_continuation` 会吞掉卡住提醒（2026-10-28 记，未修）：7bc3aff 在主导变 Idle 时调 `continue_run(id, false)`，`deliver=false` 只挡住计数和投递，卡住检测照跑（`continuation.rs` 约 :336-353）。若主导变 Idle 那一刻恰好有 worker 安静满 `stuck_minutes`，这次刷新会写入卡住文案、`next_at=now`，并把 `stuck_watch.progress_at` 重置为现在，但什么都不发；下一秒时钟重算时安静时长已归零，文案退回普通「继续推进」并照常投递，主导收到的是普通唤醒，「N 分钟没进展、在等谁」丢了。修法：`deliver=false` 时跳过卡住判定、不重置 `progress_at`，只刷新 phase / next_step / next_at；补测试：主导在 worker 安静满阈值时变 Idle，下一次唤醒仍是卡住提醒

## 十、agent 描述文件

> 2026-10-26 第一轮：内置三家的硬编码迁到描述文件，hub 按描述扫描本机、Agents 页显示结果。格式与优先级见 [DESIGN.md](DESIGN.md) §9.3。

- [x] 协议留在代码里，协议以外的写进 `plugin.toml`：内置三家在 `crates/plugins/agents/*.toml`（`include_str!` 编进 hub），外部 agent 同一种格式；`locate`（env / commands / paths / bundled / shim / version）、`install`（hint / docs / login / notes / label）、`[models]`（fallback / efforts）；每项都能按系统给。`manifest.rs` 不再只认 `protocol = "acp"`
- [x] 查找统一成 `Located { argv, path, source, version }`（`crates/core/src/cli.rs`），三家驱动不再各写一套：`native_command` > `env` > `PATH` > 自家安装路径 > 桌面版自带，命中后按名字过 shim（`codex-vendor` 解 npm 外壳、`node-script` 改用 `node <script>`）；版本 5 秒超时
- [x] 查找不缓存、版本按 (path, 修改时间) 缓存：hub 运行期间装上、就地升级、桌面版更新删掉旧构建目录，`installed()` / 开会话 / `/v1/plugins` 都拿到当下的结果；版本在 hub 启动时后台并行探（`Driver::probe_cli()`），探测期间 `version` 为空但不挡接口；claude / codebuddy 开会话时才取命令，codex 的进程键仍是未解析的 `codex app-server`（接管要按它匹配）。测试用注入的 `Machine` 和临时目录覆盖「后来装上」「旧构建被删、出现更新的构建」「文件时间变了重新探测」
- [x] `PluginInfo.cli`（`installed` / `path` / `source` / `version` / `install_hint` / `docs`；新增可选字段，不升 `API_VERSION`），`bindings/` 已重新生成；设置 → 智能体页顶部「本机的 CLI」，显示 `2.1.304 · 原生安装`，悬停看完整路径，没装的给可复制的安装命令
- [x] 迁移有据可查：描述文件与旧 `PluginConfig::claude()/codex()/codebuddy()` 逐字段相等的对照测试（迁完改成固定期望值）；本机三家迁移前后的 argv 逐字节相同
- [x] 安装按钮：按 `install.hint` 执行并把进度显示在 GUI（2026-10-26，9971310：描述文件 `[[install.methods]]` 声明安装与升级方式，hub install / upgrade 接口与 `cli.install` 事件流，GUI 一键安装/升级与确认框；Claude 的真实安装未在本机实测）
- [ ] 从 CLI 读模型列表：Codex 仍读 `models_cache.json`，其余家各自有列表接口后接进来
- [ ] 描述文件里的账号段：登录方式、配置目录与环境变量也写进描述，加一家账号不用改 `*_account.rs`
- [ ] 版本钉住与自下载 CLI（DESIGN §9.4 的"已验证范围"要靠它）

## 十一、交给 CrossAgents 自己改自己

> 2026-10-26，任务书见 [tasks/SELF.md](tasks/SELF.md)。按 A → B → C 顺序，A 做完再放开后面。

- [ ] A1 run 共享 `node_modules`，GUI 的 `tsc --noEmit` 与 `node --test` 进 `crossagents.toml` checks
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
- [x] 落地到人自己的 checkout 时，只在暂存区有改动、或未提交 / 未跟踪的文件与 run 要改的文件重叠时拒绝，其余改动原样保留（2026-10-26，`workspace.rs::check_checkout`；之前一律拒绝，人在主目录改着别的东西时 run 落不了地）

## 十二、run 的收尾：再开 Work 与删除

> 2026-10-26，方案见 [tasks/R1.md](tasks/R1.md)。起因：Pia 主导的 run 已 `completed`，再开 Work 被拒；停在 `planning` 的旧 run 删不掉。A、B 都已完成。
> 已定：硬删除不做归档、未落地的分支默认保留（R1 §3.2）。

A 已结束 run 的主导再开 Work：

- [x] `work_start` 只拦未结束的 run（现在只看 `planner`，与 `team.rs::lead_run` 标准不一致）（2026-10-26，`work.rs`：`r.planner == caller.id && !r.status.is_terminal()`）
- [x] `NewRun` 加可选 `repo` / `base_branch`；`home_of(agent)`：住在某 run 集成工作区里的主导，取回该 run 的仓库与基线。`work_decided` 与 `POST /v1/runs` 都走它（否则新 run 以旧集成工作区为仓库、以旧 run 分支为基线）（2026-10-26，`engine.rs::home_of` / `run_lived_in`）
- [x] 旧 run 未落地时，Work 卡片提示新 Work 从 `main` 开始、不含上一个 run 的改动（2026-10-26，提示按旧 run 的 `base_branch` 写；不自动合并旧分支）
- [x] 测试：结束后能再开且落在原仓库、基于 `main`；未结束仍拒绝；首页以旧主导新建同样正确（2026-10-26，`mock-agent/tests/work_start.rs` 新增三条）

B 删除 run：

- [x] core：`Store::delete_run`、`EventKind::RunDeleted`（2026-10-26，另加 `CoreError::ConflictFiles`：409 带上挡路的文件）
- [x] store-sqlite：一个事务删 `human_items`、`notes`、`tasks`、`runs`（不依赖 CASCADE）（2026-10-26，`human_items` 没有 run 列，按 JSON 里的 `run_id` 删；删不存在的 run 返回 NotFound）
- [x] orchestrator：`delete_run(id, { branches, force })`，只删已结束的；按序移除主导副本与招募的 agent、任务工作区、集成工作区与构建目录、按需删分支，最后删库并发事件；单步失败只记 warn（2026-10-26，`delete.rs`；返回 `DeletedRun` 清单）
- [x] 分支：已落地默认删，未落地默认留（`branches=true` 才删）；集成工作区有未提交改动且未落地时，不带 `force` 返回 409 并列出文件（2026-10-26，`workspace::delete_branch` / `uncommitted`）
- [x] 事件不删，GUI 对已删 run 的事件不显示链接（2026-10-26，事件保留在日志；Agents 页的「run 招募」只在 run 还在时显示链接）
- [x] hub：`DELETE /v1/runs/{key}?branches=&force=`，`DeleteRunResponse` 与 bindings；cli：`crossagents run rm`（2026-10-26，`ApiErrorBody.files` 供 GUI 列文件）
- [x] GUI：Run 页「删除 / 取消并删除」、侧栏右键删除、确认框列清理项与分支选项、收到 `RunDeleted` 移出列表、正在看的被删就回首页；MockBridge 补数据（2026-10-26，`deleteRun.tsx`；确认框三种见 `docs/screens/r1/`）
- [x] 测试：store 级联；`tests/delete_run.rs` 覆盖未结束 409、已落地全清、未落地保留分支、脏工作区要 force、原对话不受影响（2026-10-26，`store-sqlite` 一条 + `mock-agent/tests/delete_run.rs` 五条）
- [x] 搬进来的主导不删：从没开过会话的主导是被挪进集成工作区而不是复制的，可能是人从名单里选的；删 run 时停掉它、cwd 改回原目录，清单里列在 `returned`，确认框写明「会保留」（2026-10-27，`lead_in` 记下 `lead_home`，`delete.rs::send_home`，`AgentHost::stop_agent`；测试 `a_lead_moved_into_the_run_goes_back_home`）
- [x] 工作区删不掉、靠 `remove_dir_all` 兜底删掉目录后补 `git worktree prune`，不在 `.git/worktrees/` 留残留记录（2026-10-27，`WorkspaceProvider::prune`；没有测试，Windows 占用文件的场景不好造）

## 十三、借鉴 Multica、magpie、Lancer

> 2026-10-27 整理，依据 [Multica 调研](research/multica.md) 与 [magpie 与 Lancer 调研](research/magpie-lancer.md)（都是 2026-10-26）。
> 顺序按两份调研 §4：先交接卡和选账号，再做 Multica 的三条唤醒规则；心跳与排队超时随 §八 B1 一起做。「不学的」见各自 §3，这里不再列。

第一批（P0）：

- [x] 交接卡（Lancer）：`task_assign`、改派（`task.rerouted`）、克隆接手时生成「发起方 → 接手方、文件、摘要、决策、待解问题、交接时上下文大小」，`api-types` 先定结构，事件带字段，对话里显示成卡片（magpie-lancer §2.1）
  - 2026-10-27：`Handoff`（assign / reassign / reroute / escalate）存在 `Task.handoffs`，事件 `task.handed_off`，先存、发布，再给新 worker 发 `brief::handover` 文字版；生成点：第一次派活、`task_assign`（可带 `reason`，派给 run 的 escalate 队友算升级）、额度改派。文件 = worktree 对 base 的改动加未提交文件，读不到留空只 warn；决定 = 规格 interfaces 加驳回意见（`TaskReport.rework`）；上下文大小取上一个 worker 最后一次 `agent.usage`，没报就空
  - 展示：`run show` 打印交接链，trace 每张卡一个节点；GUI 任务板每个任务下一条交接链、任务详情「交接」、主导对话里的编排行，点开同一张卡片；mock 场景 `handoff`，截图在 [screens/handoff](screens/handoff/)
  - 2026-10-27 审查后补：交接时把上一任的 `report.summary`、`report.failure` 随卡片带走并从任务上清掉，`progress` 改为换手原因，下一张卡不把 hub 写的原因当成 worker 的话（测试 `each_card_carries_what_its_own_worker_left`、`a_worker_that_said_nothing_hands_over_nothing_said`）；主导对话的交接行按它主导的全部 run 加载（`useRunsLedBy`），不再受侧栏 12 个 run 的限制；「已升级」看最后一张交接卡是不是 escalate（`isEscalated`），mock 不再手动置 `escalated`；上下文未知只写「未知」
  - 还没有的：`escalated` 现在没有代码会置位（H1-3 去掉了自动升级），所以只有「`task_assign` 给 escalate 队友」这一条升级路径真的会出卡
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

- [ ] hub：提交图接口，`git log --all`，带父提交和分支 / tag 标记，可分页
- [ ] hub：本地与远程分支列表，含 upstream 和领先 / 落后数量
- [ ] hub：工作区状态，`git status --porcelain=v2`
- [ ] hub：单个提交的详情和 diff
- [ ] hub：只能查 hub 已知的项目目录（agent 的工作目录、run 的仓库），其余路径拒绝；Rust 测试覆盖每个接口和越界拒绝
- [ ] GUI：新增 `#/git` 页：左边本地 / 远程分支，中间带泳道的提交图（分支和 tag 标在提交上，HEAD 高亮），右边提交详情和 diff；顶部显示未提交改动的数量
- [ ] GUI：MockBridge 补一套 git 数据，截图检查
- [ ] 验收：`cargo test --workspace` 与 GUI 构建通过
- [ ] 顺带验证：由 CrossAgents 自己开 Work 来做（主导拆任务交给便宜模型），实测 R1-A 与交接卡

## 十五、会话属于工作区，run 只记录谁参与过

> 2026-10-27，设计见 [DESIGN.md](DESIGN.md) §4.7，方案见 [tasks/S1.md](tasks/S1.md)。起因：run B 承接 run A 时想让 run A 的 planner 接着当主导，并由它决定用新会话还是 run A 用过的会话；现在主导被复制、worker 每个任务克隆一个用完就删、删 run 连带删会话。
> 已定（S1 §5）：派活默认新开执行会话（`fresh: true`），复用要写 `fresh: false`；人在聊的会话可以被派活，但人的请求优先、可中断；归档不自动清理，可恢复、可批量删除。

- [x] 会话的家：`Agent.home`（所属工作区）与迁移；跨 run 的原子占用表 `agent_claims`；驱动能力 `move_cwd`；`AgentHost::place` / `send_home`，用本人时恢复失败报错不新开（S1 §3.1）（2026-10-27）：迁移 0009 先 home = cwd，启动时 `migrate_homes` 把 worktree 里的会话归到 run 的 repo、吸收 `lead_home`；`claim_agent` 一条 INSERT；`place(carry)` 恢复失败报错；忙时 `send_home` 记待办（fbf76ce）
- [x] 主导是被选中的会话本人，不复制；run 结束送回工作区；`home_of`、`lead_home` 改由 `home` 取代（S1 §3.2）（2026-10-27）：不能换目录的驱动才复制，`run.lead_note` 写明；复制的主导在 run 结束时归档，恢复时重建检出（S1 §7 第 2 条）
- [x] 派活 `fresh` 默认 true 新开执行会话，`fresh: false` 用本人；不同工作区、不能换 cwd、已归档、人的请求在排队、被占用时返回原因；会话页显示正在执行的任务并可中断（S1 §3.3）（2026-10-27）：`restore: true` 恢复归档的会话；动态检查不过时任务留在 pending 下次再试；`/v1/agents/{key}/execution` 与 `…/interrupt`（77b603a）
- [x] 收尾不删会话、只释放本 run 的占用：本人回工作区，执行会话归档；删除旧 run 不影响正被新 run 占着的会话（S1 §3.4，吸收 §十一 A3）（2026-10-27）：`DeleteRunResponse.agents` 改为 `{ returned, archived }`；`AgentHost::remove_agent` 删掉（66f66f6）
- [x] 交接卡补执行结果 `outcome`，run 页按任务列出执行并链接到会话（S1 §3.5）（2026-10-27）：卡片加 `fresh`（新开 / 本人）和 `outcome`；run 页「参与者」可打开、恢复已归档的执行会话；侧栏按 `home` 分组；会话页「正在执行」条带中断；截图见 [screens/s1](screens/s1/)
- [x] 开场说明与 `agent_list` 列出工作区里可用的会话及最近一次执行（S1 §3.6）（2026-10-27）：最多 12 个、按最近使用排序；`agent_list` 另列本工作区已归档的会话（77b603a）
- [ ] 真实环境补测：Claude Code 会话在 worktree 干完回仓库根目录 `--resume`，对话完整、CLI 的会话列表里找得到（S1 §4）（2026-10-27）在 CLI 层做了：`--resume` 对话完整，删掉 worktree 后仍能恢复；但会话文件留在 worktree 的项目目录，仓库根目录的列表大概率看不到它（S1 §4 末尾）；定了不复制、不链接（S1 §7 第 14 条）；hub 层 `place` 没接真实账号测
- [x] 会话页提供「复制原生恢复命令」：带正确目录（会话文件所在的项目目录）和账号环境（代理、配置目录），人能在终端里接着用同一个 session id（2026-10-27）：hub 新增只读 `GET /v1/agents/{key}/resume-command`（`crates/hub/src/resume.rs`），会话页顶栏「终端恢复」展开命令，Windows 给 PowerShell 写法。目录取会话现在的 cwd（hub 也在这里按 id 恢复，S1 §4 实测可行）；会话文件留在别的项目目录时写明位置，提示在这里打开恢复列表可能看不到它。环境只带账号配置目录（`CLAUDE_CONFIG_DIR` / `CODEX_HOME` / `CODEBUDDY_CONFIG_DIR`）和代理，代理里的用户名密码去掉，不带任何令牌。外部会话、驱动不能恢复（ACP）、with-native（没有终端恢复命令）、没开过会话、`context_lost`、目录不在了、找不到 CLI 时复制按钮置灰并写明原因；hub 正开着这个会话时照给命令，但提示先停掉。Claude / CodeBuddy 用 `--resume <id>`，Codex 用 `codex resume <id>`，真实 CLI 的交互式恢复没有在本机实测。测试：`resume.rs` 单元测试与 `crates/mock-agent/tests/resume_command.rs`；截图见 [screens/context-lost](screens/context-lost/)
- [x] 上下文没延续、代码没恢复的警告在会话页顶部提示条显示，手动关掉才消失（`apps/gui/src/notices.ts`）：原来这些 `agent.warning` 只折进最后一轮，恢复时会埋在旧消息里，没开过轮次的会话则直接丢掉。mock 场景 `context-lost*`、`code-lost*` 用 hub 的原文（MockBridge `HUB_SAYS`），截图见 [screens/context-lost](screens/context-lost/)
- [ ] 兼容性增强：仓库根目录的 Claude 原生会话列表能看到在 worktree 里跑过的会话。先实测交互式 `claude --resume` 列表的行为，再决定做法（不在 `send_home` 里复制或链接会话文件，见 S1 §7 第 14 条）

## 十六、并行跑 run / task 时暴露的四个问题

> 2026-10-27，run `01a17e22`（补齐持续推进的 5 个缺口）里三个 worker 并行跑的时候实测出来的，都已核实到具体原因，均未在那个 run 里修。

- [x] **共享编译目录在并行 worktree 下串味**（2026-10-28 修，见 §九 ① 第二条）：`18d4f6a` 让同一仓库的所有 run 共用 `<worktrees>/targets/<仓库>-<哈希>` 作为 `CARGO_TARGET_DIR`，省下重复编译依赖的时间。但 workspace 成员的包名和版本号在各个 worktree 里完全相同，产物会互相覆盖：A 的 worktree 会拿到 B 编译出的 `crossagents-core` / `crossagents-api-types`。实测两种假错误——A 报 `unresolved import crossagents_api_types::UpdateRunPolicyRequest`（代码里明明有），B 报 `missing field stuck_minutes`；换成独立 `CARGO_TARGET_DIR` 两边都干净通过。hub 跑 task 验收时撞上这个会把好改动判成失败。现在每个 worktree 一份 `<root>/<run>/targets/<tree>`，用完随 checkout 一起回收（细则见 §九 ①）
- [x] **`schedule()` 不被 `task_create` / `task_assign` / `task_update` 触发**（2026-10-28 修）：`crates/orchestrator/src/engine.rs` 的 `schedule_reporting` 只在计划批准、任务合并/失败这些时刻跑。所以在一个跑着的 run 里新建或改派一条任务，它会一直停在 pending、没有 assignee、也没有任何错误记录，直到别的任务事件顺带触发一次调度。实测等了十几分钟都不动，直到另一条任务合并。核实时 `task_create`（含 worker delegate 拆子任务）和 `task_assign` 的 pending 分支已经在写完后调用 `schedule_reporting`，只剩 `task_update` 没调。现在 `task_update` 也在锁内重新读 run、写完后 `schedule`，返回的任务是调度后的状态，返回结构不变；run 不在 Running 时照旧不派活。测试在 `crates/orchestrator/src/tests_scheduling.rs`：新建无依赖任务、改派 pending 任务、把依赖改成已合并的任务，三种情况都会当场派出，run 暂停时不会派出。当时 run `01a17e22` 的数据已经不在库里，没法确认那次卡住是不是只因为 `task_update`，也可能是旧代码，或者是 `max_parallel` / scope 冲突造成的正常排队
- [ ] **`crates/mock-agent/tests/work_start.rs::a_proposer_that_can_move_leads_the_work_itself` 在重负载下偶发失败**：提案者还在应答时 work_start 被拒（`409 busy: lyra is answering a person's message`）。机器空闲时连跑三次 8/8 过，整机重编译时复现过一次。是测试自身的竞态，不是功能问题。
- [ ] **持续推进「唤醒投递失败」那条 Notice 没有集成测试**：实现和代码审查都做了（首次失败发一条、投递成功清标记、再失败再发一条），但真实 hub 下让 `host.notify` 稳定失败做不到——store 只有 INSERT 没有改插件的接口，删 agent 又会先被 `continuation_step` 的 `get_agent` 挡住。要覆盖得加一个可注入失败的 Host 测试替身。
