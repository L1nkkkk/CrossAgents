# 进程监督：agent 进程比 hub 活得久

> 状态：已实现（2026-10-23）· 参考 Cutex 的做法，见 TODO §一

## 1. 目标

hub 停止、崩溃或升级都不打断正在干活的 agent。新起来的 hub 把它们接管回来，正在跑的那一轮照常跑完。agent 自己挂了，它的整棵进程树一起清掉，不留孤儿。

## 2. 结构

```
hub ──TCP(127.0.0.1)──> runtime host ──stdio──> codex app-server / claude -p
                         │                         └─ shell、MCP、dev server、构建……
                         └─ Windows Job Object（KILL_ON_JOB_CLOSE），句柄由 host 自己持有
```

- 运行时宿主（runtime host）是 `crossagents runtime-host --dir <home>/runtimes --id <id>`，hub 用 `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB` 拉起，所以它不跟着 hub 或终端一起退出。
- 宿主先把自己放进一个新的 Job Object，再启动 agent，这样 agent 的所有后代进程都在这个 job 里。宿主一退出（不管什么原因），系统就把整棵树杀掉。
- 宿主把 agent 的 stdout/stderr 按行编号（`seq`）发给当前连着的 hub，并保留 hub 还没确认的部分。hub 重连后先补发这些，再接着实时转发。协议见 `crates/drivers/src/host/wire.rs`。
- `<home>/runtimes/<id>.json` 是宿主的登记：pid、端口、一次性令牌、`owner`（谁可以接管）、隔离方式。启动参数（含环境变量）写在 `<id>.spec.json` 里，宿主读完就删，不出现在命令行上。

用宿主的只有能接管的驱动：`codex-native`（按 Profile 加环境分 owner）和 `claude-native` / `codebuddy-native`（按会话 id 分 owner）。`acp-generic` 仍是 hub 的直接子进程，但也放进 hub 持有的 Job Object，hub 一退整棵树都会被清掉。

配置：`config.toml` 里的 `[hub] runtime_hosts = false` 可以关掉宿主，全部回到直接子进程。测试默认关闭（`HubConfig::new`）。

## 3. hub 退出

- `crossagents hub stop`（默认）：hub 调 `host::leave()`，所有宿主连接停止读写，会话不再关 stdin、也不发 Kill。宿主里的 agent 继续跑，输出留在宿主里等下一个 hub。非宿主的会话照旧 stop。
- `crossagents hub stop --agents`（`POST /v1/shutdown?stop_agents=true`）：和以前一样，先停掉所有 agent 再退出。
- hub 崩溃：等同于默认的 leave，连接断开不算失败。

## 4. hub 启动（`Runtime::recover`）

1. `host::discover()`：读所有登记，用令牌握手。能握手的留着等认领，握不上的说明宿主已不在，删掉登记。不按 pid 杀任何进程，所以 pid 被系统复用也不会误杀。
2. 从 `hub-state.json` 记录的事件位置往后扫，算出每个 agent 没收尾的事：已开始没结束的那一轮，以及已投递还没开始的消息。
3. 上次处于活动状态的 agent，调驱动的 `adopt`：
   - 接管成功：会话照常装上，状态是 `running` 或 `idle`，并发布 `agent.adopted`。没结束的那一轮由 `adopted_turn` 等它跑完，结束原因照实记录。
   - 没有可接管的进程（宿主已死、被人杀了）：agent 标成 `stopped`。没结束的那一轮记为 `agent.turn_completed { stop_reason: "interrupted" }`，并发一条说明。这一轮不会自动重跑，结果可能已经部分生效（命令执行过、文件改过），是否重来交给人或 Planner 决定。
4. 已投递还没开始的消息重新排回队列（agent 会照常启动或 resume），不会重复发布 `agent.input`。
5. 没人认领的宿主通过各自的连接发 Kill，停掉。
6. 协作工具的令牌存在 `agent-tokens.json`，被接管的 agent 挂载的 `crossagents mcp` 重启后仍能用原令牌访问 hub。

## 5. 退出原因

`SessionEventKind::Exited` 和新事件 `agent.exited` 带上 `cause`：

| cause | 含义 |
|---|---|
| `exited` | 自己退出，退出码 0 |
| `crashed` | 自己退出，退出码非 0 |
| `killed` | CrossAgents 让它停的 |
| `lost` | 宿主不告而别（被外部杀掉） |

stop 或进程意外退出时，队列里还没开始的消息会被撤回（`agent.input_withdrawn`），不会在下次启动时又冒出来。

## 6. 和 Cutex 的差异

- Cutex 在守护进程之外还有一个由计划任务拉起的 supervisor。我们这一步只做到“hub 不在时 agent 照常跑”，hub 自身的常驻和开机自启留给 TODO §三。
- 宿主等待 hub 读取退出结果的时间有上限：没有 hub 时最多 30 分钟；hub 连着时等 30 秒的确认；hub 主动要求杀掉的进程，连接一断就退出。

## 7. 已知限制

- 宿主会继承拉起它的进程的可继承句柄。前台 `hub run | …` 的管道要等 agent 结束才会关闭；`hub start` 拉起的 hub 已经清掉了这些句柄，不受影响。
- 接管 codex 时，如果正在跑的那一轮的 id 一直没在后续消息里出现，就无法向 Codex 发 interrupt，只能在本地结束等待。
- 未在 Linux/macOS 上实测：宿主在这些平台上用进程组代替 Job Object。
