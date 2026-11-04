# CodeBuddy CLI 实测：轮中写入的 user 消息（steer）怎么处理

> 2026-11-05 · CodeBuddy CLI **2.179.2**（`@tencent-ai/codebuddy-code`，Windows x64）· 模型 `deepseek-v4.1-flash` · 已登录（`apiKeySource: copilot.tencent.com`）· 走 `HTTPS_PROXY=http://127.0.0.1:7890`
>
> 探针脚本是仓库外的临时文件 `C:/tmp/steer-probe.mjs`（未入库，只用于这次实测）；原始 NDJSON 在 `C:/Users/Link/AppData/Local/Temp/steer-*.ndjson`。下面的事件摘录做了字段裁剪（只留 `type` / `subtype` / `uuid` / `isReplay` / 文本），行首是相对进程启动的秒数。
> 同一套控制协议的 Claude 侧结论见 [claude-stream-json.md](claude-stream-json.md)。

## 1. 结论

| 问题 | 实测答案 |
|---|---|
| (1) 轮中再写一条 user 消息，在哪读到？ | **不在工具边界读，也不是被忽略——是等本轮的模型循环结束后才被消费，然后另起一轮**（同一个进程、同一个 session）。在工具执行中写、在工具结果刚回来时写，行为一样：那条消息一直躺在 stdin 的缓冲区里，本轮 4 个工具照跑不误。 |
| (2) 这一轮出几个 `result`？ | **一条 user 消息一个 `result`**。本轮跑了 4 个工具调用，仍然只有 **1** 个 `result`。被插进来的那条消息另起一轮、再出 **1** 个 `result`，所以「轮中插一句」在同一个进程里会看到 **2 个 `result`**，两个 `subtype` 都是 `success`。 |
| (3) 消息在最后一次模型调用之后才到？ | **仍然不并入本轮**。最终文本已经打出来了、`result` 还没打出来时写进去，它会正常成为下一轮：`system/init`（新一轮）→ 回显 → 上一轮的 `result` → 新一轮正文 → 新 `result`。本轮答案不受影响，也不会被重写。 |
| (4) 读到时 stdout 有什么信号？ | 加 `--replay-user-messages` 时，消息**被消费的那一刻**（不是写入的那一刻）回显一条 `type:"user"` 事件，带 `"isReplay": true`、原文文本块、CLI 新生成的 `uuid` 和新的 `_requestId`；紧挨着它前面是一条新的 `system/init`（`session_id` 与首轮相同）。**不加**这个开关就没有任何回显，只剩那条 `system/init` + `system/status` 能表明「下一轮开始了」。 |

一句话：**CodeBuddy 的 stream-json 输入没有轮中插话的语义，只有排队**。轮中写入等于「排在本轮之后的下一条消息」，模型在本轮里看不到它。

## 2. 怎么测的

```
node <bin>/codebuddy  -p \
  --input-format stream-json --output-format stream-json --verbose \
  --replay-user-messages --dangerously-skip-permissions \
  --model deepseek-v4.1-flash --session-id <uuid>
```

（Windows 上 `codebuddy` 只是 npm 的 `.cmd` 壳，Node 直接 spawn 会 ENOENT，所以探针用 `node <...>/@tencent-ai/codebuddy-code/bin/codebuddy` 起进程。）

stdin 写的是 Claude 侧同款结构，`uuid` 由探针自己指定，以便检查 CLI 是否原样回显：

```json
{"type":"user","uuid":"<self>","session_id":"<session>","parent_tool_use_id":null,
 "message":{"role":"user","content":"…"}}
```

第一条 prompt 要求模型**分别**调用 4 次 Bash（`echo STEP1` / `sleep 3; echo STEP2` / `sleep 3; echo STEP3` / `echo STEP4`），制造多个工具边界和约 12 秒的可插入窗口。steer 消息的正文是「立刻停止剩余步骤，只回复一行：STEERED-OK」，这样只要它生效就一定能看出来。

| 场景 | 什么时候写第二条 | 目的 |
|---|---|---|
| `during` | 单个 `sleep 12` 工具**执行中**（写入时刻 8.36s） | 工具在跑时 stdin 到底读不读 |
| `tool` | 第一个 `tool_result` 回来的**工具边界**（5.56s） | 是不是工具边界读 |
| `late` | 最后一次 `tool_result` 之后、最终文本刚出（17.57s） | 最后一次模型调用之后才到 |
| `post` | `result` 之后（18.52s） | 轮与轮的基线 |
| `pre` | 两条消息在进程启动前一起写进 stdin | 会不会合并成一轮 |

