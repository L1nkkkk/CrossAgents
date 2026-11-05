# 内置功能搬进扩展：逐项评估

> 只做评估，不改代码。证据截至 HEAD `9dfce5fc73b4b9b5607a51b6304badcd93256039`（E1–E4 已合入）。
> 重新计数 `crates/orchestrator/src/tools.rs:97` 的静态 `TOOLS`，共有 67 个 `ToolDef`；动态扩展工具不计入。
> 范围：hub 的工具表 `crates/orchestrator/src/tools.rs:97` 起的 67 个内置工具，以及 GUI 的工具列面板
> （`apps/gui/src/ToolColumn.tsx`）和其他页面上可以指认的内置界面。
>
> 这份评估回答的是 [EXTENSIONS.md](EXTENSIONS.md) §1 那句「现有内置功能不搬。等接口用熟了，再逐个判断有没有搬的价值」
> ——把「逐个判断」做出来。结论先说：**不改接口就能搬的只有 3 个 hub 工具和 1 个 GUI 面板；其中适合当第一个试点的只有事件流面板。**

## 0. 结论

**现在就能搬（接口已够；差异逐条写明）**

| 项 | 位置 | 差异 / 条件 |
|---|---|---|
| `note_list` / `note_read` | `tools.rs:243` / `tools.rs:232`，`crates/orchestrator/src/notes.rs:105` | 返回值能重建；但扩展 token 是通用 read，能读别的 run 的全文（见 §2.1 的条件） |
| `gui_screenshot` | `crates/orchestrator/src/tools.rs:564`，`crates/orchestrator/src/screenshot.rs:78` | 零新 hub 接口，但依赖 checkout + `apps/gui/node_modules`，且 超过 1 MiB stdout 上限的 base64 会使调用失败（见 §2.1） |
| 事件流面板 | `apps/gui/src/ToolColumn.tsx:411` | 试点进行中，差异与验收见 `extensions/debug/README.md` |

**补接口才能搬**：`expertise_search`、`expertise_get`、`usage_get`、`agent_get`、`agent_list`、`whoami`、
`watch_list`、`watch_cancel`、`remind_list`、`remind_cancel`、`message_withdraw`、`agent_send`、`inbox_read`、
`note_write`、全部 `lesson_*` 与 `issue_*`、`expertise_pending`、`expertise_triage`、`task_list`、`task_get`、
`workspace_read/diff/run`、`checks_run`、`agent_set_model/interrupt/retire`，
以及改动、文件、提交图、后台、任务五个面板、扩展设置块、接管内置工具卡。

**不应该搬**（§1 不变量：事件日志、任务与合并状态机、驱动、权限、存储）：
`plan_submit`、`task_create/update/assign/cancel/answer/accept/verify/rework/drop/progress/complete/fail/ask`、
`review_request`、`review_submit`、`branch_sync`、`run_land`、`run_finish`、`run_next`、`work_start`、
`agent_spawn`、`remind_set`、`watch`、`human_ask`、`human_notify`、
`extension_*` 六个机制工具；GUI 侧的对话、Run 页、设置、工作区、侧栏、菜单与快捷键、终端、提问与通知、安全模式入口。

**事件流试点进行中，差异与验收见 `extensions/debug/README.md`。** 决定：手动安装启用、默认没有、发布版启用后可见、内置面板保留、不补宿主能力。

## 1. 判定口径

### 1.1 什么算「能搬」

三条同时成立才算「现在就能搬」：

1. **接口够**：工具只用扩展工具进程拿得到的东西——manifest 里能声明的字段
   （`crates/hub/src/extensions/manifest.rs:11-20`）、stdin 里的 `caller`
   （`crates/hub/src/extensions/runner.rs:675`）、扩展 token 能过的 REST
   （`crates/hub/src/access.rs:56`）、以及本机进程和文件系统。GUI 只用 `ca`
   （`apps/gui/src/extensions/host.ts:76-110`）与 `ca.hub.request` 能过的
   `apps/gui/electron/allowed.cts`。
2. **等价**：输出与现在一致，或差异明确、可接受并写进验收。只是「拼得出来但少一半字段」算补接口。
3. **不碰 §1 的不变量**（`docs/EXTENSIONS.md:14-21`）。

### 1.2 扩展工具现在实际拿得到什么

