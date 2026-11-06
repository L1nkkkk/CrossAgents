# 人的消息怎么送达：对照 Cutex 050 §2 的只读核对

> 只读核对：**没改代码，没运行任何东西**（不跑 hub、不跑测试）。结论全部来自读源码。
> 依据：`docs/TODO.md` §二十五 第 2 条；Cutex desktop [`050-owner-message-delivery.md`](https://github.com/senxiu-official/cutex-desktop/blob/9fbae59caa37c09eaa89d127a233f83d49601203/docs/design/050-owner-message-delivery.md) §2（固定 `9fbae59`，只读不运行）。
> 核对对象：本仓库 `42d287b`，分支 `crossagents/7cfd5794-delivery-audit`。
> 读过的文件：`crates/hub/src/runtime.rs`、`crates/hub/src/api.rs`、`crates/api-types/src/lib.rs`、`crates/core/src/event.rs`、`crates/core/src/model.rs`、`crates/drivers/src/claude_native/mod.rs`、`apps/gui/src/outbox.ts`、`lib/steer.ts`、`lib/errors.ts`、`api.ts`、`session.ts`、`activity.ts`、`liveTurn.tsx`、`notices.ts`、`Sidebar.tsx`、`views/Session.tsx`、`views/SessionComposer.tsx`、`views/SessionSteer.tsx`、`electron/allowed.cts`、`electron/main.cts`。

## 0. 先给结论

| # | 不变式（050 §2） | 结论 | 一句话 |
|---|---|---|---|
| ① | 「忙」只有一个来源（§2-2） | **部分** | 界面的「忙」确实只读 `agent.status`，但 hub 的准入用另一套（`inbox.is_idle`），「接下来」又是从 `started_at` 推的；`queued` 这个字段界面从来没读 |
| ② | 一条消息一个固定位置，状态原地变（§2-3） | **违反** | 自己发的消息会走「聊天气泡 → 接下来 → 聊天」三次搬家，因为排队的 turn 被 `filter(started)` 挡在对话外 |
| ③ | 每个原因码有专门映射和对照测试，未知的如实显示（§2-4） | **违反**（后半条成立） | 我们根本没有原因码：`SendMessageRequest/Response`、`AgentInput` 都只有一个 `queued: bool`；插话被拒 → 不报错，界面上只说「排队」，为什么排没有 |
| ④ | 不自动换方式重发；超时可见、可重试、重试不重复（§2-9） | **违反** | 失败重试是**新的 HTTP 请求、新的消息 id**；Electron 30 s 超时被写成「连不上 hub」，人很可能重发一条已经送达的消息 |
| ⑤ | 人自己发起的中断不弹提示（§2-8） | **符合** | 中断只留下该轮的「— 本轮已中断」尾注，没有弹窗、没有系统通知、不计未读；建议补一个回归测试锁住 |

其余五条（§2-1/5/6/7/10）在 §6 里简评。

---

## 1. ①「忙」只有一个来源（050 §2-2）— **部分**

### 证据

界面这一侧做得比 Cutex r270 好：所有「在不在工作」都读 hub 的同一条 `AgentStatus`，没有各自推算。

- `apps/gui/src/views/SessionComposer.tsx:182` `const running = agent.status === "running";`
- `apps/gui/src/Sidebar.tsx:462` `const running = !uncertain && agent.status === "running";`
- `apps/gui/src/liveTurn.tsx:146` `if (agent.status !== "running" || !pulse || pulse.over) return undefined;`
- `apps/gui/src/views/Session.tsx:557` 中断按钮同样按它显示。

hub 侧这个字段的来源也单一：`Control::TurnStarted` → `Running`（`runtime.rs:1929`），`TurnFinished` 且队列空 → `Idle`（`runtime.rs:1969-1972`）。

**但是「忙」在系统里有四个不同的算法，其中决定去向的那一个界面拿不到：**

| 谁 | 依据 | 位置 |
|---|---|---|
| 界面（输入框、侧栏、中断按钮） | `agent.status == "running"` | 上列四处 |
| hub 准入：这条要排队吗 | `!live.inbox.is_idle()`（能不能立刻起一轮） | `runtime.rs:1327` |
| 「接下来」有多少条 | transcript 里 `started_at` 为空 | `Session.tsx:57`、`Session.tsx:233` |
| 挪动/派活前的「忙」 | `Inbox::busy_with()` 另写一套四种说法 | `runtime.rs:157-169` |

而 hub 在响应里给了 `queued`、在事件里给了 `queued`（`api.rs:1211-1228`、`core/src/event.rs:82`），界面**一次都没读**：

```ts
// apps/gui/src/outbox.ts:42-45
// `steered: false` in the answer is not a failure: the hub queued the message instead ...
const sent = await api.sendMessage(...);
```

### 用户能看到的后果

1. **同一个回车，有时发送、有时排队，界面事先说的不一样。** agent 正在启动时（`status = "starting"`）按 Enter，输入框按「发送」处理（`steer.ts:29`）。若这次启动正是**前一条消息**触发的，这条会等在 agent 锁上（`deliver` → `start()`），而输入框在整个等待期间都按「不在工作」处理；等它落地时 `deliver` 很可能把它算成排队（`queued = true`），气泡先显示「正在唤起…」（`Session.tsx:698`），随后这条消息出现在「接下来」。两条并发时 `queued` 甚至可能都是 `false`（见 §7 第 8 条）。
2. **external agent 上永远如此。** 这类 agent 不启动会话（`runtime.rs:1289-1316` 直接 `Ok((message_id, true))`），`status` 一直是 `Stopped`；输入框提示还写着「agent 离线时会自动启动」（`steer.ts:39`），而它不会——消息进「接下来」等到外部进程 `inbox_read` 才走。
3. 「接下来」的副标题写死「本轮结束后依次发给 X」（`SessionComposer.tsx:73`），在没有回合在跑时（上面两种）是假话。

### 建议改法

- **最小改动（先止血）**：输入框和提示把「有消息在排队」也算作忙——`const busy = running || queued.length > 0`，`composerAction` 的 `running` 参数改传它。这样「界面说发送、实际排队」在主人消息这条路上消失。
- **根治**：hub 在 transcript 快照 / 事件里给出真正的会话状态（`idle | starting | turn | held`）＋ `queuedCount` ＋ 每条消息的 `queuedReason`（见 ③），界面只读它；`AgentStatus` 退回「进程状态」，不再兼职「回合在跑」。
- 顺手把 `busy_with` 的四种说法与这条状态对齐，别再各自造句。

**范围**：`crates/hub/src/runtime.rs`（`Inbox` 暴露状态、`deliver` 带上 reason）、`crates/api-types/src/lib.rs`（会话状态字段）、`apps/gui/src/views/SessionComposer.tsx`、`apps/gui/src/lib/steer.ts`、`apps/gui/src/views/Session.tsx`（QueuePanel 标题）、`apps/gui/src/Sidebar.tsx`（若改读新字段）。

### 需要的测试

- `lib/steer.test.ts`：`busy = running || queued` 时 Enter = 排队、Ctrl+Enter = 插话/排队；没有回合也没有排队时 Enter = 发送。
- hub 集成：给 `starting` 中的 agent 连发两条，第二条返回 `queued: true` 且带 `queuedReason`（现在只能断言 `queued`）。
- 组件/纯函数级：QueuePanel 在 `queuedCount > 0 && status != running` 时标题不说「本轮结束后」。

---

## 2. ②一条消息一个固定位置（050 §2-3）— **违反**

### 证据

一条消息在界面上最长会走三个位置（插话是四个）：

1. 乐观气泡进聊天：`outbox.ts:59` 把它加进 `items`，`Session.tsx:265` 在 `Conversation` 里渲染。
2. hub 的 `agent.input` 一到，气泡**消失**：`outbox.ts:83-89` 的 `arrived()` 只看 transcript 里有没有这个 id，不看它开没开始。
3. 同一条消息出现在「接下来」：对话只渲染已开始的回合，排队的被挡在外面——

```tsx
// apps/gui/src/views/Session.tsx:57
const started = (turn: TranscriptTurn) => !!turn.started_at || !!turn.completed_at;
// apps/gui/src/views/Session.tsx:233
const queued = turns.filter((t) => !started(t));
// apps/gui/src/views/Session.tsx:264
<Conversation agent={agent} turns={turns.filter(started)} ...>
```

4. 轮次真开始时，`agent.turn_started` 补上 `started_at`（`session.ts:131-134`），这条消息**又搬回聊天**。

「接下来」的「立即发送」不是原地提升，而是**撤回再新发**：

```tsx
// apps/gui/src/views/SessionComposer.tsx:93-94
await withdraw(turn);
await api.sendMessage(agent.id, { text: turn.input, now: true, ... });
```

### 用户能看到的后果

- 正是 Cutex 报告里的第 1 条：「发的消息在聊天和『接下来』之间来回跳」。忙时按 Enter，气泡在聊天里闪一下就不见了，跑到输入框上方；几秒后又自己回到聊天里。
- 插话被拒（见 ③）时也是这次搬家：主人以为插进了当前回合，其实排到了下一轮。
- 「立即发送」有**丢消息**的窗口：`withdraw` 成功、`send` 失败时，队列里那条已经删了，新的没建成，只弹一行错误（`SessionComposer.tsx:52-59`），文字没了（附件也白传一遍）。
- 每条消息没有自己的状态（发送中/排队中/已插入/失败）可看，状态是「位置」表达的。

### 建议改法

两个方案，建议先做 B 再评估 A：

- **B（小，够守住不变式）**：让 outbox 那条气泡成为这条消息**唯一**的一行，从提交到结束都在聊天里，在气泡上原地显示状态（发送中 → 排队中 · 原因 → 已插入等读取 → 失败可重试）。「接下来」只列**编排**发来的消息，不再列主人自己发的；主人自己的排队在气泡上看。
- **A（Cutex 4.4 的做法，较大）**：「接下来」保留，但只作**引用**（一行摘要，点一下跳到聊天里那一行），本体永远在聊天里；状态字段由 hub 提供。
- 两种方案都要求「立即发送」变成**同一条消息原地提升**：新增 `POST /v1/agents/{key}/messages/{id}/send-now`（或 `now: true` 带已有 id），hub 把队首的这条挪到最前并中断，不新建消息。
- `agent.input_withdrawn` 之后应保留一行「已撤回」，而不是让消息凭空消失（现在 `session.ts:111-115` 直接删）。

**范围**：`apps/gui/src/outbox.ts`、`apps/gui/src/session.ts`（回合状态字段）、`apps/gui/src/views/Session.tsx`（`filter(started)` 与渲染顺序）、`apps/gui/src/views/SessionComposer.tsx`（QueuePanel）、`crates/hub/src/runtime.rs` ＋ `crates/hub/src/api.rs`（原地提升）、`apps/gui/src/api.ts`、`apps/gui/src/electron/allowed.cts`（新路由要加白名单）。

### 需要的测试

- 纯函数：给定事件序列 `input(queued:true) → turn_started → turn_completed`，这条消息在「一行」模型里的 key 不变、状态依次是排队中/进行中/结束。
- hub 集成：`send-now` 用同一条消息 id（旧 id 消失、新 id 不出现）；`withdraw` 后 `send_now` 失败不得丢消息。
- GUI 选择器：`queued` 列表不含 `from.kind === "human"` 的消息（方案 B）。

---

## 3. ③原因码与对照测试（050 §2-4）— **违反**（「未知如实显示」这半条成立）

### 证据

我们**没有原因码这一层**，请求、响应、事件三处都只有一个布尔：

```rust
// crates/api-types/src/lib.rs:160-177
pub struct SendMessageRequest { pub text: String, pub now: Option<bool>, pub steer: Option<bool>, pub attachments: ... }
// crates/core/src/event.rs:78-83
AgentInput { id, message_id, text, queued: bool, from, ... }
```

`SendMessageResponse` 也只有 `queued` / `steered` 两个布尔（`bindings/SendMessageResponse.ts`）。HTTP 错误只有七种粗类（`crates/api-types/src/lib.rs:2416-2426`：`bad_request / unauthorized / forbidden / not_found / conflict / agent_error / internal`），没有 `runtimeNoSteer`、`noActiveTurn`、`hostBusy` 这类东西可以映射。

**最要命的一处：插话失败被吞掉，什么都不告诉人。**

```rust
// crates/hub/src/runtime.rs:1188-1195
// Unsupported, no turn any more, or the message never reached the CLI:
// it must not be lost, so it goes through the queue.
Err(err) => { tracing::debug!(%message_id, %err, "cannot steer; queueing the message") }
...
let (message_id, queued) = self.deliver(key, envelope, false).await?;
```

失败原因只进 `tracing::debug`；接口回 `{queued, steered:false}`，界面两个都不看（`outbox.ts:42-45`）。于是「驱动不支持插话」「回合刚好结束」「写进 CLI 失败」三种完全不同的情况，在界面上是同一种：**一条悄悄排队、没有任何说明的消息**。

其余两点：

- 发送失败显示的是 hub 的英文原文，绕过 `lib/errors.ts`：`Session.tsx:683` `没发出去：{message.error}`，而 `errorText` 就是 `err.message`（`api.ts:472-474`）。`errors.ts` 只认 CLI 缺失/代理/登录（`errors.ts:32-44`），消息送达的错误一个都没覆盖。
- **未知被归成了错的原因**：Electron 请求 30 s 超时（`electron/allowed.cts:106`、`110-113`）与「连不上」被写成同一句话——
  ```ts
  // electron/main.cts:163-166
  signal: AbortSignal.timeout(timeoutFor(method, path)),
  ...
  throw new Error(`cannot reach the hub: ${message(err)}`);
  ```
  人看到「没发出去：cannot reach the hub: The operation was aborted due to timeout」。超时的真实含义是**不知道发没发出去**，报成「连不上」会直接导致 ④ 的重复发送。
- 「如实显示」这半条是成立的：我们从不把未知原因改写成「被拒绝」，原文照出。问题是没有专门映射、没有对照测试，而且最该显示的那一种（插话被拒）连原文都没有。

### 用户能看到的后果

- 按 Ctrl+Enter 插话，驱动其实没插进去，消息排到了下一轮；主人要等到下一轮开始才知道（或者一直不知道）。
- 看到英文内部错误；同一句话既可能表示「没发出去」也可能表示「可能已经发出去了」，无法据此决定要不要重发。
- 排查时没有依据：客户端一行日志都没有（`console.error` 只出现在扩展里），hub 侧只有一条 debug。

### 建议改法

1. **加一层原因码**（`QueuedReason` / `SteerFailure` 枚举，`#[serde(rename_all = "snake_case")]`，只追加不改名，写法照 `050 §5.4`）。按现有代码能如实报的只有这些，不要多加：

   | 场景 | 码 | 界面上的话 |
   |---|---|---|
   | 有回合在跑，消息排在它后面 | `turnRunning` | 本轮结束后发送 |
   | 前面已有主人消息在排 | `ownerQueueAhead` | 前面还有 N 条 |
   | 目标是不在 hub 里跑的 external agent | `externalAgent` | 等它自己来取 |
   | 插话：驱动不支持 | `runtimeNoSteer` | 这个驱动不能插话，已排到下一轮 |
   | 插话：回合已结束 | `noActiveTurn` | 这一轮刚结束，已排到下一轮 |
   | 插话：写进 CLI 失败 | `steerWriteFailed` | 没插进去，已排到下一轮 |
   | 归档 / 消息已开始 / 附件不收 / 空文本 | `archived` / `alreadyStarted` / `attachmentRefused` / `empty` | 各自的句子 |
   | 连不上 | `unreachable` | 没发出去（hub 没响应） |
   | 超时 / 断线但请求可能已到 | `outcomeUnknown` | 不确定发没发出去，先别重发 |

2. 把 `session.steer()` 的错误分类（现在是任意 `CoreError`）带上来，放进响应与 `agent.steered` 旁边的失败事件（或 `agent.input` 的 `queuedReason` 字段）。
3. 新增 `apps/gui/src/lib/delivery.ts`：一个码一个句子，穷举，配 `delivery.test.ts` 做「枚举每个变体都有句子」的对照测试（和 `taskState.ts` 一样的做法）。
4. `Sending` 的错误行改走这张表 ＋ `errors.ts` 兜底，原文放在 `title` 里（现在 `message.error` 已经是全文，正好塞 title）。
5. Electron 把 `AbortError`/`TimeoutError` 映射成 `outcomeUnknown`，不要和 `ECONNREFUSED` 混在一起。

**范围**：`crates/api-types/src/lib.rs`、`crates/core/src/event.rs`、`crates/hub/src/runtime.rs`、`crates/hub/src/api.rs`、`apps/gui/src/lib/delivery.ts`（新）＋ test、`apps/gui/src/lib/errors.ts`、`apps/gui/src/outbox.ts`、`apps/gui/src/api.ts`、`apps/gui/src/electron/main.cts`。

### 需要的测试

- Rust：枚举穷举测试（每个变体都能序列化且出现在导出 schema 里，照 `owner_input_reasons_are_enumerated_everywhere` 的思路）。
- hub 集成：`steer: true` 对一个不支持插话的驱动 → `steered:false` ＋ `reason:"runtimeNoSteer"`；回合刚好结束 → `noActiveTurn`；对照现有 `crates/hub/tests/api.rs:666` 那条附件用例的位置补。
- GUI：`delivery.test.ts` 表驱动；`Sending` 的文案测试（错误码 → 中文，未知码 → 原文）。

---

## 4. ④不自动换方式重发；超时可见、可重试、重试不重复（050 §2-9）— **违反**

### 证据

**重试不是幂等的。** 失败后点「重试」＝重新 POST 一次，hub 现场生成一个新的 `MessageId`：

```ts
// apps/gui/src/outbox.ts:68-73
export function retry(key: number) { ... void deliver(item).catch(() => {}); }
```
```rust
// crates/core/src/event.rs:490-499
pub fn new(from: MessageSource, text: String) -> Self { Self { id: MessageId::new(), ... } }
```

`SendMessageRequest` 里没有客户端 id（`crates/api-types/src/lib.rs:160-177`），hub 也没有按内容去重。**对照**：agent 之间发消息的工具早就有幂等键（`crates/orchestrator/src/messaging.rs:354`，工具说明里写着「Retrying the same call is safe」），人这条路没有复用。

**超时会被当成「没发出去」。** 30 s 到点后 Electron 抛 `cannot reach the hub: …`（`electron/main.cts:163-166`），界面显示「没发出去 ＋ 重试」（`Session.tsx:681-690`）。而请求可能早已在 hub 里落了队列——特别是发给一个要冷启动的 agent（`deliver` 里 `self.start(key).await?` 会等到 CLI 起来；`runtime.rs:1318`）。于是「重试」= 两条一样的消息、两轮回答。

**「不自动换一种方式重发」也破了。** 插话写失败后，hub 自动把它改成排队（`runtime.rs:1190-1195`）。更糟的是驱动先记账再写：

```rust
// crates/drivers/src/claude_native/mod.rs:1191-1198
turn.steers.push(PendingSteer { id: message_id, snippet });
turn.steered.push(message_id);
self.shared.enqueue(permit, &message)
...
self.shared.wait_written(done).await
```

写失败时这条 id 已经记在「本轮插话」里了，hub 又把它当新消息排进队列；如果字节其实送到了 CLI，模型就会**读到两次**（一次当插话、一次当下一轮）。这条我没能证实可复现，见 §7。

**做得对的部分**：失败确实停在原地、有「重试」按钮，不自动重试；插话未被模型读到时会明确告诉人「模型没有读到这条插话，需要的话请重新发送」（`SessionSteer.tsx:46`、`claude_native/mod.rs:522`），这一处完全符合「不自动替人重发」。

### 用户能看到的后果

- 网络抖一下 / hub 冷启动慢，重试一次 → 同一个问题被回答两遍，多花一轮额度。
- 断线重连后同一句话出现两条（如果两条都进了队列）。
- 插话写失败时可能一边排在队列里、一边又作为插话被读到（同一句话两遍）。

### 建议改法

1. **客户端消息 id**：`SendMessageRequest` 加 `client_message_id: Option<MessageId>`（或 `idempotency_key: String`）。GUI 在 `outbox.sendMessage` 里生成一次，存进 `Outgoing`，`retry` 复用同一个。
2. **hub 去重**：`deliver` 前按 `(agent, client_message_id)` 查——已在等待/在跑就直接返回原来那条（和 `orchestrator::messaging` 同一套思路，代码可搬）。
3. **把「不知道」和「没发出去」分开**：超时 → `outcomeUnknown`（见 ③ 的表），文案说清「先别重发，稍等看它有没有出现」，并且**这一条不出重试按钮**（或者重试也带同一个 client id，天然不会重复）。
4. **插话失败不要自动换路**：驱动返回「写失败」时，回一个可见的失败（代价是这条消息要人决定要不要重发），而不是默默排队；只有「明确没开始」的两种情况（不支持插话、回合已结束）才可以自动排到下一轮，并且**必须带原因**。
5. 修 `claude_native::steer`：`wait_written` 失败时把 `turn.steers/steered` 里刚记的那条撤回，或者让 hub 在这一种错误上重试同一条插话而不是新建回合。

**范围**：`crates/api-types/src/lib.rs`、`crates/hub/src/runtime.rs`、`crates/hub/src/api.rs`、`crates/core/src/event.rs`（若事件也要带 client id）、`crates/drivers/src/claude_native/mod.rs`、`apps/gui/src/outbox.ts`、`apps/gui/src/api.ts`、`apps/gui/src/electron/main.cts`。

### 需要的测试

- hub 集成：同一 `client_message_id` 连发两次 → transcript 只有一条 turn、只起一轮；第一次已开始后再发 → 返回同一条，不新起一轮。
- hub 集成：插话写失败 → 队列里没有它，或返回可见失败（二选一，取决于选哪种改法），且不会同时出现在 `agent.steered` 和队列里。
- GUI 纯函数：`retry()` 复用同一个 client id；超时错误不显示「重试」或显示「不确定」文案。
- 驱动单测：`wait_written` 失败后 `turn.steers` 不含该 id。

---

## 5. ⑤人自己发起的中断不弹提示（050 §2-8）— **符合**

### 证据

- 中断就是 `session.interrupt()`，没有额外动作：`runtime.rs:1391-1401`；HTTP 是 `POST /v1/agents/{key}/cancel`（`api.rs:1152-1158`）。
- 回合以 `cancelled` 结束，界面只在轮尾写一行：`Session.tsx:1302` `cancelled: "本轮已中断"`，渲染在 `TurnFoot`（`Session.tsx:1311-1319`）。没有弹窗、没有确认框。
- 通知那一层只认「对话丢了上下文」这类连续性警告，与中断无关：`notices.ts:52-53` 只收 `isContinuityWarning`。
- 未读标记不把 `cancelled` 当异常：`activity.ts:189-192` 只在 `error / interrupted` 时记未读，`cancelled` 不算。
- GUI 整个项目（含 `electron/`）没有 `new Notification`、没有系统通知；站内条只有 `notices.ts` 那一种，且只收连续性警告。

对比 Cutex 的 bug ④（「已经中断，请继续发送」）：我们没有这句话，也没有等价物。

### 一个判断点（见 §7 第 5 条）

`— 本轮已中断` 是**该轮自己的停止原因**，不是提示；但如果主人认为「不留提示」也包括这一行，那就是唯一要动的地方。我没有替人决定，留在这里。

### 需要的测试

现在这条是「碰巧对」，没有测试锁住。补一个：

- GUI 纯函数/事件级：`cancel` → `agent.turn_completed{stop_reason:"cancelled"}` 之后，notice 列表为空、unread 不新增。
- 反向：`interrupted`（hub 重启导致的丢失）**要**记未读、**要**提示——现在的分支是对的（`activity.ts:191`），以后改这块时别把两者一起删掉。

---

## 6. 其余五条（050 §2-1/5/6/7/10）简评

| # | 不变式 | 结论 | 依据 |
|---|---|---|---|
| §2-1 | 内部锁忙不改变消息去向，只让它等、且看得见在等 | **部分** | 我们没有 Cutex 那把全机锁，但「去向」仍由**时序**决定：同一个键在 `status` 快照的哪一刻按下，会分别变成发送/排队/插话（`steer.ts:26-29` + `runtime.rs:1327`），而等在哪一步没有任何说明——只有「发送中…」（`Session.tsx:698`）。给冷启动中的 agent 发消息，HTTP 会一直挂到 `start()` 完成（`runtime.rs:1318` 等 `start` 的 agent 锁），Electron 30 s 超时就够把一次正常启动判成失败 |
| §2-5 | bridge worker 不阻塞等全机锁 | **不适用** | 没有全机锁、没有 bridge worker。最接近的是每个 agent 一把 `tokio::Mutex`（`runtime.rs:202`），`deliver` 等它时不持其他锁；没发现死锁路径，但我没有穷举 |
| §2-6 | 执行前在锁内复核暂停 | **不适用** | 我们没有「暂停」这一层；准入只有 `archived` / 附件 / `inbox` 三处检查，没有并发复核的语义 |
| §2-7 | 「锁忙」和「暂停」是两件事，原因码不同 | **不适用（但教训适用）** | 没有这两种状态；可对应的是「有回合在跑」vs「前面有人排队」vs「external agent」，而它们现在都只是 `queued: true`，见 ③ |
| §2-10 | 桌面错误日志记原始 code / reason / 原生错误（脱敏） | **违反** | 客户端一行都不记：`ApiError` 有 `code`（`api.ts:120-133`）但消息路径从不用它；全仓 `console.error` 只在扩展宿主里。hub 侧插话失败只有 `tracing::debug`（`runtime.rs:1191`）。出问题时人和维护者都没有依据 |

---

## 7. 不确定的地方（不猜）

1. **没有运行任何代码。** 上面每条路径都是读源码推出来的；「同一个回车下去向会变」是按 `deliver` 与 `start` 的先后推的，没有实测时序。
2. **插话写失败会不会真的让模型读到两次**（`claude_native/mod.rs:1191-1198`）：字节是否可能已经到达 CLI、只是 `wait_written` 报错，取决于写入实现。我没法只靠读代码确定；要么补一条驱动测试，要么先把 `turn.steers/steered` 的记账挪到写成功之后。
3. **30 s 够不够一次冷启动**：`REQUEST_TIMEOUT_MS = 30_000`（`allowed.cts:106`），而 `deliver` 会等 `start()` 把 CLI 拉起来。慢机器上 CLI 启动超过 30 s 时，④ 的「超时 → 重发 → 两条」就从理论问题变成常规问题。没测过。
4. **`agent.status` 与 `inbox.is_idle` 还有没有别的分叉点**：我确认了三种（消息投递后到 `TurnStarted` 之间、external agent、hub 重启后 `requeue`）。orchestrator 自己的发送路径我没逐条核。
5. **`本轮已中断` 算不算「提示」**（§5）：这是取舍，请人定。
6. **mock 与真 hub 是否一致**：`apps/gui/src/dev/MockBridge.ts:1005` 用 `a.status === "running"` 模拟忙，若按本文件改语义，mock 要一起改，否则 `preview:mock` 里的验收会骗人。
7. 我**没有**核对 `docs/SESSION-FIRST.md`、`docs/tasks/S1.md` 里对队列的旧约定是否与本文件建议冲突；改之前值得先读一遍。
8. **并发发送可能乱序、`queued` 可能都报 `false`**：`deliver` 取 `live` 后就把 map 锁放掉（`runtime.rs:1319-1327`），`is_idle()` 的判断、`publish` 和 `push` 都不在同一把锁里；两个请求（并排两个窗格、或 Home 与会话同时发）可能都在「队列为空」时通过判断，然后按调度顺序入队。同一条输入框有 `busy` 挡着（`SessionComposer.tsx:227`），所以只在多入口时出现。我按代码推断的，没有并发实测。

---

## 8. 可勾选清单（按建议优先级）

> 优先级按「人多久会撞上一次」×「撞上有多难受」排。前三条是 Cutex 报告里 ①③④ 的翻版，建议一起做。

- [ ] **P0-1　一条消息一行，状态原地变**（治 ②，顺带 ①）
  让主人自己发的消息从提交到结束只在聊天里占一行；「接下来」不再搬主人消息的本体；`agent.input_withdrawn` 留一行「已撤回」。
  范围：`outbox.ts`、`session.ts`、`views/Session.tsx`、`views/SessionComposer.tsx`；约 4 个文件。测试：事件序列下一行一状态的纯函数测试。

- [ ] **P0-2　插话失败要有原因，且不许静默换路**（治 ③ 的主症状）
  `steer_envelope` 把 `session.steer` 的错误分类带回响应/事件；GUI 新增 `lib/delivery.ts` 做码→中文；`runtimeNoSteer`/`noActiveTurn` 允许自动排下一轮但必须显示，`steerWriteFailed` 显示失败给人决定。
  范围：`api-types`、`core/event.rs`、`runtime.rs`、`api.rs`、`lib/delivery.ts`(新)+test、`outbox.ts`；约 6 个文件。测试：枚举穷举 + hub 集成对照三种插话失败。

- [ ] **P0-3　重试不重复：客户端消息 id＋hub 去重**（治 ④）
  `SendMessageRequest` 加 `client_message_id`；`outbox` 生成一次、重试复用；hub 按 `(agent, id)` 去重；超时映射成 `outcomeUnknown`，文案说清「可能已发出」。顺带把 `deliver` 的「算 queued」与「入队」放进同一把每-agent 锁（§7 第 8 条）。
  范围：`api-types`、`runtime.rs`、`api.rs`、`outbox.ts`、`api.ts`、`electron/main.cts`；约 6 个文件。测试：同 id 连发两次只有一条 turn；两条并发按发起顺序入队。

- [ ] **P1-4　输入框与「接下来」读同一份忙**（治 ①）
  先做最小版：`busy = running || queued.length > 0`，修 `composerHint/Placeholder` 对 external agent 的假话；再把 hub 的会话状态（`idle/starting/turn/held` + `queuedCount`）接进 transcript 快照。
  范围：`views/SessionComposer.tsx`、`lib/steer.ts`+test、`views/Session.tsx`、`Sidebar.tsx`、（第二步）`runtime.rs`/`api-types`；先 3 个文件。测试：`steer.test.ts` 补 busy 参数。

- [ ] **P1-5　「立即发送」原地提升，不再撤回重发**（治 ②/④ 的丢消息窗口）
  新路由 `POST /v1/agents/{key}/messages/{id}/send-now`：把现有这条挪到队首并中断，同 id；失败不动原消息。
  范围：`runtime.rs`、`api.rs`、`api-types`、`api.ts`、`allowed.cts`、`SessionComposer.tsx`；约 6 个文件。测试：提升后 id 不变；提升失败队列不变。

- [ ] **P2-6　发送失败走 `errors.ts`，并记原始码**（治 ③ 尾/§2-10）
  `Sending` 的错误改成「映射后的中文 + 原文进 title」；失败时 `console.error` 记 `{status, code, message}` 一次（脱敏）。
  范围：`views/Session.tsx`、`outbox.ts`、`lib/errors.ts`+test；3 个文件。测试：`errors.test.ts` 补送达错误。

- [ ] **P2-7　中断无提示加回归测试**（守住 ⑤）
  不加功能，只锁现状：`cancelled` 不产生 notice / 未读；`interrupted` 仍然产生。
  范围：`activity.ts` 的测试（新）、`notices` 相关纯函数。测试：一条事件级用例。

- [ ] **P3-8　驱动侧：`steer` 写失败不留记账**（治 ④ 的重复读取，需先确认 §7-2）
  把 `turn.steers/steered` 的 push 挪到写成功之后，或写失败时回滚。
  范围：`crates/drivers/src/claude_native/mod.rs`＋驱动测试。测试：写失败后 `turn.steers` 为空、hub 不会两条路径同时持有同一 id。
