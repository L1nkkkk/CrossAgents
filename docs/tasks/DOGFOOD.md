# DOGFOOD 任务书：用 CrossAgents 开发 CrossAgents

> 目标：让 CrossAgents 达到能“自举”的程度——用户用 CrossAgents 的 Work 开发 CrossAgents 本身。
> 来源：2026-10-24 任务清单（P0 四项、P1 四项），每项一个提交。每节先写设计，再写实现；
> 「不做」和「待实测」写在各节末尾。

## 1. 自举隔离（P0）

### 问题

正在跑的稳定 hub 和 worktree 里的开发构建互相干扰：

- 运行时宿主和 `crossagents mcp` 都从 `current_exe()` 拉起。hub 从 `target/debug/crossagents.exe` 起时，宿主比 hub 活得久，一直锁着这个文件，`cargo build` 报 “拒绝访问”。
- Work 任务的 worker 在 `<home>/worktrees/<run>/<task>` 里跑 `cargo`；hub 若继承了某个 `CARGO_TARGET_DIR`，所有 worker 共用同一个 target 目录，互相锁、互相覆盖。
- agent 想验证自己改的 hub，只能 `hub stop` 再起——停掉的正是正在跑它的那个 hub。

### 设计

| 项 | 做法 |
|---|---|
| 不锁构建产物 | hub 不再从构建目录直接拉起长寿进程。`HubConfig::load` 算出“稳定副本”：当前 exe 不在 `<home>/bin/`（安装版）或 `<home>/shadow/` 下时，按文件内容哈希复制到 `<home>/shadow/<hash>/crossagents.exe`，运行时宿主与 `mcp` 都从副本起。`hub start` 也从副本拉起后台 hub，于是 `target/` 下的 exe 只在 `hub start` 那一瞬间被占用。`hub run`（前台）本身仍占着自己的 exe，但它退出后不留锁。旧副本在启动时尝试删除（Windows 上正被使用的删不掉，自然保留；超过 1 天才删） |
| 副本不进 `bin/` | `<home>/bin/` 是安装版 GUI 的地盘，GUI 会升级、停掉从那里跑的 hub；开发副本放 `shadow/`，GUI 不碰 |
| 任务独立 target | 工作目录在 `<home>/worktrees/` 下的 agent（run 的集成目录、每个任务的 worktree），环境里加 `CARGO_TARGET_DIR=<worktree>/target`（绝对路径）。Codex 一个 app-server 进程共享一份环境，所以不同 worktree 的 Codex 任务各占一个进程：这是有意的代价（并行上限缺省 3） |
| 不误停自己的 hub | `hub run` 启动时把 `CROSSAGENTS_HUB_HOME=<home>` 写进自身环境，子进程（宿主、agent、agent 的 shell）都继承。CLI 的 `hub stop` 发现目标 home 就是 `CROSSAGENTS_HUB_HOME` 时拒绝执行，提示改用 `hub sandbox`；确有需要加 `--force`。不改每个 agent 的启动环境，也就不影响 Codex 进程按环境分组与接管 |
| 临时 dev hub | `crossagents hub sandbox start [--dir DIR] [--detect]`：home 缺省是 `$CARGO_TARGET_DIR/dev-hub`，没有就 `./target/dev-hub`；写一份 `config.toml`（`port = 0`、`runtime_hosts = false`、`[accounts] detect = false`，`--detect` 才读本机厂商登录），用当前 exe（即 worktree 里刚编出来的那个）的稳定副本起后台 hub，打印 home、端口和 `CROSSAGENTS_HOME=…`。后续命令带上这个环境变量，就都打到 dev hub（不加全局 `--home` 参数：`account add` 已有同名参数）。`hub sandbox stop` 停掉它连同它的 agent，`hub sandbox status` 查看 |
| 测试隔离 | 已有：`TestHub` 用临时目录和 `port = 0`，CLI 测试设 `CROSSAGENTS_HOME` 与 `port = 0`，`HubConfig::new` 不读本机登录、不写事件日志。worker 在 worktree 里跑 `cargo test` 时，测试起的 hub 都在临时 home 上，碰不到稳定 hub；`CROSSAGENTS_HUB_HOME` 只拦“目标就是稳定 home”的 `hub stop`，不影响测试 |

