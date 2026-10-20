# V2 任务书：Tauri GUI

> 依据：[DESIGN.md](../DESIGN.md) §11、§11.1 · 状态：已完成 · 2026-10-21

## 1. 范围

`apps/gui/`：Tauri 2 + React（TypeScript、Vite）。GUI 是 hub 本地 API 的**纯客户端**，与 CLI 平等。

| 视图 | 内容 | 用到的 API |
|---|---|---|
| Run 列表 | 状态、标题、创建时间；「新建 run」表单（请求、Planner、Worker、可选 Escalate / Reviewer、并行数、免审批） | `GET/POST /v1/runs` |
| Run 详情 | 待审批时显示计划（每个任务的规格与依赖），**批准** / **驳回**（必须填理由）；任务表实时更新状态；取消 run | `GET /v1/runs/{id}`、`…/approve`、`…/reject`、`…/cancel` |
| Task 详情 | 规格、Worker 总结、验收输出、diff stat、失败原因、未决问题、审查意见 | `GET /v1/runs/{id}`（任务在 `RunDetail.tasks` 里） |
| Agents | 列表与状态；选中后看实时输出（`agent.output`、输入、工具调用、轮次结束）；消息框发消息；登记新 agent | `GET/POST /v1/agents`、`POST /v1/agents/{key}/messages`、`GET /v1/plugins` |
| 用量 | 按 role / model / run / agent 分组的成本与峰值上下文 | `GET /v1/usage?by=…` |

不在本任务内（DESIGN §11.1 其余部分，需要 hub 新增能力）：审批时编辑规格、完整 diff 查看、自由编排 Pipeline。

## 2. 设计决定

| 决定 | 原因 |
|---|---|
| **Tauri 侧只做传输**：两个命令 `hub_request(method, path, body)`、`hub_events_start()`，外加连接状态 | 业务逻辑全在 hub（DESIGN §11）。webview 不能直接请求：hub 没有 CORS，`EventSource` 也带不了 `Authorization` 头 |
| 每次请求都重新读 `hub.json`（端口）与 `token`，路径遵循 `CROSSAGENTS_HOME` | hub 重启换端口、换令牌后 GUI 无需重启 |
| SSE 在 Rust 侧读取，原样把 `data` 转发给前端（`hub://event`），断线后带 `Last-Event-ID` 重连 | 不重复、不丢事件；Rust 侧不解析事件类型，新增事件无需改 Rust |
| 首次连接从头回放事件（`since=0`），前端每个 agent 的输出只保留最后约 200 KB | 打开 GUI 就能看到 agent 之前的输出；内存有上限 |
| **前端忽略未知事件类型**；事件只用于（防抖后）重新拉取对应资源，加上追加 agent 输出 | 状态以 REST 结果为准，事件丢了也能自愈；兼容以后的新事件 |
| 前端有传输抽象：Tauri 里走 `invoke`，普通浏览器里走同源 `fetch` / `EventSource` | 以后复用于远程/手机查看（DESIGN §11.1）；开发时 `npm run dev` 的 Vite 代理会读令牌并转发到 hub，可直接在浏览器里调试 |
| API 类型直接引用 `bindings/*.ts`（`@bindings` 别名），不复制 | 与 hub 保持同一份定义；改类型后跑 `cargo test -p crossagents-api-types` 重新生成 |
| `apps/gui/src-tauri` 是独立的 Cargo workspace，不加入根 workspace | `cargo test --workspace` 不用编译 Tauri 与 WebView 依赖 |
| 路由用 URL hash（`#/runs/<id>`），不引入路由库；样式手写 CSS，跟随系统深浅色 | 依赖少 |

## 3. 构建

```sh
cd apps/gui
npm install
npm run tauri dev          # 开发：热重载
npm run tauri build -- --no-bundle   # 产出 src-tauri/target/release/crossagents-gui(.exe)
npm run dev                # 只跑前端，在浏览器打开 http://localhost:1420（经 Vite 代理连 hub）
```

需要 Node.js、Rust，Windows 上需要 WebView2（Windows 11 自带）。

## 4. 验收

- Windows 上 `npm run tauri build -- --no-bundle` 成功
- 对 mock 插件（README 插件目录示例）：在 GUI 里登记 Planner / Worker、新建 run、看到计划、批准，任务实时变为 `merged`，run 变为 `completed`；驳回必须填理由
- Agents 视图能看到实时输出并发消息；用量面板四种分组都能显示
- 根目录 `cargo test --workspace` 照常通过；hub 行为未改
