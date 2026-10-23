# 待办清单

> 来源：进程守护与 Cutex 对照的讨论（2026-10-23）。依据见 [DESIGN.md](DESIGN.md) §2、§4，
> [codex-app-server 实测](research/codex-app-server.md)，[claude-stream-json 实测](research/claude-stream-json.md)，
> [Cutex 前端拆解](research/cutex-gui-teardown.md)。
>
> 状态：`[ ]` 未开始 · `[~]` 进行中 · `[x]` 完成

## 一、进程监督（优先）

目标：hub 或子进程挂了，不再整体停摆；一轮中断有明确状态；能自动 resume。

按 Cutex 的做法改为“agent 进程比 hub 活得久，新 hub 接管”，设计见 [PROCESS-SUPERVISION.md](PROCESS-SUPERVISION.md)。

- [x] 子进程退出时记录原因（exited / crashed / killed / lost），`Exited` 带 `cause` 与退出码，另发 `agent.exited` 事件（超时类原因未单独区分）
- [ ] 子进程崩溃后自动拉起：未做。决定：中断的那一轮不自动重放（可能重复执行命令、重复改文件），只标 `interrupted` 交给人或 Planner
- [x] 中断的那一轮给出明确状态：hub 重启时无法接管的轮次记为 `stop_reason: "interrupted"` 并附说明；能接管的轮次照常跑完
- [x] 运行时宿主登记在 `<home>/runtimes/`，hub 启动时握手确认后接管，不按 pid 杀进程（防 pid 复用误杀）；无人认领的宿主经连接停掉
- [x] Windows 上每个 agent 放进独立 Job Object（KILL_ON_JOB_CLOSE），宿主一退整棵树清掉；`acp-generic` 的 job 由 hub 持有
- [x] hub 退出默认把宿主里的会话留给下一个 hub（不 suspend，也就不会留下半截会话文件）；`hub stop --agents` 才停掉全部 agent
- [ ] 在真实 codex / claude 上验证一次“hub stop → hub start 后轮次继续”（目前只在 mock app-server 上有端到端测试）

## 二、hub 与 GUI 边界

- [ ] 梳理 hub 的本地 API：列出 GUI、CLI 实际调用的接口，确认哪些需要鉴权
- [ ] 本地 API 增加 token 与权限边界（参考 cutex：令牌只留在 Electron 主进程）
- [ ] GUI 与 hub 版本不一致时的处理策略（拒绝连接并提示，或只读降级）

## 三、常驻守护（等第一步稳定后再做）

- [ ] 设计 hub 常驻模式：关闭窗口后 hub 继续运行，窗口重开时重新连接并拉取快照
- [ ] Windows 自启方案：计划任务或服务，二选一并写明理由（权限、登录前运行、崩溃重拉的节流）
- [ ] hub 的停止命令：优雅停止，等待会话 suspend 完成后再退出
- [ ] 无窗口时的额度与预算上限：默认"空闲即挂起"，超过预算自动暂停并通知

## 四、可观测性

- [ ] `cli` 增加状态命令：hub 是否运行、各会话状态、子进程 pid、最近退出原因
- [ ] 事件日志落盘，便于无窗口时排查问题
- [ ] GUI 显示"hub 离线"时的诊断信息，而不是空白

## 五、Claude 运行时（对照 Cutex 的借鉴项）

- [ ] 设置源开关：Profile 级别选择是否读取项目的 CLAUDE.md、`.claude/` 规则、hooks、权限（`--setting-sources` 目前是固定值，见 `crates/drivers/src/claude_native/wire.rs`）
- [ ] hooks 默认关闭，需显式开启（参考 cutex 的 "Allow commands" 开关）
- [ ] 运行时版本钉住：每个 agent 记录所用的 Claude Code 版本，升级后单独 repin，而不是全局切换
- [ ] 每次升级 Claude Code 后跑 `claude_native` 冒烟测试（DESIGN.md §测试）

## 六、Codex 驱动

- [ ] 单进程线程数上限压测（codex-app-server 研究里标为"未确认"）
- [ ] 空闲线程是否会被服务端自动卸载，确认后更新挂起策略
- [ ] 每个线程的 MCP 子进程开销统计，超过阈值时提示

## 七、多模态输入

现状：`SendMessageRequest` 只有 `text` 字段（`bindings/SendMessageRequest.ts`），driver 层也没有附件类型，发不了图片和文件。

