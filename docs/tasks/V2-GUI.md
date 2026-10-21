# V2 任务书：桌面 GUI（Electron）

> 依据：[DESIGN.md](../DESIGN.md) §11、§11.1 · 状态：已完成 · 2026-10-21
>
> 外壳最初用 Tauri 2 实现，同日改为 Electron（见 §2 第一行）；Tauri 版本留在 git 历史里。

## 1. 范围

`apps/gui/`：Electron + React（TypeScript、Vite）。GUI 是 hub 本地 API 的**纯客户端**，与 CLI 平等。

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
| **外壳用 Electron，不用 Tauri** | 前后端分离：GUI 外壳只剩 TypeScript，不再需要第二个 Rust/Cargo 工程和 WebView2；各平台渲染一致（自带 Chromium），打包工具（electron-builder）成熟。代价是体积大（免安装目录约 370 MB），对本地开发工具可以接受。两套外壳不并存 |
| **主进程只做传输**：`hub:request`（method, path, body）与 `hub:events-start` 两个 IPC 通道，外加连接状态 | 业务逻辑全在 hub（DESIGN §11）。页面不能直接请求：hub 没有 CORS，`EventSource` 也带不了 `Authorization` 头 |
| **令牌只在主进程**：主进程读 `token`、加 `Authorization` 头、转发到 `http://127.0.0.1:<port>`；页面只拿到状态码和响应体 | 页面里出问题（XSS 等）也拿不到令牌。主进程只接受 `GET`/`POST`/`DELETE` 和 `/v1/` 开头的路径，只响应来自本应用页面的 IPC |
| preload 用 `contextBridge` 只暴露 `window.crossagents` 的四个函数：`request`、`startEvents`、`onEvent`、`onStatus` | 最小接口；不暴露 `ipcRenderer` 或任何 Node API |
| **安全设置**：`contextIsolation: true`、`nodeIntegration: false`、`sandbox: true`；`index.html` 里有 CSP（`script-src 'self'`、`connect-src 'self'` 等）；只加载本地 `dist/index.html`，不加载远程内容；拒绝新窗口、拒绝导航到别处、拒绝 `<webview>`、拒绝所有权限请求 | Electron 官方安全清单的要点。开发时 Vite 插件只在 dev server 上放宽 CSP（HMR 需要内联脚本和 WebSocket），构建产物保持严格 |
| 每次请求都重新读 `hub.json`（端口）与 `token`，路径遵循 `CROSSAGENTS_HOME` | hub 重启换端口、换令牌后 GUI 无需重启 |
| SSE 在主进程读取，原样把 `data` 转发给页面（`hub:event`），断线 2 秒后带 `Last-Event-ID` 重连；页面刷新时重新开始 | 不重复、不丢事件；主进程不解析事件类型，新增事件无需改外壳 |
| 首次连接从头回放事件（`since=0`），前端每个 agent 的输出只保留最后约 200 KB | 打开 GUI 就能看到 agent 之前的输出；内存有上限 |
| **前端忽略未知事件类型**；事件只用于（防抖后）重新拉取对应资源，加上追加 agent 输出 | 状态以 REST 结果为准，事件丢了也能自愈；兼容以后的新事件 |
| 前端有传输抽象：有 `window.crossagents` 时走 preload，普通浏览器里走同源 `fetch` | 以后复用于远程/手机查看（DESIGN §11.1）；开发时 `npm run dev` 的 Vite 代理会读令牌并转发到 hub，可直接在浏览器里调试 |
| API 类型直接引用 `bindings/*.ts`（`@bindings` 别名），不复制 | 与 hub 保持同一份定义；改类型后跑 `cargo test -p crossagents-api-types` 重新生成 |
| 主进程与 preload 用 `tsc` 编译成 CommonJS（`.cts` → `dist-electron/*.cjs`），不另加打包器；React、Vite 等都是 devDependencies | 沙箱里的 preload 必须是 CommonJS；页面已被 Vite 打包，安装包里不需要 `node_modules` |
| 路由用 URL hash（`#/runs/<id>`），不引入路由库；样式手写 CSS，跟随系统深浅色 | 依赖少；hash 路由在 `file://` 下也能用 |

## 3. 构建

```sh
cd apps/gui
npm install
npm run electron:dev       # 开发：Vite dev server + Electron 窗口，页面热重载（改 electron/ 下的文件要重启）
npm run electron:build     # Windows 构建（electron-builder，免安装目录，不签名）：release/win-unpacked/CrossAgents.exe
npm run dev                # 只跑前端，在浏览器打开 http://localhost:1420（经 Vite 代理连 hub）
npm run typecheck          # 页面与 electron/ 两套 tsconfig 都检查
```

只需要 Node.js。Electron 与 electron-builder 版本在 `package.json` 里固定。

## 4. 验收

- Windows 上 `npm run electron:build` 成功，产出的 `CrossAgents.exe` 能直接运行
- 对 mock 插件（README 插件目录示例）：在 GUI 里登记 Planner / Worker、新建 run、看到计划、批准，任务实时变为 `merged`，run 变为 `completed`；驳回必须填理由
- Agents 视图能看到实时输出并发消息；用量面板四种分组都能显示
- 根目录 `cargo test --workspace` 照常通过；hub 行为未改
