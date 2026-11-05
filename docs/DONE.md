# 已完成清单

> 从 [TODO.md](TODO.md) 挪出的已完成（`[x]`）与已决定不做（`[-]`）的条目，2026-10-30 起。
> 章节编号与 TODO 一一对应，别处引用的「TODO §九」之类在这里同号可查；未完成的条目仍留在 TODO。
> 以后某条做完，就从 TODO 剪到这里对应的一节，保留原文与日期。

## 一、进程监督（优先）

按 Cutex 的做法改为“agent 进程比 hub 活得久，新 hub 接管”，设计见 [PROCESS-SUPERVISION.md](PROCESS-SUPERVISION.md)。

- [x] 子进程退出时记录原因（exited / crashed / killed / lost），`Exited` 带 `cause` 与退出码，另发 `agent.exited` 事件（超时类原因未单独区分）
- [-] 子进程崩溃后不自动拉起（2026-10-23 已决定）：中断的那一轮不自动重放（可能重复执行命令、重复改文件），只标 `interrupted` 交给人或 Planner
- [x] 中断的那一轮给出明确状态：hub 重启时无法接管的轮次记为 `stop_reason: "interrupted"` 并附说明；能接管的轮次照常跑完
- [x] 运行时宿主登记在 `<home>/runtimes/`，hub 启动时握手确认后接管，不按 pid 杀进程（防 pid 复用误杀）；无人认领的宿主经连接停掉
- [x] Windows 上每个 agent 放进独立 Job Object（KILL_ON_JOB_CLOSE），宿主一退整棵树清掉；`acp-generic` 的 job 由 hub 持有
- [x] hub 退出默认把宿主里的会话留给下一个 hub（不 suspend，也就不会留下半截会话文件）；`hub stop --agents` 才停掉全部 agent
- [x] 修复：`codebuddy-native` 包装层没转发 `adopt`，导致 CodeBuddy 无法接管（实测发现，mock 测试只覆盖 codex 所以没测出）
- [x] 给 claude-native / codebuddy-native 补接管的自动化测试：mock 新增 `stream-json` 模式（`crates/mock-agent/src/stream_json.rs`），`tests/adopt.rs` 对 codex、claude、codebuddy 各跑一遍"轮次跑到一半 hub 停 → 新 hub 接管 → 轮次跑完、进程不变"；已验证能抓到 codebuddy 漏转发 `adopt` 那类 bug
- [x] 宿主会继承拉起者的可继承句柄（Windows）：hub 拉起宿主前清掉自身 std 句柄的继承标志（`drivers::process::stop_inheriting_std_handles`，`hub start` 复用同一函数），`hub run | …` 在 hub 退出时立即结束；回归测试 `tests/pipe.rs`。hub 直接拉起的子进程（acp-generic）之间的管道句柄泄漏未处理，它们随 hub 一起结束，影响不大
- [-] 接管 codex 时，如果正在跑的那一轮的 id 一直没出现，就没法向 Codex 发 interrupt，只能本地放弃等待（已知边界：只影响中断是否及时，不影响接管正确性，该轮仍会按 `interrupted` 收尾）

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
- [x] 验收：只读凭据打写接口得 403，会话 token 打管理接口得 403，错误信息能看出缺哪一级（`crates/hub/tests/api.rs` `credentials_have_levels`）

令牌与免鉴权面：

- [x] Electron 主进程加「方法 + 路径」白名单：只放行 GUI 实际调用的接口，页面无法触达 `/v1/shutdown` 与 `/v1/me*`；新增调用要同步改 `ALLOWED`（[main.cts](../apps/gui/electron/main.cts)），拒绝信息会点明这一点
- [x] 收敛免鉴权面：`/v1/health` 只回 `status` 与 `api`，`version`、`uptime_secs` 收进需鉴权的 `/v1/info`；CLI `hub status` 改读 `/v1/info`（A2 放开 `listen` 后这些就是白送的情报）
- [x] 令牌轮换确认：GUI 每次调用重读 `token`；CLI 每次构造 `Client` 时重读（`for_hub` 内 `read_token`），都是短命进程，hub 重启换 token 不需要重启客户端
- [-] token 文件 Windows ACL：`write_private` 只在 Unix 分支设 `0o600`，Windows 是裸 `fs::write`。核查后不加：`~/.crossagents` 不在云同步目录内，NTFS 默认 ACL 已限制到本用户、SYSTEM 与 Administrators，同机其他用户读不到。home 若被移到同步目录再处理

版本不一致：

- [x] `api` 不等（GUI 期望 v2、hub 还是 v1）→ 拒绝连接，提示 hub 与 GUI 版本不匹配、请一起更新，不静默降级。事件流连上后先查 `/v1/info` 的 `api`（[hub.ts](../apps/gui/src/hub.ts) `API_VERSION`，升版时要和 api-types 一起改）；不等则丢弃事件、拒绝其余请求、整页换成说明，hub 重启后自动重查。mock 场景「版本不匹配」可看效果
- [x] 给 `API_VERSION` 定升版规则：写进 [API-AUTH.md](API-AUTH.md) §5，分「必须升版」（删字段、改语义或单位、改路径方法、新增必填、收紧校验）与「不必升版」（新增可选字段、新接口、新错误码取值）两类

## 三、常驻守护（等第一步稳定后再做）

> §一 完成后，hub 停了 agent 照常跑，下一个 hub 接管。这一节剩下的是让 hub 本身常驻、开机自启。

- [x] hub 的停止命令：`hub stop` 只停 hub，agent 留给下一个 hub；`hub stop --agents` 停掉全部 agent（取代原来的"等待会话 suspend 后再退出"）
- [x] 安装版 GUI 自带 hub：安装包带同版本 `crossagents.exe`，启动时复制到 `<home>/bin/<版本>/` 并从那份启动（运行时宿主、`mcp` 都从 `current_exe()` 拉起，按版本分目录才不锁住安装目录）；没有 hub 就起，自己的 hub 版本旧就 `hub stop` 再起新版，agent 被接管；手动起的 dev hub、远程 hub 不碰。判断依据：`hub.json` 的 `exe` 在 `<home>/bin/` 下。旧版本目录在无进程使用后清理。设计见 [MULTI-NODE.md](MULTI-NODE.md) §13，手测见 [tasks/U1.md](tasks/U1.md)
- [x] GUI 自动更新：electron-builder 改出 NSIS 安装包（按用户安装），electron-updater 从 GitHub Releases 后台下载、退出时安装；打包前检查 GUI 与 Cargo 版本一致
- [x] 建公开的发布仓库 `L1nkkkk/CrossAgents-releases`（主仓库私有，见 MULTI-NODE §13.5），发布 v0.1.0（2026-10-25：`electron:build` 打包后用 `gh release create` 上传，资产名与 `latest.yml` 一致）
- [x] 发 0.1.1 时验证真实的在线更新：装着 0.1.0 的 GUI 能在后台下载并在退出时装上新版（2026-10-28：启动 3 秒发现 0.1.1，按 blockmap 差量下载 8.4 MB / 116 MB；托盘退出后静默安装约 6 秒；重开后起 `bin/0.1.1` 的 hub 并清掉 `bin/0.1.0`）
- [x] 旧 hub 遇到新库时说清楚：本机跑过 dev hub 后 `state.db` 已有 0.1.0 不认识的迁移（`migration 9 was previously applied but is missing`），0.1.0 的 hub 直接退出，GUI 只显示「cannot start the local hub」，hub.log 里没有原因。至少把这句错误带进 GUI 和 hub.log，提示「数据库来自更新的版本，请升级」（2026-10-29：`store-sqlite` 把 `MigrateError::VersionMissing` 译成「the database comes from a newer CrossAgents (it has migration N, this build knows up to M); upgrade CrossAgents to open it」；`hub run` 失败时把这句写进 `hub.log` 的 ERROR 行；`hub start` 在子进程退出后读回该文件自本次尝试之后的最后一条 ERROR，拼进报错，GUI 的离线页因此显示原因而不是只有「cannot start the local hub」）
- [x] 无窗口时的额度与预算上限（2026-10-24 决定，见 [tasks/DOGFOOD.md](tasks/DOGFOOD.md) §5）：hub 不在时只记账、hub 回来时结算——宿主缓存并补发用量事件，新 hub 启动时先读账并立即检查一次预算，再恢复调度；不在宿主里强制。Chat 默认上限仍未做。原记录：暂缓（2026-10-23 决定）。现状：Work run 的 `policy.budget` 只在 hub 在时检查（`crates/hub/src/budget.rs`），hub 不在时宿主里的 agent 不受限，Chat 会话没有上限。开工前要定：hub 不在时超限是宿主停在轮次之间，还是只记账等 hub 回来处理；Chat 要不要默认上限

## 四、可观测性

