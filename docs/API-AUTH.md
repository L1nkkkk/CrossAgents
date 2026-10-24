# 本地 API 与鉴权

> 2026-10-23 复核，对应 [TODO.md](TODO.md) §二。路由以 [crates/hub/src/api.rs](../crates/hub/src/api.rs) 为准，
> 调用方以 [apps/gui/src/api.ts](../apps/gui/src/api.ts)、[crates/cli/src/cmd](../crates/cli/src/cmd) 反查。
> 本文只描述现状与计划，改代码时先改这里。

## 1. 现状

hub 只监听 `127.0.0.1`（`[hub] listen` 尚未可配，A2 才放开），鉴权分三层：

| 层 | 中间件 | 凭据 | 谁用 |
|---|---|---|---|
| 免鉴权 | 无 | 无 | 存活探测 |
| 总 token | `require_token` | `~/.crossagents/token` | GUI（经 Electron 主进程）、CLI |
| 会话 token | `require_agent_token` | `~/.crossagents/agent-tokens.json` 里每 agent 一个 | 外部 agent 经 MCP 接入 |

要点：

- 总 token 是两段 uuid v4（64 位 hex），首次启动生成，比较用常数时间（`constant_time_eq`）。
- 令牌文件与 `hub.json`（端口）都在 `~/.crossagents/`。GUI 的令牌只在 Electron 主进程按需读取
  （[main.cts](../apps/gui/electron/main.cts)），页面通过 IPC 转发，拿不到令牌本身。
- hub 重启会换新 token，GUI 每次调用重读，所以无需重启 GUI。
- 会话 token 落盘（`agent-tokens.json`）是为了 hub 重启接管后，已接管的 agent 仍能调工具；加入时签发新 token，旧的立即失效。

2026-10-24 起有了梯度（[tasks/DOGFOOD.md](tasks/DOGFOOD.md) §7）：`require_token` 先认凭据——总 token 为 admin、`<home>/token-read` 为 read、会话 token 只到 `/v1/me*`——再由 `crates/hub/src/access.rs` 的策略点按下表的级别放行，级别不够返回 403 `forbidden` 并写明缺哪一级。

## 2. 接口清单

级别按"读 / 写 / 管理"划分，用于后续拆分凭据。等级是目标，不是现状。

### 2.1 免鉴权

| 方法 | 路径 | 级别 | 返回 | 调用方 |
|---|---|---|---|---|
| GET | `/v1/health` | 探测 | `HealthResponse`（仅 `status`、`api`） | CLI `hub status` / `hub start` 的存活探测 |

`/v1/health` 只回 `status` 和 `api`。`version`、`uptime_secs`、`pid`、`home` 已收进需鉴权的 `/v1/info`：
A2 放开 `listen` 后，健康检查不能再白送版本指纹和运行时长。

### 2.2 总 token

| 方法 | 路径 | 级别 | 调用方 |
|---|---|---|---|
| GET | `/v1/info` | read | GUI、CLI |
| GET | `/v1/agents` | read | GUI、CLI |
| POST | `/v1/agents` | write | GUI、CLI |
| GET | `/v1/agents/{key}` | read | CLI |
| DELETE | `/v1/agents/{key}` | write | GUI、CLI |
| POST | `/v1/agents/{key}/start` | write | CLI |
| POST | `/v1/agents/{key}/stop` | write | GUI、CLI |
| POST | `/v1/agents/{key}/cancel` | write | GUI、CLI |
| POST | `/v1/agents/{key}/compact` | write | GUI |
| POST | `/v1/agents/{key}/messages` | write | GUI、CLI |
| DELETE | `/v1/agents/{key}/messages/{id}` | write | GUI |
| GET | `/v1/agents/{key}/transcript` | read | GUI、CLI |
| GET | `/v1/agents/{key}/turns/{id}/diff` | read | GUI、CLI |
| GET | `/v1/agents/{key}/attachments/{id}` | read | GUI |
| POST | `/v1/agents/{key}/profile` | write | GUI |
| POST | `/v1/agents/{key}/settings` | write | GUI |
| POST | `/v1/agents/{key}/model` | write | GUI |
| POST | `/v1/agents/{key}/name` | write | GUI |
| POST | `/v1/agents/{key}/topic` | write | GUI |
| POST | `/v1/agents/{key}/archive` | write | GUI |
| POST | `/v1/agents/{key}/connect` | write | GUI |
| GET | `/v1/notes` | read | GUI |
| GET | `/v1/plugins` | read | GUI、CLI |
| POST | `/v1/plugins/{name}/models` | write | GUI |
| GET | `/v1/usage` | read | GUI、CLI |
| GET | `/v1/accounts` | read | GUI、CLI |
| POST | `/v1/accounts` | admin | GUI、CLI |
| DELETE | `/v1/accounts/{id}` | admin | GUI、CLI |
| GET | `/v1/accounts/{id}` | read | CLI |
| GET | `/v1/accounts/{id}/status` | read | CLI |
| GET | `/v1/accounts/{id}/usage` | read | GUI、CLI |
| GET | `/v1/accounts/{id}/models` | read | CLI |
| POST | `/v1/accounts/{id}/login` | admin | GUI、CLI |
| GET | `/v1/quota` | read | GUI |
| GET | `/v1/questions` | read | GUI |
| POST | `/v1/questions/{id}/answer` | write | GUI |
| POST | `/v1/questions/{id}/dismiss` | write | GUI |
| GET | `/v1/runs` | read | GUI、CLI |
| POST | `/v1/runs` | write | GUI、CLI |
| GET | `/v1/runs/{key}` | read | GUI、CLI |
| POST | `/v1/runs/{key}/approve` | write | GUI、CLI |
| POST | `/v1/runs/{key}/reject` | write | GUI、CLI |
| POST | `/v1/runs/{key}/cancel` | write | GUI、CLI |
| POST | `/v1/runs/{key}/land` | write | GUI |
| POST | `/v1/repos/init` | write | GUI |
| POST | `/v1/runs/{key}/finish` | write | GUI |
| GET | `/v1/runs/{key}/diff` | read | GUI |
| GET | `/v1/runs/{key}/tasks/{task}/diff` | read | GUI |
| POST | `/v1/runs/{key}/tasks/{task}/review` | write | GUI |
| GET | `/v1/events` | read | GUI（SSE）、CLI |
| POST | `/v1/shutdown` | admin | CLI `hub stop` |

