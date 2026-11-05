# 流式规则（stream rules）

TODO §25-4。规则平时不进上下文；agent 的输出或工具调用命中规则的正则时，hub 把这条规则送进当前回合。参考：oh-my-pi 的 [ttsr-injection-lifecycle](https://github.com/can1357/oh-my-pi/blob/b7221d994869ed36a81b44e60827786f572498ed/docs/ttsr-injection-lifecycle.md)，但去重按回合数算，见下文。

## 规则文件

仓库根的 `.crossagents/rules/*.md`（没有 `.git` 的目录则用工作目录本身）。一个文件一条规则：

```markdown
---
id: no-sleep-wait
match: sleeping
match: "retry (\\d+) times"
on: output
---

别用 sleep 等结果：用轮询或事件，然后接着做。
```

frontmatter 字段：

- `id`：规则名，默认文件名（不含 `.md`）。同名的第二条跳过并记 warn。
- `match`：正则，可多条。写法一是每行一个 `match: …`，写法二是单独一行 `match:` 后面跟 `- …` 列表。值两端的引号会去掉，其余原样（不做转义）。
- `on`：`output`（默认，只看 agent 的文字输出）、`tool`（只看工具调用：工具名/类型加上参数文本）、`both`。

正文（frontmatter 之后）就是要送给 agent 的规则文本，去掉首尾空白。正文为空、没有 `match`、`on` 值不认识、frontmatter 没闭合、正则编译不过的文件：记一条 warn 并跳过那条规则，会话照常启动。

## 什么时候送、怎么送

- 会话启动时读一次规则。输出按文本块匹配，并保留本回合输出的末 4 KB，所以跨块的匹配也能命中；工具调用按它的名字与参数文本匹配。
- 命中后，若驱动支持插话（steer），规则作为插话送进正在跑的回合，界面上是 `agent.steered` 事件。
- 驱动不支持插话（或插话失败）时，hub 中断当前回合（`interrupt`），规则作为下一条消息排在等待队列最前面运行，界面上是消息来源为 `Notice` 的 `agent.input`。被打断的那一回合的输出不会自动重发，只有规则本身作为新一轮的输入。
- 送进去的文本以 ``CrossAgents rule `<id>` (from the project's stream rules; …)`` 开头，所以界面上看得出是规则。

## 去重

- 同一条规则在同一会话里，**20 个回合之内只送一次**：在回合 `t` 送过，回合 `t+1` 到 `t+19` 不再送，回合 `t+20` 起可以再送。回合按会话开始的回合计数，不按时间。
- 同一回合内，一条规则只送一次（即使输出和工具调用都命中）。
- 会话的上下文被压缩（`POST /v1/agents/{key}/compact` 成功）之后，所有规则重新算作没送过。
- 去重状态只在会话内存里：会话停了、换了新进程，就重新开始。

## 扩展（接口）

`RuleSource` trait（`crates/hub/src/stream_rules.rs`）是规则来源的接口。项目文件（`ProjectRuleFiles`）是第一个实现。扩展要加来源，实现这个 trait 并把它放进 `load_from` 的来源列表即可；这一版没有接任何扩展，也没有扩展侧的实现。

## 代码位置

- `crates/hub/src/stream_rules.rs`：解析、加载、匹配、去重（纯逻辑，单元测试在文件末尾）。
- `crates/hub/src/runtime.rs`：接入。`forward_events` 按事件调用 `StreamRules`，`inject` 负责插话或中断重发，`Runtime::compact` 通知会话已压缩。
- `crates/mock-agent/tests/stream_rules.rs`：集成测试。

## 没做或没验证的

- 只用 mock agent 测过（stream-json 的 `--steer-merge` 路径，以及默认 ACP mock 的不能插话路径）。真实 Claude / Codex 的插话行为没实测。
- 只看 hub 主动发起的压缩。驱动自己压缩（比如 CLI 自动压缩）时没有事件，去重状态不会重置。
- 界面没在真实 GUI 里看过；插话事件本身没有 `from` 字段，规则靠消息正文的标题区分。
- 扩展来源没有接。
