# 内置功能搬进扩展：逐项评估

> 只做评估，不改代码。证据截至 `0cec783`（`cleanup: 放行内嵌字体…`）。
> 范围：hub 的工具表 `crates/orchestrator/src/tools.rs:97` 起的 67 个内置工具，以及 GUI 的工具列面板
> （`apps/gui/src/ToolColumn.tsx`）和其他页面上可以指认的内置界面。
>
> 这份评估回答的是 [EXTENSIONS.md](EXTENSIONS.md) §1 那句「现有内置功能不搬。等接口用熟了，再逐个判断有没有搬的价值」
> ——把「逐个判断」做出来。结论先说：**只有 5 个 hub 工具（3 组）和 4 个 GUI 面板能在不改接口的前提下搬，其中值得搬的只有 `gui_screenshot` 和事件流面板。**

## 0. 结论

**现在就能搬（接口已够，等价或差异可接受）**

| 项 | 位置 | 一句话 |
|---|---|---|
| `gui_screenshot` | `crates/orchestrator/src/tools.rs:564`，`crates/orchestrator/src/screenshot.rs:78` | 只依赖 `caller.cwd` 和一个 node 脚本，零新接口 |
| `note_list` / `note_read` | `tools.rs:243` / `tools.rs:232`，`crates/orchestrator/src/notes.rs:105` | `GET /v1/notes` 返回全文，自己按 key/前缀过滤即可 |
| `expertise_search` / `expertise_get` | `tools.rs:348` / `tools.rs:364` | `/v1/expertise/search` 与 `/v1/expertise/{id}` 够用，两处 owner 要自己合并 |
| 事件流面板 | `apps/gui/src/ToolColumn.tsx:411` | `ca.events.on` 能拿到同一条流，零 hub 调用 |
| 改动面板 | `ToolColumn.tsx:154` | transcript/turn diff 都在 `allowed.cts:40-41`，但要重写 diff 渲染 |
| 文件面板 | `ToolColumn.tsx:113-120` | files tree/content/resolve/image 都在 `allowed.cts:96-99`，但要重写查看器 |
| 提交图面板 | `ToolColumn.tsx:123-130` | graph + commit diff 在 `allowed.cts:101`，布局代码要重写 |

**补接口才能搬**：`usage_get`、`agent_get`、`agent_list`、`whoami`、`watch_list`、`watch_cancel`、`remind_list`、
`remind_cancel`、`message_withdraw`、`note_write`、全部 `lesson_*` 与 `issue_*`、`expertise_pending`、
`expertise_triage`、`task_list`、`task_get`、`workspace_read/diff/run`、`checks_run`、
`agent_set_model/interrupt/retire`，以及后台面板、任务面板、扩展设置块、接管内置工具卡。

**不应该搬**（§1 不变量：事件日志、任务与合并状态机、驱动、权限、存储）：
`plan_submit`、`task_create/update/assign/cancel/answer/accept/verify/rework/drop/progress/complete/fail/ask`、
`review_request`、`review_submit`、`branch_sync`、`run_land`、`run_finish`、`run_next`、`work_start`、
`agent_send`、`agent_spawn`、`inbox_read`、`remind_set`、`watch`、`human_ask`、`human_notify`、
`extension_*` 六个机制工具；GUI 侧的对话、Run 页、设置、工作区、侧栏、菜单与快捷键、终端、提问与通知、安全模式入口。

**第一个试点：把 `gui_screenshot` 搬成扩展工具。**（理由与验收见 §6）

## 1. 判定口径

### 1.1 什么算「能搬」

三条同时成立才算「现在就能搬」：

1. **接口够**：工具只用扩展工具进程拿得到的东西——manifest 里能声明的字段
   （`crates/hub/src/extensions/manifest.rs:11-20`）、stdin 里的 `caller`
   （`crates/hub/src/extensions/runner.rs:498`）、扩展 token 能过的 REST
   （`crates/hub/src/access.rs:56`）、以及本机进程和文件系统。GUI 只用 `ca`
   （`apps/gui/src/extensions/host.ts:76-110`）与 `ca.hub.request` 能过的
   `apps/gui/electron/allowed.cts`。
