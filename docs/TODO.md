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
- [ ] 发 0.1.1 时验证真实的在线更新：装着 0.1.0 的 GUI 能在后台下载并在退出时装上新版
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
- [~] 大小与类型限制：单文件 10 MB、每条 8 个、合计 20 MB，图片 / PDF / JSON / 文本；hub 返回 400，GUI 发送前同规则检查。CLI 还不能发附件
- [x] GUI：拖拽、粘贴截图、选择文件；输入框显示缩略图，发送前可删除
- [~] `claude-native`：映射到 stream-json 的图片内容块（PDF 为 `document`，小文本内联），实测图片与文件的支持范围还没做
- [~] `codex-native`：映射到 app-server 的图片输入（`localImage`），实测后确认字段
- [ ] `acp-generic`：读取 agent 声明的 prompt 能力，未声明图片则不允许发图
- [ ] 按能力降级：驱动不支持的附件类型，GUI 禁用入口并说明原因，不静默丢弃
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