- **身份不是调用者**：`caller` 只给 `kind/id/name/groups/cwd/run/task`
  （`runner.rs:82-95`）。扩展 token 是 `Grant::Extension`，级别只能是 read 或 write，
  到不了 `/v1/me*`（`crates/hub/src/api.rs:242-249`），也调不了「以人的身份调工具」的路由
  （`access.rs:118-124`）。
- **E4 改判：原结论「没有事件驱动、没有常驻进程」被推翻。** `manifest.rs:34-63` 声明 `[[on]]` 与 `[service]`。钩子按事件起进程，只观察，stdin 的 caller 是 `{kind: "event"}`，不是 agent（`crates/hub/src/extensions/hooks.rs:149-194`）；不重试、不回放，总线落后、并发满或限速都会丢事件（`hooks.rs:1-16`、`:108-125`、`:172-212`），不接受 `extension.*`（`manifest.rs:153-170`）。
- **常驻不等于可靠调度与身份**：每个启用扩展至多一个 service，退出后退避重启；停用、换版本和 hub 退出均会停止它（`crates/hub/src/extensions/service.rs:1-9`、`:241-281`、`:313-338`）。它可自行维护定时器或消费事件，但恢复状态、补漏与幂等要扩展自己实现；扩展 token 仍非 agent 会话 token（`api.rs:242-249`）。
- **一次调用一个进程**，超时、stdout 上限、并发上限见 `docs/EXTENSIONS.md:106-127`；
  输出走 `images` 约定（`crates/cli/src/cmd/mcp.rs:79-95`）。
- **权限是组名的交集**：manifest 写一组组名（`manifest.rs:17`），调用时 hub 把调用者**此刻**的组
  传进去（`tools.rs:878-891` 用 `self.groups(caller)`），运行器取交集（`runner.rs:591-603`）。
  所以 `[lead]` 就是「此刻是 lead」，`[lead, assignee, reviewer]` 是 OR，内置的
  `PLANNER/RUN/WORKER` 组合（`tools.rs:15-22`）都能直接表达，不需要多份 manifest。
  真正变粗的不是组名，而是 REST token：它只有 read/write，不绑定调用方，也不绑定目标归属
  （`access.rs:125-133`）。

### 1.3 GUI 扩展现在实际拿得到什么

- 面板上下文只有 `{ follow, extension }`（`host.ts:34`）。工具列渲染扩展标签页时也只传 `follow`
  （`ToolColumn.tsx:80-83`），而内置面板还拿 `target`、`routeTask`、`routeRun`
  （`ToolColumn.tsx:107-133`；`App.tsx:211-225` 只在路由页给出 `task`/`run`）。
  所以扩展面板拿不到「故意打开的轮次/文件」，也拿不到 run 路由的 run 或 task。
- 只能给自己的工具注册卡片（`host.ts:436-441`），接管不了内置工具卡（`Session.tsx:896-900`）。
- `ca.hub.request` 就是主程序自己的请求（`host.ts:464-466` → `apps/gui/src/api.ts:194` →
  `allowed.cts`），用的是人的 token。`allowed.cts` 里有 admin 级路由（终端 `allowed.cts:19-22`、
  plugin 安装 `allowed.cts:52`），所以这不是「只读面板」的能力边界（证据与范围见 §5）。
- import 不到 `src/` 的任何模块（`docs/EXTENSIONS.md:365`），`DiffView`、`Avatar`、`useSession`、
  `useHubData`、时间格式、i18n 都得自己重写。

## 2. hub 工具逐项

### 2.1 现在就能搬

