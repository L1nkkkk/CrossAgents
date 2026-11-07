# 本地 API 与鉴权

> 2026-10-23 复核，对应 [TODO.md](TODO.md) §二。路由以 [crates/hub/src/api.rs](../crates/hub/src/api.rs) 为准，
> 调用方以 [apps/gui/src/api.ts](../apps/gui/src/api.ts)、[crates/cli/src/cmd](../crates/cli/src/cmd) 反查。
> 本文只描述现状与计划，改代码时先改这里。

## 1. 现状

hub 只监听 `127.0.0.1`（`[hub] listen` 尚未可配，A3 才放开；A2 走 SSH 隧道），鉴权分五层：

| 层 | 中间件 | 凭据 | 谁用 |
|---|---|---|---|
| 免鉴权 | 无 | 无 | 存活探测 |
| 总 token | `require_token` | `~/.crossagents/token` | GUI（经 Electron 主进程）、CLI |
| 设备 token | `require_token` | `cad_` + 64 位 hex；库里只存 SHA-256 | client 按 read / write / admin；node 仅节点通道（B1） |
| 会话 token | `require_agent_token` | `~/.crossagents/agent-tokens.json` 里每 agent 一个 | 外部 agent 经 MCP 接入 |
| 扩展 token | `require_token` | 每次扩展工具调用现签，只在 hub 内存里，进程结束作废 | 扩展工具进程回调 hub |

要点：

- 总 token 是两段 uuid v4（64 位 hex），首次启动生成，比较用常数时间（`constant_time_eq`）。
- 令牌文件与 `hub.json`（端口）都在 `~/.crossagents/`。GUI 的令牌只在 Electron 主进程按需读取
  （[main.cts](../apps/gui/electron/main.cts)），页面通过 IPC 转发，拿不到令牌本身。
- hub 重启会换新 token，GUI 每次调用重读，所以无需重启 GUI。
- 会话 token 落盘（`agent-tokens.json`）是为了 hub 重启接管后，已接管的 agent 仍能调工具；加入时签发新 token，旧的立即失效。

2026-10-24 起有了梯度（[tasks/DOGFOOD.md](tasks/DOGFOOD.md) §7）：`require_token` 先认凭据——总 token 为 admin、`<home>/token-read` 为 read、会话 token 只到 `/v1/me*`——再由 `crates/hub/src/access.rs` 的策略点按下表的级别放行，级别不够返回 403 `forbidden` 并写明缺哪一级。

## 2. 接口清单

级别按「读 / 写 / 管理」划分，由 [access.rs](../crates/hub/src/access.rs) 的 `required` 执行：读是 read，改状态是 write，碰凭据或 hub 的生命周期是 admin。

### 2.1 免鉴权

| 方法 | 路径 | 级别 | 返回 | 调用方 |
|---|---|---|---|---|
| GET | `/v1/health` | 探测 | `HealthResponse`（仅 `status`、`api`） | CLI `hub status` / `hub start` 的存活探测 |

`/v1/health` 只回 `status` 和 `api`。`version`、`uptime_secs`、`pid`、`home` 已收进需鉴权的 `/v1/info`：
A3 放开 `listen` 后，健康检查不能再白送版本指纹和运行时长。

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
| GET | `/v1/agents/{key}/execution` | read | GUI |
| POST | `/v1/agents/{key}/execution/interrupt` | write | GUI |
| GET | `/v1/agents/{key}/resume-command` | read | GUI |
| GET | `/v1/notes` | read | GUI |
| GET | `/v1/plugins` | read | GUI、CLI |
| POST | `/v1/plugins/{name}/models` | write | GUI |
| POST | `/v1/plugins/{name}/install` | admin | GUI |
| POST | `/v1/plugins/{name}/upgrade` | admin | GUI |
| POST | `/v1/plugins/refresh` | admin | GUI |
| GET | `/v1/extensions` | read | GUI |
| GET | `/v1/extensions/{name}/logs` | read | GUI；与 `extension_logs` 相同的日志尾部 |
| POST | `/v1/extensions/{name}/enable` | admin | GUI |
| POST | `/v1/extensions/{name}/disable` | write | GUI |
| POST | `/v1/extensions/{name}/auto-trust` | admin | GUI |
| GET | `/v1/extensions/{name}/pending/diff` | read | GUI |
| POST | `/v1/extensions/{name}/tools/{tool}` | write | GUI（面板） |
| GET | `/v1/extensions/calls/{id}` | read | GUI（面板） |
| DELETE | `/v1/extensions/calls/{id}` | write | GUI（面板） |
| GET | `/v1/usage` | read | GUI、CLI |
| GET | `/v1/devices` | read | GUI、CLI |
| POST | `/v1/devices` | admin | CLI；默认 client / write，返回 `{ device, token }`，token 只显示一次。本轮 GUI 不签发（[allowed.cts](../apps/gui/electron/allowed.cts)） |
| DELETE | `/v1/devices/{id}` | admin | GUI、CLI；幂等，返回吊销后的设备 |
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
| DELETE | `/v1/runs/{key}?branches=&force=` | write | GUI、CLI |
| POST | `/v1/runs/{key}/approve` | write | GUI、CLI |
| POST | `/v1/runs/{key}/reject` | write | GUI、CLI |
| POST | `/v1/runs/{key}/cancel` | write | GUI、CLI |
| POST | `/v1/runs/{key}/land` | write | GUI |
| POST | `/v1/repos/init` | write | GUI |
| POST | `/v1/runs/{key}/finish` | write | GUI |
| GET | `/v1/runs/{key}/diff` | read | GUI |
| GET | `/v1/runs/{key}/trace` | read | GUI、CLI |
| GET | `/v1/runs/{key}/tasks/{task}/diff` | read | GUI |
| POST | `/v1/runs/{key}/tasks/{task}/review` | write | GUI |
| GET | `/v1/events` | read | GUI（SSE）、CLI |
| POST | `/v1/shutdown` | admin | CLI `hub stop` |
| GET | `/v1/terminals` | admin | GUI |
| POST | `/v1/terminals` | admin | GUI |
| DELETE | `/v1/terminals/{id}` | admin | GUI |
| GET | `/v1/terminals/{id}/output?connection=` | admin | GUI |
| POST | `/v1/terminals/{id}/input` | admin | GUI |
| POST | `/v1/terminals/{id}/resize` | admin | GUI |
| POST | `/v1/terminals/{id}/takeover` | admin | GUI |