- [x] `cli` 增加状态命令：`hub status` 除后台 agent 进程（pid、启动时间、隔离方式）外，汇总各会话——总数、归档数、各状态计数、正在干活的名单；处于 error 的会话逐个读 transcript 的 `error`，截成一行显示（最多 20 个）。`--json` 同步给出 `sessions` 与 `errors`
- [x] 事件日志落盘：每条事件一行 JSON 写进 `logs/events-<date>.jsonl`，与 `GET /v1/events` 同格式；独立线程无缓冲写入（读方立即可见，`Get-Content -Wait` 可跟），队列满时丢弃并计数、不拖慢 hub；按天滚动，启动时删 14 天前的文件（只认 `events-*.jsonl`，不动别人的文件）。可关：`[hub] event_log = false`。读：`crossagents events --log [--since N]`，hub 没在跑也能读
- [x] GUI 显示"hub 离线"时的诊断信息：侧栏未连接时显示 `offline: <原因>`，原因来自 Electron 主进程且带修复指引（如 `hub is not running (…); start it with crossagents hub start`）
- [x] 每个会话都显示进度，不只 run（2026-10-31）：起因是 run f87e53d3 里主导看 `usage_get` 的 `peak_context`（codebuddy 轮末才上报，轮中一直是 0）和 `workspace_diff`（对未提交改动返回空 stat）误判一个 worker 卡住并中断了它，其实它已写了 28 个文件。
  - ① hub 侧活动信号：`crates/orchestrator/src/running.rs` 新增独立的 `AgentActivity`，挂在 `RunningCommands` 内、由它现有的 `push` 转发事件（`engine.rs` 无需改动），记录本轮开始时间、最近一次输出 / 工具调用的时间与种类（`run_command` / `edit_files` / `read_files` / `search` / `replying` / `thinking` / `tool_call`——2026-10-31 G1-3 那轮由中文显示串改为英文枚举，见下）、本轮按路径去重的改动文件数；只在内存里，重启后一无所知，轮末或进程退出即清空。`agent_get` / `agent_list` 返回 `activity` 段（`turn_elapsed_seconds`、`last_activity_kind`、`last_activity_ago_seconds`、`files_touched`），不在轮次中为 null——主导判断卡没卡住看这个，不再依赖插件的 usage 上报。
  - ② GUI：侧栏会话行在轮次中显示本轮已跑多久 + 最近活动，安静超过 `QUIET_AFTER` 显示「X 分钟没动静」；会话页头部加一条 `TurnLive`，带本轮时长、最近活动、所在任务与其 `progress`、run 的任务计数（复用 `lib/taskState.ts` 的 `progress`）、acceptance 检查通过数、安静提示。活动三项出自 `lib/workGroups.ts` 新增的 `liveProgress` / `quietLabel`，与轮次下方的实时行同源。
  - ③ 不编百分比：没有步骤总数就只显示活动，不画进度条；只显示 hub 真有的计数（run 的任务数、acceptance 检查）。worker 自报 `step/total` 仍未做，等需要时再说。
  - 顺带修了 `workspace_diff` 对有未提交改动的 worktree 返回空 stat：`workspace.rs` 的 `diff_stat` 原先用 `git diff --stat {base}...HEAD`，只算已提交的，改成与同文件 `diff` 一致的做法（`--intent-to-add` 后 `git diff --stat <merge_base>`），`tasks.rs` 里的 `committed` 变量随之改名为 `stat`。
- [x] `AgentActivity` 缺过期保护（2026-10-31）：同文件 `RunningCommands` 早有 `STALE_AFTER_HOURS = 2`，文件头注释写明了理由「a command whose end was never reported is taken as gone」；上一轮新加的 `AgentActivity` 没有对应保护，条目只在 `AgentTurnCompleted` / `AgentExited` 时清除。进程被杀而这两个事件没发出来时，`AgentActivity::of` 会一直报告「本轮已跑 N 天、正在运行命令」。**这恰好朝着这个信号要防的方向骗主导**：它存在的理由就是让主导别把在干活的 worker 误判成卡住，反过来把死掉的会话报成在干活同样有害。现在 `of()` 按 `last`**而不是 `started`** 判过期（一轮跑两小时是正常的，两小时没有任何活动才是异常），`ACTIVITY_STALE_AFTER_HOURS = 2`，超过就当这一轮不存在返回 `None`；`push` 顺手 `retain` 掉过期条目，`by_agent` 不再无限涨。单测 `running::tests::activity_expires_from_last_event_and_is_pruned_on_push`，它全程不发 Completed / Exited，正是「进程被杀、结束事件丢失」那个形态，并顺带钉住了关键设计点：`started` 在 3 小时前而 `last` 在 90 分钟前时 `of()` 仍返回 Some，长轮次不会被误杀。审阅时记下一个边角进 [TODO.md](TODO.md) §二十二：`push` 开头的 `retain` 会让「超过两小时没有任何中间事件的工具调用」在恢复后把 `started` 和 `files_touched` 归零，方向安全（宁可少报不多报），本轮没改。
- [x] 中文显示串不该进给模型看的 JSON（2026-10-31）：`last_activity_kind` 经 `crates/orchestrator/src/messaging.rs` 由 `agent_list` / `agent_get` 返回给**模型**，同一个 JSON 里其余字段全是英文（`busy` / `idle` / status），工具协议也是英文，它却放的是 `"运行命令"` / `"修改文件"` 这类 GUI 显示串。上一轮 brief 说的「照 GUI `liveActivity` 那套词、别另造」指的是**分类口径**（execute→跑命令、edit→改文件…），不是把中文显示串发给模型。改为英文枚举 `run_command` / `edit_files` / `read_files` / `search` / `thinking` / `replying` / `tool_call`，与原分类一一对应（含那处 `kind == "edit_files"` 的字符串比较——改漏了 `files_touched` 会默默变成 0 而编译不报错）。GUI 侧没改：`apps/gui/src/lib/workGroups.ts` 的 `liveActivity` 从 GUI 自己的 transcript items 算中文串，不读这个字段；grep 过 `last_activity_kind` 确认没有别的消费者。

## 五、Claude 运行时（对照 Cutex 的借鉴项）

- [x] 设置源开关（2026-10-24，DOGFOOD §6：编辑队友面板三档预设，`POST /v1/agents/{key}/settings`）：Profile 级别选择是否读取项目的 CLAUDE.md、`.claude/` 规则、hooks、权限。驱动层已能读——`setting_sources` 从 agent config 取，默认 `project,local`（`crates/drivers/src/claude_native/mod.rs`，透传给 `--setting-sources`）；缺的是 GUI 开关与几个预设组合

## 六、Codex 驱动

- [x] code-mode 模型（模型目录里 `tool_mode: code_mode_only`，如 gpt-6-luna）只看得到 `exec`，MCP 工具藏在 `tools.mcp__crossagents__<tool>` 里：只被告知「调用 task_complete」的 worker 找不到它，每轮都不上报、任务停住（2026-10-25 真实 run 里发现）。Codex agent 的开场说明写明工具名与 `exec` 里的调法；派给 worker 的任务末尾都提醒用 `task_complete` 收尾

## 七、多模态输入

现状：`SendMessageRequest` 只有 `text` 字段（`bindings/SendMessageRequest.ts`），driver 层也没有附件类型，发不了图片和文件。