| 工具（定义 / 实现） | 为什么够 | 差异 / 条件 | 值不值 |
|---|---|---|---|
| `note_list`（`tools.rs:243` / `notes.rs:131-156`） | `GET /v1/notes`（`api.rs:151`）回 `Note` 全文（`crates/core/src/note.rs:23-32`）。重建规则：`scope` 缺省时先 run 后 team（`notes.rs:140-148`）；没有 run 就跳过 run（`notes.rs:20-25`）；`scope=team` 不带 run；key 要 `trim`；每条按 200 字符截断成 preview（`notes.rs:151`） | **权限不随包**：`list_notes` 不校验调用方，`run` 是查询参数（`api.rs:1000-1016`）；扩展的 read token 是通用 read（`access.rs:125-133`），所以它能传任意 run，读到别的 run 的全文。返回值能复现，hub 强制的「只看自己那份」复现不了 | 低-中：省不下多少核心代码；要接受这条权限缺口 |
| `note_read`（`tools.rs:232` / `notes.rs:105-129`） | 同上，先 run 后 team（`notes.rs:113-124`），按 key 精确匹配，找不到回 `NotFound` | 同上的读取面；另外 `NotFound` 的文案要自己造 | 低：和 `note_list` 同一份实现，适合一起搬 |
| `gui_screenshot`（`tools.rs:564` / `screenshot.rs:78-131`） | 逻辑是「跑 checkout 里的脚本，把 PNG 放进 `images`」。脚本只打印 `{path,url}`（`apps/gui/scripts/screenshot.mjs:9`、`:76`），base64 由调用方读文件（`screenshot.rs:123-128`） | **不是只依赖 cwd 和 node**：内置先要求 `caller.cwd` 是 CrossAgents checkout 且有 `apps/gui/node_modules`（`screenshot.rs:86-97`）；脚本自己起 Vite + Electron、读 `vite.config.ts` 与 mock preview（`screenshot.mjs:43-59`）。**cwd 也不同**：扩展工具的 cwd 是快照（`runner.rs:1082`），包装器要显式用 `caller.cwd` 里的脚本路径并在那里启动，`--out` 给绝对路径（脚本按 `process.cwd()` 解析，`screenshot.mjs:40`）。**输出有上限**：扩展工具的 stdout 上限 1 MiB（`runner.rs:46`），超了整次失败（`runner.rs:1145`）；内置没有这个限制（`screenshot.rs:124-128` 直接 base64 进结果）。**超时**：内置 180 秒（`screenshot.rs:18`），扩展默认 60 秒，manifest 要写 180 | 中：它是唯一能真的从核心删掉一块的工具。但大图必须换交付方式（只回 path、或缩图），这是设计决定，不是接口缺口 |
| 事件流面板（GUI） | 见 §3 | 见 §3 | 中：最适合验证 E2 |

### 2.2 补接口才能搬

E4 不改变这里的身份、归属和 REST 缺口：钩子 caller 只有事件身份（`hooks.rs:181-184`），service 使用扩展 token（`service.rs:4-5`），不是被包装工具的调用者。以下 `remind_list/cancel` 与 `watch_list/cancel` 的原结论不变；扩展自建另一套数据并非读取或取消 hub 原有记录（`messaging.rs:566-585`、`:639-657`）。