- [ ] 消息结构从纯文本改为内容块（text / image / file），`SendMessageRequest` 增加 `attachments`，重新生成 ts-rs 绑定
- [ ] 附件落盘到 hub 数据目录，按会话归档并记录 sha256，不把 base64 直接存进 SQLite
- [ ] 大小与类型限制：单文件上限、类型白名单，超限时 GUI 和 CLI 同时报错
- [ ] GUI：拖拽、粘贴截图、选择文件；输入框显示缩略图，发送前可删除
- [ ] `claude-native`：映射到 stream-json 的图片内容块，实测图片与文件的支持范围
- [ ] `codex-native`：映射到 app-server 的图片输入，实测后确认字段
- [ ] `acp-generic`：读取 agent 声明的 prompt 能力，未声明图片则不允许发图
- [ ] 按能力降级：驱动不支持的附件类型，GUI 禁用入口并说明原因，不静默丢弃
- [ ] 历史展示：时间线渲染图片和文件卡片，transcript 导出时包含附件引用
- [ ] 跨 agent 转发：Worker 之间传附件时明确是复制还是引用，避免路径跨工作目录失效
- [ ] 安全：附件路径不能让 agent 读到工作目录之外的文件，除非用户显式选择

## 八、多节点（固定中心 hub + 节点）

> 来源：多机协同讨论（2026-10-23），设计见 [MULTI-NODE.md](MULTI-NODE.md)。
> 取代原「远期」里的"每台机器一个 hub、GUI 同时连多个 hub"：已比较并放弃，理由见 MULTI-NODE §2。
> 并行方式：不拆仓库，每阶段先在 `api-types` 定接口并生成 bindings，core 与 GUI 两条线各用一个 worktree 和分支，GUI 先对着 MockBridge 开发。

开工前要定：

- [ ] 中心放哪台：先用这台 Windows（A 阶段），B1 之后再考虑迁到云服务器当纯中心（只放个人项目，公司代码先确认合规）
- [ ] 连通方式：局域网 / Tailscale / 公司内网，决定第一版是否自己做 TLS
- [ ] 两条线并行时完工监视器怎么提交：按分支提交，还是由人合并

A1 账号与设备：

- [ ] core：`users`、`devices` 表（client / node 两类，token 只存哈希，可吊销），现有表加 `user_id`、`node_id`，老数据迁到默认用户和本机节点
- [ ] core：`/v1/auth/login`、`crossagents user add`、设备列表与吊销接口
- [ ] GUI：登录页、设备管理页

A2 远程 GUI：

- [ ] core：`[hub] listen` 可配置，默认仍为 `127.0.0.1`；远程访问走设备 token
- [ ] GUI：Electron 主进程改读连接配置（本机 / 远程地址 + token），设置加「连接」页、断线提示，连不上中心时可切回本机
- [ ] 验收：Mac GUI 连上 Windows hub，两端对话一致，断线后按 `Last-Event-ID` 续传

B1 node 代理：

- [ ] core：`crossagents node join`、配对码、hub 与 node 的双向通道（WSS 还是 gRPC 流待定）
- [ ] core：驱动拆成两半，调度留在 hub，拉起 CLI 与协议翻译挪到 node；单机模式也走内置节点，只保留一套执行路径
- [ ] core：节点上报已装插件、登录状态、可用模型；掉线时事件写本地日志，重连补发
- [ ] core：协作工具 MCP 走 node 转发还是直连中心（待定）
- [ ] GUI：配对确认弹窗、节点列表与在线状态

B2 跨节点 Chat：

- [ ] core：模型 id 带节点名（`codex@mac`），不指定节点时按"发起方节点优先、其次空闲"挑选；未装或未登录的插件不提供
- [ ] core：Chat 里跨节点招人；Work run 第一版仍限单节点
- [ ] GUI：`@` 菜单按节点分组，显示「节点离线」

B3 远程开发：

- [ ] core：列目录、读文件、附件索引接口
- [ ] GUI：先选机器再选目录的远程目录浏览框，最近目录记住所在机器
- [ ] GUI：diff 与文件查看器，截图和构建产物附件预览

存储与负载：

- [ ] 附件（截图、产物、日志、大 diff）留在节点，hub 只存引用（节点、路径、大小、哈希），按需拉取
- [ ] 节点保留自己 agent 的追加事件日志，兼作 hub 灾备重建来源
- [ ] 流式字块事件量大：一轮结束后合并或定期清理旧字块
- [ ] 节点离线缓存的上限与过期策略
- [ ] 云服务器部署：hub 设内存上限、只经 HTTPS/WSS 并限制来源，附件中转受公网带宽限制