- [x] 消息结构从纯文本改为内容块（text / image / file），`SendMessageRequest` 增加 `attachments`，重新生成 ts-rs 绑定（收件箱里是 `Prompt { text, attachments }`，`DriverSession::prompt_with`；见 [tasks/DOGFOOD.md](tasks/DOGFOOD.md) §3）
- [x] 附件落盘到 hub 数据目录，按会话归档并记录 sha256，不把 base64 直接存进 SQLite（`<home>/attachments/<agent id>/<sha256>`）
- [x] 大小与类型限制：单文件 10 MB、每条 8 个、合计 20 MB，图片 / PDF / JSON / 文本；hub 返回 400，GUI 和 CLI（`crossagents send --file <path>`，可给多次）发送前同规则检查
- [x] GUI：拖拽、粘贴截图、选择文件；输入框显示缩略图，发送前可删除
- [x] `acp-generic`：读取 agent 声明的 prompt 能力，未声明图片则不允许发图（`initialize` 的 `agentCapabilities.promptCapabilities` 记在会话上，图片按 ACP image block 发送；没声明就由 hub 返回 400，不再退化成路径引用）
- [x] 按能力降级：驱动不支持的附件类型，GUI 禁用入口并说明原因，不静默丢弃（`Capabilities.attachments`：claude-native / codebuddy-native 收图片、PDF、文本，codex-native 不收 PDF，acp-generic 启动前为 `null`、会话启动后按声明；transcript 把它给 GUI，`POST /v1/agents/{key}/messages` 遇到不收的类型返回 400 并说明原因；GUI 收窄文件选择器的 accept，粘贴/拖入不支持的类型时报出原因）

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
- 2026-10-27 trace run `01a17c78…dccf42e8` 发现的问题（已修的部分；未修的仍在 TODO）
  - [x] 检查没跑完就被合并（2026-10-28 修）：`task_complete` 不再直接置 `Passed`，改为 `Running|WaitingAnswer|Stalled → Verifying`，并给这次验收生成一个独立 UUID 写在 `report.verification` 上。hub 在后台 `report_done` 里提交、跑 `spec.acceptance`、把这次检查对应的 commit SHA 记进 `report.verified_sha`，然后才 `Verifying → Passed` 并把 worker 总结转给主导；回写前校验「任务仍是 Verifying 且标识一致」，不一致（返工、新一轮验收、run 被取消）就丢弃。现在 `Passed` 只表示「报告已就绪，等 issuer 决定」，检查失败、超时、执行异常都如实记在 `report.checks` 里，不新增状态。`task_accept` 的门禁：（1）任务在 `Verifying` 时一律拒绝，`force` 也不绕过；（2）普通 accept 要求检查全通过且 worktree 相对已验证 SHA 无新改动（未提交文件也算），只合并这个 SHA，不再 `commit_all`；（3）检查失败 / `unchecked`（`spec.acceptance` 为空）/ 中断 / 接收 Stalled / 验证后有新改动，都要 `force: true` 加非空 `reason`，原因写进 `report.forced` 与任务进度行留审计；（4）`task_drop`、`task_cancel`、删除 run 在 `Verifying` 时同样拒绝（都不持锁等验收，避免与 `report_done` 死锁）。重启后遗留的 `Verifying` 转 `Passed` 并标 `report.interrupted`（按失败处理），需要时用新工具 `task_verify` 生成新 UUID 重验。`let_go` 的归档 / 送回失败不再只记日志，而是一路返回到 `release_agents` 的调用方，`cleanup_worktree` 在无法确认 agent 已停时跳过删除、保留 checkout。测试见 `mock-agent/tests/verify_gate.rs`
  - [x] 构建目录按 worktree 隔离与回收（2026-10-28 修，同 §十六第一条）：`CARGO_TARGET_DIR` 从「同一仓库所有 run 共用一个 `<root>/targets/<repo>-<hash>`」改为 `<root>/<run>/targets/<tree>`，`<tree>` 是 worktree 的目录名（`integration` 或 task key），worker 与 hub 验收在同一 tree 里共用，`CARGO_HOME` 仍共享。`workspace::worktree_env` 不再依赖 `repo_key`；cwd 落在 `<run>/targets` 下、正好是 `<run>` 本身、或旧的 `<run>/target` 时返回 None（不设变量）。`targets` 是保留名：`plan::validate` 拒绝同名 task key，`keys_in_use` 跳过它。回收：`cleanup_worktree` 在 worktree 删掉后回收该 tree 的 target；`remove_integration` 只回收 `targets/integration`，不再递归删整个 `targets`；`<run>/targets` 与 `<run>` 空了才 `remove_dir`。删除前先改名 `.trash-*`，改名失败就留到下次重试（Unix 上改名不会失败，正确性靠「checkout 已不在、任务不在 Verifying」这条规则）。启动时与每次回收时扫描所有 `<run>/targets/*`，清掉 checkout 已不在且不在 `Verifying` 的孤儿目录和残留 `.trash-*`。旧布局 `<root>/targets/<repo-key>` 与 `<run>/target` 不再写入，沿用原来的 `keys_in_use` 保护逻辑、阈值改为 0（没人用就删）。`build_cache_limit`（`[hub] build_cache_gb`）改为「整个 worktrees 根目录下构建产物总量」的软预算：超出只回收上面允许删的目录，回收后仍超就记 warn 并发 `hub.build_cache` 事件，不动正在用的目录，不做硬上限、不做派工准入；默认值 30 GB → 120 GB（每个 worktree 一份 target，默认并行度 3 的 run 最多占 4 份）。不接 sccache。冷编译实测（Windows 本机，全新 target）：`cargo check --workspace` 32 秒、646 MB；`cargo test --workspace --no-run` 68 秒、10.0 GB（含 debug info 与全部测试二进制）——预算按后一个量级算，四份就是 ~40 GB，120 GB 约等于三个 run 同时在跑
  - [x] 门禁与回收的补修（2026-10-28，同一 trace 的后续）：① `report_done` 任何一步出错（`commit_all`、worktree 缺失、`base_of`、写库）都不再让任务停在 `Verifying`——错误写进 `report.failure`、清掉 `verified_sha`，在锁内确认「仍是 Verifying 且标识一致」后转 `Passed`，并把「什么都没验证」连同原因转给 issuer；`complete` 与 `task_verify` 两处 spawn 走同一条路。② `engine.rs::resume` 的 `Verifying` 不再受「continuation 开着且 worker 忙」那段 `continue` 影响：验收属于 hub 不属于 worker，一律标 `interrupted` 转 `Passed`。③ `cancel`（取消整个 run）在有任务 `Verifying` 时返回 Conflict，写明等结果再取消；检查放在 run 锁外，不持锁等验收。④ `lead_left` 不再调递归删整个 `targets` 的入口，改为 `prune_run_targets` + 新的 `run_targets_in_use`：只回收 checkout 已不在的 tree，`Verifying` 的保留；删除 run 仍一次清空整份（`remove_run_build_dirs`）。⑤ `task_accept` 强制合并时 `merged_sha` 返回真正合入的提交（worktree 当时的 HEAD），不再回填 `verified_sha`。⑥ 释放失败的测试：AgentHost 替身让 `send_home` / `archive_agent` 报错，断言 checkout 与 `targets/<tree>` 都留着，正常释放时照样删。测试都在 `crates/orchestrator/src/tests_verification.rs`（替身 host / workspace / verifier + 内存 SQLite）
  - [x] 派工通知写的是模板名（2026-10-29 修）：`engine.rs::approve_locked` 的 "Who does each task" 用的是 worker 模板（如 Dara），`task.handed_off` 的 `to_name` 也一样，实际干活的是克隆出来的 `w-<run>-<task>`，主导照着通知给 Dara 发消息会碰到 `you are in no run`。修法：`Handoff.to_name` 改为实际执行会话名，新增可选字段 `template` 记录来源模板（`crates/core/src/handoff.rs`，serde default、旧记录可读；`to_label()` 输出「w-xxx（来自模板 X）」，brief handover 与 hub trace 都用它；fresh:false 由会话本人接手时不写模板）；批准通知改为「#key 由模板 X (model) 派生的新执行会话执行」，fresh:false 写「由 X 本人执行」，并说明会话名在派出时另行通知；dispatch、`task_assign`、quota 改派在 `hand_off` 之后经 `Engine::notify_dispatched` 告诉 issuer「#key 已派给 <会话>（来自模板 X），给它发消息用这个名字」。测试：`crates/mock-agent/tests/handoff.rs`（`the_lead_is_told_the_execution_sessions_name`，以及按新语义改过的 `the_first_worker_gets_an_assign_card_from_the_lead`、`task_assign_hands_over_the_files_the_worker_changed`），`crates/core/src/handoff.rs` 单测（无 template 时不序列化、link 文案），`pipeline.rs` 与 `cli.rs` 的文案断言同步更新
  - [x] 克隆把原会话的 `config.spawned_by_run` 也带过来了（2026-10-29 修）：`fresh_agent`（engine.rs）与 `routed_worker`（quota.rs）克隆后经 `Engine::unmark_recruit` 去掉 `spawned_by_agent` / `spawned_by_run`；`team.rs` 招募出来的队友本身保留这两个键，`messaging.rs:56` 的语义不变。测试：`crates/mock-agent/tests/handoff.rs::a_cloned_worker_does_not_carry_the_templates_recruit_marks`
  - [x] GUI：编辑类工具失败（with-cli 报 "模型返回的待替换代码块未匹配到整行"）时，工具行仍按模型打算改的 diff 显示 `+2 -0`、`detail.error` 不显示（2026-10-29 修）：新增 `apps/gui/src/lib/toolcall.ts` 统一判定：`toolFailed(call)`（`status === "failed"` 或带 `detail.error`）、`toolError(call)`（先取 `detail.error`，没有就退回结果原文，MCP 的 `content[0].text` 也解开，规则同 ActionCard 原来的 raw）、`toolChanges` / `toolShowsStat`（失败时为空 / false，因为 `detail.changes` 是模型打算改的 diff 而非真改动）。`apps/gui/src/views/Session.tsx` 的 `ToolStep` 失败时不出 `+N −M`、不渲染待改 diff，显示红点和 `失败`（`.tool-step.failed`，标题用 `--danger-text`；非 0 退出码仍写 `退出 N`），展开详情第一段就是失败原因原文，与结果原文相同时只留一份；`ActionCard` 与 `workGroups.failed`（折叠汇总的「N 个失败」）都改用它，成功调用照旧。轮次的 +N −M 汇总来自 hub 的 `turn.diff`，不含工具打算的改动，未改。测试：`apps/gui/src/lib/toolcall.test.ts`（node --test，7 条）；mock 场景 `tool-fail`（"工具调用失败"）可截图核对
- [x] 2026-10-28 三处让 run 收尾失败、`cargo test --workspace` 在 main 上红的问题（与业务改动无关，逐条修好）
  - with 驱动的 MCP 超时按秒写、with-cli 按毫秒读（2026-10-28 修）：现象是主导用 with 插件调 hub 的 `run_finish` 时，每次都在 1.8 秒后报「请求超时, 超时时间:1800 ms」。`run_finish` 要同步跑 `cargo test --workspace`（几分钟），主导永远拿不到检查结果，run 停在 `planning`，只能靠人去看 `run.checks`。实例 run `01a18178…43af3611` 连调 3 次次次超时。原因是 `crates/drivers/src/with_native/mod.rs` 的 `MCP_TIMEOUT_SECS = 1800` 被原样写进 `mcpServers.<name>.timeout`，而 with-cli 把这个字段当毫秒（它自己的报错文案就是 `请求超时, 超时时间:%d ms`，可从 `with-cli.exe` 里读到）。改为 `MCP_TIMEOUT_MS = 30 * 60 * 1000`，配置写 `1800000`，注释写明单位与依据，单元测试断言「30 分钟换算成的毫秒数」。其余驱动一起核过：codex 不写超时（用默认）、claude / codebuddy 的 `--mcp-config` 不写超时、acp 协议没有这个字段，单位都与各自客户端一致，未改。`run_finish` 仍是同步的，检查时间没缩短
  - `delete_run.rs` 两条测试卡在归档时机（2026-10-28 修，改测试）：`a_landed_run_goes_with_its_worktrees_branches_and_lead`（`:170`）与 `a_dirty_checkout_asks_for_force`（`:268`）在 `run_finish` / `POST /v1/runs/{id}/finish` 返回后立刻断言主导已归档、检出已删。6df4957 之后这是错的时序：复制出来的主导要等「本轮结束且没有排队消息」才归档（`AgentHost::archive_agent_after_turn` + `runtime.archiving`），检出由 `orchestrator.leaving → lead_left` 在那之后清。先核过行为本身：两条路径（主导自己 `run_finish`、人点结束）下，主导空闲就当场归档、正在忙就挂进 `archiving` 等回合结束，都会归档并删检出，没有永远不归档的路径，所以只改测试——新增 `until_lead_left` 轮询「已归档 + 检出没了」（100ms × 300 次，带 panic 文案），断言放到等待之后，注释从「run 结束时归档」改为「结束这一轮后归档」。同一类时序假设在 `lead.rs`（归档后恢复检出、分支没了重建、删 agent 清检出）、`work_start.rs`（结束后再开 Work）、`session_reuse.rs`（`pinned_run` 建的两条 `a_copy_that_resumes_only_where_it_ran_*`）里也一起改了：等归档与检出删除、等恢复后检出重建、并先等关单那一轮真正结束（否则恢复后的等待会吃到上一轮的 `turn_completed`，把旧输出当成回答）。`lead.rs` 那条 landed run 的恢复不再依赖「`lead_left` 删没删分支」的竞态，测试自己先删分支，让检出固定从 base 重建。另外 `tests/common/mod.rs` 的 `Watcher::until` 从 15 秒放宽到 30 秒：整套 `cargo test --workspace` 并发时 15 秒不够（e7b18db 已记为既有 flake），等的是一个本来就会到的事实，不该提前判失败
  - 持续推进的检查点停在 `working`，重启前后不一致（2026-10-28 修）：`continuation::reserved_due_time_survives_restart_and_default_runs_stay_unchanged`（`:502`）唤醒后读到 `phase="working"`、`next_at=None`，重启同一个 run 变成 `phase="continue"`、`next_at=last_attempt_at+1h`，`attempts` 与 `last_attempt_at` 相同。根因：检查点只在 hub 时钟每秒一次的 `continue_runs` 里写，主导回合结束时没有任何人重算它，于是「主导正在这一轮里」那个观测（`working`、没有预留）被持久化下来；重启后的第一次推导直接是 `continue` 并重新预留，两边自然不同。引入提交 `3d8f6e5`（`feat: add opt-in bounded run continuation`，`git log -S "The lead has a turn or queued messages"` 只命中它，此后 `continue_run` 的调用方一直只有时钟）。修法：`continue_run` 加 `deliver` 开关，新增 `Orchestrator::settle_continuation(lead)`，在 `AgentStatusChanged → Idle`（回合已发布结束、且队列为空时才置 Idle）时按同样的事实重写该 run 的检查点；`deliver=false` 保证不计数、不投递唤醒，已经到期的 `next_at` 留给时钟下一次投递。测试在重启前先 `phase(..., "continue")` 等检查点落定，并断言 `attempts == 1`、`next_at - last_attempt_at == 60 分钟`