| 工具 | 缺什么（证据） | 值不值 | 风险 |
|---|---|---|---|
| `expertise_search`（`tools.rs:348` / `crates/orchestrator/src/expertise/tools.rs:163-222`） | REST 只能查：`GET /v1/expertise/search` 按 owner 读 store（`api_expertise.rs:240-260`），返回 `ListExpertiseResponse`；工具还会给每条记一次 `UsageSource::Pulled`，带上调用者的 run 与 task（`expertise/tools.rs:213`、`:217-222`），REST 没有写使用记录的入口。返回也少包装：工具有 `note`（`expertise/tools.rs:217-222`），REST 没有 | 低：核心只需要一个通用搜索 | 要新增「记录 pull」的路由，等于把「谁在哪个 run 用了哪条」公开成 API；owner 还得自己解析（person 默认，`api_expertise.rs:106-117`；project 靠 `/v1/projects` 的 paths 匹配 cwd，`api_projects.rs:18-38`） |
| `expertise_get`（`tools.rs:364` / `expertise/tools.rs:224-245`） | REST 的 `GET /v1/expertise/{id}` 按完整 id 直读 store，返回裸 `ExpertiseEntry`（`api_expertise.rs:154-159`）：**不做 owner 检查**（所以不会像工具那样对别的项目回 404）、**不支持唯一前缀 tag**（`own_entry` 的 tag 匹配在 `tools.rs:99-122`）、**不返回 `{entry, tag}` 包装**、**不记 usage**（`expertise/tools.rs:242-244`） | 低 | 同上；另外「按 tag 取」是 brief 里的约定，搬走会让 brief 的 tag 失效 |
| `usage_get`（`tools.rs:401` / `notes.rs:159-167`） | 现在按调用者的 run 过滤并补 `budget`/`minutes_elapsed`；`GET /v1/usage` 只有 `by` 分组（`api.rs:2548-2551`），没有 run 过滤 | 不值：核心的「这个 run 花了多少」是产品语义 | 自己按事件重算会读到全 hub 的事件 |
| `agent_get`（`tools.rs:111` / `messaging.rs:325-341`） | 输出含 `activity`（进程内运行状态）和 `current_tasks`（跨 run 汇总，`messaging.rs:231-244`）；`GET /v1/agents/{key}` 只回 `Agent`（`api.rs:625-630`） | 不值：列表/详情是核心导航 | 用 `GET /v1/runs` 逐个拼是 O(runs)，且 `activity` 永远缺 |
| `agent_list`（`tools.rs:105` / `messaging.rs:249-293`） | 同上，另加 archived sessions 与 claim（`busy_with`）；REST 没有等价物 | 不值 | 同上；拼出来的列表会和 GUI 的列表不一致 |
| `whoami`（`tools.rs:99` / `messaging.rs:191-228`） | 需要 profile/model/tools/角色与「我持有哪个 run 的哪个角色」；`caller` 只有 id/name/groups/cwd/run/task（`runner.rs:82-95`） | 不值：这是协议入口工具，模型第一句就会调 | 要让扩展自己再查 3-4 个接口，慢且容易和 `groups` 不一致 |
| `agent_send`（`tools.rs:117` / `messaging.rs:344`） | 缺「以调用者身份发」的接口：`POST /v1/agents/{key}/messages` 固定 `MessageSource::Human`（`api.rs:1189-1192`），也没有「发送者必须是这个 agent」的检查 | 不值：发消息是协作核心 | 搬过去会变成「人发的」，接收方的 `fromPerson` 判断会错（`Session.tsx:862`） |
| `inbox_read`（`tools.rs:407` / `notes.rs:171-174`） | `take_inbox` 是「取走」，绑定 agent 身份；没有 REST 等价物 | 低 | 身份接口一旦有了，这条也要重新判断归属 |
| `message_withdraw`（`tools.rs:135`） | 需要「以该 agent 身份撤回」；`DELETE /v1/agents/{key}/messages/{id}` 是人的路由，也没有发送者校验 | 不值 | 中：撤回别人的排队消息 |
| `watch_list`（`tools.rs:413` / `messaging.rs:639`） | `GET /v1/background`（`api.rs:1021`）返回**所有人**的提醒与关注，没有 owner 过滤；扩展只能客户端筛，仍会把别人的数据读进进程 | 不值 | 隐私面：扩展进程能看到全体 agent 的提醒内容 |
| `watch_cancel`（`tools.rs:419`） | 没有取消关注的路由（`/v1/background` 只提供读，`DELETE` 只对 reminder，`api.rs:152-153`）；也没有归属校验 | 不值 | 中：要么新建一条能删任何人关注的写路由，要么搬不过去 |
| `remind_list`（`tools.rs:163`） | 同 `watch_list` | 不值 | 同上 |
| `remind_cancel`（`tools.rs:169`） | `DELETE /v1/background/reminders/{id}` 不校验归属（`api.rs:1121-1127` 注释写明「for any owner」） | 不值 | 高：搬过去等于给扩展「删任何人的提醒」的能力 |
| `note_write`（`tools.rs:217` / `notes.rs:29`） | 只有 `GET /v1/notes`（`api.rs:151`），没有写路由；`text: null` 删除、作者归属都在 orchestrator 里 | 中：共享笔记是协作核心，扩展写反而绕开作者归属 | 高：作者会变成扩展还是调用者，没有接口能表达 |
| `lesson_search` / `lesson_get`（`tools.rs:318` / `tools.rs:333`） | `api.rs` 与 `api_projects.rs` 里没有任何 `lesson` 路由（全文 grep 为空） | 中：排障记录是跨 run 的核心资产 | 要为扩展开一组只读路由，等于把内部存储模型公开 |
| `lesson_create` / `lesson_merge` / `issue_keep` / `lesson_report` / `issue_pending`（`tools.rs:265/285/303/390/254`） | 同上，且写入还涉及 run 的 issue 笔记与候选合并 | 不值 | 高：写侧没有接口，搬过去要新建一整组 API |
| `expertise_pending`（`tools.rs:370`） | `GET /v1/expertise/candidates`（`api_expertise.rs:34`）有，但 owner 范围要按 caller 自己算（`candidates.rs:283-298`） | 低 | 中：算错 owner 会看到别人的候选 |
| `expertise_triage`（`tools.rs:376`） | 没有 triage 路由（只有 person 的 accept/reject，`api_expertise.rs:35-42`）；`suggested_by_lead` 与 reason 的写入口在 orchestrator | 低 | 中：要让扩展改候选的展示状态，需要新路由与权限点 |
| `task_list` / `task_get`（`tools.rs:480` / `tools.rs:491`） | 只读可拼：`GET /v1/runs/{key}` 回 `RunDetail { run, tasks }`（`api-types/src/lib.rs:1749-1752`）。但没有 `status`/`mine` 过滤，run 解析要自己做 | 不值：任务读和任务写是一体的，拆开只会让模型在两套名字间选 | 中：读搬走、写留在 hub，`task_get` 的 `report/checks/diff stat` 字段要逐一对齐 |
| `workspace_read` / `workspace_diff`（`tools.rs:544` / `tools.rs:534`） | `GET /v1/runs/{key}/tasks/{task}/files/content` 与 `.../diff` 存在（`api.rs:206,219-222`），但 task→worktree 的解析和路径校验在 orchestrator | 低 | 中：路径校验重写一遍容易出目录穿越 |
| `workspace_run` / `checks_run`（`tools.rs:554` / `tools.rs:582`） | 缺「在任务 worktree 里跑命令」的接口。扩展自己能 spawn，但 worktree 解析要自己做 | 不值：验收检查是任务状态机的一部分 | 高：绕过 hub 的 Job Object 树杀与超时（`docs/EXTENSIONS.md:106-127`），失败形态会变 |
| `agent_set_model` / `agent_interrupt` / `agent_retire`（`tools.rs:664/675/658`） | 路由存在（`api.rs:133`、`api.rs:138-141`、`api.rs:136`），组也能表达（见 §1.2），缺的是「目标归属」：read/write token 不绑定调用者，任何写 token 都能改任意队友（`access.rs:125-133`） | 低 | 高：manifest 的组只决定「谁能调」，不决定「能对谁做」 |

