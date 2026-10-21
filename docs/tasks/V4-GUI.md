# V4 任务书：借鉴 Cutex 的对话与派活界面

> 依据：[Cutex 前端拆解](../research/cutex-gui-teardown.md) · 前序：[V3 任务书](V3-GUI.md) · 状态：第一阶段完成 · 2026-10-22

## 1. 范围

V3 让 GUI 以对话为中心。V4 把 Cutex desktop 在「多个 agent 互相派活」上的做法搬过来：对话里看得出谁发的消息、每个任务在什么状态；工具步骤不刷屏；排队的消息看得见、改得了；需要你处理的事情就在输入框上方；可以把 Planner 和 Worker 并排开。

多主机、设备权限、内置浏览器、插件沙箱和 Cutex 那样完整的输入框留到以后，要按 CrossAgents 的模型重新设计（拆解 §8）。

## 2. 状态表

所有地方（侧栏、任务卡片、run 页、需要你条）都用同一张表：符号加一个词，不单靠颜色。代码在 `apps/gui/src/lib/taskState.ts`，有测试。

| 符号 | 含义 |
|---|---|
| ○ | 在等别人或等名额 |
| ● | 正在动（呼吸） |
| ✓ | 完成 |
| ✕ | 没有结果就结束 |

| 任务状态 | 词 | 分组 |
|---|---|---|
| pending | 依赖未合并时「等 a、b」，否则「排队中」 | 等待 |
| running | 运行中（第 2 次起「第 N 次尝试」） | 进行中 |
| waiting_answer | 等 Planner 回答 | 等待 |
| verifying / retrying / reviewing | 验收中 / 重试中 / 审查中 | 进行中 |
| escalated | 已升级 | 进行中 |
| passed | 待合并 | 等待 |
| merged | 已合并 | 已结束 |
| failed / cancelled | 失败 / 已取消 | 已结束 |

run：拆解中（进行中）、**待审批（需要你）**、进行中、已完成、失败、已取消。分组顺序固定为 需要你 → 等待 → 进行中 → 已结束，组内保持计划顺序。

## 3. 对话

| 内容 | 做法 |
|---|---|
| 消息来源 | 人的消息是右侧气泡；编排和其他 agent 的消息是一行：`↙ 编排 · #schema ✓ 已合并  第一行 ▸`、`↙ 来自 [头像] Rex  第一行 ▸`。展开看全文；带任务的展开后还有任务卡片 |
| 任务卡片 | `[头像] Rex  #auth  标题 ……… ● 运行中 · 3m ▸`。运行中标题换成 Worker 当前的动作（`$ cargo test`）。展开：要做什么、提问、失败原因、审查意见、总结、改动、验收（只列失败的命令）、操作（打开、在旁边打开、任务详情） |
| 步骤折叠 | 工具调用和推理是「步骤」，正文和错误不是。一串步骤在后面出现正文之前保持展开（显示最新 3 步，更早的收起）；之后折成一句「✓ 2 个命令 · 1 次编辑 · 读了 3 个文件」，失败的步骤留在这句下面。只有一步不折。手动展开 / 收起优先。代码在 `lib/workGroups.ts`，有测试 |
| 本轮状态 | 进行中的一轮最下面是 `••• 42s · 运行命令`（推理中 / 回复中 / 修改文件 / 读取文件 / 搜索 / 调用工具）；结束后是 `已工作 1m 05s · 11:02`，悬停看开始 → 结束；非正常结束写原因 |
| 箭头 | 只表示状态：▸ 收起，▾ 向下展开，▴ 向上展开（输入框上方的 run 条） |

## 4. 输入框和它上方

从上到下：

1. **需要你**：其他 agent 等你的事，一行不滚动。待审批的计划和出错排前面并着色；「完成」是你发的消息有了回复而你还没看；两个以上「完成」合成「N 个已完成」，放不下的进「+N」。× 或中键：完成的标为已读，其余隐藏到状态变化。当前打开的会话不出现；并排时只在主栏显示。
2. **run 条**（Planner 的会话）：每个进行中的 run 一行 `▸ 标题 · 1/3 已合并 · 1 进行中 ● 进行中`，向上展开成按分组的任务卡片，展开状态按 run 记住。
3. **接下来**：排队的消息。你的消息可以「立即发送」（打断当前一轮先发这条）、「编辑」（撤回并放回输入框）、「删除」；编排发来的只显示。
4. **计划审批停靠**：Planner 有待审批的计划时，计划停靠在输入框的位置（草稿保留）。键盘 1 批准、2 驳回并说明、3 / Esc 先回复 Planner；先回复后输入框上方留一行「Plan 待审批 · 展开审批」。
5. **输入框**：空闲时 Enter 发送；忙时 Enter 排队、Ctrl+Shift+Enter 打断并发送、Esc 中断，并显示红色停止按钮。

注意：对 run 里的 Worker 用「立即发送」会中断它当前的一轮；Worker 没上报就结束一轮会被提醒，多次后任务失败。