- [x] `settle_continuation` 会吞掉卡住提醒（2026-10-28 记，2026-10-29 修）：7bc3aff 在主导变 Idle 时调 `continue_run(id, false)`，`deliver=false` 只挡住计数和投递，卡住检测照跑，会写入卡住文案并把 `stuck_watch.progress_at` 重置为现在却什么都不发，下一次唤醒退回普通「继续推进」，「N 分钟没进展、在等谁」丢了。修法：`crates/orchestrator/src/continuation.rs` 的卡住判定放进 `if deliver` 分支，只在投递那一趟跑；settle 不判定安静时长，不写 `stuck_watch` 的 `progress_at` / `event_seq`，不新建也不清空（非等待阶段的清空也只在 `deliver` 时做）。连带问题：settle 会写 `state.phase`，下一次投递那一趟的 `changed_phase` 就成了 false，上一阶段的安静时长会被算进新阶段；所以 settle 在 phase 真的变了时把已存在的 watch 按换阶段的规则重置（`progress_at=now`、`event_seq=最新`），这是「新阶段从现在起算」而非卡住判定，不吞提醒，watch 不存在时留给投递那一趟创建。测试：`crates/mock-agent/tests/continuation.rs::settling_the_checkpoint_at_idle_does_not_swallow_a_stuck_report`（`quiet(&t, id, 5)` 拨到安静满阈值后调 `settle_continuation`，断言 `progress_at` 未重置、`attempts` 仍为 0、没有投递；随后 `continue_runs()` 的唯一唤醒含 `No progress for 5 minutes` 与等待对象），把 `if deliver` 改回旧行为时该测试失败
- [x] 收尾检查和主导自己的构建抢同一个编译目录（2026-10-28 修）：run `01a182ed…27bd5f0e` 的主导 22:33 自己开了 `cargo test --workspace`，没跑完就调 `run_finish`；hub 在同一个 `<run>/targets/integration` 里再跑一遍，先是 `Blocking waiting for file lock`，再是 Windows 不让覆盖正在执行的测试程序（`LNK1104 adopt-….exe`），被报成「Fix it in your checkout」，主导会去找不存在的 bug。修法三处：orchestrator 从工具调用事件跟踪每个 agent 正在跑的命令（`crates/orchestrator/src/running.rs`，回合结束或进程退出即清空，超过 2 小时没报结束的不算），检查开始前若检出里还有命令在跑就拒绝并说清是谁的哪条命令、从几点开始；检查失败里有 `LNK1104`、`os error 32`、文件锁等「文件被占用」特征时改报环境冲突而不是代码错误；开场说明的 checks 一句加上「hub 自己会跑，只跑和改动相关的测试，收尾前等自己开的构建结束」。测试：`lead.rs::the_checks_wait_for_the_leads_own_build`（mock 的 `[running]` 模拟没结束的命令）、`running.rs` 两条单测
- [x] with 主导每一轮都报「不支持该模型: claude-opus-5.5」（2026-10-29 修）：with-cli 当天自动更新到 0.33.0 后，hub 起的 with 会话全部在 1 秒内失败，而直接跑 `with-cli chat --model=claude-opus-5.5` 正常。原因是驱动用 `--config` 传的是 `~/.bg-agent/bg-agent-client.yaml` 的副本（10/15 生成后没再更新），新版 with-cli 拿到这份旧配置就不认它自己列出的模型；换成安装目录 `background_agent_cli/etc/bg-agent-client.yaml`（随 CLI 一起更新）就好。修法：`with_native::installed_config` 按 `<install>/bin/with-cli` 找 `<install>/etc/bg-agent-client.yaml`，有就复制它，没有才退回用户文件；用户自己的 MCP 服务器仍按用户文件里的 `mcp_config_path` 合进来。测试 `the_installed_configuration_wins_over_the_users`。要换上新 hub 才生效
- [x] `team.rs::the_lead_cannot_put_a_teammate_on_a_model_nobody_runs` 依赖本机账号（2026-10-29 记，同日修 `a9d32b2`）：本机装了 with 插件时 `agent_spawn` 把 `gpt-6-luna` 解析到 `with` 并招募成功，断言「no logged-in account runs model gpt-6-luna」失败。按 TODO 当时记的办法修的——换成任何真实插件都不会提供的模型名 `no-such-model-zz`，不再依赖宿主机装了什么
- [x] 升级会话的名字漏了 `e-` 前缀（2026-10-29 记，2026-10-30 修）：名字的前缀表示这个执行会话是什么——`w-` 普通 worker、`e-` 升级给强模型的 worker、`r-` 评审。判断只写在 `engine.rs` 派活那一处，`tasks.rs` 重新派活（`:438`）和 `quota.rs` 换账号（`:110`）两处把 `"w"` 写死了，于是任务升级后建出来的会话仍叫 `w-`，名字与它的实际身份不符。TODO 原文只记了一处，实际是两处。修法是抽出 `engine::worker_name(run, task)` 三处共用，而不是在三处各抄一遍那个三元表达式（那样还会再漂）；`r-` 那处不动，评审与升级无关。单测 `tests_scheduling.rs::an_escalated_task_names_its_session_with_e` 断言只有前缀不同。补修（2026-10-30，run f87e53d3 #escalation-prefix）：`task_assign` 把未标 `escalated` 的任务直接交给 `run.config.escalate` 那位队友时仍取 `w-`；`worker_name` 加 `template` 参数，前缀由新的 `engine::worker_prefix(run, task, template)` 决定（`task.escalated` 或模板就是 escalation 队友时给 `e`），派活、`task_assign`、额度改派三处照旧共用。测试：`tests_scheduling.rs::a_new_session_is_named_e_on_the_escalation_teammate`、`mock-agent/tests/handoff.rs::giving_it_to_the_escalation_teammate_is_an_escalation`，`mock-agent/tests/quota.rs` 的非升级改派补断言仍是 `w-`

## 十、agent 描述文件

> 2026-10-26 第一轮：内置三家的硬编码迁到描述文件，hub 按描述扫描本机、Agents 页显示结果。格式与优先级见 [DESIGN.md](DESIGN.md) §9.3。

- [x] 协议留在代码里，协议以外的写进 `plugin.toml`：内置三家在 `crates/plugins/agents/*.toml`（`include_str!` 编进 hub），外部 agent 同一种格式；`locate`（env / commands / paths / bundled / shim / version）、`install`（hint / docs / login / notes / label）、`[models]`（fallback / efforts）；每项都能按系统给。`manifest.rs` 不再只认 `protocol = "acp"`
- [x] 查找统一成 `Located { argv, path, source, version }`（`crates/core/src/cli.rs`），三家驱动不再各写一套：`native_command` > `env` > `PATH` > 自家安装路径 > 桌面版自带，命中后按名字过 shim（`codex-vendor` 解 npm 外壳、`node-script` 改用 `node <script>`）；版本 5 秒超时
- [x] 查找不缓存、版本按 (path, 修改时间) 缓存：hub 运行期间装上、就地升级、桌面版更新删掉旧构建目录，`installed()` / 开会话 / `/v1/plugins` 都拿到当下的结果；版本在 hub 启动时后台并行探（`Driver::probe_cli()`），探测期间 `version` 为空但不挡接口；claude / codebuddy 开会话时才取命令，codex 的进程键仍是未解析的 `codex app-server`（接管要按它匹配）。测试用注入的 `Machine` 和临时目录覆盖「后来装上」「旧构建被删、出现更新的构建」「文件时间变了重新探测」
- [x] `PluginInfo.cli`（`installed` / `path` / `source` / `version` / `install_hint` / `docs`；新增可选字段，不升 `API_VERSION`），`bindings/` 已重新生成；设置 → 智能体页顶部「本机的 CLI」，显示 `2.1.304 · 原生安装`，悬停看完整路径，没装的给可复制的安装命令
- [x] 迁移有据可查：描述文件与旧 `PluginConfig::claude()/codex()/codebuddy()` 逐字段相等的对照测试（迁完改成固定期望值）；本机三家迁移前后的 argv 逐字节相同
- [x] 安装按钮：按 `install.hint` 执行并把进度显示在 GUI（2026-10-26，9971310：描述文件 `[[install.methods]]` 声明安装与升级方式，hub install / upgrade 接口与 `cli.install` 事件流，GUI 一键安装/升级与确认框；Claude 的真实安装未在本机实测）

## 十一、交给 CrossAgents 自己改自己

> 2026-10-26，任务书见 [tasks/SELF.md](tasks/SELF.md)。按 A → B → C 顺序，A 做完再放开后面。

- [x] T1 hub 核心不绑语言（2026-10-28）：构建目录等环境变量改由预设表（按根目录标志文件命中、可组合）加 `crossagents.toml` 的 `[run.env]` 声明，占位符 `{build}`；没配检查时明确报「未验证」；占用识别分确定 / 可能相关两级。依赖目录（node_modules、.venv）不默认共享，另行设计（任务书 [tasks/TOOLCHAIN.md](tasks/TOOLCHAIN.md)）
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