### 2.3 不应该搬

依据是 `docs/EXTENSIONS.md:14-21`：任务与合并状态机、驱动、权限、存储留在 hub；拆出去只会多出跨进程事务。
这一节只放**核心执行与存储本身**；「包装一层 API 但目前缺接口」的工具在 §2.2，不在这里。

| 工具 | 为什么留 |
|---|---|
| `plan_submit`（`tools.rs:455`）、`task_create`（`469`）、`task_update`（`497`）、`task_assign`（`509`）、`task_cancel`（`522`）、`task_answer`（`528`）、`task_accept`（`601`）、`task_verify`（`613`）、`task_rework`（`619`）、`task_drop`（`625`）、`branch_sync`（`633`） | 任务状态机与合并是同一组不变量；`task_accept` 还要保证「只合并验收过的提交」（`tools.rs:602`），拆到进程外就会出现「批准时状态已变」 |
| `review_request`（`588`）、`review_submit`（`745`） | 审查是 run 的角色与消息投递，`review_submit` 还会生成 expertise 候选（`tools.rs:1102-1117`） |
| `run_land`（`681`）、`run_finish`（`695`）、`run_next`（`440`）、`work_start`（`425`） | run 的落地/收尾/下一轮是状态机与策略（`docs/EXTENSIONS.md:14-17`） |
| `agent_spawn`（`639`） | 招募名额与可用模型由 run/会话的策略限制（工具说明 `tools.rs:640`），`ExtensionHost` 只给工具列表与调用（`host.rs:171-189`） |
| `remind_set`（`146`）、`watch`（`175`） | **原结论「一次调用无法完成定时器与事件监听」不足以证明扩展不能实现；新结论：仍不建议等价迁移。** service 能自行计时，hook 能响应匹配事件（`service.rs:241-281`、`hooks.rs:149-194`），但 hook 会丢事件且不补发；service 在停用、换版和重启间断。内置持久化调用者归属、去重、每人上限与任务解析（`messaging.rs:504-562`、`:589-635`），通知投递也要保持原语义；扩展 token 不能冒充 agent。E4 没提供这些等价保证。可单独做容忍丢失的事件响应型新包装，不能称为替代内置 watch/remind |
| `human_ask`（`190`） | 是否允许提问、等多久、超时用哪个默认值，都由 **run 的策略**决定（`crates/orchestrator/src/human.rs:2-3`、`:47-62`、`:99-103`）；这是 run 的决策，不是扩展能替代的一次调用。注意：这和「启用扩展必须由人批准」是两回事（`docs/EXTENSIONS.md:444-456` 讲的是后者） |
| `human_notify`（`206`） | 通知摘要的合并与投递在 runtime |
| `task_progress`（`710`）、`task_complete`（`716`）、`task_fail`（`733`）、`task_ask`（`739`） | worker 对任务状态机的写；`task_complete` 还触发提交、跑验收、记录 expertise 使用（`tools.rs:1041-1069`） |
| `extension_api`（`771`）、`extension_check`（`777`）、`extension_request_enable`（`783`）、`extension_logs`（`789`）、`extension_tools`（`795`）、`extension_call`（`801`） | 这六个就是扩展机制本身（`tools.rs:1124-1156`）；搬走会让机制自我指涉 |

