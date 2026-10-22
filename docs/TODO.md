# 待办清单

> 来源：进程守护与 Cutex 对照的讨论（2026-10-23）。依据见 [DESIGN.md](DESIGN.md) §2、§4，
> [codex-app-server 实测](research/codex-app-server.md)，[claude-stream-json 实测](research/claude-stream-json.md)，
> [Cutex 前端拆解](research/cutex-gui-teardown.md)。
>
> 状态：`[ ]` 未开始 · `[~]` 进行中 · `[x]` 完成

## 一、进程监督（优先）

目标：hub 或子进程挂了，不再整体停摆；一轮中断有明确状态；能自动 resume。

- [ ] 子进程退出时记录原因（正常退出 / 崩溃 / 被杀 / 超时），写入会话事件（`Exited` 事件已有，补上原因分类）
- [ ] 子进程崩溃后自动 resume，限制重试次数并退避，超过上限转 `blocked`，等人处理
- [ ] 中断的那一轮给出明确状态（`interrupted`，附原因），不要静默丢失
- [ ] 每个子进程写 pid 记录（hub 数据目录下），hub 启动时先清理上次遗留的子进程
- [ ] Windows 上子进程用 `codex.exe` / `claude` 直接启动，验证杀 hub 后无孤儿（参见 codex-app-server 研究 §启动）
- [ ] hub 退出时优雅地 suspend 所有会话，避免留下半截的 Claude 会话文件

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

## 八、远期

- [ ] 多主机：每台机器一个 hub，GUI 同时连多个 hub（以本地守护为前提）
- [ ] 远程 hub 经 SSH 端口转发接入