## 十五、会话属于工作区，run 只记录谁参与过

> 2026-10-27，设计见 [DESIGN.md](DESIGN.md) §4.7，方案见 [tasks/S1.md](tasks/S1.md)。起因：run B 承接 run A 时想让 run A 的 planner 接着当主导，并由它决定用新会话还是 run A 用过的会话；现在主导被复制、worker 每个任务克隆一个用完就删、删 run 连带删会话。
> 已定（S1 §5）：派活默认新开执行会话（`fresh: true`），复用要写 `fresh: false`；人在聊的会话可以被派活，但人的请求优先、可中断；归档不自动清理，可恢复、可批量删除。

- [x] 会话的家：`Agent.home`（所属工作区）与迁移；跨 run 的原子占用表 `agent_claims`；驱动能力 `move_cwd`；`AgentHost::place` / `send_home`，用本人时恢复失败报错不新开（S1 §3.1）（2026-10-27）：迁移 0009 先 home = cwd，启动时 `migrate_homes` 把 worktree 里的会话归到 run 的 repo、吸收 `lead_home`；`claim_agent` 一条 INSERT；`place(carry)` 恢复失败报错；忙时 `send_home` 记待办（fbf76ce）
- [x] 主导是被选中的会话本人，不复制；run 结束送回工作区；`home_of`、`lead_home` 改由 `home` 取代（S1 §3.2）（2026-10-27）：不能换目录的驱动才复制，`run.lead_note` 写明；复制的主导在 run 结束时归档，恢复时重建检出（S1 §7 第 2 条）
- [x] 派活 `fresh` 默认 true 新开执行会话，`fresh: false` 用本人；不同工作区、不能换 cwd、已归档、人的请求在排队、被占用时返回原因；会话页显示正在执行的任务并可中断（S1 §3.3）（2026-10-27）：`restore: true` 恢复归档的会话；动态检查不过时任务留在 pending 下次再试；`/v1/agents/{key}/execution` 与 `…/interrupt`（77b603a）
- [x] 收尾不删会话、只释放本 run 的占用：本人回工作区，执行会话归档；删除旧 run 不影响正被新 run 占着的会话（S1 §3.4，吸收 §十一 A3）（2026-10-27）：`DeleteRunResponse.agents` 改为 `{ returned, archived }`；`AgentHost::remove_agent` 删掉（66f66f6）
- [x] 交接卡补执行结果 `outcome`，run 页按任务列出执行并链接到会话（S1 §3.5）（2026-10-27）：卡片加 `fresh`（新开 / 本人）和 `outcome`；run 页「参与者」可打开、恢复已归档的执行会话；侧栏按 `home` 分组；会话页「正在执行」条带中断；截图见 [screens/s1](screens/s1/)
- [x] 开场说明与 `agent_list` 列出工作区里可用的会话及最近一次执行（S1 §3.6）（2026-10-27）：最多 12 个、按最近使用排序；`agent_list` 另列本工作区已归档的会话（77b603a）
- [x] 会话页提供「复制原生恢复命令」：带正确目录（会话文件所在的项目目录）和账号环境（代理、配置目录），人能在终端里接着用同一个 session id（2026-10-27）：hub 新增只读 `GET /v1/agents/{key}/resume-command`（`crates/hub/src/resume.rs`），会话页顶栏「终端恢复」展开命令，Windows 给 PowerShell 写法。目录取会话现在的 cwd（hub 也在这里按 id 恢复，S1 §4 实测可行）；会话文件留在别的项目目录时写明位置，提示在这里打开恢复列表可能看不到它。环境只带账号配置目录（`CLAUDE_CONFIG_DIR` / `CODEX_HOME` / `CODEBUDDY_CONFIG_DIR`）和代理，代理里的用户名密码去掉，不带任何令牌。外部会话、驱动不能恢复（ACP）、with-native（没有终端恢复命令）、没开过会话、`context_lost`、目录不在了、找不到 CLI 时复制按钮置灰并写明原因；hub 正开着这个会话时照给命令，但提示先停掉。Claude / CodeBuddy 用 `--resume <id>`，Codex 用 `codex resume <id>`，真实 CLI 的交互式恢复没有在本机实测。测试：`resume.rs` 单元测试与 `crates/mock-agent/tests/resume_command.rs`；截图见 [screens/context-lost](screens/context-lost/)
- [x] 上下文没延续、代码没恢复的警告在会话页顶部提示条显示，手动关掉才消失（`apps/gui/src/notices.ts`）：原来这些 `agent.warning` 只折进最后一轮，恢复时会埋在旧消息里，没开过轮次的会话则直接丢掉。mock 场景 `context-lost*`、`code-lost*` 用 hub 的原文（MockBridge `HUB_SAYS`），截图见 [screens/context-lost](screens/context-lost/)

## 十六、并行跑 run / task 时暴露的四个问题

> 2026-10-27，run `01a17e22`（补齐持续推进的 5 个缺口）里三个 worker 并行跑的时候实测出来的，都已核实到具体原因，均未在那个 run 里修。

- [x] **共享编译目录在并行 worktree 下串味**（2026-10-28 修，见 §九 ① 第二条）：`18d4f6a` 让同一仓库的所有 run 共用 `<worktrees>/targets/<仓库>-<哈希>` 作为 `CARGO_TARGET_DIR`，省下重复编译依赖的时间。但 workspace 成员的包名和版本号在各个 worktree 里完全相同，产物会互相覆盖：A 的 worktree 会拿到 B 编译出的 `crossagents-core` / `crossagents-api-types`。实测两种假错误——A 报 `unresolved import crossagents_api_types::UpdateRunPolicyRequest`（代码里明明有），B 报 `missing field stuck_minutes`；换成独立 `CARGO_TARGET_DIR` 两边都干净通过。hub 跑 task 验收时撞上这个会把好改动判成失败。现在每个 worktree 一份 `<root>/<run>/targets/<tree>`，用完随 checkout 一起回收（细则见 §九 ①）
- [x] **`schedule()` 不被 `task_create` / `task_assign` / `task_update` 触发**（2026-10-28 修）：`crates/orchestrator/src/engine.rs` 的 `schedule_reporting` 只在计划批准、任务合并/失败这些时刻跑。所以在一个跑着的 run 里新建或改派一条任务，它会一直停在 pending、没有 assignee、也没有任何错误记录，直到别的任务事件顺带触发一次调度。实测等了十几分钟都不动，直到另一条任务合并。核实时 `task_create`（含 worker delegate 拆子任务）和 `task_assign` 的 pending 分支已经在写完后调用 `schedule_reporting`，只剩 `task_update` 没调。现在 `task_update` 也在锁内重新读 run、写完后 `schedule`，返回的任务是调度后的状态，返回结构不变；run 不在 Running 时照旧不派活。测试在 `crates/orchestrator/src/tests_scheduling.rs`：新建无依赖任务、改派 pending 任务、把依赖改成已合并的任务，三种情况都会当场派出，run 暂停时不会派出。当时 run `01a17e22` 的数据已经不在库里，没法确认那次卡住是不是只因为 `task_update`，也可能是旧代码，或者是 `max_parallel` / scope 冲突造成的正常排队
- [x] **`work_start.rs::a_proposer_that_can_move_leads_the_work_itself` 在重负载下偶发失败**（2026-10-27 记，2026-10-30 修）：根因是测试辅助函数 `propose()` 只等到 Work 卡片出现（`HumanAsked`）就返回，而此时提议者那一轮**还在跑**；紧接着 `start()` 去答卡片，hub 按规矩回 `409 busy: lyra is answering a person's message`。修法是卡片之后再等一个 `AgentStatusChanged → Idle`（照 `rules.rs:85` 现成的写法），让答卡片变成确定的时序。没有改成「撞 409 就重试」（那是盖症状），也没有动 hub 那条「正在答人的话就忙」的规则（那条是对的）。实测连跑三遍各 8/8。注意产品侧还有一笔账：人在 GUI 里卡片刚出现时点「开始」会撞上同一个 409，已记进 [TODO.md](TODO.md) 同名一节

## 十七、本地资源锁与通用执行器

> 2026-10-29，run `27bd5f0e` 的 `run_finish` 检查撞上主导自己的另一轮 `cargo test`（LNK1104）。设计见 [tasks/LOCKS.md](tasks/LOCKS.md)。

- [x] L0：验证执行器被强杀时，锁释放与进程树结束的先后（Windows Job、Unix flock 继承），按结论定保证的措辞（LOCKS §5；Windows 已实测，Unix 未验证）
- [x] L1：执行器库与 `crossagents exec`：锁放在 `<home>/locks/`、一次拿全、持有记录单独写、嵌套只放行不升级的重入、放不进 Job 就不运行（LOCKS §4；强杀与关句柄两条回归见 `crates/core/tests/exec_kill_regression.rs`）
- [x] L2：hub 检查、任务验收、`workspace_run` 改走执行器；删掉 `refuse_while_running` 的拒绝；`protocol.rs` 加一句（LOCKS §6）
- [x] L3：Rust 预设的 cargo shim（`crossagents.exe` 硬链接，按 `argv[0]` 识别）
- [x] 执行器在根进程退出后被残留进程无限拖住（2026-10-29 记，同日修）：run `041aba99` 的 worker 跑 `cargo test`，真正的 cargo 早已退出，shim 却挂了十几分钟不返回，worker 一直显示“运行命令”，当天卡了三次。原因是 MSVC `link.exe` 会拉起遥测进程 `VCTIP.EXE`，它在父进程退出后还会挂很久，又在同一个 Job 里，而 `exec::run::wait_tree_empty` 要等 Job 清空才放锁、返回，没有上限。修法：新增 `exec::end_tree(tree, grace)`，根进程退出后最多再等 `LEFTOVER_GRACE`（10 秒），到点 `Tree::end`（Windows 用 `TerminateJobObject`，保留句柄以便确认清空；Unix 结束进程组）后再等树空、放锁，仍满足 LOCKS §4.3“锁持有到树结束”；结束了残留进程时 shim 在 stderr 说明。hub 检查的 `verify.rs::end_tree` 同样改为最多宽限 10 秒（之前会一直等到这条检查的超时）。测试 `exec_tree.rs::exec_what_a_command_leaves_running_is_ended_after_the_grace`、`exec_a_tree_that_ends_within_the_grace_is_left_alone`；实测：隔离环境里经 shim 编译小项目，链接留下的 VCTIP.EXE 在 10 秒后被结束，命令 10.9 秒返回。部署：没有重启 hub，只把 `<home>/shims/0.1.6/cargo.exe` 换成新构建（旧文件移到 `shims/retired/`），之后启动的 cargo 立即生效；hub 自身的检查要等下次换 hub 才用上新逻辑