## 3. GUI 面板与页面逐项

| 界面 | 位置 | 判断 | 依据 / 缺什么 | 值不值 | 风险 |
|---|---|---|---|---|---|
| 事件流 | `ToolColumn.tsx:411-440`（`useEventLog` `apps/gui/src/eventlog.ts:24-37`） | **现在就能搬有差异的试点**；要求页面级历史则补接口 | `ca.events.on` 只有实时回调（`host.ts:467-490`、`defaults.ts:18`），需在 activate 时订阅并使用扩展级缓存；内置日志在扩展异步加载前启动，缓存跨面板关闭重开（`main.tsx:24-26`、`eventlog.ts:8`、`:24-37`）。要保留页面级历史需只读缓存或回放接口 | 值：中，适合验证 E2 | 启动、停用再启用及热换版本产生历史缺口；失去 `safeTarget?.agent` 覆盖（`ToolColumn.tsx:108-112`、`:122`）。内置 events 仅开发版或 mock 显示（`panes.ts:33-37`），扩展注册不继承限制（`host.ts:391-408`），本试点接受发布版手动启用后可见；内置面板保留 |
| 改动 | `ToolColumn.tsx:154-213` | 补接口才能搬 | 数据接口齐（`allowed.cts:40-41`），但内置面板要 `safeTarget.agent`（`ToolColumn.tsx:108-112`）和 `target.turn`（`:154-161`）才能打开「故意看的那一轮」，扩展只收到 `follow`（`:80-83`，`host.ts:34`） | 低：核心的「这一轮改了什么」 | 若只做跟随当前会话的子集：能看最新一轮，失去轮次选择器、`target.file` 定位、以及「查看变更」打开的指定轮次；另外 `parseDiff`/`DiffView` 不可 import，重写大 diff 渲染容易卡 |
| 文件 | `ToolColumn.tsx:113-120`（`FileViewer.tsx:280`） | 补接口才能搬 | 同上：内置在 run 路由下用 `routeRun`（`ToolColumn.tsx:114-116`），否则用 `agent`；扩展两样都没有，只有 `follow` | 低：核心浏览 | 只做跟随会话的子集：失去 run scope（工作树/分支选择）、以及 `target.file` 定位；`FileViewer.tsx` 的路径校验、图片、高亮要重写 |
| 提交图 | `ToolColumn.tsx:123-130`（`commitgraph.tsx:33`） | 补接口才能搬 | 内置优先 `routeRun`（`ToolColumn.tsx:124-126`）；扩展没有 run，只有 `follow` | 低 | 只做会话子集：run 页上的提交图无法迁移；`graphlayout.ts` 的布局要重写，大仓库性能由扩展承担 |
| 后台 | `ToolColumn.tsx:260-317` | 补接口才能搬 | `GET /v1/background` 与 `DELETE …/reminders/{id}` 够用（`allowed.cts`），但面板需要 `routeRun ?? routeTask?.run`（`ToolColumn.tsx:131`），上下文没有；删除路由无归属检查（`api.rs:1121-1127`） | 不值：这是「hub 背后在跑什么」的安全面板，扩展坏了就看不见 | 中：扩展能看到所有人的提醒（同 `watch_list`） |
| 任务 | `ToolColumn.tsx:216-229` | 补接口才能搬 | `GET /v1/runs/{key}` 与 `/v1/agents/{key}/execution` 够用，但需要 `routeTask`/`target`；上下文没有（`host.ts:34`） | 不值：任务状态机 UI 的核心 | 中：上下文补 `run/task` 会扩大所有面板的可见范围 |
| Usage / Quota / Expertise 整页 | `App.tsx:238-243` | 补接口才能搬 | `ca.pages.register` 能注册整页并加侧栏一行（`Sidebar.tsx:114-115`），读接口也齐；但 owner/project 解析要自己算 | 不值：核心页面，改版频繁 | 中：页面一旦是扩展，主程序改版就不再统一 |
| 设置块 | `host.ts:97-99` | 补接口才能搬 | 注册的块只出现在「扩展」页（`extensions/page/Extensions.tsx:32`），`views/Settings.tsx` 里没有插槽 | 低 | 低 |
| 内置工具卡（`ActionCard`、任务卡等） | `Session.tsx:896-900` | 不该搬 | `toolCards.register` 只能给自己的工具（`host.ts:436-441`），这是有意的边界 | — | 放开就等于让扩展改写别人工具调用的展示 |
| 对话、Run 页、Home、工作区、设置页、侧栏、菜单与快捷键、终端、提问与通知、安全模式入口 | `views/Session.tsx`、`views/Run.tsx:646-667`、`Sidebar.tsx`、`menubar.tsx`、`App.tsx:173-207`、`terminal.tsx`、`questions.tsx`、`notices.ts`、`electron/main.cts` | 不该搬 | 这是宿主本身；终端还是 admin 级（`access.rs:81-83`、`allowed.cts:19-22`） | — | 扩展与主程序同页无沙箱（`docs/EXTENSIONS.md:402-406`），把这些交出去等于没有安全边界 |

