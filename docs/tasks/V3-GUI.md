# V3 任务书：以对话为中心的 GUI 改版

> 依据：[DESIGN.md](../DESIGN.md) §11.1 · 前序：[V2 任务书](V2-GUI.md)（外壳与安全设定不变） · 状态：第一阶段完成 · 2026-10-21

## 1. 范围

V2 的 GUI 是按资源分页的面板（Runs / Agents / 用量）。V3 改成以对话为中心：你和 Planner 对话，Planner 把活派出去，
GUI 让你随时进入任何一个会话。

| 区域 | 内容 |
|---|---|
| 侧边栏 | 顶部：Logo 加工作区选择（显示当前仓库名，下拉选「全部工作区」或某个仓库，选择会记住）、搜索框（按 run 标题、会话名、显示名过滤）、「新建 run」（Ctrl+N）。下面三组：「进行中」的 run 是 ◯ 节点，标题下是任务进度条（每个任务一段，按状态着色），它的团队用轨道挂在下面；「Sessions」是不属于 run 的会话；「最近」按今天 / 昨天 / 日期分组，第二行用 `⎿ 3/3 merged` 交代结果，选中或打开它的 Worker 时才展开团队。后两组各先显示 5 行，其余收在「更多 (n)」。每个 agent 一行：头像（右下角状态点）、显示名、右侧头衔，运行中改为 `✻ working`。底部：团队、用量、hub 连接状态与版本 |
| 工作区 | 侧边栏顶部的工作区选择决定侧边栏和新建 run 的范围。工作区概览 `#/w/<key>`：进行中 / 待审批 / 待你合并 / Agents 四个数字，进行中的 run，「待你合并」（已完成 run 的集成分支，附可复制的 `git merge` 命令），这个仓库的 agents，最近的 run。工作区由 hub 已有数据推出：run 的 `repo`、不属于 run 的 agent 的 `cwd`（hub 还没有项目登记，见 §6） |
| 首页 | 「今天让 agents 做什么？」输入框，Planner / Worker 选择做成输入框下的小标签，其余选项折叠在「更多选项」；Ctrl+Enter 发起 |
| 会话视图 `#/s/<name>` | 顶栏：状态、角色、插件/模型、正在做的任务、上下文用量条、中断（驱动支持 `interrupt` 时）、停止。对话：你的消息是右侧气泡（超过 600 字先折叠）；agent 正文用衬线体并识别代码块；思考折叠为「Thinking」；连续的工具调用合成一组单行条目，超过 4 个时折叠更早的；进行中的一轮显示「✻ Working… 12s」；非正常结束注明原因。Planner 有待审批的计划时，计划卡片直接出现在对话末尾。输入框：Enter 发送、Shift+Enter 换行，忙时提示会排队 |
| Run 视图 `#/runs/<id>` | 需求、分支、仓库、配置；待审批时的计划卡片（Approve / 驳回并说明）；任务列表 |
| 任务抽屉 `#/runs/<id>/tasks/<id>` | 右侧打开，不离开 Run 视图：Worker、尝试次数、分支、总结、diff stat（着色）、验收输出、规格 |
| 团队 `#/agents` | 每个 agent 一张名片卡：头像（带状态点）、显示名、头衔、人设摘要、角色、插件 · 模型、地址名；「编辑」就地改名片。「添加队友」从预设开始（架构师、Coder、设计师、安全审核专员、测试工程师、文档写手），预设填好角色、头衔、头像、颜色和人设，都可以改；头像实时预览 |
| 会话里的「人」 | 会话顶栏显示头像、显示名、头衔和 @地址；agent 每一轮回复上方有作者行（头像 + 显示名 + 头衔），像聊天一样知道是谁在说话；工作区页的 agent 列表同样显示头像 |
| 用量 | 原有功能换新样式 |

## 1.1 按专长派活（第二阶段）

- run 有团队：默认是 Planner 仓库里的全部 worker 和 reviewer，发起时可以让某人这次不参加（首页的团队标签，点一下变虚线）；`POST /v1/runs` 的 `team`、CLI `run start --team`。
- Planner 开场说明里列出团队和各自专长；`plan_submit` 的任务可写 `assignee` / `reviewer`，hub 校验不在团队或角色不对的名字（整个计划被拒并说明原因）。
- 批准后任务记下 `worker_template` / `reviewer_template`，派活从对应队友克隆，克隆继承名片，所以「Rex 的安全审查」在 GUI 里就是 Rex。
- GUI：计划卡片每个任务右侧显示被派给谁（头像 + 名字 + 头衔，未指定显示「默认」）；任务列表显示做这件事的人；run 顶栏显示 Planner 本人与团队头像；完成的 run 有「合并结果」横幅和可复制的 `git merge` 命令；长需求折叠成 4 行。
- 测试：`planner_assigns_tasks_by_specialty`（团队写进开场说明、按 assignee 克隆）、`unknown_assignee_rejects_the_plan`。