2. **等价**：输出与现在一致，或差异明确可接受。只是「拼得出来但少一半字段」算补接口。
3. **不碰 §1 的不变量**（`docs/EXTENSIONS.md:14-21`）。

### 1.2 扩展工具现在实际拿得到什么

- **身份不是调用者**：`caller` 只给 `kind/id/name/groups/cwd/run/task`
  （`runner.rs:82-95`）。扩展 token 是 `Grant::Extension`，级别只能是 read 或 write，
  到不了 `/v1/me*`（`crates/hub/src/api.rs:242-249`），也调不了「以人的身份调工具」的路由
  （`access.rs:118-124`）。
- **没有事件驱动、没有常驻进程**：manifest 只有 `tools` / `prompts` / `gui`
  （`manifest.rs:35-47`），`[[on]]` 与 `[service]` 属于 E4，未实现（`docs/EXTENSIONS.md:436`）。
- **一次调用一个进程**，超时、stdout 上限、并发上限见 `docs/EXTENSIONS.md:106-127`；
  输出走 `images` 约定（`crates/cli/src/cmd/mcp.rs:79-95`）。
- **权限只有组名字符串**（`manifest.rs:17`，检查在 `runner.rs:406`、`runner.rs:414`），
  内置的 `PLANNER/RUN/WORKER/REVIEWER` 组合（`tools.rs:15-22`）要拆成多份 manifest 才能表达。

### 1.3 GUI 扩展现在实际拿得到什么

- 面板上下文只有 `{ follow, extension }`（`host.ts:34`）：run 路由没有 `follow`
  （`apps/gui/src/App.tsx:211-225` 只把 `task`/`run` 交给内置面板），所以面板拿不到 run、task、
  打开它的轮次或文件。
- 只能给自己的工具注册卡片（`host.ts:436-441`），接管不了内置工具卡（`Session.tsx:896-900`）。
- `ca.hub.request` 就是主程序自己的请求（`host.ts:464-466` → `apps/gui/src/api.ts:194` →
  `allowed.cts`），用的是人的 token。`allowed.cts` 里有 admin 级路由（终端 `allowed.cts:19-22`、
  plugin 安装 `allowed.cts:52`），所以这不是「只读面板」的能力边界（见 §5）。
- import 不到 `src/` 的任何模块（`docs/EXTENSIONS.md:322`），`DiffView`、`Avatar`、`useSession`、
  `useHubData`、时间格式、i18n 都得自己重写。

## 2. hub 工具逐项

### 2.1 现在就能搬

| 工具（定义 / 实现） | 为什么够 | 值与代价 | 风险 |
|---|---|---|---|
| `gui_screenshot`（`tools.rs:564` / `screenshot.rs:78-128`） | 全部逻辑是「在 `caller.cwd` 跑 `node scripts/…`，把 PNG 放进 `images`」。`caller.cwd` 在 stdin 里（`runner.rs:90`），`images` 是既有约定（`mcp.rs:79`）。`groups: ALL`（`tools.rs:566`）→ manifest 写 `["everyone"]` 即可 | 值：中。它是一次性的开发辅助，放在核心里每次发版都要带着 node/Electron 相关路径。代价：默认不装扩展就没有这个工具，需要文档说明 | 低。失败只影响一次调用；不碰任何 hub 状态。唯一变化是超时：内置等 180 秒（`screenshot.rs:18`），扩展工具默认 60 秒（`docs/EXTENSIONS.md:110`），manifest 要写 `timeout_secs = 180` |
| `note_list`（`tools.rs:243` / `notes.rs:131-156`） | `GET /v1/notes`（`api.rs:151`）返回 `Note` 全文（`crates/core/src/note.rs:23-32`）。scope=run 用 `caller.run`，scope=team 不带 run。前缀过滤在扩展里做 | 值：低-中。省不下多少核心代码，只是把「读共享笔记」挪到可替换的位置 | 低。多一次全量读取；team 作用域本来就是全局的（`notes.rs:17-27`），没有额外泄露 |
| `note_read`（`tools.rs:232` / `notes.rs:105-129`） | 同上，列出后按 key 精确匹配；`notes.rs:113-124` 的「先 run 后 team」顺序可以在扩展里照做 | 值：低。和 `note_list` 同一份实现，适合一起搬 | 低。找不到时的 `NotFound` 要自己造，错误文案会变 |
| `expertise_search`（`tools.rs:348` / `crates/orchestrator/src/expertise/tools.rs:163`） | `GET /v1/expertise/search`（`api_expertise.rs:30`）按 owner 查。扩展要查两次：person（默认 owner，`api_expertise.rs:106-117`）和 project（`GET /v1/projects` 的 paths 匹配 `caller.cwd`，`api_projects.rs:18-38`），再合并 | 值：低。核心只需要一个通用搜索，扩展版反而要自己拼作用域 | 中：owner 解析靠 cwd 字符串匹配项目路径，多 worktree 时可能选错（推测，未验证项目匹配规则）；`kind` 过滤与 `limit` 的合并语义要和现在一致 |
| `expertise_get`（`tools.rs:364`） | `GET /v1/expertise/{id}`（`api_expertise.rs:43-46`） | 值：低 | 低。要处理「不在自己的作用域里」的 404 |

