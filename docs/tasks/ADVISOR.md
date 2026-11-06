# 跨厂商顾问：逐轮旁观与意见投递

> 2026-11-06 · 设计稿，未实现。对应 [TODO §二十五第 1 条](../TODO.md#二十五借鉴-oh-my-pi-与-cutex-021)。
> 参考 [oh-my-pi advisor-watchdog.md，固定 b7221d9](https://github.com/can1357/oh-my-pi/blob/b7221d994869ed36a81b44e60827786f572498ed/docs/advisor-watchdog.md)，只读文档，未运行它。
> 下文工具、字段、事件和 API 凡标为“拟新增”，均为实施契约，不代表当前已可用。

## 1. 决定与现有依据

给目标 Agent 订阅一个使用别家模型的顾问。顾问有自己的普通 AgentId、模型、上下文和用量，通过 hub 收到目标每轮新增转录，以 `advise` 提交具体意见。目标自行判断是否采纳；意见不是审批、任务验收或强制执行命令，`blocker` 也不替代人的权限。

采用**普通独立会话＋顾问订阅关系**，不增加 `Role::Advisor`、`ClaimRole::Advisor` 或 `Group::Advisor`。依据 [AGENT-WORKSPACE.md §2](../AGENT-WORKSPACE.md#2-领域模型与边界)：职责属于工作，Agent 是平等会话。默认使用所选会话的配置 fresh 创建一个新会话，清楚展示模板与实际执行者；不暗中占用人的原会话。MVP 不支持借用忙碌会话本人。顾问会话可打开、聊天、归档；人向它聊天时暂停自动评阅，避免混用正在运行的 review 上下文。

`crates/orchestrator/src/tools.rs` 按 `Group` 暴露工具，实际枚举和 `groups_of` 在 `messaging.rs`；当前仍有历史 role 推导。新工具放 `Everyone`，执行时检查工作区、订阅归属、实际调用者与目标占用关系，不按旧 role 标签筛选。run 专属操作仍依据 run 关系检查。hub 是给模型用的，不以“顾问只能说话”“worker 不能配置顾问”等身份闸门限制工具。顾问指令默认要求旁观、核实、只用 `advise` 回报；工具权限沿用普通会话策略，**不能声称该指令提供了只读隔离**。需要限制写入时使用现有权限策略。

现有落点与差距：

| 已核对的代码 | 当前行为 | 本设计需要补齐 |
|---|---|---|
| `crates/hub/src/transcript.rs` 的 `Fold` | 把事件折叠为回合；后到的工具结果、输入更新、插话会改变已有回合；有 `last_seq` | 按事件序号生成可重放的增量投影，不能按回合数组长度切片 |
| `crates/hub/src/api.rs` 的 `fold_agent`、`transcript` | `/v1/agents/{key}/transcript` 折叠所有历史，没有增量查询参数 | 保留原 API；新增顾问增量视图及上下文代际 |
| `crates/hub/src/runtime.rs` | runtime 排空输出再发 `AgentTurnCompleted`；`steer_envelope` 可插话，不支持或失败会排队；`send_now_envelope` 中断后优先送达 | 顾问投递策略、停止闩锁、来源元数据、原子决策 |
| `runtime.rs::notify` | 可更新等待中的汇总，但会启动会话 | 不能直接用它保存静默卡片或人停止后的意见 |
| `crates/hub/src/replies.rs` | 对主动队友消息的终轮文本自动回传，且防止转发消息互相回弹 | 顾问触发与意见均排除自动回信，避免把普通结束文字当成意见 |
| `runtime.rs::compact` | 调驱动压缩；没有统一的顾问上下文重置通知 | 明确压缩成功/历史替换的 epoch 事件及不支持能力的边界 |
| `budget.rs`、`usage.rs` | 用量是 session 内累计值，启动新 session 时结算；run 归属主要由 planner/assignee 事件推导 | 顾问的显式用量归属及局部预算，不能靠伪造任务归属计费 |

跨厂商按解析后的模型 provider/family 判定，不按 CLI 插件名猜：同一个 CLI 可接不同 provider。MVP 必须显式选模型；同厂商或 provider 未知时返回具体原因，状态显示 `model_unresolved`，不悄悄回退主模型。目标或顾问换模型后重新检查，失败则暂停订阅。是否允许人工覆盖未知 provider 留给人定。

## 2. 关系与持久状态（拟新增）

`AdvisorBinding` 保存 `id, target_id, advisor_id, workspace_id, run_id?, task_id?, enabled, generation, state, limits`。一个目标最多一个启用绑定，一个顾问执行会话只服务一个绑定；不允许自己顾问自己、互相订阅或顾问链。此为避免反馈环与上下文争用的关系约束，不是永久角色。默认关闭，不因新建 run 自动启用。

SQLite 是权威状态；hub 内存只缓存调度与互斥锁。拟新增关系表、review 表和 advice/outbox 表，必要事件进入已有事件流，供 GUI 和重启恢复使用：

| 状态 | 保存内容与意义 |
|---|---|
| 游标 | `target_epoch, captured_seq, reviewed_seq`；前者是已可靠存入 review 的高水位，后者是已结束评阅的高水位 |
| Review | `review_id, binding_generation, epoch, from_seq, through_seq, target_turn_id, status, trigger_message_id`，以及固定的投影输入；同一绑定串行，一次模型评阅可合并多个尚未评阅的回合 |
| 投递 | `advice_id, review_id, severity, text, evidence_refs, fingerprint, status, delivery_message_id?, source_turn, run_id?`；卡片、排队、已插话、已读、失败、被替换、过期可区分 |
| 防抖 | 去重历史最多 4096 个规范化指纹及最高等级；review 的非 blocker 已占槽数；`cooldown_remaining`；成功投递 ID |
| 人的控制 | 目标级 `manual_stop_latch` 与 `control_generation`；绑定开关代际；人停止/继续来源和原因必须持久化 |
| 费用 | 顾问执行 session 的归属片段、review 用量基线、绑定累计消费及预算暂停状态 |

开关、游标推进、review 入队和 outbox 记录以事务写入。先持久化后异步送达；崩溃恢复重复处理事件时，唯一键 `(binding, epoch, through_seq)` 和固定消息 ID 防重复。驱动已收但 hub 未确认的情况显示“送达未知”，不假装 exactly-once，不自动中断重发；查到读确认再更新。顾问文字输出不自动投递，只有被接受的 `advise` 生成意见。

## 3. 每轮新增转录

这里的“轮”是 hub 的 `AgentTurnCompleted`，即一次输入驱动的 runtime 回合，不是 token 分片或单次工具调用。当前 runtime 不把驱动自己的所有工具循环边界提升为独立回合；MVP 不承诺工具调用期间实时审阅。主会话异步继续，不等待顾问；顾问读完上一轮时，下一轮可能已在运行，此时可以插话。

1. 订阅已持久化的 `AgentTurnCompleted`，以该事件 `seq` 为本批次闭区间上界。runtime 已先 flush 输出；投影只读 `(captured_seq, through_seq]` 的目标事件。后到的输出/工具更新按其实际 seq 留到下批，不能因回合已结束而漏掉。
2. 复用 `Store::agent_events` 和 `transcript::Fold` 的关联规则；维护所需的 turn/tool 索引，把后到更新标成“更新已有项”。保留原始 message/tool ID、seq、stop_reason、模型变更、人的输入、可见输出与工具结果。只提供驱动实际报告的内容，不宣称可获得供应商隐藏推理；附件只传可访问引用和元数据，不自动跨账号复制文件。
3. 拟新增 `/v1/agents/{key}/transcript/delta?after_seq=N&through_seq=M&epoch=E`，返回 `epoch, next_seq, through_seq, has_more, items`；分页不得越过固定 M。epoch 不符返回 `reset_required`。服务端调度直接调用同一投影服务，不经 HTTP 自轮询；原全量 endpoint 保持兼容，用于人查看。工具可拟增 `agent_transcript` 暴露同一有界视图，不靠顾问反复拉全量。
4. 事务保存固定输入和 `captured_seq` 后启动 review；正常结束推进 `reviewed_seq`，包括合法的“无意见”。失败保留原 review/cursor，明确重试或暂停；不能先推进 reviewed 再丢失输入。合并积压保留证据范围；单次输入设可配置字节/token 上限，超限分批，不静默截断。
5. 中途启用默认从当前 seq 开始，只附目标、任务 brief/约束、工作目录及模型身份，不重放全部旧聊天。人可显式选“从最近一轮补看”。没有目标新增内容不启动 review。

### 压缩与重置

转录事件日志是审计历史，不等于模型压缩后正在使用的上下文。拟新增 `AgentContextReset { epoch, reason, baseline_ref?, at_seq }`：hub 主动 compact 成功后发布；失败不重置；会话历史替换、fork/resume 到另一历史、顾问自身上下文重建也使 binding generation 增长。仅重启并接管原 session 不当成压缩。

重置时取消旧 review，拒绝其迟到 `advise`，清私有评阅上下文、游标区间、去重和每轮配额；新 epoch 先生成有界基线再继续增量。若驱动能给当前摘要/快照，则以它为基线；当前通用接口未保证这一点。MVP 在没有摘要时用“任务约束＋最近若干已完成轮”的有界转录投影，并标记“hub 重建视图，非供应商压缩快照”，禁止假称完整重放当前模型上下文。不得从 seq=0 无限读回所有旧历史。

已接收意见保留审计卡片；未投递旧 epoch 意见标记过期，不再自动注入，blocker 卡片仍提示人复核。停止闩锁、消费累计、投递幂等记录不因压缩清零；冷却保留剩余目标轮数，防借压缩连发打断。旧 review 的额度只在新 review 中重开。

驱动内部自动压缩目前没有统一事件。实施须增加可选的驱动上下文重置报告并接入；能报告的驱动纳入验收。不能报告的驱动显示“自动压缩重置不可观测”，允许人手动重置顾问，不以 token 突降猜测并宣称已覆盖。

### 顾问意见不回喂

增加结构化 advisory 来源（binding/advice/review ID），贯穿 Envelope、`AgentInput`、`AgentSteered` 与附加提示段。投影按元数据排除所有顾问注入段、其引用摘要和顾问协议工具回执，而不是正则删除带 `<advisory>` 的人的原文。人消息附带意见时仅剥离附加段，保留人的正文。目标对意见采取行动产生的代码、工具结果和解释仍可成为下一次评阅证据。

顾问增量输入使用独立 review 触发来源，不伪装成目标通过 `agent_send` 发问；advisory 来源亦不进入 `ReplyTracker.asked`。`replies.rs` 需显式覆盖这两类消息。顾问唤醒目标产生的纯意见续轮不再单独调度 review（也不消耗评阅节奏），其新增工作暂存，随下一次人的/任务的正常回合送给顾问，防止“意见→续轮→评阅→意见”永动。

## 4. `advise` 与投递规则（拟新增）

工具参数：`advise({binding_id, review_id, severity: nit|concern|blocker = nit, text, evidence_refs?, idempotency_key?})`。服务器根据调用者 token 校验 advisor_id、当前 generation/epoch 和 review 生命周期，不信任参数自报身份；不接受任意 `to`。绑定可由普通会话配置，`advise` 则只有当前绑定的实际评阅者可写入，这是关系校验。

返回 `advice_id, status, reason, delivery_message_id?`。status 区分 `pending_boundary / card / queued / steered / duplicate / noise / rate_limited / stale / failed`；“接受”仅保证已持久化，不能称目标已读。拟支持原 advice_id 更新卡片投递状态，GUI 只占一个位置。意见中含具体证据、风险和可行动建议；提示目标衡量判断、可以拒绝。

### 过滤、去重与配额

- 按 NFKC、大小写折叠、Unicode 字母/数字以外符号归空格、连续空格折叠生成指纹，保留中文字符。空串及整句空话如 `lgtm`、`done`、`complete`、`nothing to add`、`stop`、`没有问题`过滤；“done 后会丢失消息，因为……”不能只因关键词被误杀。所有级别均需内容，blocker 不豁免空话过滤。
- 同绑定同 epoch 的相同意见在相同/更低等级丢弃；升高等级可接受，必须带新证据，提示顾问只在确实达到 blocker 时重提。去重在投递前执行并覆盖待发卡片；升级更新原卡片，不重复占位置。MVP 不做另一轮模型语义去重；换措辞绕过属于已知局限。
- **每次 review 最多 4 条非 blocker**；合并多轮不会获得 4×N。过滤、重复不占槽。已路由的意见占槽，不可撤回腾位；同 review 中未投递的 nit 可被 concern 替换，返回说明被替换。更早 review 的待发意见不能被新 review 偷占槽。blocker 不占这 4 个槽，但仍受去重、调用循环限制和总预算约束。
- 同一 review 对 `advise` 的重试以工具调用 ID/幂等键复用结果，不重扣配额。冷却与去重跨 hub 重启保留，避免重启后突发打断。

### 目标状态与送达

优先级为：**人工停止/归档、run 暂停或结束、预算与权限约束 > blocker 唤醒 > concern 冷却 > nit 边界**。“blocker 一定唤醒”指目标仍获授权自动运行时，不被普通结束或冷却吞掉；绝不越过人按停。

| 等级 | 目标正在运行 | 目标正常结束、空闲 | 人按停或自动运行被禁止 |
|---|---|---|---|
| nit | 持久化，攒到最近目标回合边界并入下一条合法输入；不主动中断 | 留卡片，下一次输入携带，不仅为 nit 新开轮 | 留卡片 |
| concern | 不在冷却时允许插话；冷却中降为边界待发 | 不在冷却时合并意见唤醒一轮；冷却中留卡片 | 留卡片 |
| blocker | 忽略冷却，立即尝试插话或明确的中断投递 | 启动/唤醒一轮，即使上一轮是正常最终答复 | 留醒目的 blocker 卡片与待处理提示，不自动运行 |

nit 的“边界”不保证恰好赶上下一轮：顾问异步返回时目标可能早已越过该边界，应等下一个边界。不得把 idle 时调用 `notify` 当成静默保存。

运行中优先使用 `steer_envelope`。当前它不支持插话或插话失败时会**排队**，并不会自动中断。实施新增顾问投递模式，以同一个 envelope ID 原子选择：已知不支持 steer 的驱动，对 concern/blocker 使用 `send_now_envelope` 的中断后投递语义；不是先让 `steer_envelope` 排队再盲目 send_now，避免双送。驱动临时失败只能在“明确未接收”时把同一消息从等待队列原子提升为中断投递；接收未知则保留未知状态/卡片，不自动换方式重发。中断能力也不可用或被权限拒绝时显示具体失败原因，blocker 保留待处理并通知人，不能声称已经唤醒。

每个目标的路由在同一控制锁/actor 内完成：读取停止代际、检查预算、写 outbox、选择 steer/中断/排队；驱动调用返回后复查代际。如果人按停与投递竞态，未读意见退回卡片，撤回尚未开始的顾问触发，禁止失败回调再续跑；已经插话的无法“撤销已读”，记录真实状态，但仍不允许自动开下一轮。人的消息优先，顾问不得插队覆盖人正在排队的输入。

成功插话或成功中断投递 concern/blocker 后，设置 `cooldown_remaining=3`。以这次投递所在目标回合为锚点，之后三个不同目标回合完成事件各减一次，重复事件不减；第三轮完成后 concern 才可再次打断。若在 idle 唤醒，唤醒轮作为锚点。顾问自己的回合不计数。blocker 仍可穿过冷却，每次成功投递重新设为 3；路由失败、仅留卡片或普通排队不启动冷却。

### 人主动停止与“随人的消息一起送”

当前 `cancel` 只调用 interrupt，`StopReason` 也不足以区分人停止、顾问中断、故障和恢复。拟给控制入口增加来源/原因；人的 cancel/stop 在调用驱动前先持久设置停止闩锁，归档同理。顾问中断不能设置这把闩锁；普通 `EndTurn` 不能解除它，重启/压缩/切模型也不能解除。

停止后仍可收尾保存正在评阅产生的意见，但不再发起新的自动评阅，也不唤醒目标。下次**人实际发送消息或明确继续**时才解除闩锁，将待发意见按 severity/source_turn 合并附加：人的正文在前、独立意见段在后，GUI 挂在人气泡下，保留来源和每条 advice_id；不更改人的正文或消息身份。人的消息进入发送事务时固定意见快照，之后到的意见留给下一次，防止不断改写正在送的输入。

复用 TODO 第 3 条未来的“提示随人的消息一起送”装配接口；当前尚未实现该不变式，不能假设现有 `notify` 已满足。如果第 3 条未先落地，MVP 需实现这一个共享的结构化附加段接口，而非为顾问另建字符串拼接队列。快照与标记已附加原子完成；仅发送成功才消费，失败保留；重试人的同一 message_id 不重复追加。人也可清除/忽略卡片，清除后不再携带。绑定关闭后保留审计卡片，但不再自动携带，须显式恢复或人选“带上这些意见”。

## 5. 用量、预算与故障

账单发生在**顾问自己的 AgentId/实际账号/session**，目标的 token 与成本不混入顾问计数；主会话阅读意见消耗的 token 自然记主会话。界面可展示“目标＋顾问”小计，但全局总计不可再次相加。顾问触发的失败、重试、无意见评阅也计费。

run 内绑定的顾问消费计入该 run 总预算，同时受绑定的较小上限约束；run 外归入 no-run/工作区消费，并有绑定硬上限。拟新增显式 `UsageAttribution` 事件/归属片段，供 `budget.rs` 与 `usage.rs`（以及时间序列视图）共同折叠，不靠 `TaskStatusChanged` 冒充 assignee。MVP 顾问执行 session 不跨 run 复用，绑定归属期间冻结；归属改变先结算累计基线再开新执行 session，避免现有“一个 agent 最后一个 run”映射把旧账移走。

沿用每 session 用量累计值取最新值、Starting 时结算的规则，不能把每条 `AgentUsage.tokens.total/cost_usd` 当增量求和；重放与压缩不能重复计费。顾问更换进程或重新评阅仍属于同一绑定累计预算，开关顾问不归零。

拟支持 `max_tokens, max_usd, max_reviews`，至少设置一个供应商无关的有限 `max_tokens` 或 `max_reviews`，不能只有未知价格的美元上限。建议 MVP 默认最多 20 次评阅，并要求开启时显示可修改的 token/美元上限；具体默认值留给人决定。每轮输入、输出与工具循环也设上限，避免一次 review 无界耗费。

启动 review 前检查绑定余量与 run 状态/预算，预留此次最大输出并串行消耗；80% 警示，100% 暂停新评阅，保留积压和意见。已在飞的请求尽可能取消并结算真实用量，不能承诺供应商延迟报账下零超支；显示最大在飞风险。顾问预算用尽只暂停顾问；run 总预算用尽沿用 orchestrator 的暂停流程，同时禁止意见自动唤醒目标。额度提高后须明确恢复，不靠定时器偷偷继续。

未知成本显示“未报告”，不能算作免费；美元硬上限无可用价格或真实报账时，不允许以此作为唯一限额。顾问暂时故障默认保留 review，最多三次有界重试（均计入预算），之后暂停并显示原因，不阻塞目标；MVP 不自动跨模型 fallback。接受未知投递的重试遵循上一节的防重复约束，不与模型请求重试混为一谈。

## 6. run 内外入口与生命周期（拟新增）

共享工具：`advisor_set({target, model?, template?, enabled, limits, catch_up?})`、`advisor_status({target})`、`advisor_reset({target})`；`advise` 如上。set 首次开启必须有模型/模板和有限预算；重复调用幂等。reset 只重建顾问评阅上下文，不清账、不解除人的停止、不自动恢复预算暂停。工具描述返回状态及可行动原因，避免让模型靠 GUI 猜开关是否生效。

共享人用 API：`GET/PUT /v1/agents/{key}/advisor`，`POST .../advisor/reset`，`GET .../advisor/notes`，`POST .../advisor/notes/{id}/dismiss`；模型通过已有 `/v1/me` 工具分派访问同一服务，不开放另一条绕过身份检查的接口。设置返回绑定代际、实际模型/provider、游标、积压、用量与状态。

| 场景 | 创建与控制 | 生命周期与展示 |
|---|---|---|
| run 外普通会话 | 人从会话工具区选择模型开关；模型可给自己配置顾问，也可在工作区既有协作授权内给空闲目标配置，必须返回实际目标和消费上限 | 顾问是项目成员的普通会话；目标详情显示关联；目标停止/归档则冻结，关闭订阅不删除历史 |
| run 内 lead/assignee/reviewer | 同一套工具/API；本 run 控制者依实际关系配置目标，本人可配置自己；跨 run 占用冲突报错，无需固定角色资格 | 记录 run/task 归属，顾问不伪造执行/评审任务，不作为 `review_request` 结论；run 页可查看关联顾问与消费 |
| run 暂停、结束、移交 | 暂停冻结评阅和自动唤醒；结束后在飞 review 可保存卡片但不得重开任务或影响 land；恢复需明确启用 | 取消旧代际，任务重新分派不把绑定悄悄迁给新 assignee；新目标需重新配置；保留旧关系审计 |

工具可发现性对所有普通 Agent 一致，实际可操作范围沿用既有协作授权：同工作区、目标本人/自己的协作对象、run 的实际控制关系，以及占用规则。不能因 `Group::Everyone` 意味着任意会话都能重配别人的顾问。GUI 给出“启用/暂停/关闭”、模型、最近检查轮次、延迟、费用、停止原因；“暂停”保留积压，“关闭”结束当前评阅、增加代际、不再自动消费卡片。全局默认仍关闭。解除绑定不删除实际顾问会话，可由人另行归档。

顾问卡片标等级、证据轮次和投递状态，点击定位目标与顾问记录；blocker 形成需人复核事项，读过不等于处理完。意见送达不是完成任务，dismiss 不是模型采纳。重连从服务端恢复同一个 advice_id，避免聊天和队列各出现一张。

## 7. 最小可用版

包括：一个目标一个跨厂商顾问、fresh 普通会话、每个 hub 回合异步评阅、有界增量与持久游标、`advise` 三等级、4 条非 blocker、3 轮冷却、结构化来源与反馈环隔离、人工停止闩锁及下次人消息携带、显式计费归属和有限预算、工具/API/最小 GUI 开关及卡片、hub 重启恢复。compact 成功重置及可观测驱动重置纳入范围；不可观测的自动压缩如实展示能力限制。

不在 MVP：多顾问 roster、WATCHDOG.yml 配置语言、严格同步等顾问赶上、每次工具调用即评阅、自动语义去重、自动换厂商 fallback、独立只读沙箱、新的永久角色体系、自动影响审批/验收/合并。第 3 条共享消息附加机制是依赖；不得以此为由先交付会偷偷续跑或用单独通知多耗一轮的版本。

### 拟改文件清单（实施时，不在本任务修改）

| 文件 | 改动 |
|---|---|
| `crates/core/src/advisor.rs`（新增）、`lib.rs`、`ids.rs`、`store.rs` | 绑定/review/advice 模型、稳定 ID、存储契约 |
| `crates/core/src/event.rs`、`driver.rs` | advisory 元数据、review 触发来源、控制原因、上下文 reset 与可选驱动报告能力 |
| `crates/store-sqlite/src/lib.rs` | 迁移、事务与恢复查询；幂等键及 outbox |
| `crates/hub/src/advisor.rs`（新增）、`lib.rs` | 调度器、guard、预算门控、恢复与关闭等待 |
| `crates/hub/src/transcript.rs`、`api.rs`、`crates/api-types/src/lib.rs` | 增量投影、API schema、卡片与状态、旧 API 兼容 |
| `crates/hub/src/runtime.rs`、`replies.rs`、`collab.rs`、`notices.rs` | 原子意见路由、停止闩锁、回信排除、会话指令、共享附加段与第 3 条接线 |
| `crates/orchestrator/src/tools.rs`、`messaging.rs`、`engine.rs` | 工具/schema、关系校验、run 生命周期冻结；不增 Advisor Group |
| `crates/hub/src/budget.rs`、`usage.rs`、`usage_series.rs` | 显式归属片段、累计基线、顾问子预算与显示一致性 |
| `crates/drivers/src/` 中实际报告压缩的适配器 | 将可观测的压缩/历史替换接到统一 reset 事件；不声称所有驱动天然具备 |
| `apps/gui/src/views/Session.tsx`、`ToolColumn.tsx`、`session.ts`、`activity.ts`、`workspace.ts`、`lib/i18n.ts` 及 API 类型绑定 | 设置、单位置意见卡片、附在人消息下的段、状态恢复与文案；实施时按实际模块组织细化 |
| `crates/mock-agent` 的 mock 驱动场景、`tests/advisor.rs`（新增） | 可控输出、插话能力、失败/停机竞态场景 |
| `docs/TODO.md`、本稿及相关 API 文档 | 以实际实现回填能力边界和验收证据 |

### 测试清单

消息投递相关集成测试统一放 **`crates/mock-agent/tests/`**，主要新增 `advisor.rs`，复用 `steer.rs`、`steer_failure.rs`、`compact.rs`、`human.rs`、`quota.rs` 的 harness；不把关键投递正确性只藏在 hub 纯函数测试里。

1. 逐轮增量：固定 seq 上界、分页、重复/漏订阅事件补读；工具结果更新旧轮不漏；启用中途默认不回放；积压合并仍最多 4 条；故障重试不前进 reviewed 游标。
2. 插话/中断：支持 steer 的 concern、blocker；不支持时仅一次中断投递；已明确失败、接收未知、刚好结束、并发人消息，均不双送、不把排队称为已插话；验证已读确认与卡片状态。
3. 边界与冷却：nit 不打断且 idle 不开轮；concern idle 唤醒；blocker 在正常结束后仍唤醒；成功打断后随后三轮 concern 不打断，第三轮完成才解禁；blocker 豁免且重设冷却；失败不扣冷却。
4. 人停止：意见返回前/steer 中/中断回调中按停；停止后 concern/blocker 均只留卡片，notify 不启动目标；重启、压缩、切模型不解除闩锁；下次人消息携带一次、正文在前、失败可重试、消息快照后的意见不混入。恢复 run 也不能替人解除停止。
5. 防反馈：顾问输入和 advise 不触发 ReplyTracker 自动回信；目标增量不含自己的/其他顾问注入段，人的原文不误删；意见续轮不再启动独立 review，下一正常轮仍能看到实际工作。
6. 重置：compact 成功、失败、可观测自动压缩、顾问自身重建、历史切换；旧 generation 工具拒绝，新 epoch 有界基线，旧意见过期、停止和账单不清；不可观测能力显示正确。
7. 恢复：写 review 后崩溃、保存 advice 后崩溃、驱动已收回执未落库；恢复不重扣配额、不重发未知消息、不重复卡片。关闭后迟到 advise 无效，结束的 run 不被唤醒。
8. 授权与身份：不同历史 role 都能使用工具；冒用绑定、跨工作区/跨 run 占用、自订阅/互订阅被拒；fresh 身份不冒充模板；同 provider/未知 provider、换模型和归档有具体状态。
9. 用量与限额：累计事件不累加、Starting 后正确结算、未知美元不按零、无意见/重试照计费、run 内外归属、重开顾问不清预算、子预算不误停目标、run 总预算禁唤醒。

纯函数测试放 `hub` 对应模块：Unicode 规范化、整句空话与有内容句的区别、等级去重/升级、4096 FIFO、槽位替换、epoch 清理、预算片段折叠。SQLite 测试覆盖事务和唯一键。GUI 用现有 mock 场景检查开关、停止后的 blocker、未知投递、单卡片、窄屏及重连；前端 reducer 测试校验同 ID 原位更新。以上是实施验收计划，本设计任务没有运行这些测试。

## 8. 留给人定的问题

1. **MVP 默认额度**：建议每绑定最多 20 次评阅，另显示可选 token/美元上限；是否还要给工作区统一的顾问总限额？关闭再开启不重置已花额度。
2. **跨厂商判定**：provider 未知时默认暂停，是否允许人明确覆盖？是否允许同厂商不同模型作为后续通用“旁观者”模式？MVP 默认坚持别家模型。
3. **最终答复后的 concern**：本稿按 TODO 建议自动唤醒（受冷却和停止约束）；是否改成只有 blocker 自动唤醒、concern 留卡片？这会直接影响续轮费用，须在实施前确认。
4. **已有普通会话本人当顾问**：MVP 默认 fresh 新建；后续是否开放显式借用空闲本人，以及暂停自动评阅后人工聊天如何继续原评阅上下文？
5. **跨账号共享范围**：启用时明确将目标后续转录交给所选厂商；是否需要工作区级允许名单和更细的附件/敏感段排除？现有会话权限不等于跨厂商内容已脱敏。

除这些产品选择外，停止优先、来源隔离、不重复送达、有限预算和无角色闸门作为本设计的不变式，不等待上述选择才能明确。
