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
| 一个 run 一份 target | 工作目录在 `<home>/worktrees/<run>/` 下的 agent（run 的集成目录、每个任务的 worktree），环境里加 `CARGO_TARGET_DIR=<home>/worktrees/<run>/target`（绝对路径）：同一 run 共用，依赖只编一次，同时编译时在 cargo 的锁上排队；不在任何 checkout 里，不会被提交；run 的集成目录删掉时一起删（结束、取消、主导归档或删除时）。工作区自己的 crate 按源码路径区分，每个 worktree 仍各编一份。最初是每个 worktree 一份，2026-10-26 改成按 run 共享，见 §9。Codex 一个 app-server 进程共享一份环境，现在同一 run 的 Codex 任务环境相同，可以共用一个进程 |
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

## 4. main 动过之后的落地（P0）

### 问题

一个 run 跑几个小时，期间人在 main 上提交了别的东西（自举时几乎必然：人和 agent 同时改 CrossAgents）。现在 `run_land` 直接把 run 分支合进 base：干净时没事，冲突时报错退出，run 卡在“收尾中”，只能人手工处理。

### 设计

落地前先把 base 的最新内容带进 run 分支，在集成 worktree 里做（run 已结束、集成目录已清掉时临时重新检出）：

| 情况 | 做法 |
|---|---|
| base 没动 | 照常落地 |
| base 动了、能自动合并 | 在集成 worktree 里 `git merge --no-ff <base>`（提交信息写明是同步 base），然后照常落地 |
| 有冲突 | 中止这次合并，不落地；在 run 里建任务 `sync-<base>`（已有同名且已结束时加序号），发起人是 lead，负责人是 run 的默认 worker，正文写明要在任务 worktree 里 `git merge <base>`、解决哪些文件的冲突、跑测试再 `task_complete`，冲突片段附在规格的参考里。run 已结束（completed）时重新打开为 running。`run.note` 写明为什么没落地、建了哪个任务；lead 收到一行通知。任务被 `task_accept` 合进 run 分支后，lead（或人）再落地一次，这时 base 已在 run 分支里，直接落地 |

为什么用 merge 而不是 rebase：

- run 分支上是一串 `task_accept` 的合并提交，任务 worktree（含委派出去的子任务）都从它分出来。rebase 改写这段历史，还开着的 worktree 和分支就对不上了，合并提交还会被拍平。
- rebase 要逐个提交重放，冲突可能出现多次、出现在中间状态；merge 只在最终状态上解决一次，正好交给一个 worker 一次做完。
- 落地方式本身可选 squash：在乎线性历史的人用压缩合并，run 分支里多一个同步提交无所谓。

接口变化：`Orchestrator::land` 返回 `LandOutcome`（`Landed` 或 `Deferred { task, files }`）；`run_land` 工具在推迟时返回 `{ "landed": null, "deferred": { "task": "sync-main", "conflicts": [...] } }`；`POST /v1/runs/{key}/land` 照旧返回 `RunDetail`，原因在 `run.note`、新任务在任务列表里。

### 不做

- 不自动解决冲突（例如按“保留双方”机械合并）：冲突需要读懂两边意图，交给 worker。
- 不在 run 进行中持续同步 base（`branch_sync` 已能把 run 分支带进任务；base 只在落地时同步一次）。

### 待实测

- 真实 worker 拿到 `sync-main` 任务后能在 worktree 里完成合并并报告；冲突很大时一个 worker 是否够用。

## 5. 看额度派活（P1）

### 问题

hub 已经知道每个账号的额度（`/v1/quota`：5 小时 / 每周窗口用了多少、是否已撞限、CodeBuddy 的 credits 余额），但派活时不看：任务照样发给已经撞限的账号，worker 一开工就 429，任务停在「停了 · 未上报」，等人发现。

### 设计

**派活前选账号**（`dispatch`，以及 hub 接住 429 后的改派）：hub 给 orchestrator 一个 `AgentHost::route(template, team)`，答案三选一：

| 答案 | 条件 | 做法 |
|---|---|---|
| 照旧 | 模板的账号健康（或额度未知、或插件不归 hub 管账号） | 照常克隆模板 |
| 换账号 | 模板账号快用完（任一窗口 ≥ 90%）或已撞限（任一窗口 100%、厂商报 `reached`、credits 余额 ≤ 0），同厂商还有健康账号 | 克隆模板，但 `config.account` 改成那个账号（模型不变） |
| 换队友 | 同厂商没有健康账号，run 团队里有别家厂商、账号健康的 worker | 用那个队友当模板（换厂商、换模型） |