### 2.3 会话 token

| 方法 | 路径 | 级别 | 调用方 |
|---|---|---|---|
| GET | `/v1/me` | read | 外部 agent（MCP） |
| GET | `/v1/me/tools` | read | 外部 agent（MCP） |
| POST | `/v1/me/tools/{name}` | write | 外部 agent（MCP） |

会话 token 只认自己那一个 agent，不认总 token，也不能访问 `/v1/agents` 等下表中的管理面。
扩展的接口（`/v1/extensions*`）也在它到不了的范围内：agent 调扩展工具走 `/v1/me/tools/{name}`
或内置的 `extension_call`，由 hub 代为执行，拿不到启用和自动信任这两个开关。

> `/v1/extensions*` 这九条是 [EXTENSIONS.md](EXTENSIONS.md) §3 定下的约定，级别写在 `access.rs`，路由在
> `api.rs`，都有测试。

## 3. 分级口径

**read**：只读，不改变任何状态。可安全开放给远程设备（A2 的只读设备）。

**write**：改变会话、run、界面状态，但不动凭据、不停 hub。

**admin**：能碰凭据或 hub 生命周期——`/v1/shutdown`、账号增删与登录。这些要么能中断所有人的工作，要么能写入新的 API 凭据。

`/v1/terminals*` 全部归 admin，读也一样：终端以这台机器的用户身份执行输入的内容，读输出能看到里面打印过的任何东西（包括 token）。agent 的会话 token 到不了这里，终端输出也不进模型。

`POST /v1/plugins/{name}/install` 与 `/upgrade` 也归 admin：它们会在这台机器上执行程序。执行的命令只来自描述文件（`[[install.methods]]` 的 `run` / `upgrade`），请求只给插件名和动作，传不了命令；一次插件只跑一个任务，第二个请求返回 409。`POST /v1/plugins/refresh` 同理：它会在后台运行各 CLI 的版本命令（`locate.version`），只是重新查找和探测，不接受任何命令。

`/v1/extensions*` 分成"信任"和"使用"两半（[EXTENSIONS.md](EXTENSIONS.md) §6）：

- `POST /v1/extensions/{name}/enable` 与 `/auto-trust` 归 admin：批准一份快照等于让 hub 以这台机器的用户身份
  执行这段代码，和 `/v1/plugins/{name}/install` 同一性质；`auto-trust` 更进一步，开着就是以后每次改动都不再问。
  这两条必须由人点，所以放在 agent 的会话 token 到不了、只读凭据也到不了的级别。
- `POST /v1/extensions/{name}/disable` 归 write：收回信任只会让更少的代码跑起来，而面板出错时 ErrorBoundary 上那个
  「停用」按钮不该比界面其余部分要求更高的凭据。重新启用仍然要 admin，所以拿 write 停用再启用这条路不通。
- `POST /v1/extensions/{name}/tools/{tool}` 和 `DELETE /v1/extensions/calls/{id}` 归 write：调一个人已经批准过的
  工具是普通的状态改动。工具自己拿到的是另一套凭据——每次调用现签、只在内存里、级别最高 write（EXTENSIONS.md §3），
  和这里的路由级别是两回事。