### 2.2 补接口才能搬

| 工具 | 缺什么（证据） | 值不值 | 风险 |
|---|---|---|---|
| `usage_get`（`tools.rs:401` / `notes.rs:159-167`） | 现在按调用者的 run 过滤并补 `budget`/`minutes_elapsed`；`GET /v1/usage` 只有 `by` 分组（`api.rs:2548-2551`），没有 run 过滤 | 不值：核心的「这个 run 花了多少」是产品语义 | 自己按 `/v1/events` 重算会读到全 hub 的事件；GUI 侧 `/v1/events` 还不在 `allowed.cts` 里 |
| `agent_get`（`tools.rs:111` / `messaging.rs:325-341`） | 输出含 `activity`（进程内运行状态）和 `current_tasks`（跨 run 汇总，`messaging.rs:231-244`）；`GET /v1/agents/{key}` 只回 `Agent`（`api.rs:625-630`） | 不值：列表/详情是核心导航 | 用 `GET /v1/runs` 逐个拼是 O(runs)，且 `activity` 永远缺 |
| `agent_list`（`tools.rs:105` / `messaging.rs:249-293`） | 同上，另加 archived sessions 与 claim（`busy_with`）；REST 没有等价物 | 不值 | 同上；拼出来的列表会和 GUI 的列表不一致 |
| `whoami`（`tools.rs:99` / `messaging.rs:191-228`） | 需要 profile/model/tools/角色与「我持有哪个 run 的哪个角色」；`caller` 只有 id/name/groups/cwd/run/task（`runner.rs:82-95`） | 不值：这是协议入口工具，模型第一句就会调 | 要让扩展自己再查 3-4 个接口，慢且容易和 `groups` 不一致 |
| `watch_list`（`tools.rs:413` / `messaging.rs:639`） | `GET /v1/background`（`api.rs:1021`）返回**所有人**的提醒与关注，没有 owner 过滤；扩展只能客户端筛，仍会把别人的数据读进进程 | 不值 | 隐私面：扩展进程能看到全体 agent 的提醒内容 |
| `watch_cancel`（`tools.rs:419`） | 没有取消关注的路由（`/v1/background` 只提供读，提醒的 `DELETE` 只对 reminder，`api.rs:152-153`）；也没有归属校验 | 不值 | 中：要么新建一条能删任何人关注的写路由，要么搬不过去 |
| `remind_list`（`tools.rs:163`） | 同上 | 不值 | 同上 |
| `remind_cancel`（`tools.rs:169`） | `DELETE /v1/background/reminders/{id}` 不校验归属（`api.rs:1121-1127` 注释写明「for any owner」） | 不值 | 高：搬过去等于给扩展「删任何人的提醒」的能力 |
| `message_withdraw`（`tools.rs:135`） | 需要「以该 agent 身份撤回」；`DELETE /v1/agents/{key}/messages/{id}` 是人的路由，也没有发送者校验 | 不值 | 中：撤回别人的排队消息 |
| `note_write`（`tools.rs:217` / `notes.rs:29`） | 只有 `GET /v1/notes`（`api.rs:151`），没有写路由；`text: null` 删除、作者归属都在 orchestrator 里 | 中：共享笔记是协作核心，扩展写反而绕开作者归属 | 高：作者会变成扩展还是调用者，没有接口能表达 |
| `lesson_search` / `lesson_get`（`tools.rs:318` / `tools.rs:333`） | `api.rs` 与 `api_projects.rs` 里没有任何 `lesson` 路由（全文 grep 为空） | 中：排障记录是跨 run 的核心资产 | 要为扩展开一组只读路由，等于把内部存储模型公开 |
| `lesson_create` / `lesson_merge` / `issue_keep` / `lesson_report` / `issue_pending`（`tools.rs:265/285/303/390/254`） | 同上，且写入还涉及 run 的 issue 笔记与候选合并 | 不值 | 高：写侧没有接口，搬过去要新建一整组 API |
| `expertise_pending`（`tools.rs:370`） | `GET /v1/expertise/candidates`（`api_expertise.rs:34`）有，但 owner 范围要按 caller 自己算（`candidates.rs:283-298`） | 低 | 中：算错 owner 会看到别人的候选 |
| `expertise_triage`（`tools.rs:376`） | 没有 triage 路由（只有 person 的 accept/reject，`api_expertise.rs:35-42`）；`suggested_by_lead` 与 reason 的写入口在 orchestrator | 低 | 中：要让扩展改候选的展示状态，需要新路由与权限点 |
| `task_list` / `task_get`（`tools.rs:480` / `tools.rs:491`） | 只读可拼：`GET /v1/runs/{key}` 回 `RunDetail { run, tasks }`（`api-types/src/lib.rs:1749-1752`）。但没有 `status`/`mine` 过滤，run 解析要自己做 | 不值：任务读和任务写是一体的，拆开只会让模型在两套名字间选 | 中：读搬走、写留在 hub，`task_get` 的 `report/checks/diff stat` 字段要逐一对齐 |
| `workspace_read` / `workspace_diff`（`tools.rs:544` / `tools.rs:534`） | `GET /v1/runs/{key}/tasks/{task}/files/content` 与 `.../diff` 存在（`api.rs:206,219-222`），但 task→worktree 的解析和路径校验在 orchestrator | 低 | 中：路径校验重写一遍容易出目录穿越 |
| `workspace_run` / `checks_run`（`tools.rs:554` / `tools.rs:582`） | 缺「在任务 worktree 里跑命令」的接口。扩展自己能 spawn，但 worktree 解析要自己做 | 不值：验收检查是任务状态机的一部分 | 高：绕过 hub 的 Job Object 树杀与超时（`docs/EXTENSIONS.md:106-127`），失败形态会变 |
| `agent_set_model` / `agent_interrupt` / `agent_retire`（`tools.rs:664/675/658`） | 路由存在（`api.rs:133`、`api.rs:138-141`、`api.rs:136`），但缺「按调用者权限拒绝」的策略点；现在靠 `PLANNER` 组（`tools.rs:664-679`），扩展只有粗粒度 groups | 低 | 高：manifest 的 `groups` 是静态字符串，写 `["lead"]` 就等于所有 lead 都能改任意队友的模型 |