### 不做

- 不做 `hub run` 的前台自我重启（让前台 hub 也从副本跑）：前台 hub 是人手动起的，退出即解锁。
- 不在 worktree 里写 `.cargo/config.toml`：会被 `task_complete` 的 `git add -A` 提交进去。

### 待实测

- 真实 Codex 任务在 worktree 里 `cargo build`，确认 target 落在 worktree 下、稳定 hub 的 exe 没被锁。

## 2. GUI 里的代码审查闭环（P0）

### 问题

现在任务卡片只有每轮的 diff（worker 被移除后连这个也没了）和一段 `diff --stat`。人想看“这个任务一共改了什么”“整个 run 落地前会进 main 的是什么”，只能自己去 worktree 里 `git diff`；看完有意见，只能去 worker 的对话里手打一段话，说不清是哪个文件哪一行，也没法顺手把任务打回。

### 设计

接口（都在总 token 下；读级 / 写级见 API-AUTH）：

| 方法 | 路径 | 返回 | 说明 |
|---|---|---|---|
| GET | `/v1/runs/{key}/tasks/{task}/diff` | `ChangesResponse` | 任务 worktree 相对它的基准（父任务分支或 run 分支）的全部改动，含未提交的；与 `workspace_diff` 同源。任务已合并、worktree 已清掉时返回 409，提示看 run 的 diff |
| GET | `/v1/runs/{key}/diff` | `ChangesResponse` | run 分支相对 base 分支的总 diff（`git diff base...run`），即落地会带进 base 的内容。无 git 的 run 返回 409 |
| POST | `/v1/runs/{key}/tasks/{task}/review` | `Task` | 人对任务提意见：`comments`（每条 `path` + 可选 `line` / `side`，或只给 `path` 表示整个文件）、可选 `note`、`rework`。正文由人写，hub 只按“文件:行 — 意见”排成一条消息，以人的身份（`MessageSource::Human`）送进 worker 的收件箱，作为它的下一轮。`rework: true` 时等同人做了一次 `task_rework`：已报告完成 / 停了 / 审查中的任务回到运行中、尝试次数加一。同时给发起人（lead）发一行通知，免得它不知道任务被人打回 |

`ChangesResponse { base, head, diff, truncated }`：`diff` 上限 2 MB，超出截断并置 `truncated`。

GUI：

- 任务详情（工具列「任务」窗格和任务路由）新增「审查」节：文件列表（点一下跳到该文件）＋逐文件 diff，带新旧行号。点某一行的行号在下面展开输入框写意见；文件头上有「评论整个文件」。写好的意见先攒在草稿里（可删），底部一个总评框和两个按钮：「发给 worker」（任务状态不变）和「打回重做」。任务没开工、已结束时不显示按钮，只看 diff。
- run 页落地卡片（收尾中、完成但未落地）里加「查看全部改动」，展开 run 分支对 base 的总 diff，同样带文件列表；无 git 的 run 不显示。
- 行号由 `lib/diff.ts` 从 hunk 头算出（新增行用新文件行号，删除行用旧文件行号、`side: "old"`），有单元测试。

### 不做

- 不做评论线程和“已解决”状态：意见一次性送给 worker，之后的往来就是普通对话。
- 不做并排（split）diff 视图：工具列窄，统一用 unified。
- 任务合并后不再提供任务级 diff（分支已删）；看 run 总 diff。

### 待实测

- 真实 worker 收到带行号的意见后能按位置改；打回后任务卡片状态与尝试次数正确。