## 4. 缺的接口（汇总）

E4 已提供事件响应和常驻进程（`manifest.rs:34-63`），不再把它们列为缺口；可靠回放、身份与归属仍未由 E4 提供（`hooks.rs:108-125`、`api.rs:242-249`）。容忍丢失的事件响应型包装可用 `[[on]]`，需要自行计时的可用 `[service]`；两者均须逐个验证语义，不能直接等同于内置通知。

hub 侧（按价值排序）：

1. **给扩展一个「我是谁」的只读视图**：`/v1/me` 只认会话 token（`api.rs:242-249`）。没有它，`whoami`/`agent_get`/`agent_list`/`agent_send`/`inbox_read` 都拼不全（缺 `activity`、`current_tasks`、`tools`，也没有发送者身份）。
2. **`/v1/usage` 的 run 过滤**（`api.rs:2548-2551`）：否则 `usage_get` 无法等价。
3. **按 owner 过滤的 background 视图**（`api.rs:1021`）：`watch_list`/`remind_list` 现在会读到全体。
4. **带归属检查的提醒删除与取消关注**（`api.rs:1121-1127` 的删除不校验归属；关注根本没有删除路由，`api.rs:152-153`）。
5. **notes 的写路由与 key/前缀过滤**（`api.rs:151`）；读侧还要能按调用方限定 run（`api.rs:1000-1016`）。
6. **lessons 的一组只读路由**：`api.rs` 里一个都没有。
7. **expertise 的两条**：记录 `Pulled` 使用（`expertise/tools.rs:213`）、按 tag/作用域取条目并返回 `{entry, tag}`（`expertise/tools.rs:99-122`、`:242-244`）。
8. **任务读路由的过滤参数**（`status`/`mine`）：`GET /v1/runs/{key}` 只给全量 `RunDetail`。
9. **expertise triage 的写入口**（`api_expertise.rs:34-42` 只有 accept/reject）。

GUI 侧：

10. **面板上下文补 `run`/`task`/`target`（含 `turn`、`file`）**（`host.ts:34` 只有 `follow`；内置面板在 `ToolColumn.tsx:107-133`）。
11. **设置页的插槽**（`views/Settings.tsx` 没有 `useExtensions`；块只落在 `Extensions.tsx:32`）。
12. **导航/坞位 API**：扩展不能打开别的页、不能选坞、不能把自己提到前面（`panes.ts:162-175` 由宿主决定）。
13. **共享组件与数据层的只读出口**：`DiffView`、`Avatar`、`useSession`、`useHubData`、`clock/full`、`lib/i18n`（`docs/EXTENSIONS.md:365` 明确 import 不到）。

## 5. 风险（跨条目）