- `GET /v1/extensions`、`/pending/diff`、`/logs`、`/v1/extensions/calls/{id}` 归 read：都只是读，日志和待批准快照的差异也只是读。
  待批准记录看得见但批不动，正是分级想要的效果。

### 扩展 token

扩展工具进程拿到的 `CROSSAGENTS_TOKEN` 是 `Grant::Extension { name, level }`，由 `require_token` 认出来，
再交给同一个策略点：

- 级别只有 read 或 write（取自扩展描述文件里的 `access`，类型是 `ExtensionAccess`，表达不出 admin），
  按上面的路由表放行：read 的扩展 POST 任何东西都是 403，write 的扩展到不了 admin 路由（停 hub、账号、终端、
  启用扩展、自动信任）。消息里写明是哪个扩展的 token。
- 到不了 `/v1/me*`：那一组只认会话 token，扩展的 token 在那里是 401。
- **调不了 `POST /v1/extensions/{name}/tools/{tool}`**，read 还是 write 都一样（403）。这条路由给工具的
  `caller` 填的是 `{ "kind": "human" }`，工具声明了 `human` 才能被它调到；只有人的凭据（总 token、只读 token、client 设备，即 `Grant::is_person()`）才算人。
  不拦的话，write 级的扩展拿自己的 token 调这条路由就能以人的身份调只开放给 `human` 的工具。
  `access::refusal` 里按路由拒绝，路由处理函数里再核对一次凭据类型。
- 进程结束就作废，每次调用一个新的；hub 重启后全部失效；每个请求在日志里记下扩展名。
- 事件钩子（`[[on]]`）和常驻进程（`[service]`）拿到的也是同一套 `Grant::Extension`：级别同样取自 `access`。
  钩子的 token 随那次进程结束作废；常驻进程的 token 随进程生命周期存在，进程重启换新，hub 退出后全部失效
  （[EXTENSIONS.md](EXTENSIONS.md) §3「事件响应」「常驻进程」）。

两个边界判断：

- `POST /v1/agents/{key}/connect` 归 write 而非 admin：它签发的是该 agent 自己的会话 token，作用域被 `require_agent_token` 限住，拿不到别的 agent 和总 token。
- `POST /v1/plugins/{name}/models` 归 write 而非 admin：只影响界面显示哪些模型，不改账号能力。

设备签发与吊销属于 admin：它们新增或收回访问凭据。列表属于 read：只返回设备资料，不含 token 或哈希。未吊销的设备名唯一，1–64 个字符，重名返回 409；吊销保留记录，下一次请求 401，已打开的事件流和终端输出流在 5 秒内断开。last_seen_at 每台设备每分钟最多写一次。

## 4. 待改造

按 [TODO.md](TODO.md) §二 的条目推进，顺序是清单 → 分级 → 收口。

- 把 `require_token` 抽成策略点，按 read / write / admin 校验；路由表只标级别，不写鉴权逻辑。
- `ErrorCode` 增 `Forbidden`：越权返回 403 而不是 401。401 表示"没带凭据或凭据无效"，403 表示"凭据有效但级别不够"，GUI 据此置灰入口并说明原因。
- Electron 主进程维护"方法 + 路径"白名单，只放行 GUI 实际用到的接口（已做，`apps/gui/electron/allowed.cts`；`allowed.test.cts` 从 `src/api.ts` 抽出全部调用逐条核对，漏登记就失败）。这把收窄放在主进程，页面即使被注入了也无法调用 `/v1/shutdown`。
- A1 已增加设备 token 与 `Grant::Device`（[DONE.md](DONE.md) §八），client 按设备级别走现有策略表，node 只走节点通道（B1）。总 token、只读 token、会话 token 与扩展 token 行为不变；密码登录、users 表、可信设备确认、登录限速留待 A3。write 级等于能在宿主上执行命令，A2 把设备 token 发到别的机器之前要先重分级（[TODO.md](TODO.md) §八 A2）。
- `Grant` 增加了 `Extension { name, level }`（E1 已做，见上面「扩展 token」和 [EXTENSIONS.md](EXTENSIONS.md) §3）。
  路由表不变，只是多一种凭据能走到现有的策略点；`Grant` 因带名字不再是 `Copy`，`refusal` 接受 `&Grant`。

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

0.1.x 例外：GUI 与 hub 同一安装包发布，不存在旧 GUI 连新 hub 的情况，破坏性变更暂不升版（S1 改 `DeleteRunResponse` 即属此类）。A2（远程 GUI）开工时统一升到 `v2`，并同时做 §二 的版本不一致提示。本轮 A1 只新增接口，按本节不算破坏性变更。

### GUI 侧比对

- `api` 不等 → 拒绝连接，提示 hub 与 GUI 版本不匹配、需一起更新。不静默降级：字段缺失导致的报错会更难查。
- `version` 不等但 `api` 相同 → 允许连接，在侧栏版本号旁标出不一致，隐藏该版本没有的功能入口。