都不行时照旧派（宁可跑一次撞限，也不让 run 卡住），并写明。每次换都“在 run 里说出来”：任务卡片的进度行写原因（如“改用账号 codex-b：codex-a 的 5 小时窗口已用 97%”），发 `task.rerouted` 事件，给 lead 一行通知。额度只用已缓存的读数（账号状态缓存或 agent 刚报的），不为派活现查，查询在后台补。

**撞限时改派**：worker 的 `agent.rate_limits` 报 `reached`（Claude 的 rate_limit_event 拒绝、Codex 的 `rateLimitReachedType`），且它正在做任务时，hub 记下这个账号已撞限，按上表找替身：新克隆接手同一个 worktree，原 worker 停掉移除，任务正文原样再发一次并附一行事实（前任因额度停下、worktree 里的改动保留）。找不到替身时只通知 lead。

**预算在 hub 不在时**（`budget.rs`，TODO §三 末条）——决定：**只记账，hub 回来时结算**，不在宿主里强制。理由：

- hub 不在时不会开始新的一轮（消息由 hub 投递），能继续花钱的只有正在跑的那一轮；宿主把这一轮的输出（含用量事件）缓存着，新 hub 接管时补发，账一笔不丢。
- 在宿主里强制意味着把预算规则搬进“只转发、不判断”的宿主，还得处理宿主之间共享一个 run 的总账，得不偿失。
- 落实“结算”：新 hub 启动时先读账、立刻检查一次预算（超了就按原规则暂停 run 并问人），然后才恢复调度（`orchestrator.resume()`），不再等 5 秒的定时检查，也就不会先派出新任务再暂停。

### 不做

- 不按价格或剩余量在多个健康账号间“均衡”：只在不健康时换，换到第一个健康的。
- Chat 会话（不属于 run）不改派：人直接在对话里看得到撞限，自己换模型。

### 待实测

- 真实 Codex / Claude 撞限时 `reached` 的报法与恢复时间；CodeBuddy credits 余额字段的格式。

## 6. 统一的项目规则（P1）

### 问题

每家 agent 只读自己的规则文件：Claude Code 读 `CLAUDE.md`（`--setting-sources` 含 `project` 时），Codex 读 `AGENTS.md`，其余（CodeBuddy、ACP agent）什么都不读。同一个仓库里混用几家时，规则要么写几份、要么有人看不到。CrossAgents 自己的仓库就是这样：约定写在 `CLAUDE.md` 和任务书里，Codex worker 看不到。

### 设计

hub 在开会话时把一份项目规则写进开场说明（`SessionSpec.instructions`，各驱动按自家格式注入：Claude 的 `--append-system-prompt`、Codex 的 `developerInstructions`、ACP 的首条消息前缀），按以下顺序找规则文件，从 agent 的工作目录往上找到仓库根（有 `.git` 的目录）为止：

| 文件 | 注入给谁 |
|---|---|
| `CROSSAGENTS.md` | 所有 agent（没有哪家会自己读它，它就是给 CrossAgents 用的统一规则） |
| 否则 `AGENTS.md` | 除 Codex 外的 agent（Codex 自己读） |
| 否则 `CLAUDE.md` | 除“会自己读 CLAUDE.md 的 Claude”外的 agent；Claude 的 `setting_sources` 不含 `project` 时也注入 |

只取一份，单个文件最多 64 KB（超出截断并注明）；注入时标明来源文件，正文原样。规则在会话开启时读，改了规则从下一个会话起生效（worker 每个任务都是新会话）。

**设置源开关**（TODO §五）：编辑队友的面板里给 Claude 系（claude、codebuddy）加「读取 Claude 设置」三档预设：项目与本地（缺省，`project,local`）、全部（`user,project,local`，会带上本机 `~/.claude` 的个人设置和 hooks）、都不读（空，只用 CrossAgents 注入的规则，最可控）。新接口 `POST /v1/agents/{key}/settings { setting_sources }`（写级），写进 `config.setting_sources`，下个会话生效。

### 不做

- 不合并多份规则文件（三份都在时只取 `CROSSAGENTS.md`）：合并的语义（覆盖还是叠加）说不清，交给人在 `CROSSAGENTS.md` 里写全。
- 不把规则写进 worktree 的文件里（会被提交）。
- hooks 默认关闭（TODO §五 下一条）没做：“都不读”一档已能关掉 hooks。

### 待实测

- `--setting-sources ""` 在当前 Claude Code 版本上是否等于“都不读”；CodeBuddy 是否自己读 `CLAUDE.md`（目前按不读处理，会注入，至多重复一次）。