- **工具名会变**：扩展工具叫 `<扩展名>__<工具名>`（`tools.rs:916`、`docs/EXTENSIONS.md:139-151`）。而 hub 的说明直接点名内置工具（`crates/orchestrator/src/protocol.rs:11`、`:104`、`crates/orchestrator/src/brief.rs:266`）。`[[prompts]]` 只能追加、不能替换（`docs/EXTENSIONS.md:305-315`），所以旧名字会留在说明里，模型可能照着调一个不存在的工具。搬任何被说明点名的工具（`task_complete`、`agent_send`、`human_ask`）都会踩这个。
- **REST token 不绑定归属**：扩展的 read/write token 是通用的（`access.rs:125-133`），路由本身多半不校验「这个调用者能不能动这个目标」（notes 的 `run` 参数 `api.rs:1000-1016`、提醒删除 `api.rs:1121-1127`）。组名能表达「谁能调」，表达不了「能对谁做」。
- **可用性绑到扩展状态**：扩展被停用会杀掉运行中的调用（`docs/EXTENSIONS.md:115-121`），内置工具不会。把常用工具搬走等于让它依赖扩展是否启用、是否加载失败、是否安全模式。
- **审批摩擦**：改一次扩展要人批准一次（`docs/EXTENSIONS.md:402-423`），内置工具改一次发版即可。对核心工具这是负收益。
- **GUI 扩展拿的是人的 admin token**：主进程从 `~/.crossagents/token` 读总 token（`apps/gui/electron/main.cts:70-77`），`hub:request` 用它发请求（`main.cts:141-154`），hub 把总 token 当 `Level::Admin`（`api.rs:372-373`）。`allowed.cts` 白名单里的 admin 路由（终端 `19-22`、plugin 安装/升级 `52`）因此对扩展可用。范围限定在白名单内——白名单外的 admin 路由（`/v1/shutdown`、账户创建/登录）仍然到不了（`main.cts:145-150` 的 `allowed()` 检查）。
- **全量读接口**：`/v1/background`、`/v1/agents` 是全量；`/v1/notes` 虽然按 scope 与 run 过滤（`api.rs:1000-1016`），但不校验调用方，扩展可以自己指定 run。

## 6. 推荐顺序与第一个试点

1. 事件流试点进行中，差异与验收见 `extensions/debug/README.md`。决定：手动安装启用、默认没有、发布版启用后可见、内置面板保留、不补宿主能力。
2. `note_list` + `note_read`：接受 §2.1 的读取面后评估。
3. `gui_screenshot`：先确定大图交付方式与运行环境（§2.1）。
4. 事件响应型新包装：选择容忍漏事件的非核心用途验证 E4；需要定时的可评估 service。它不是 `remind_*`/`watch` 的等价替代（§1.2、§2.3）。
5. 改动、文件、提交图等面板等 §4 上下文缺口解决；其余核心工具按 §4 身份、归属等缺口逐项重评。

## 7. 推测与未验证

- 「`gui_screenshot` 搬到扩展后，同参数能得到一致的 PNG」——**已撤回**。1 MiB stdout 上限（`runner.rs:46`）对 base64 的影响没有实测；大图会不会超限、超限后改用 path 交付是否可接受，都没有验证。
- 「扩展工具一次调用一个进程，因此 `gui_screenshot` 的耗时和现在一致」——推测。Electron/Vite 启动占大头，多一层进程应当可忽略，但没测。
- 「项目路径能唯一匹配 `caller.cwd`」——推测。`/v1/projects` 只给 paths（`api_projects.rs:26-36`），worktree 与多 checkout 场景下的匹配规则没有读到实现。
- 「扩展面板重写 diff/文件查看器的成本」——按 `FileViewer.tsx`（1127 行）与 `commitgraph.tsx` 的体量估计，没有实际移植验证。
- 「搬走 `note_read` 后错误文案与 `NotFound` 语义一致」——推测。REST 的 404 与 `CoreError::NotFound` 的映射在 `api.rs` 的错误转换里，未逐条比对。
- 各 CLI 对扩展工具名长度/前缀的处理只按 `docs/EXTENSIONS.md:145-151` 的规则推断，没有新测。
- **已按代码核对**：E4 的 hook 是尽力而为、会丢事件、不重试不回放；service 可常驻并监督重启，但停用、换版和 hub 退出会中断（`hooks.rs:1-16`、`:108-125`、`service.rs:1-9`）。内置提醒与关注按调用者持久化并检查归属（`messaging.rs:504-562`、`:566-585`、`:589-657`）。这是静态代码核对，不是本次端到端实测。
- **未验证**：具体事件响应型包装能否容忍漏事件、补偿和去重是否满足业务，以及 service 自建定时器在重启后的恢复语义；不能据 E4 存在就推断等价替代 `remind_*`/`watch`。