## 2. hub 新增

| 接口 | 说明 |
|---|---|
| `GET /v1/agents/{key}/transcript` | `Transcript { agent, driver, capabilities, turns, usage, last_seq }`。每轮 `TranscriptTurn { message_id, input, sent_at, started_at, completed_at, stop_reason, items }`；`items` 是 `output`（同一流的连续输出合并）、`tool_call`（已应用后续更新）、`error`。由 hub 从该会话的事件折叠而成（`crates/hub/src/transcript.rs`） |
| `GET /v1/events?agent=<名字或 id>` | 只发与该会话有关的事件（与 `EventKind::agent_id` 一致）；会话不存在返回 404 |
| `Store::agent_events` | 按会话取事件；默认实现扫描全部事件，SQLite 用 `json_extract` 直接筛选 |

`last_seq` 是读取前事件流的最新序号：客户端从这里继续跟实时事件，既不漏也不重。

## 3. 设计决定

| 决定 | 原因 |
|---|---|
| **对话历史在 hub 折叠**，客户端只折叠之后的实时事件（`src/session.ts` 与 Rust 版逻辑相同） | V2 每次打开都从第 0 条事件回放来拼历史，事件一多首屏就慢；现在一次 REST 请求拿到，断线重连时重新拉取即可自愈 |
| **事件流首次连接只要实时事件**（`?live=true`），断线后按 `Last-Event-ID` 续传 | 各视图的状态都从 REST 读，不再需要回放全部历史 |
| `useHubData` / `useSession` 接受「比已应用的更新」的响应，而不是只接受最后一次请求的响应 | 事件持续到达时，旧逻辑每次新请求都作废进行中的请求，页面会一直停在「加载中」 |
| **侧边栏的设计语言「节点与轨道」**：run 是 ◯ 节点，团队像 git 分支一样挂在轨道上（进行中的轨道带靛蓝色调），任务进度条一眼看进度；历史用 Claude Code 的 `⎿` 结果行；agent 用头像表示（形状标记 `Mark` 留在会话顶栏的角色标签上）。中途试过完全照搬 Claude Code 的纯文字分组列表，信息太少，回到这一版 | CrossAgents 本身就是「一个 Planner 分出多个 Worker」的图；agent 是有名字、头像、头衔的队友，而不是进程 |
| 强调色只有靛蓝一种，主按钮（发送、Approve）每屏最多一个；状态用小圆点 + 文字，进行中的状态呼吸闪烁 | 参考 Claude Code 的克制风格，但不用它的品牌色 |
| agent 正文用 `Charter / Georgia`，中文回落到微软雅黑 | Windows 上中文衬线（宋体）显得旧；拉丁字母用衬线即可区分「谁在说话」 |
| 不引入 Markdown 库与图标字体：正文只识别代码块和行内代码，图标是内联 SVG | 依赖少；CSP 只允许本地资源 |
| 工具调用默认只显示最近 4 条 | 多 agent 并行时完整展开会刷屏 |

## 4. 顺带修复

- Vite 开发代理：页面在事件流中途刷新时，被中止的流抛出未处理的 `error`，整个 dev server 崩溃；现在吞掉并关闭连接。
- Vite 开发代理：响应头立即 `flushHeaders()`，否则实时事件流在第一条事件前一直挂着，GUI 显示「连接中…」。

## 5. 验收

- `cargo test --workspace` 通过；新增 `transcript_groups_the_conversation_by_turn`、`events_can_be_filtered_by_agent`（mock agent 全链路）与折叠逻辑的单元测试。
- `npm run build` 通过（`tsc` + `vite build`）。
- 用 mock 插件在浏览器里走通：首页发起 run → Planner 对话里出现计划卡片 → Approve → 任务实时变为 merged → 任务抽屉显示总结、diff stat、验收；
  会话里发消息、看到「Working…」、中断后显示「本轮已中断」；深色与浅色都检查过。

## 6. 留给下一阶段

- 按能力显示的会话操作：回退、分叉、换模型（需要 hub 先暴露 N1 驱动的这些能力）。
- 完整 diff 查看（按轮 / 按任务）与事件流抽屉；`ask_user` 与 Worker `task_ask` 作为对话里的提问卡片。
- 在对话里 `@worker` 直接给某个 Worker 发消息。
- 编排输入（engine 发给 Planner 的提示）与人发的消息在对话里区分显示：需要 `agent.input` 带上发送方。
- Settings 页：Profile、账号（M5 已有 API）、项目配置。
- hub 的项目登记（DESIGN §4 的 Project）：GUI 现在从 run 和 agent 的路径推出工作区；登记后可以有空工作区、项目级配置，以及「待你合并」知道分支是否已合并。