## 3. 原始事件摘录

### 3.1 工具执行中写入（`during`）——最决定性的一次

```
0.01s >>> WRITE 只做一件事：调用 Bash 运行 `sleep 12; echo LONGDONE`…
3.25s <<< system/init
3.29s <<< user/ ECHO isReplay=True  text="只做一件事…"
5.35s <<< assistant/ blocks=['thinking']
5.35s <<< assistant/ blocks=['tool_use']
8.36s >>> WRITE [STEER-PROBE] 立刻停止剩余步骤…只回复一行：STEERED-OK   ← 工具还在跑
17.47s <<< user/ tool_result                       ← sleep 12 结束
19.41s <<< assistant/ blocks=['text'] text='输出原样如下：```LONGDONE```…'
19.43s <<< system/init                              ← 新一轮
19.47s <<< user/ ECHO isReplay=True text="[STEER-PROBE]…"   ← 到这里才被读走
19.64s <<< result/success num_turns=6  result='输出原样如下：…'
20.74s <<< assistant/ blocks=['text'] text='STEERED-OK'
20.91s <<< result/success num_turns=7  result='STEERED-OK'
```

写进去到被读走隔了 **11 秒**（8.36 → 19.47），中间工具照常跑完、本轮照常给出原答案。若要「工具边界读」，这时早该有反应。

### 3.2 工具边界写入（`tool`）

```
5.56s <<< user/ tool_result                 ← 第 1 个工具结果
5.56s >>> WRITE [STEER-PROBE]…STEERED-OK    ← 就在这个边界写
7.23s <<< assistant/ blocks=['tool_use']    ← 第 2 个工具照发
10.36s <<< user/ tool_result
11.74s <<< assistant/ blocks=['tool_use']
14.82s <<< user/ tool_result
16.35s <<< assistant/ blocks=['tool_use']
16.41s <<< user/ tool_result
17.50s <<< assistant/ blocks=['text'] text='四次调用都按顺序执行完毕，输出依次是 STEP1、STEP2、STEP3、STEP4。'
17.53s <<< system/init
17.54s <<< user/ ECHO isReplay=True text="[STEER-PROBE]…"
17.54s <<< system/status
17.72s <<< result/success num_turns=12 result='四次调用都按顺序执行完毕…'
19.62s <<< assistant/ blocks=['text'] text='STEERED-OK'
19.79s <<< result/success num_turns=13 result='STEERED-OK'
```

### 3.3 最后一次模型调用之后写入（`late`）

```
16.34s <<< user/ tool_result
17.57s <<< assistant/ blocks=['text'] text='四次调用依次输出 STEP1、STEP2、STEP3、STEP4，全部成功。'   ← 最终答案已出
17.57s >>> WRITE [STEER-PROBE]…STEERED-OK
17.61s <<< system/init
17.62s <<< user/ ECHO isReplay=True text="[STEER-PROBE]…"
17.66s <<< system/status
17.79s <<< result/success num_turns=12 result='四次调用依次输出…全部成功。'
18.84s <<< assistant/ blocks=['thinking']
19.00s <<< assistant/ blocks=['text'] text='STEERED-OK'
19.01s <<< result/success num_turns=14 result='STEERED-OK'
```

注意顺序：**下一轮的 `system/init` 和回显先于上一轮的 `result` 出现**。按 `result` 判定「本轮结束」的客户端会晚一步。

### 3.4 `result` 之后写入（`post`）——基线

```
18.33s <<< result/success num_turns=11 result='四次调用的输出依次为…'
18.52s >>> WRITE [STEER-PROBE]…STEERED-OK
18.55s <<< system/init
18.55s <<< user/ ECHO isReplay=True text="[STEER-PROBE]…"
18.60s <<< system/status
19.69s <<< assistant/ blocks=['text'] text='STEERED-OK'
19.85s <<< result/success num_turns=13 result='STEERED-OK'
```

与 3.1–3.3 完全同构：**轮中写和轮后写在 CLI 眼里没有区别**，都是下一轮。