### 2.3 会话 token

| 方法 | 路径 | 级别 | 调用方 |
|---|---|---|---|
| GET | `/v1/me` | read | 外部 agent（MCP） |
| GET | `/v1/me/tools` | read | 外部 agent（MCP） |
| POST | `/v1/me/tools/{name}` | write | 外部 agent（MCP） |

会话 token 只认自己那一个 agent，不认总 token，也不能访问 `/v1/agents` 等下表中的管理面。

## 3. 分级口径

**read**：只读，不改变任何状态。可安全开放给远程设备（A2 的只读设备）。

**write**：改变会话、run、界面状态，但不动凭据、不停 hub。

**admin**：能碰凭据或 hub 生命周期——`/v1/shutdown`、账号增删与登录。这些要么能中断所有人的工作，要么能写入新的 API 凭据。

两个边界判断：

- `POST /v1/agents/{key}/connect` 归 write 而非 admin：它签发的是该 agent 自己的会话 token，作用域被 `require_agent_token` 限住，拿不到别的 agent 和总 token。
- `POST /v1/plugins/{name}/models` 归 write 而非 admin：只影响界面显示哪些模型，不改账号能力。

## 4. 待改造

按 [TODO.md](TODO.md) §二 的条目推进，顺序是清单 → 分级 → 收口。

- 把 `require_token` 抽成策略点，按 read / write / admin 校验；路由表只标级别，不写鉴权逻辑。
- `ErrorCode` 增 `Forbidden`：越权返回 403 而不是 401。401 表示"没带凭据或凭据无效"，403 表示"凭据有效但级别不够"，GUI 据此置灰入口并说明原因。
- Electron 主进程维护"方法 + 路径"白名单，只放行 GUI 实际用到的接口（已做）。这把收窄放在主进程，页面即使被注入了也无法调用 `/v1/shutdown`。
- A1 引入 users / devices 后，策略点换成"设备或用户 → 级别"的映射，路由表和白名单都不动。

### Windows 上的令牌文件

`write_private` 只在 Unix 分支设 `0o600`，Windows 分支是裸 `fs::write`。核查结论：`~/.crossagents` 不在任何云同步目录内，
NTFS 默认 ACL 已限制到本用户、SYSTEM 和 Administrators，同机其他用户读不到。故暂不加 ACL 代码，仅在 home 被移动到同步目录时再处理。

## 5. 版本协商

`API_VERSION` 定义在 [crates/api-types/src/lib.rs](../crates/api-types/src/lib.rs)，目前是 `v1`。

### 何时升版

算破坏性变更，必须把 `API_VERSION` 加一：

- 删除或重命名字段、接口、枚举取值
- 改字段含义或单位（例如 `uptime_secs` 改成毫秒、`started_at` 从本地时间改 UTC）
- 改路径、方法或状态码含义
- 新增必填请求字段，或把可选字段改成必填
- 收紧校验，导致旧客户端原本合法的请求被拒

不算破坏性变更，不升版：

- 新增可选字段或新接口
- 新增错误码取值、新增枚举取值（前提是客户端忽略未知值，这点是硬性要求）
- 放宽校验

### GUI 侧比对

- `api` 不等 → 拒绝连接，提示 hub 与 GUI 版本不匹配、需一起更新。不静默降级：字段缺失导致的报错会更难查。
- `version` 不等但 `api` 相同 → 允许连接，在侧栏版本号旁标出不一致，隐藏该版本没有的功能入口。