### 2.3 不应该搬

依据是 `docs/EXTENSIONS.md:14-21`：任务与合并状态机、驱动、权限、存储留在 hub；拆出去只会多出跨进程事务。

| 工具 | 为什么留 |
|---|---|
| `plan_submit`（`tools.rs:455`）、`task_create`（`469`）、`task_update`（`497`）、`task_assign`（`509`）、`task_cancel`（`522`）、`task_answer`（`528`）、`task_accept`（`601`）、`task_verify`（`613`）、`task_rework`（`619`）、`task_drop`（`625`）、`branch_sync`（`633`） | 任务状态机与合并是同一组不变量；`task_accept` 还要保证「只合并验收过的提交」（`tools.rs:602`），拆到进程外就会出现「批准时状态已变」 |
| `review_request`（`588`）、`review_submit`（`745`） | 审查是 run 的角色与消息投递，`review_submit` 还会生成 expertise 候选（`tools.rs:1102-1117`） |
| `run_land`（`681`）、`run_finish`（`695`）、`run_next`（`440`）、`work_start`（`425`） | run 的落地/收尾/下一轮是状态机与策略（`docs/EXTENSIONS.md:14-17`） |
| `agent_send`（`117`） | 消息源必须是该 agent。`POST /v1/agents/{key}/messages` 固定 `MessageSource::Human`（`api.rs:1189-1192`），搬过去会变成「人发的」，接收方的 `fromPerson` 判断会错（`Session.tsx:862`） |
| `agent_spawn`（`639`） | 招募名额与可用模型由 run/会话的策略限制（工具说明 `tools.rs:640`），`ExtensionHost` 只给工具列表与调用（`host.rs:171-189`），拿不到策略 |
| `inbox_read`（`407` / `notes.rs:171-174`） | `take_inbox` 是「取走」，绑定 agent 身份；没有 REST 等价物 |
| `remind_set`（`146`）、`watch`（`175`） | 定时器与事件监听在 hub 进程内跑；扩展没有常驻进程（`manifest.rs:35-47`，E4 未做） |
| `human_ask`（`190`） | run 策略可能直接拒绝或超时取默认答案（`crates/orchestrator/src/human.rs`），和「必须有人点一次」的批准模型冲突（`docs/EXTENSIONS.md:400-412`） |
| `human_notify`（`206`） | 通知摘要的合并与投递在 runtime |
| `task_progress`（`710`）、`task_complete`（`716`）、`task_fail`（`733`）、`task_ask`（`739`） | worker 对任务状态机的写；`task_complete` 还触发提交、跑验收、记录 expertise 使用（`tools.rs:1041-1069`） |
| `extension_api`（`771`）、`extension_check`（`777`）、`extension_request_enable`（`783`）、`extension_logs`（`789`）、`extension_tools`（`795`）、`extension_call`（`801`） | 这六个就是扩展机制本身（`tools.rs:1124-1156`）；搬走会让机制自我指涉 |