## 3. agent 能验证自己的工作（P0）

### 问题

消息只有纯文本（TODO §七）：人发不了截图，agent 改了 GUI 也看不到改成了什么样，只能靠人肉眼验收。

### 设计

**消息带附件**（落实 TODO §七 的前几条）

| 层 | 做法 |
|---|---|
| 接口 | `SendMessageRequest` 增可选 `attachments: [{ name, mime, data }]`（`data` 为 base64），不升 API 版本（新增可选字段）。限制：每条最多 8 个、单个 10 MB、合计 20 MB；类型白名单 `image/png|jpeg|gif|webp`、`application/pdf`、`application/json`、`text/*`。超限或类型不符返回 400，GUI 发送前先按同一规则检查 |
| 存储 | 解码后算 sha256，落盘到 `<home>/attachments/<agent id>/<sha256>`（同一会话同内容只存一份），SQLite 里只有事件中的元数据 `Attachment { id: sha256, name, mime, size, path }`，不存 base64 |
| 事件与历史 | `agent.input` 增可选 `attachments`，transcript 的轮次随之带上；`GET /v1/agents/{key}/attachments/{id}` 返回 base64 内容（只读该会话目录下、id 为 64 位十六进制的文件），供时间线显示缩略图 |
| 投递 | 收件箱里的消息从纯文本改为 `Prompt { text, attachments }`。`DriverSession` 增 `prompt_with(&Prompt)`，缺省实现把附件写成文字引用（文件名、类型、绝对路径）追加在正文后，任何驱动都不会静默丢附件 |
| claude-native | stream-json 的 user 消息改为内容块数组：图片 → `image`（base64），PDF → `document`（base64），文本类 → `text` 块（`<file name=…>` 包住内容，单个 256 KB 内），其余 → 路径引用 |
| codex-native | `turn/start` 的 `input`：图片 → `{ type: "localImage", path }`，文本类 → 追加 `text` 项，其余 → 路径引用 |
| GUI | 输入框支持粘贴截图、拖入文件、点回形针选文件；发送前显示缩略图 / 文件卡片，可删；时间线里人的消息下显示附件（图片缩略图，点开看大图） |

**agent 自己截 GUI 的图**

- `apps/gui/scripts/screenshot.mjs`：用 Vite 以 mock 模式起一个临时预览（随机端口），再用项目里已有的 Electron 起一个隐藏窗口打开 `?scenario=<场景>`（可带路由、宽高、主题），等页面稳定后 `capturePage()` 存 PNG，然后关掉两者。不依赖 hub、不碰人的 GUI。命令行：`npm --prefix apps/gui run screenshot -- --scenario review --out shot.png [--width 1280 --height 800 --theme dark --route '#/…' --wait 1500]`。
- 协作工具 `gui_screenshot`（所有人可用）：hub 在调用者的工作目录里跑上面的脚本（工作目录里得有 `apps/gui` 并装好依赖），超时 3 分钟，把 PNG 作为图片返回；`crossagents mcp` 把结果里的 `images` 转成 MCP 的 `image` 内容块，模型直接看得到。工作目录不对、依赖没装时返回能照着做的错误信息。

### 不做

- 附件只做到人 → agent。agent 之间转发附件（TODO §七 “跨 agent 转发”）、`acp-generic` 按能力限制、transcript 导出带附件，留在 TODO §七。
- 截图只截 mock 预览，不截连真 hub 的 GUI：mock 场景可重放、不依赖状态，适合验收；需要新界面时 agent 照规矩加 mock 场景。

### 待实测

- Claude Code 对 `document`（PDF）块与大图的实际支持范围；Codex `localImage` 字段名以实测为准（目前按 app-server v2 的 `UserInput` 写）。
- 真实 agent 调 `gui_screenshot` 后能“看见”图片（Claude Code 与 Codex 对 MCP 图片结果的支持）。