## 7. 分级审批与权限（P1）

### 问题

两件事：

- agent 以放行模式运行，`git push`、改 `.github/` 里的 CI、`rm -rf ~` 这类动作不经过任何人。自举时 worker 改的就是 CrossAgents 自己，推错了或改坏了 CI，代价落在人身上。
- 本地 API 只有一把总 token，能读的就能停 hub、删账号（TODO §二）。

### 设计

**审批（守卫模式）**

- 新的权限策略 `PermissionPolicy::Guard`：安全的照常放行，危险的问人。驱动侧：Claude 不加 `--dangerously-skip-permissions`，每次 `can_use_tool` 都交给 hub 判断（安全的立即放行，不打扰人）；Codex 用 `danger-full-access` 沙箱加 `untrusted` 审批，命令请求交给 hub；ACP 的权限请求同样交给 hub。
- 谁用守卫：Work 任务的 worker 默认用（run 策略新字段 `approvals`，缺省 `guard`，可设 `off` 回到放行）；Chat 会话不变（人就在旁边看着），单个 agent 可以在 `config.permission` 里设 `guard`。**这是一个产品取舍，用户可改：** 只对无人看守的 worker 默认打开。
- 什么算危险（hub 里的纯函数，有单元测试）：命令里有 `git push`、`gh pr merge` / `gh release`、`npm publish` / `cargo publish` / `docker push`、`curl … | sh`、`sudo`、`crossagents hub stop`，或 `rm -rf` 指向 `/`、`~`、`..`、盘符根；改动的文件在 `.github/`、`.gitlab-ci.yml`、`.circleci/`、`azure-pipelines.yml`、`Jenkinsfile`。
- 怎么问：沿用「需要你」的 `HumanItem`，新动作类型 `HumanAction::Approval`，选项「允许」「拒绝」，缺省「拒绝」。超时跟 run 的 `human_ask` 策略（`timeout` 用它的时长，否则 30 分钟）；`human_ask = deny` 的全托管 run 直接拒绝并告诉 agent 原因。agent 那一轮在等待期间挂着（就像在等终端确认），回答后继续；拒绝时 agent 收到“被人拒绝”。

**权限分级**（TODO §二）

- `require_token` 拆成“认证 + 策略点”：认证得出凭据的级别（总 token = admin；新增只读 token `<home>/token-read` = read；会话 token = 只认 `/v1/me*`），策略点按路由表查该路由要求的级别（与 API-AUTH 表一致），不够返回 **403** `forbidden`，信息写明缺哪一级。401 仍只表示凭据缺失或无效。
- `ErrorCode` 增 `Forbidden`（不升 API 版本：新增错误码取值）。
- A1 引入用户与设备时只换“凭据 → 级别”这一步，路由表不动。

### 不做

- 不解析 shell 语法做精确判断（如变量展开后的路径）：按文本匹配，宁可多问。
- 不给 Chat 默认开守卫；不做“记住这次允许”的长期规则。
- GUI 还没有据 403 置灰入口（GUI 总是用总 token，碰不到 403）。

### 待实测

- Claude Code 在非放行模式下每个 Bash / Edit 都走 `can_use_tool` 时的流畅度；Codex `untrusted` 下的审批请求形状。

## 8. 可追溯的失败（P1）

### 问题

一个任务失败或卡住，要回答“哪一轮出的事、跑过哪些命令、哪条验收没过、花了多少”，现在得翻多个 worker 的对话（任务合并后 worker 被删，对话就看不到了）、事件流和用量页，拼起来。

### 设计

一个按 run、按任务聚合的“追溯”视图，数据只来自事件（店里的 `events` 表与磁盘上同格式的 `logs/events-*.jsonl`），所以 worker 删了也能查、hub 没跑也能查：

- 折叠逻辑在 hub 的 `trace.rs`（纯函数，单元测试）：吃一串事件，按 run 归到 lead 与各任务——`task.status_changed` 告诉哪个 agent 在做哪个任务（改派、返工都记得）；每个 agent 的轮次（开始、结束、停止原因、报错 / 进程退出）、它跑的命令（`agent.tool_call` 里 `execute` 类的标题加 `agent.tool_detail` 的退出码与耗时）、`workspace_run`（`task.command`）、验收（`task.checked` 的每条命令与结果）、用量（`agent.usage` 按会话累计，同 `budget.rs` 的口径）、改派（`task.rerouted`）。每个任务给出一行“首个失败”：最早的失败验收、非零退出的命令，或出错的那一轮。
- 接口 `GET /v1/runs/{key}/trace` → `RunTrace`（读级）。
- CLI `crossagents run trace <id>`：hub 在时走接口；`--log` 时直接读 `logs/events-*.jsonl`，hub 不在也能查。
- GUI：run 页加「追溯」一节，按任务列出状态、首个失败、花费，展开看每一轮、每条命令（失败的标红、带输出尾巴）、每条验收。