## 3. GUI 面板与页面逐项

| 界面 | 位置 | 判断 | 依据 / 缺什么 | 值不值 | 风险 |
|---|---|---|---|---|---|
| 事件流 | `ToolColumn.tsx:411-440`（`useEventLog` `apps/gui/src/eventlog.ts:24-37`） | **现在就能搬** | `ca.events.on`（`host.ts:467-490`）拿的是同一条流；调试页只在 `__BUILD__.dev`/`__MOCK__` 出现（`panes.ts:33`） | 值：中。它是最不像核心的面板，适合验证 E2 | 低。差异：只能看扩展加载之后的事件（和现在 `eventlog.ts:24` 的「页面打开以来」一样）；扩展拿不到 `/v1/events` 历史（`allowed.cts` 无该路由） |
| 改动 | `ToolColumn.tsx:154-213` | **现在就能搬**，但要重写渲染 | `GET /v1/agents/{key}/transcript` 与 `turns/{id}/diff` 都在 `allowed.cts:40-41`；`follow` 足够定位会话 | 值：低。核心的「这一轮改了什么」 | 中：`parseDiff`/`DiffView`（`diffview.tsx`）不可 import，重写一遍大 diff 的渲染容易卡；轮次选择器的状态也要重建 |
| 文件 | `ToolColumn.tsx:113-120`（`FileViewer.tsx:280`） | **现在就能搬**，但要重写查看器 | files tree/content/resolve/image 在 `allowed.cts:96-99`；scope 是 agent 或 run | 值：低。核心浏览 | 中：`FileViewer.tsx` 自带路径校验、图片与高亮，重写成本高；run scope 需要上下文里的 run |
| 提交图 | `ToolColumn.tsx:123-130`（`commitgraph.tsx:33`） | **现在就能搬**，但要重写布局 | `GET /v1/{runs,agents}/{key}/graph` 与 commit diff 在 `allowed.cts:101` | 值：低 | 中：`graphlayout.ts` 的布局算法要重写；大数据量下的性能由扩展承担 |
| 后台 | `ToolColumn.tsx:260-317` | 补接口才能搬 | `GET /v1/background` 与 `DELETE …/reminders/{id}` 够用（`allowed.cts`），但面板需要 run 作用域，而上下文只有 `follow`（`host.ts:34`；`App.tsx:211-225`）；删除路由无归属检查（`api.rs:1121-1127`） | 不值：这是「hub 背后在跑什么」的安全面板，扩展坏了就看不见 | 中：扩展能看到所有人的提醒（同 `watch_list`） |
| 任务 | `ToolColumn.tsx:216-229` | 补接口才能搬 | `GET /v1/runs/{key}` 与 `/v1/agents/{key}/execution` 够用，但需要 `routeTask`/`target`；上下文没有（`host.ts:34`） | 不值：任务状态机 UI 的核心 | 中：上下文补 `run/task` 会扩大所有面板的可见范围 |
| Usage / Quota / Expertise 整页 | `App.tsx:238-243` | 补接口才能搬 | `ca.pages.register` 能注册整页并加侧栏一行（`Sidebar.tsx:114-115`），读接口也齐；但 owner/project 解析要自己算 | 不值：核心页面，改版频繁 | 中：页面一旦是扩展，主程序改版就不再统一 |
| 设置块 | `host.ts:97-99` | 补接口才能搬 | 注册的块只出现在「扩展」页（`extensions/page/Extensions.tsx:32`），`views/Settings.tsx` 里没有插槽 | 低 | 低 |
| 内置工具卡（`ActionCard`、任务卡等） | `Session.tsx:896-900` | 不该搬 | `toolCards.register` 只能给自己的工具（`host.ts:436-441`），这是有意的边界 | — | 放开就等于让扩展改写别人工具调用的展示 |
| 对话、Run 页、Home、工作区、设置页、侧栏、菜单与快捷键、终端、提问与通知、安全模式入口 | `views/Session.tsx`、`views/Run.tsx:646-667`、`Sidebar.tsx`、`menubar.tsx`、`App.tsx:173-207`、`terminal.tsx`、`questions.tsx`、`notices.ts`、`electron/main.cts` | 不该搬 | 这是宿主本身；终端还是 admin 级（`access.rs:81-83`、`allowed.cts:19-22`） | — | 扩展与主程序同页无沙箱（`docs/EXTENSIONS.md:361-363`），把这些交出去等于没有安全边界 |