### 3.5 启动前就把两条都写进去（`pre`）

```
0.01s >>> WRITE 请依次做四个独立的工具调用…
0.01s >>> WRITE [STEER-PROBE]…STEERED-OK
…第一轮 4 个工具跑完…
18.51s <<< assistant/ blocks=['text'] text='四次调用依次输出 STEP1、STEP2、STEP3、STEP4，全部成功。'
18.53s <<< system/init
18.55s <<< user/ ECHO isReplay=True text="[STEER-PROBE]…"
18.79s <<< result/success num_turns=12
23.41s <<< assistant/ blocks=['text'] text='STEERED-OK：第 1–4 步此前已全部完成，因此没有剩余步骤需要停止。'
23.43s <<< result/success num_turns=14
```

两条消息不会合并——CLI 按行顺序一轮一轮地跑，模型在第二轮才看到「停止」这句话，答案也如实说了「已全部完成」。

### 3.6 不加 `--replay-user-messages`（`tool` 同场景）

```
4.79s >>> WRITE [STEER-PROBE]…STEERED-OK
…本轮照旧跑完 4 个工具…
17.68s <<< assistant/ blocks=['text'] text='四次调用均成功，依次输出：STEP1、STEP2、STEP3、STEP4。'
17.71s <<< system/init          ← 唯一能看出「有新消息被消费」的事件
17.73s <<< system/status
17.87s <<< result/success num_turns=13
21.24s <<< assistant/ blocks=['text'] text='STEERED-OK'
21.26s <<< result/success num_turns=15
```

## 4. 顺带确认的细节

| 项 | 实测 |
|---|---|
| 自定义 `uuid` | **不被保留**。写进去的 `uuid` 在任何输出里都不出现；回显事件的 `uuid` 是 CLI 自己生成的（等于它的 `messageId`），`_requestId` 也是新的。这一点与 Claude Code 不同（那边 hub 指定的 uuid 会原样进历史、可用于回退/分叉）。 |
| 回显的判定字段 | `isReplay: true` + `message.content[0].text` 原文；`parent_tool_use_id` 为 null。光看 `uuid` 匹配不到自己写的那条。 |
| `system/init` | **每一轮都重发一条**，`session_id`、`uuid`、`cwd`、`model`、`tools` 与首轮一致。它是「一次请求开始」的标记，不是「新会话」。 |
| `result` 的个数 | 每条 user 消息恰好 1 个；一轮里工具调用再多也不增加。 |
| `num_turns` | 在同一个进程里**持续累加**（11 → 13 → 15），不是本轮轮数，别拿它数轮次。 |
| 进程存活 | 一轮结束、`result` 打出来后进程**不退出**，继续等 stdin（实测静置 100 秒仍在）。 |
| stdin 读取时机 | 只在轮与轮的间隙读。工具执行中、工具结果回传时都不读——所以「写进去了」不等于「CLI 收到了」。 |

## 5. 对 CrossAgents 的含义

- 想在 CodeBuddy 上做「轮中插话、模型立刻改主意」是做不到的：CLI 不会在本轮的模型循环里重新读 stdin。hub 侧唯一能兑现的语义是**排队**——把消息写进去，本轮答案照旧，下一轮再处理。UI 上必须说清是「排队到本轮之后」，否则用户会以为已生效（本轮答案里根本没有那条指令）。
- 想要真正的「打断」，跟 steer 是两件事：那需要另找 CodeBuddy 的控制消息（`control_request` 之类）或直接杀进程，本次未测，不能拿本机结论外推。
- hub 若在轮中写入，要预期**多出一次 `result`**：一次用户操作 → 两条 user 消息 → 两个 `result`。会话落盘时别把它当成「同一轮的两个结果」。
- 要判断「排队的那条被消费了」，**不要**用自定 uuid：用 `user` 事件的 `isReplay: true` 加文本匹配，或者干脆用紧邻其前的那条新的 `system/init`（同 `session_id`）。这两者在无 `--replay-user-messages` 时只剩后者。
- 由于下一轮的 `system/init`/回显可能出现在上一轮 `result` **之前**，把 `result` 当作「本轮彻底结束、可以发下一条」的判据是安全的但偏晚；把 `init` 当轮首则更早，但要注意首轮也有一个 `init`。

**不可以开 steer。**