### 不做

- 不存新的数据：全靠已有事件。一个 run 的事件从 store 全量读一遍；事件很多时会慢（目前每个 run 几千条，可接受）。
- 不做跨 run 的统计。

### 待实测

- 长 run（上万条事件）时接口耗时；磁盘日志被 14 天清理后，更早的 run 只剩 store 里的事件。

## 9. 第一次自举实测（2026-10-26）

第一次用 Work 在 CrossAgents 本身上做一件真事：TODO §七「CLI 还不能发附件」。

### 怎么跑的

- 独立的测试 hub（scratch home），主导 Claude Opus，项目是本仓库，另有一个 Sonnet worker 在团队里。托管「我审批」。
- 请求原文：给 `crossagents send` 加 `--file <path>`（可多次），按 hub 的规则（单文件 10 MB、每条 8 个、合计 20 MB，图片 / PDF / JSON / 文本）先检查再发，加测试，更新 TODO。

### 结果

- 4 分钟跑完，没有交计划、没有派任务：主导判断够小，自己在集成目录里做完，`run_finish` 收尾（W1 的轻量 Work 路径）。
- 质量可用：复用 `api-types` 里 hub 的上限常量和类型判断，不是抄数字；先查类型和大小再读文件；2 个单元测试 + 1 个 e2e；跑了 fmt 和 clippy；顺手改了 TODO.md 和 DESIGN.md 里 send 的用法。我在它的 worktree 里重跑 `cargo test -p crossagents-cli`，13 个全过。
- 一个提交（作者 CrossAgents，消息用主导在 `run_finish` 给的），压缩合并进 main 成为 `a5128dc`；main 上 `cargo test -p crossagents-cli` 通过。

### 卡住或别扭的地方

| # | 现象 | 影响 | 方向 |
|---|---|---|---|
| 1 | 每个 run / 任务的 worktree 各有一份 `CARGO_TARGET_DIR`：这次一个集成目录就 2.3 GB，第一次 `cargo test -p crossagents-cli` 约 1.5 分钟 | 多任务的 run 是 N 份全量编译、N × 2.3 GB；改到 hub 这类大 crate 时编译会成为主要耗时 | **已改**（2026-10-26）：同一 run 共享 `<worktrees>/<run>/target`，run 的集成目录删除时一起删；sccache 没做 |
| 2 | 轻量 Work 没有任何 hub 跑的验收：对错全凭主导自述 | 自动化时没人复核；这次是我手动重跑测试才确认 | **已改**（2026-10-26）：项目根目录的 `crossagents.toml` 声明 `[run] checks = [...]`（可选 `timeout_secs`，缺省 600）。run 有改动时，主导调 `run_land` 或 `run_finish` 之前，hub 在 run 的集成目录里跑这些命令，用 run 共享的 cargo target；没全过就拒绝这次调用，把失败的命令和输出交给主导，修好再调。结果存在 `run.checks`，收尾通知里写「验收：N/N 通过」。主导开场就会看到要跑哪些检查。文件写错了也算失败，免得一个拼写错误悄悄关掉验收。人从 run 页落地或结束不受这个限制。本仓库声明的是 `cargo test --workspace`，超时 1800 秒 |
| 3 | 主导试图 `python - <<EOF` 跑脚本，本机只有 Store 占位 python | 白费一步，它自己绕过去了 | 开场说明里带上本机可用的工具链（或在第一次找不到命令时提示） |
| 4 | run 页底部堆满别的 run 的「告诉你」通知 | 看这个 run 时被无关卡片挤占 | run 页只显示本 run 的通知，其余留给首页 |
| 5 | 提交作者是 `CrossAgents`，不是人 | 落地后 main 的历史里是 hub 的身份 | 落地时可选用人的 git 身份，或加 `Co-authored-by` |
| 6 | 压缩合并的提交消息是 `crossagents: <run 标题>`，主导在 `run_finish` 写的 `feat(cli): …` 丢了 | main 上的提交消息不像人写的，也不合约定格式 | squash 时用 run 分支上最后一个主导提交的消息（或 `run_finish` 的 `message`） |

### 这次没覆盖到