## 十八、工作组：持续的工作空间，run 是其中一轮

- [x] G1-3（2026-10-31）：主导**真的够得着** `run_next`。上一轮把工具加好了（在 `RECRUITERS` 组里、主导看得见），但 LEAD 提示词末段还写着「answer, but start no new work in a closed run」——工具存在却被提示词禁止用，等于没加。按 [tasks/G1.md](tasks/G1.md) §3.6 改为：一轮结束后人还会说话，他提出**新目标**时用 `run_next` 在本工作组开下一轮、不要用 `work_start`；问题、闲聊、对上一轮的追问直接回答，不开新一轮；只有人明确要另开一个独立的 Work 才用 `work_start`。**hub 不猜这句话算不算新任务，交给模型判断**是这里的设计前提，所以写的是口径而不是规则。
  - `work_start` 被已结束一轮的主导调用时报 409 并指向 `run_next`，错误里带工作组 id。没有误伤它的主要用途：不在任何 run 里的普通会话照常可用，`crates/mock-agent/tests/activity.rs::closed_lead_is_directed_to_next_round_while_chat_can_propose_work` 一条测试同时断言三件事——主导在已结束一轮里调 `run_next` 能开出下一轮、同一处境调 `work_start` 得 Conflict 且错误里有 `run_next`、普通会话调 `work_start` 仍 `is_ok`。
  - 审阅（Leo）发现第一版的这道拦截**堵死了它自己指的那条出路**：`work_start` 的参数只有 `request` 和 `title`，没有任何「我就是要开独立 Work」的出口，而 G1-1 的迁移给每个旧 run 都建了工作组，于是 work_id 几乎总是 Some——净效果是任何当过主导的会话此后永远调不了 `work_start`，偏偏 409 的文案还写着「use work_start only for a separate, independent Work」，指着一条刚被封掉的路；模型照提示做被拒、照错误信息做还是被拒，只会反复试。本轮修掉：`work_start` 加可选参数 `separate`，不带就 409 并**在文案里点名这个参数**，带了就放行。这和 §3.6 的设计前提是一套逻辑——hub 不猜人的话算不算新任务，门禁只负责把选择摆明，由模型表态。测试里补了一条断言：同一处境下 `separate: true` 能成功（原来那条 `is_ok` 断言覆盖的是从没进过任何 run 的 chat，证明的是「普通会话不受误伤」，构造不出这个矛盾场景）。
  - 人仍可不经 `work_start` 开独立 Work（GUI 首页、`POST /v1/works`）——这条路从来没被影响，审阅时确认过。

- [x] G1-2（2026-10-31）：新增 `run_next(title, request, from?)`，只有已结束且有工作组的 run 主导能调用，当场开下一轮，不出 Work 卡片，继承计划审批与落地策略。`POST /v1/works/{id}/runs` 共用规则，`CreateWorkRunRequest.from` 与 bindings 同步；`from` 只接受 `base`。
  - worktree 续轮默认从未落地且有改动的上一轮分支开始；已落地、已取消、无改动或显式 `from: "base"` 时从真正基线开始。续轮在原集成 checkout 内切分支，复用会话与 `targets/integration`；不能搬目录的复制主导也不再复制。分组 run 取消时停主导进程、将 worktree 改动留在原轮分支、释放占用，保留会话目录供续轮；未分组 run 的取消行为不变。原地模式保留当前目录内容，没有分支链与连带落地。没有增加保留 / 清理策略字段。
  - 落地沿同组分支链找到基线，包含所有未落地祖先，落地提交说明列轮次，祖先的 `landed.by` 为“随第 N 轮落地”，同时关闭其待落地通知。旧轮次可以单独落地，使用临时 checkout，不切换当前主导的分支；只允许清理或删除最后一轮，避免破坏共享 checkout。开场信息列最近最多五轮的标题、状态、落地状态和总结首行。
  - `crates/mock-agent/tests/run_next.rs` 覆盖 MCP 续轮、API `from: "base"`、已落地或取消后基线续轮、进行中拒绝、目录与缓存复用、重启后三轮连带落地、原地模式、旧轮单独落地和复制主导续轮。G1-3 / G1-4 / G1-5 未在本轮实施。

> 2026-10-29 设计见 [tasks/G1.md](tasks/G1.md)，2026-10-30 run `01a18a4a` 做了第一层。
> 与 §十九 冲突处以 [tasks/LEAD.md](tasks/LEAD.md) §9 为准。

- [x] G1-1：`works` 表与迁移、`Run.work_id`、事件与 `/v1/works*` 接口；同一组同时只有一个未结束的 run。迁移 `0011_works.sql` 建 `works(id, created_at, updated_at, data JSON)`，`runs` 上按 `json_extract(data,'$.work_id')` 建**部分唯一索引**作为库一级的保证；`create_run_for_work` 另在进程内按 `work:<id>` 取现成的 `lock_card` 互斥，查到未结束的 run 就回 409 并点明是哪个 run（id 与标题）。`work_id` 在 `RunCreated` 发出**之前**就随 `Run` 落定，不是事后关联——API 层事后补既不原子、事件也发不对。`Run.work_id` 带 `#[serde(default)]`，旧库的 run 读出来就是未分组，不自动归组，原有流程一字不变。`Store` 的四个新方法都带默认实现，其他实现者不受影响。接口：`POST /v1/works`、`GET /v1/works`、`GET /v1/works/{id}`（work 与它的 run，最新在前）、`POST /v1/works/{id}/runs`（首轮默认配置，后续沿用最近一轮的）。两条集成测试走真实 hub 与真实 SQLite，其中一条真的模拟旧库——剥掉 `work_id`、删掉索引、表和迁移记录再重开——并把迁移 SQL 再执行一遍证明幂等
  - 留了个隐患并写在 `RunStatus::is_terminal()` 上：索引里的三个终态是 SQL 字面量，和 `is_terminal()` 是**两份名单**。以后加终态只改 Rust 那边，那个工作组就再也开不出新一轮，症状隐蔽（不是报错，是 409 说「还有一个开着的 run」）。而且**已应用的迁移一个字都不能改**（sqlx 校验和，哪怕只加注释），真要改索引得另起 `0012_*.sql`

## 二十、发起时选会话

> 2026-10-30，run `01a18a4a`。人的原话：「我发起 work/chat 的时候，可以选择新的 session 或者已有 session」。

- [x] 后端：`POST /v1/runs` 加 `fresh?: boolean` 与 `restore?: boolean`。**不说 `fresh` 保持原有的自适应**（主导能移动就用它本人带着对话、否则复制），`false` 要求就用本人、过不了检查回 409 而不悄悄退化成复制（人点了「用已有会话」却拿到一个没有对话的新会话是错的），`true` 一律新建。就地运行时主导不换目录，所以 `fresh: false` 不再要求驱动能换目录——`may_take_self` 加 `needs_move` 入参，任务侧照旧要求；否则「显式选用这个会话」会比不选更严格，而且严在恰好不需要移动的情况上。新增 `POST /v1/agents/{key}/chat` 在已有会话上继续对话（只读查 `claim_of` + `held_by` 报 409，**不动 `deliver`**——run 给自己占着的 agent 投递是正常的，在 `deliver` 里加 claim 检查会把整个 hub 的派活打死），`GET /v1/session-candidates?repo=<工作区>` 复用现成的 `workspace_sessions` 列候选。会话可用性是**参考快照**：chat 不 claim，查到发之间状态可变，真正的互斥只有 `claim_agent` 那条原子插入
  - 关键一条：**归档的会话 `can_use` 为真**、`reason` 为 `archived`，因为它配上 `restore` 是能用的。若按「有 reason 就禁用」来写，`restore` 这条路从界面上永远点不到——复审时返工过一轮才改对。不可用的原因用 `NotReady` 枚举而不是字符串，`can_use()` 和它长在一起，新增变体时编译器会要求说明算不算可用
- [x] GUI：首页发起 Work / Chat 都能选「新会话」或某个已有会话（`apps/gui/src/lib/sessionChoice.ts`）。没选时 `fresh`/`restore` 一个都不发，请求与从前逐字节相同；禁用只看 `can_use` 不看 `reason`；409 不是异常而是常态（候选是快照），显示 hub 的原话并重新拉一次候选让人改选。已有会话开 Chat 走 `/chat` 不再建新 agent。另一半修掉了批计划时「除已有队友只剩主导同家模型」：`Run.tsx` 原先 `plugins.data?.find(p => p.name === lead?.plugin)?.models` 只取主导那个插件的模型，数据本来是齐的只是被筛掉了；改为列所有已装插件的模型按家族分组，**新 worker 的插件与账号从模型所属插件来，不再跟主导一致**（这是之前 Kiran 起不来的病根：只给 model 不给 plugin，deepseek 被配到 claude 插件上，任务永远 pending），测试里有正向加一条反向断言「不许用主导的账号」。同名模型被多家提供时标签带插件名，未登录或 CLI 未安装的插件其模型仍列出但禁用并写明原因
- [x] 已有会话开 Chat 也能带附件：`StartChatRequest` 加可选 `attachments`，`/chat` 与 `/messages` 走同一个 `store_uploads`（先按 agent 能力检查再存）；只带文件不改会话主题。界面上去掉了「选已有会话时不能带附件」那句拒绝
- [x] Chat 没选工作区时列得出候选：`GET /v1/session-candidates` 的 `repo` 可省，省了就列独立聊天（家在 `<home>/chats/` 下的会话，按创建时间新的在前）；聊天不搬家，挡住它的只有被 run 占着或已归档。测试 `attachments::a_standalone_chat_is_offered_and_carried_on_with_files`