## 5. 侧栏和版面

- 成员行的状态尾巴分三级：出错（红色块）、待审批（琥珀色块）；✻ working，第二行显示最新动作；没看过的「● 完成」，否则是头衔。顺序不随状态变化。
- 未读记在本机 localStorage（`crossagents.unread`），只记你发的消息得到的回复和出错的轮次；打开会话即清除。最新动作只在页面打开期间可知。
- **两栏并排**：侧栏 Ctrl/⌘ 单击、中键单击或悬停时的 ▯ 按钮，在当前会话旁边打开；中间分隔条可拖，双击平分；右栏有 ×。布局记在本机（`crossagents.panes`）。
- 危险操作点两次：「停止」「取消 run」第一次变红并写「再点一次…」，3 秒后复原，不再弹确认框。
- run 页的任务按分组排，每行是 `#key 标题 / 负责人 / 状态词`。
- 新 token：`--peer`（编排和 agent 消息展开后的底色）。

## 6. hub 新增

| 项 | 说明 |
|---|---|
| `MessageSource` | `human` / `orchestrator { run_id, task_id? }` / `agent { agent_id }`。`agent.input` 的 `from`（旧事件没有，按人处理）和 `TranscriptTurn.from` |
| orchestrator 投递 | 每处 `deliver` 都带来源：开场、批准 / 驳回、run 结束是 run 级；派活、合并、失败、提问、回答、返工、提醒是任务级。`agent_send` 是 `agent` |
| 收件箱 | runtime 的 mpsc 换成可撤回、可插队的队列（`Inbox`） |
| `DELETE /v1/agents/{key}/messages/{message_id}` | 撤回还没开始的消息，发 `agent.input_withdrawn`；已开始或不存在返回 409 |
| `SendMessageRequest.now` | 插到最前面并中断正在进行的一轮 |

## 7. 验收

- `cargo test --workspace` 通过；新增 `queued_messages_can_be_withdrawn_or_sent_now`，`planner_assigns_tasks_by_specialty` 检查编排消息带 run 和 task，transcript 折叠测试覆盖来源和撤回。
- `npm test`（Node 自带测试运行器，测 `src/lib/*.test.ts`）、`tsc`、`vite build` 通过。
- 用 mock 插件在浏览器里走通：Planner 计划停靠 → 3 先回复 → 展开 → 1 批准；编排消息行显示 `#schema ✓ 已合并`；run 条按分组展开，运行中的任务显示 Worker 的动作；排队两条后「编辑」放回输入框、「立即发送」打断当前一轮；外部发给两个 agent 的消息完成后出现「2 个已完成」；Ctrl+单击在旁边打开，刷新后恢复；深色与浅色都检查过。

## 8. 留给下一阶段

- 轮尾摘要和按轮 diff：hub 还没有发布驱动的 `TurnDiff`（N1 §6）。
- 停止后暂停队列（Cutex 的「已随停止暂停」）、拖动排序、插话（steer，需要驱动支持在工具边界读入）。
- run 卡片的胶囊位置、目标与预算环。
- 提问卡片停靠：hub 目前没有直接问人的工具，Worker 的提问发给 Planner。
- `useRunTree` / `useAgents` 在每个视图里各自请求，事件一多会重复拉取，应合成一个共享的存储。
- 多主机、设备权限、内置浏览器、插件沙箱、完整的输入框（拆解 §8）。

## 9. 真实 Codex 走通（2026-10-22）

用本机 Codex 登录（codex-cli 0.162.0-alpha.2，Codex 桌面版自带）在 GUI 里走通：添加队友 → 单聊（流式输出、命令步骤、回答）→ Planner + Worker 的 run（计划 → 批准 → 完成 → 合并）。为此改了：

- 内置 `codex` 插件默认驱动改为 `codex-native`；`[plugins.codex] driver = "acp-generic"` 可改回。
- PATH 上没有 `codex` 时，用 Codex 桌面版自带的 `%LOCALAPPDATA%\OpenAI\Codex\bin\<build>\codex.exe`（取最新的）。
- `codex-native` 没配置模型时，从 `$CODEX_HOME`（缺省 `~/.codex`）的 `models_cache.json` 列出选择器里的模型。
- 新增 `POST /v1/agents/{key}/model`（`SetModelRequest`）：改存储里的模型，运行中的会话从下一轮起换。GUI「编辑」里可以改模型，也可以移出团队（点两次）。
- 新建队友默认选插件列出的第一个模型。原因是 `~/.codex/config.toml` 里的默认模型可能不支持当前登录，实测 `gpt-6-astra` 对 ChatGPT 账号报错。
- Electron 版的工作目录可以用系统对话框选（`pickFolder`）。
- 修复：对已结束的 turn 发 `turn/interrupt`，codex 不会回应，停掉 Worker 要等 120 秒请求超时才能合并；现在中断和退订最多等 5 秒。