- 交计划、多个 worker 并行改同一仓库（冲突、合并、各自编译）。
- Codex worker（code-mode 工具名）在真实仓库里的表现。
- 改动涉及 GUI 时的验证（截图、mock 预览）。

## 10. 第二次自举实测：多 agent，全用 DeepSeek（2026-10-26）

做 TODO §七的两条：`acp-generic` 按 agent 声明的 prompt 能力收附件，以及「按能力降级」（hub 对不支持的类型返回 400，GUI 禁用入口并说明原因）。

### 怎么跑的

- 用真实的 hub（`~/.crossagents`）。主导 Lyra、worker 模板 Sage 都是 CodeBuddy 上的 DeepSeek v4.1 Flash；策略是计划要人批、人来落地，worker 走守卫审批。
- 请求里要求后端和 GUI 分成任务并行做，接口先定好。
- 第一次带上 §9 第 2 条的验收：本仓库 `crossagents.toml` 声明 `cargo test --workspace`。

### 结果

- 约 33 分钟收尾。主导先在 run 分支上提交接口（`768eb56`：`AttachmentSupport`、`Capabilities.attachments`、各驱动的固定能力），再交计划：后端给 Sage，GUI 给它自己招的 Rune（也是 DeepSeek）。两个任务的 spec 很细：要改的文件、接口不许动、范围、验收命令都写了；GUI 任务的验收自带 `npm ci`。
- 两个 worker 都卡在权限上（见下表第 1 条）。Rune 在主导催过一次后写完了 GUI，但上报被拒，只好发消息给主导；主导自己看 diff、跑类型检查和测试，在它的 worktree 里提交并合并。Sage 两次停下、没有改动，主导取消了后端任务，自己在集成目录里写完。
- `run_finish` 前 hub 跑了 `cargo test --workspace`，通过；收尾通知里写了「验收（crossagents.toml）：1/1 通过」。
- 我复核：GUI 的 typecheck 和 54 个单测通过；在 mock 预览里截图，codex 的输入框下面显示「这个 agent 只收图片和文本，PDF 发不了（codex-native）」。
- run 分支 22 个文件，+865 −62，还没落地。

### 卡住或别扭的地方

| # | 现象 | 影响 | 方向 |
|---|---|---|---|
| 1 | CodeBuddy 的 worker 改文件、跑 Bash、调协作工具（含 `task_complete`）全被 "Permission denied by SDK" 拒，hub 那边每次都判了允许 | 守卫审批下的 CodeBuddy agent 基本只能读；worker 白跑、上报不了 | **已改**（2026-10-26）：CodeBuddy 2.174 读权限回复里的 `allowed` / `reason`，不认 Claude Code 的 `behavior` / `message`，缺了就当拒绝。驱动对 CodeBuddy 额外带上这两个字段。三家原生驱动都补了真实 CLI 的「允许后真能写」冒烟测试（以前只测拒绝，读不懂的回复恰好也是拒绝，所以一直没暴露） |
| 2 | run 状态是 failed（「1 of 2 tasks did not merge」），其实主导把取消的任务自己做完了，验收也过了 | 看状态会以为没做成；更糟的是 failed 的 run 不让落地（这次只好用 git 手动压缩合并，hub 不知道已落地），收尾通知却写着「等你落地」 | **已改**（2026-10-26）：有任务没合并、但 run 分支有改动且项目验收全过时算 completed，备注写「N of M tasks did not merge; the run branch has the rest and passed the checks」。failed 的 run 也能落地，由人决定（没声明验收的项目，主导接手后仍是 failed）；分支没改动照样拒绝 |
| 3 | 任务合并或取消后，worker 的克隆连同对话记录一起删了 | 事后查不到 worker 当时卡在哪（这次是在它还在时读到的） | 克隆归档而不删，或在 run 的追溯里留下它的最后几轮 |
| 4 | 验收只有 cargo；GUI 的类型检查和单测要 `npm ci`，没放进 `crossagents.toml` | GUI 改动的对错靠 worker 自述加人复核 | run 共享一份 `node_modules`（像共享 cargo target 那样），再把 GUI 检查加进验收 |
| 5 | mock 预览里没有对话的 agent，transcript 的 `capabilities` 是 null | 截图看不到这类 agent 的能力提示，要换一个有对话的 | mock 也按插件给出能力 |

### 这次没覆盖到

- 真实 Claude / CodeBuddy 收图片和 PDF 的实际效果（§七的实测项）。
- 修好权限之后 CodeBuddy worker 完整走一遍「改、测、上报」。