## 二十一、自举暴露的 hub 缺陷

> 2026-10-30，run `01a18a4a` 记下的五条。回头对着事件和 worker 的会话记录查了一遍，**当时的诊断有两处不对**，按查到的写：没有发生「30 分钟超时回落」（提问 03:16:46 发出、03:20:57 就答了，那个 run 也没开超时）；主导的答复也没丢——它排在 worker 正在跑的那一轮后面，worker 那一轮其实已经按改好的 spec 在干活，被主导两条 `now` 消息先后打断，答复始终被插队的消息挤在后面。

- [x] 已有提问时再 `task_ask` 换个说法被拒（`invalid task transition WaitingAnswer -> WaitingAnswer`）：问题其实已经转给了主导，worker 却收到报错，以为问不出去，改走消息，两边从此对不上。现在新说法**替换**开着的提问，返回里 `replaces` 指向旧的那条；测试 `tests_scheduling::asking_again_replaces_the_open_question`
- [x] 「两边看到的状态互相矛盾」：并不矛盾——主导第二次 `task_answer` 时问题已经被它第一次答掉了。`has no open question` 现在写明任务处于什么状态，并说「已经答过的会在 worker 这一轮结束后读到」；`task_answer` 的返回带 `read: now | after_current_turn`，worker 正在一轮里时明说答复在排队、打断只会让别的消息插到它前面
- [x] `task_drop --requeue` 后重排撞上旧分支而失败：丢弃时 worker 还在跑，检出和分支按规矩保留，重排却用同一个名字建。现在派活时名字被占（目录在或分支在）就用 `<key>-2`、`-3`……另起，旧的原样不动；`WorkspaceProvider::has_branch`；测试 `pipeline::a_task_starts_beside_a_branch_a_dropped_attempt_left`
- [x] `task_create` 的 `assignee`「静默无效」：其实生效了（任务一秒后就以 Jasper 开工），是返回里给的是排程之前的快照（`assignee: null`、`pending`），主导据此又 `task_assign` 了一次，多起了一个执行会话。返回改为排程之后的样子；测试在 `a_created_task_with_nothing_to_wait_for_starts_at_once` 里加了断言
- [x] 答复「不一定到得手」：提示词两头补上。worker：spec 才是决定，被改过或在等答复时先 `task_get` 再问；再问会替换开着的提问。主导：改动放行范围的决定要同时写进 spec（`task_update`）；worker 在一轮里时答复会在这一轮结束后读到，打断它重复答复只会让别的消息插队

## 二十二、仓库卫生

> 2026-10-30，run `01a18a4a` 里顺手修掉的。

- [x] `cargo fmt --all -- --check` 在**没人改动过的文件**上失败：本地 rustfmt 升到 1.10.0-stable（2026-10-13）后换行取舍变了，仓库上次全量格式化用的是更旧的版本，九个文件有漂移。这让每条任务的格式检查都挂在与它无关的文件上，噪声盖掉真问题，还逼得我 force 接受了一条本来合格的任务。已用 rustfmt 1.10 统一重排（纯机械，不改行为）
- [x] `cargo clippy --workspace --all-targets -- -D warnings` 在干净分支上也通不过：仓库一直有五处告警。这是我给 works-table 写验收命令时没核实造成的——**给 worker 设了一道过不去的关**，它照实报告被挡住，我才发现。修掉四处（多余的 `.into_iter()`、嵌套 `if` 合 let-chain、三元组起名 `Reply`、多余的 `mut`），`EventKind` 的 `large_enum_variant` 加带理由的 `#[allow]`（真修法是装箱，见 [TODO.md](TODO.md) 同名一节）。教训：**写验收命令前要先在基线上自己跑一遍**
- [x] `cargo test --workspace` 的偶发失败 `settling_the_checkpoint_at_idle_does_not_swallow_a_stuck_report`（2026-10-30 记，2026-10-31 查到名字，2026-11-05 修，`d798e28`、`6984fb5`）：不是当时猜的「和并发负载抢时间」，而是测试的 `quiet()` 不持 run 锁改写时间戳，后台 pass 可能把旧副本写回，抹掉伪造的静默期。改为在同一把 run 锁内改写，并显式等回合结束事件落库。同日另修了 `delete_run` 和 `steer_failure` 两条不稳定测试

## 十九、Lead 执行模式与工作区保留

> 2026-10-29 设计见 [tasks/LEAD.md](tasks/LEAD.md)，2026-10-30 run `01a18a4a` 做了 P1。
> 顺序：G1-1 → P1 + G1-2 → G1-3 / G1-4 / G1-5 → P2 → P3。

- [x] P1 拆分收工与清理：`finish_run` 只释放调度占用并落 `Completed`/`Failed`，不再 `let_go` 主导、不删集成检出；删掉 `checkout_vacated` 那条「移动即清理」的路径（会话搬走不再顺手收掉保留的工作区）。新增 `Orchestrator::cleanup_run(id, force)` 与 `POST /v1/runs/{key}/cleanup`（人的权限），与 `delete_run` 共用 `clean_run`，清理保留 run 记录与分支。边界靠 `owns_checkout`：必须非原地、不是仓库本身、父目录正是 `<worktrees>/<run>`、名字不是 `targets`；`checkout_unused` 再排除任何以它为 home 的 agent 与任何未归档的 cwd，所以用户仓库、agent 的 home 及其子目录、别的活跃会话都碰不到，被新 run 占用的会话不动。worker 的检出在任务被接受后即可清理，但先过归属与占用检查。落地不再自动拆掉保留的工作区
  - **无条件保留，没有策略字段。** 原计划写的是「保留 / 清理策略（工作组级，默认保留）」，核文档后发现 §9 的表指向的 §10 第 3 条把它列为待定（只写了「倾向工作组级」），所以不在这轮定。语义两句：默认保留，要收就显式收。见 [TODO.md](TODO.md) 同名一节里「要人定」那条
  - 四条已修好的行为都保住了（见 §九），各有测试在跑：复制出来的主导仍走 `archive_agent_after_turn` + `leaving`，连「归档落在回调注册之前」的竞态复查都照 `claims::put_away` 复刻了一份；**任何 warning 都保守地同时保留检出与 `targets`**——占用记录可能已经消失，「没有 claim」不等于进程已停；`targets` 仍按 `prune_run_targets` 的规则回收，没有改回递归删整份；`Verifying` 期间的防护未动
  - 行为上有一处人该知道的变化：`delete_run` 现在对**有未提交改动**的 run 一律拒绝（要 `force`），从前只在「未落地」时拒绝。落地之后在检出里新写的东西从前会被直接删掉，这不对——LEAD §8 要求有未保存的结果就保留现场并提示
  - 十四条现有测试的期望按新语义逐条改过（改期望、没删断言）。新增五条，其中 `session_reuse.rs::a_finished_work_can_start_another_round_and_keeps_the_previous_workspace` 真的走了 works 接口，顺带证明 `Completed` 在那个部分唯一索引的终态名单里
  - `run_next`（G1-2）的接点：从 `create_run_for_work` 进来，复用已保留的会话与检出、重新记录基线与任务归属；`integration_path` 与 `owns_checkout` / `checkout_unused` 就是「把按 run 的物理树映射改成共享工作区归属」的那几处

## 二十三、发布

> 2026-10-30。

- [x] **0.1.17 发出去的安装包里没有 hub**（同日修，`614af13` + 0.1.18 重发）：`build.extraResources` 从 `../../target/release/crossagents.exe` 把 hub 打进安装包，而那次 `cargo build --release` 跑在设了 `CARGO_TARGET_DIR` 的 shell 里（run 的构建目录），产物落到别处，源文件根本不存在；**electron-builder 不因缺少 `extraResources` 而失败**，照样打包。后果：更新到 0.1.17 的 GUI 没有新 hub 可装进 `<home>/bin/<版本>/`，只能继续用旧 hub（界面显示「界面 v0.1.17 · hub v0.1.16」）；全新安装则完全没有 hub。修法：`apps/gui/scripts/check-version.mjs` 除了比对两处版本号，还要求那个二进制存在、且 `--version` 报的就是本版本，失败时说明该怎么办与 `CARGO_TARGET_DIR` 这个坑。0.1.17 的安装包、blockmap 与 `latest.yml` 已从发布仓库删除并写明原因，保留其正常的独立 hub 二进制
  - 发布流程里同时暴露的两处，都是**发完之后逐项核验**才发现的（命令本身全是退出 0）：electron-builder 的并发上传**建了两个同 tag 的 release**，安装包进了一个、手动补传的 hub 进了另一个，合并后删掉重复的那个，并重新标 Latest（Latest 随被删的那个一起没了）；手动 `gh release upload` 时文件名里的空格被换成点（`CrossAgents.Setup.0.1.18.exe`），而 `latest.yml` 指向连字符的名字，自动更新会 404，用 `PATCH .../releases/assets/<id>` 改名而不必重传 119MB
  - 教训：**打包和发布都要先在干净的工作树里做**（主检出常有未提交改动，会被编进二进制），而且那棵树里要显式清掉 `CARGO_TARGET_DIR`；**发完要逐项核对资产名、大小与 `latest.yml` 的一致性**，退出码为 0 不等于发对了

## 二十四、10-31 之后的工作

> 2026-11-06 按提交记录和设计稿补记，当时没走 TODO。这里只记做过什么，没有逐条复测；细节看对应设计稿和提交。没做完的尾巴见 [TODO.md](TODO.md) 同名一节。