## 4. 缺的接口（汇总）

hub 侧（按价值排序）：

1. **给扩展一个「我是谁」的只读视图**：`/v1/me` 只认会话 token（`api.rs:242-249`）。没有它，`whoami`/`agent_get`/`agent_list` 都拼不全（缺 `activity`、`current_tasks`、`tools`）。
2. **`/v1/usage` 的 run 过滤**（`api.rs:2548-2551`）：否则 `usage_get` 无法等价。
3. **按 owner 过滤的 background 视图**（`api.rs:1021`）：`watch_list`/`remind_list` 现在会读到全体。
4. **带归属检查的提醒删除与取消关注**（`api.rs:1121-1127` 的删除不校验归属；关注根本没有删除路由，`api.rs:152-153`）。
5. **notes 的写路由与 key/前缀过滤**（`api.rs:151`）：`note_write` 完全没有入口。
6. **lessons 的一组只读路由**：`api.rs` 里一个都没有。
7. **任务读路由的过滤参数**（`status`/`mine`）：`GET /v1/runs/{key}` 只给全量 `RunDetail`。
8. **expertise triage 的写入口**（`api_expertise.rs:34-42` 只有 accept/reject）。

GUI 侧：

9. **面板上下文补 `run`/`task`/`target`**（`host.ts:34` 只有 `follow`）。
10. **设置页的插槽**（`views/Settings.tsx` 没有 `useExtensions`；块只落在 `Extensions.tsx:32`）。
11. **导航/坞位 API**：扩展不能打开别的页、不能选坞、不能把自己提到前面（`panes.ts:162-175` 由宿主决定）。
12. **共享组件与数据层的只读出口**：`DiffView`、`Avatar`、`useSession`、`useHubData`、`clock/full`、`lib/i18n`（`docs/EXTENSIONS.md:322` 明确 import 不到）。

## 5. 风险（跨条目）