- [x] 会话优先（10-31）：聊天发起的 run 用独立主导，普通聊天不再带 run 的工具和说明（[SESSION-FIRST.md](SESSION-FIRST.md)）
- [x] 减少额度消耗阶段 1、2（[USAGE-TRIM.md](USAGE-TRIM.md)）
- [x] 发版脚本 `scripts/release.mjs`，产物核对改成硬失败（10-31）；依赖恰好同版本时不再卡住（11-06）
- [x] 平等 Agent 与项目侧栏 A–D（11-01）：任何会话都能审核和接任务，侧栏变平，加「需要你」分区和项目总览（[AGENT-WORKSPACE.md](AGENT-WORKSPACE.md)）
- [x] 专长档案（11-02）：hub 接口、工具、reviewer 检查清单、GUI 页，11-04 接真实 hub 实测（[tasks/EXPERTISE.md](tasks/EXPERTISE.md)）
- [x] Cursor 接入：`cursor-native` 驱动（11-02）、账号与交互登录（11-03）、模式分离和逐任务推理强度（11-04）、非 bypass 会话不卡在目录信任提示（11-05）（[research/cursor-cli.md](research/cursor-cli.md)）
- [x] GUI 工具区（11-03、11-04）：浏览器式标签可拖到对话下方，文件面板可渲染 Markdown，内置终端（窗口关了终端还在），「后台任务」看板
- [x] 工作区独立记录 run 实现历史，删除 run 后仍可查（11-03）；每个 run 都属于工作组（11-03）；run 结束删构建目录、合并后删 checkout、启动时清残留（11-04）
- [x] 运行中心跳改为 token 速度（11-04）
- [x] 插话（11-05）：驱动、hub、转录和前端，加真实 Claude 冒烟（[research/codebuddy-steer.md](research/codebuddy-steer.md)）
- [x] 扩展 E1–E4（11-05）：目录与批准、一次性工具、页面接口、管理页、创造工具、事件响应、常驻进程（[EXTENSIONS.md](EXTENSIONS.md)）；内置功能迁移评估和事件流面板试点（[EXTENSIONS-MIGRATION.md](EXTENSIONS-MIGRATION.md)）
- [x] 识别内容审查拦截并提示换会话或压缩（11-05）；工具卡片相对路径和按 kind 的标题（11-05）
- [x] 队友之间的回复不再丢（11-06）。起因：Rune 把文件浏览的报告写在自己的会话里，没调用 `agent_send`；后来补发时又写成了 `"Pia"`（另一个 agent），而不是 `"Pia 4"`，消息被静默投给了别人。现在：
  - 只用文字回答队友消息的那一轮，hub 把它最后一段话以作者的名义转给发件人（`crates/hub/src/replies.rs`）。转发出去的消息再被文字回答时不会转回去，不会来回弹。两边有一方在做 run 的任务时不转发。
  - 消息头带上 id（`Message from X (id …):`）；`agent_send` 只给 `reply_to` 不给 `to` 时，回给原发件人。
  - `to` 写的名字恰好是正在和你说话的队友去掉数字后缀（比如该写 `Pia 4` 写成了 `Pia`），而你和它从没互发过消息时，拒绝发送，并说明该怎么写。
  - 测试在 `crates/mock-agent/tests/collab.rs`（3 条）和 `replies.rs` 的单元测试（7 条）
- [x] 驱动与 hub 修复（11-05、11-06）：Claude 驱动写入不留半帧、失败 result 不提前结束；`run_next` 在 lead 回合内不再报 busy；Codex primary 额度误判；复制失败、回复看不到图、`run_next` 重复显示人的原话

## 二十五、借鉴 oh-my-pi 与 Cutex 0.2.1

> 2026-11-06 run `7cfd5794`。各条的来源和依据见 [TODO.md](TODO.md) 同名一节；没做完的条目留在那里。

- [x] **3. 排队的提示随人的消息一起送**（Cutex 057）：人发消息时，如果这个 agent 还排着一条没开始的通知汇总，hub 就把汇总撤回（发 `agent.input_withdrawn`），并进人的这条消息。agent 读到的是人的话在前、汇总在后，不用为汇总单独多跑一轮。`agent.input` 和转录的每一轮都多了 `notices` 字段，`text` 仍是人的原话；GUI 把提示折叠挂在人的气泡下面（「随这条消息送达 N 条提示」），重新加载转录后还在。汇总已经开始，或者人发出之后才到的通知，照原来的规则走。撤回和合并在 `digests` 锁里一次做完，加锁顺序和 `notify` 一致（评审时发现过一个丢行的竞态，已修）。
  - 代码：`crates/hub/src/runtime.rs` `take_waiting_digest`、`crates/core/src/event.rs`、`crates/hub/src/transcript.rs`、`crates/api-types`、`apps/gui/src/views/SessionMessage.tsx`、`lib/notices.ts`。
  - 测试：`crates/mock-agent/tests/notices_ride.rs`，2 个用例覆盖 3 种情形（合并且只多跑一轮；汇总已开始时不合并；合并之后再来的通知另起一条汇总）；`apps/gui/src/lib/notices.test.ts`。
  - 没验证：气泡下的提示没在真窗口里看过，只测了数据处理，过了类型检查；合并之后、那一轮开始之前 hub 如果重启，恢复时只按 `text` 重新排队，带着的汇总会丢（这是原有的恢复路径，没改）；外部 agent 不合并。
- [x] **4. hub 层的流式规则**（oh-my-pi ttsr）：人定的是「先做项目文件，扩展留接口」。规则写在仓库根的 `.crossagents/rules/*.md`，frontmatter 写 `id`、`match`（正则，可以写多条）和 `on: output|tool|both`，正文是要送进去的规则（格式见 [tasks/STREAM-RULES.md](tasks/STREAM-RULES.md)）。规则平时不进上下文；agent 的输出（4 KB 滑动窗口，跨分片也能命中）或工具调用（名字和参数）命中时，驱动支持插话就插进当前这一轮；不支持就中断这一轮，把规则排在最前面先跑。同一条规则在同一个会话里 20 轮之内只送一次，hub 发起的 compact 成功后重置。坏文件、坏正则只记 warn 并跳过。`RuleSource` trait 留给以后接扩展。
  - 代码：`crates/hub/src/stream_rules.rs`（新）；`runtime.rs` 的 `install`、`forward_events`、`inject`、`Control::Compacted`。
  - 测试：`cargo test -p crossagents-hub stream_rules`（13 条单元测试）；`crates/mock-agent/tests/stream_rules.rs`（4 条：插话路径、20 轮去重、compact 后再送、不支持插话时中断再跑）。
  - 没做 / 没验证：只用 mock agent 跑过，没在真实 Claude/Codex 上看过插话；驱动自己压缩上下文时没有事件，去重状态不会重置；中断路径不重发被中断那一轮的提示，只跑规则（agent 的会话上下文还在，能接着做）；如果被中断的是人的消息那一轮，人会看到这一轮被中断；插话事件没有 `from` 字段，只能靠正文开头的规则 id 认出是规则；只匹配工具名和参数，不匹配工具的输出；扩展那一侧没接。`inject` 在事件泵里同步等待 steer/interrupt 返回，ACP 驱动的 interrupt 最多等 5 秒，这段时间输出转发会停住。
- [x] **5. 任务交结构化结果**（oh-my-pi 的 subagent）：任务的 spec（`plan_submit` 和 `task_create` 里的）可以带 `result_schema`（JSON Schema）。`task_complete` 新增 `result` 参数：任务有 schema 时 `result` 必填，并按 schema 校验（`jsonschema` 0.33，锁文件里本来就有）；不通过就报出哪条路径不符，任务状态不变，worker 可以改了再交；通过就存进 `TaskReport.result`。`task_get` 新增 `path` 参数（JSON Pointer），只返回那一处的值，路径不存在时报错。worker 的简报里会写出 schema。GUI 的任务详情把 result 折叠显示成 JSON。
  - 代码：`crates/core/src/model.rs`、`crates/orchestrator/src/tasks.rs` 的 `complete_with_result` 和 `task_get`、`tools.rs`、`brief.rs`、`apps/gui/src/views/Run.tsx`。
  - 测试：`crates/mock-agent/tests/task_result.rs`（2 条）：缺 result 和不合 schema 的 result 被拒且状态不变，合规的存下，按 path 取值，不存在的 path 报错，没有 schema 的老调用照旧；`script.rs` 补了嵌套数组解析的单测。
  - 调试时发现：mock 场景里，run 的自动标题会截进半截 `[call …` 脚本，mock 驱动把它当成指令解析，于是卡住。测试改成显式给标题；mock 的这个毛病本身没改。
  - 没验证：没有 schema 时交 `result` 的情况没单独测；GUI 的折叠 JSON 没截图看过。
- [x] **7. 合并的活动流**（oh-my-pi agent-hub.md 的 Activity）：工具列新增「活动」标签，可放进右侧或底部 dock。所有 agent 的回复、工具调用、错误、消息（agent 之间的、人的、向人提问和人的回答）排成一条时间线，最新的在上面。类型可多选筛选（错误 / 回复 / 工具 / 消息）；范围可以是全部、某个 agent，也可以是它连同下属（按 `spawned_by_agent` 递归推算，防环）。点一条就打开那个会话，并滚到对应的那一轮；找不到那一轮时，在面板里说明。
  - 代码：`apps/gui/src/lib/activityFeed.ts`（纯函数）、`ActivityFeed.tsx`、`activityfeed.css`，在 `panes.ts` 登记标签。
  - 测试：`apps/gui/src/lib/activityFeed.test.ts`（10 条），覆盖事件折叠、流式合并、工具失败更新、轮次关联、下属推算、范围和类型筛选。
  - 截图：mock 场景 `activity`，宽、窄、浅色、深色各一张，见 `docs/screens/activity-*.png`。窄屏加深色、点工具行跳到 Rex 的会话，这两项主导另外截图看过。顺手修了场景的一个毛病：转录里预先放了同一批消息，导致同一条消息显示两次、React 报重复 key。
  - 没验证：只在 mock 里看过，没接真实 hub 的事件流；只能看到页面加载之后的事件，界面上写明了这一点；键盘操作只确认了用的是原生 button/select 并带 aria 标签，没逐项用 Tab 走一遍。