- **工具名会变**：扩展工具叫 `<扩展名>__<工具名>`（`tools.rs:916`、`docs/EXTENSIONS.md:139-151`）。而 hub 的说明直接点名内置工具（`crates/orchestrator/src/protocol.rs:11`、`:104`、`crates/orchestrator/src/brief.rs:266`）。`[[prompts]]` 只能追加、不能替换（`docs/EXTENSIONS.md:262-270`），所以旧名字会留在说明里，模型可能照着调一个不存在的工具。搬任何被说明点名的工具（`task_complete`、`agent_send`、`human_ask`）都会踩这个。
- **权限粒度变粗**：内置按 `Group` 组合（`tools.rs:15-22`），扩展只有 manifest 里的组名字符串（`manifest.rs:17`）。`PLANNER` 这类「只在某个 run 角色下成立」的限制，扩展表达不了。
- **token 不是身份**：`Grant::Extension` 永远不是 agent（`access.rs:30-37`、`118-134`）。任何「以调用者身份做事」的工具（发消息、写任务、改模型）搬出去都会变成另一个人做的，或者干脆做不到。
- **可用性绑到扩展状态**：扩展被停用会杀掉运行中的调用（`docs/EXTENSIONS.md:115-121`），内置工具不会。把常用工具搬走等于让它依赖扩展是否启用、是否加载失败、是否安全模式。
- **审批摩擦**：改一次扩展要人批准一次（`docs/EXTENSIONS.md:367-379`），内置工具改一次发版即可。对核心工具这是负收益。
- **GUI 没有沙箱，且 `ca.hub.request` 用的是人的 token**：`allowed.cts` 里有 admin 路由（终端 `19-22`、plugin 安装/升级 `52`）。一个面板扩展能开终端、能装插件。这是既有设计（`docs/EXTENSIONS.md:361-363` 已承认「不保证隔离」），但搬面板会让更多代码进入这个面。
- **只读面板也会读到全量数据**：`/v1/background`、`/v1/notes`、`/v1/agents` 都是全量接口，扩展进程里会拿到不属于调用者的数据。

## 6. 推荐顺序与第一个试点

**顺序**（每步都以上一步的验收为前提）：

1. `gui_screenshot` → 扩展工具（零新接口）。
2. 事件流 → 扩展标签页（零新接口）。
3. `note_list` + `note_read` → 扩展工具（零新接口，验证 read token 的 REST 组合与 run scope）。
4. 改动面板 → 扩展标签页（零新接口，但渲染重写，验证 `follow` 上下文是否真的够）。
5. 其余暂缓：等 §4 的 1–4 号接口出现后，再单独评估 `usage_get`、`agent_get/list`、后台面板。

**第一个试点：`gui_screenshot` 搬成 `devtools__gui_screenshot`。**

- 为什么是它：只依赖 `caller.cwd`（`runner.rs:90`）和本机 node，结果走既有 `images` 约定（`mcp.rs:79-95`），不碰任何 §1 不变量；失败只影响一次调用（`docs/EXTENSIONS.md:106-127`）。
- 一次能验证四件事：manifest 的 `groups`（写 `["everyone"]`）、`timeout_secs`（要写 180，内置就是 180 秒，`screenshot.rs:18`）、`images` 输出、`caller.cwd`。
- 验收：同样的 `scenario`/`route`/`width`/`height`/`theme`/`wait_ms` 得到与现在一致的 PNG；没有 `apps/gui/node_modules` 时的错误文案与 `screenshot.rs:92-97` 一致；在 lead、worker、conversation 三种会话里都能调用。
- 代价与遗留：默认不装扩展就没有这个工具。建议试点期间两者并存（内置保留、扩展并行），对比一两个真实任务后再决定删不删内置——这一步是推测，没有实测数据。

## 7. 推测与未验证

- 「扩展工具一次调用一个进程，因此 `gui_screenshot` 的耗时和现在一致」——推测。Electron 启动时间占大头，多一层进程应当可忽略，但没测。
- 「项目路径能唯一匹配 `caller.cwd`」——推测。`/v1/projects` 只给 paths（`api_projects.rs:26-36`），worktree 与多 checkout 场景下的匹配规则没有读到实现。
- 「扩展面板重写 diff/文件查看器的成本」——按 `FileViewer.tsx`（1127 行）与 `commitgraph.tsx` 的体量估计，没有实际移植验证。
- 「搬走 `note_read` 后错误文案与 `NotFound` 语义一致」——推测。REST 的 404 与 `CoreError::NotFound` 的映射在 `api.rs` 的错误转换里，未逐条比对。
- 各 CLI 对扩展工具名长度/前缀的处理只按 `docs/EXTENSIONS.md:145-151` 的规则推断，没有新测。
