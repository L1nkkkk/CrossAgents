# Tutti 调研

> 2026-11-11 整理，分两遍。只读了文档，没有安装、没有运行、没有读实现代码。
>
> 叫 Tutti 的项目有两个，互不相干。**人在 2026-11-11 确认说的是桌面应用 [tutti-os/tutti](https://github.com/tutti-os/tutti)**，
> 所以本文以它为主；命令行那个 [nutthouse/tutti](https://github.com/nutthouse/tutti) 是第一遍没分清时顺带看的，留在 §3。
>
> - tutti-os/tutti，两遍都核对在 [`207beee`](https://github.com/tutti-os/tutti/tree/207beeea554435618dffd3b579d0ffe112b065c4)。
>   - 第一遍读了 `README.md`、`docs/README.md`，以及 `docs/architecture/` 下的 `agent-reference-mention-resolution.md`、`agent-reference-sources.md`、`connector.md`、`workspace-issue-manager.md`、`agent-session-replay.md`、`agent-provider-cli-updates.md`、`agent-runtime-preparation.md`、`workspace-workflows.md`、`agent-activity-packages.md`。后几篇很长，每篇只读了开头 5000 到 8000 字。
>   - 第二遍把 `docs` 拉到本地，完整读了：`superpowers/specs/2026-07-28-tutti-mode-agent-orchestration-design.md`、`architecture/issue-execution.md`、`architecture/agent-side-conversations.md`、`architecture/desktop-update-admission.md`、`conventions/troubleshooting/README.md` 和 `issue-execution.md`、`adr/0002`、`adr/0004`、`adr/0006`；另读了 `troubleshooting/desktop-release.md` 的两条和 `toolchain-browser-terminal.md` 的一条。
> - nutthouse/tutti，核对在 [`6b86cca`](https://github.com/nutthouse/tutti/tree/6b86cca7457364888032e6ff9c04f2a87fc14cb2)。读了 `README.md` 全文、`docs/WHY_TUTTI.md` 全文、`docs/AGENT_INTEGRATION_CONTRACT.md` 前半。
>
> 下面写「CrossAgents 现在怎样」的地方，哪些读了实现、哪些只搜了关键字，在 §5 分开列出。
> 结论落在 [TODO.md](../TODO.md) §三十二。

## 1. 两个项目各是什么

| | tutti-os/tutti（人说的这个） | nutthouse/tutti |
| --- | --- | --- |
| 形态 | 桌面应用，Electron 加一个常驻的 Go 服务 `tuttid` | Rust 命令行 `tt`，加一个本地网页面板 |
| 一句话 | 多个 agent 共用一个工作区：对话、文件、任务、应用产物互相看得见 | 把 agent 团队和流程写成 `tutti.toml`，像基础设施配置一样版本化 |
| 接的 agent | Claude Code、Codex、Hermes，另有自带的 Tutti Agent；架构文档里还出现 Cursor、OpenCode、OpenClaw、Kimi | Claude Code、Codex、Aider、OpenClaw，另有直连模型 API 的一条路 |
| 谁来拆任务 | 普通模式由产品把目标拆成子任务，人审过再分给 agent；Tutti Mode 由一个主 agent 拆和派 | 流程写死在配置里，步骤按依赖执行 |
| 隔离 | 每个会话一份私有运行目录，任务执行用 git worktree | 每个 agent 一个 git worktree，会话跑在 tmux 里 |
| 多人 | 开源版只有一个人。多人、跨设备、借用别人的 agent 在收费的 Tutti · VM 里 | 没有 |
| 许可 | Apache-2.0 | MIT |

第一遍的判断是「tutti-os 方向不同，重的是人在各个 agent 之间搬东西」。第二遍读了 Tutti Mode 之后要改：它有一套主 agent 带子 agent 的编排，和我们的 lead 带 worker 是同一件事，而且文档里记了大量踩过的坑。§2.1 是逐项对照。

## 2. tutti-os/tutti

### 2.1 Tutti Mode 和我们的 lead / worker 对照

Tutti Mode 是它的自动编排：一个主 agent（它叫源会话）把目标拆成任务图，子 agent 各做一个任务，任务结束后叫醒主 agent 决定下一步。

| 它的机制 | 我们的情况 | 结论 |
| --- | --- | --- |
| 服务只记事实、校验显式命令、恢复投递，不替 agent 选下一步，不自动派后继任务 | 「hub 不思考」。`crates/orchestrator/src/continuation.rs` 开头写的是同一句话 | 一致 |
| 每次任务结算生成一个持久的 checkpoint，排成队，同一时刻只处理一个；主 agent 必须用命令确认，回合结束不算确认 | 任务结算给 lead 发一条分了类的通知，lead 用工具推进。没有「每次结算都必须被确认」这本账 | 不照搬，理由见表后 |
| 主 agent 的每条命令带 checkpoint 号、任务图版本号、请求标识；同一标识重复提交只生效一次，载荷不同就拒绝 | 任务 key 重复会被拒（`plan.rs:31`）；`msg_send` 有幂等键；其余 lead 工具没有 | 不照搬，理由见表后 |
| 叫醒是落盘的租约，提交标识由叫醒号确定地算出来，丢了响应按标识查回，不产生第二个回合 | 通知有落盘的待投递表，带重试次数；continuation 先落盘「已用一次额度」再投递。排队中的普通消息只在内存里 | 通知已有；消息是缺口，即 T2 |
| 固定五分钟不活动的看门狗，子 agent 的流式输出不算活动 | continuation 默认不开，开了也有次数上限；stuck watch 可选，只看 run 自己的落盘事实，只报告不打断；另有失败率断路器 | 已有，比它保守 |
| 暂停必须在每个叫醒边界都生效（生成、列出、认领、派发四处） | `continuation_step` 对暂停的 run 返回不可叫醒 | continuation 这条路已有；通知那条路没核对 |
| 任务全到终态后进入目标评审，绝不按任务数自动完成 | `finalize` 阶段叫 lead 对照原始请求核对；`run_finish` 有未结任务时拒绝，改过东西的 run 要先过项目检查 | 自审已有；独立评审见 §2.3 |
| 主 agent 名下还有未结束的执行时，删除这个会话被整体拒绝，零副作用，界面给三条出路 | `archive_agent`（`crates/hub/src/api.rs`）直接归档，不看它是不是某个进行中 run 的 lead | 缺口，即 T5 |
| 人按停主 agent 的回合，等于终止这一轮编排：被取消的回合不算活动，不推迟看门狗 | continuation 不看回合是怎么结束的。lead 一空闲就重排下一次叫醒 | 缺口，即 T6 |
| 不在持有任务锁时调用 agent 宿主、git worktree 操作或别的可重入模块 | `continue_run` 持有 run 锁时调 `host.notify` | 有没有风险没核实，见 §2.4 |
| 启动时只做本地的落盘修复，监听器就绪之前不向 agent 发东西 | 没核对 | — |

为什么不照搬 checkpoint、图版本号和租约那一套：

- 它要解决的问题我们用别的办法避开了。checkpoint 排队是为了「两个任务同时结算，主 agent 只看到一个」。我们每次结算是一条单独落盘的通知，而且每次叫醒 lead 都要它先重读 run 和任务状态，不依赖通知本身带全信息。
- 围栏本身会制造一类新的失败。它的排障记录里有好几条是主 agent 被围栏挡住：任务图版本号过期被拒、返工时少填字段被拒、依赖没满足被拒，还专门写了一条「不要靠直接改 SQLite 绕过被拒的命令」。这说明模型在围栏前会反复撞，撞急了会找旁路。我们的 worker 是便宜模型，lead 也不总是最强的那档，命令越严撞得越多。
- 体量。七条命令各带三个围栏参数，七种 checkpoint，七种叫醒状态。这是一个专职团队的投入。

值得拿的是它用这套东西守住的两条结果：人能可靠地停下来（T6），带着活的 lead 不会被误删（T5）。这两条都能用很小的机械规则做到。

### 2.2 建议借鉴的

**T1 录下真实 agent 的进出帧，测试里回放。** 来自 `agent-session-replay.md`。

它的做法：录一个会话的全过程存成 Cassette，回放时界面、服务、会话管理、各家适配器全用真的，只把「和 agent 进程通信」这一层换成按录像出帧。适配器必须先发出和录像一致的请求，录下的回复才放出来。录像带清单、哈希和大小上限，存盘前把账号、路径这类字段抹掉。文档自己承认两个代价：匹配很严，启动探测或消息顺序稍有变化录像就作废；它不验证真实服务的可用性、额度和模型行为。

对我们的意义：TODO 里有 33 行带「实测」二字，其中一类是「只有 mock 验证，真实 Claude / Codex / CodeBuddy 没跑过」。`crates/mock-agent` 的脚本是人照着协议写的，不是从真实 CLI 录的，所以协议有出入时 mock 测不出来。把真实 CLI 的输出录一次，交给 mock-agent 回放，这类条目跑过一次就变成可重复的测试，不再靠人记得去测，也不再每次花额度。CLI 升级后重录一份，和旧录像不一致的地方就是协议变了的地方，§五「每次升级后跑冒烟」可以落在这上面。

缩小的做法：只在驱动拉起子进程的那一层录和放，不做它那套经浏览器调试协议驱动界面的部分。匹配放宽到「消息类型和关键字段」，不逐字节比，避开它自己说的易碎。录的时候学 `self_check.rs`：空的临时目录、合成的小任务，录像里就没有真实代码和对话。

它管不了的：Mac 上的界面、SSH 隧道、打洞这些要真机的验收；额度和模型下线这类线上行为。

**T2 发送意图先落盘，再投递。** 来自 `workspace-issue-manager.md`、`workspace-workflows.md`，第二遍在 `issue-execution.md` 和 Tutti Mode 设计里又读到同一个做法。

它的做法：发起一次执行时，执行记录和「准备投递」的意图在同一个 SQLite 事务里写入，意图里带固定的会话标识、提交标识和提示词快照。投递前先租约；确认送到标为已投递；确认没开始就整体结清；结果不明就退回「准备投递」，启动时和定时的恢复流程用同一份内容、同一组标识重试。提交标识是从记录号确定地算出来的（比如 `issue-run:<runID>`），所以丢了响应可以按标识去问「这个回合到底起没起」，不用猜。

对我们的意义：§28.5 写着「排队中的消息只在内存里」，§28.3 写着 hub 重启前排队的消息，重启后跑出的回合没有来源。这两条是同一个缺口：消息从「hub 收下」到「agent 开始这一回合」之间没有落盘的记录。Codex 插话的 `outcome_unknown` 也是同一类，有了确定的提交标识就有了对账的依据。

第二遍核对时看到，通知这条路我们已经有落盘的待投递表（`list_pending_notices`，带 `wake_tries`，还有一条重启后重试一次的测试）。所以 T2 的范围比第一遍写的小：只剩人和 agent 发的普通消息，做法可以直接沿用通知那张表的。全是机械规则，不违反「hub 不思考」。

**T5 带着进行中 run 的 lead 不能直接归档。** 来自 Tutti Mode 设计的「源会话保护」和 `issue-execution.md`。

它的做法：主 agent 名下还有未结束的执行时，删除会话的请求被整体拒绝，返回一个固定的原因码，不产生任何副作用。界面给三条出路：返回、去看那个执行、停止并归档那个执行。主 agent 的会话真丢了（比如文件被删），执行进入「孤儿」状态，停掉叫醒和看门狗，只能由人显式归档。

对我们的意义：`archive_agent` 现在直接归档，之后只处理检出目录（`lead_left` / `lead_returned`），不看这个 agent 是不是某个进行中 run 的 lead。归档之后 worker 交回的结果没人接；`continuation_step` 会进入 `unavailable`（「The lead is archived; restore it before continuing.」），但 continuation 默认不开，不开的时候 run 就静静地挂着。§28.5 记的「已归档 lead 开新一轮，run 一直不结束」是同一类问题的另一个入口。

做法：归档前查这个 agent 是不是未结束 run 的 lead。是就拒绝，回复里列出 run，界面给「去看这个 run」和「结束这一轮再归档」两条出路。连同 §28.5 那条一起修：给已归档的 lead 开新一轮时，要么先恢复它，要么直接拒绝。改动小，全是机械规则。

**T6 人按停 lead 的回合之后，自动续跑要停下。** 来自 Tutti Mode 设计和 `troubleshooting/issue-execution.md`。

它的做法：人停掉主 agent 的回合，这个会话名下所有未结束的执行一起归档。它的文档专门写了为什么：早先的实现把被取消的回合当成一次普通的活动，看门狗过五分钟又把主 agent 叫起来，人按了停却停不住。重开应用也不会复活旧的自动化。

对我们的意义：编排器在 lead 变成空闲时调 `settle_continuation`（`engine.rs` 里 `AgentStatusChanged → Idle` 那一支），不管这个回合是正常结束还是被人按停的。`continuation.rs` 里没有任何地方看回合的结束原因。所以开了 continuation 的 run，人按停 lead 之后，过一个间隔它又会被叫起来，直到用完 `max_attempts`。这是读代码得出的，没有实测。

做法不必像它那样把整轮归档，那太重，人按停多半只是想插一句话。规则可以是：lead 的回合以「被人中断」结束时，这个 run 的 continuation 进入一个停住的阶段（和断路器的 `automation_paused` 同一个机制），人给 lead 发下一条消息或者显式重新放开之后才恢复。hub 重启后保持停住。

**T7 Codex 驱动：处理 `serverRequest/resolved`，并记下恢复后不重发审批。** 来自 ADR 0006、ADR 0002。

它的做法和实测：

- Codex 可能自己把一个挂着的审批了结掉（回合被中断、超时），这时它发 `serverRequest/resolved`。不处理这条通知，界面上的审批卡就永远挂着。
- 它实测 `codex-cli 0.142.5`：`thread/resume` 之后**不会**重发之前挂着的服务端请求，被打断的回合也不会恢复。所以重启后要把这些挂着的审批直接标成过期，不能等它重发。
- 协议类型按 Codex 源码的提交号钉住，从上游已提交的 JSON schema 生成；生成的类型容忍未知字段，通知和服务端请求都留一个「不认识的方法」兜底。

对我们的意义：`docs/research/codex-app-server.md` 记了 `serverRequest/resolved` 这条通知，但 `crates` 里搜不到对它的处理。Codex 带外了结一个审批时，hub 这边的待批准项会不会一直挂着，要看回合结束时有没有别的路径清掉它，这一点没核实。未知请求的兜底我们有（`codex_native/wire.rs` 的 `_ =>` 分支按通用的 `decision` 回答）。

做法：先核对，半小时的事。真有挂住的情况就在驱动里接上这条通知；「恢复后不重发」这条实测结论记进 `codex-app-server.md`，省得以后自己再测一遍。

**T8 起 git 子进程时清掉继承来的 `GIT_DIR`、`GIT_WORK_TREE`。** 来自 `troubleshooting/toolchain-browser-terminal.md`。

它踩的坑：测试里建临时仓库的代码，在一个 linked worktree 里被带着 `GIT_DIR` 的环境调起来，结果 `git init`、`git config` 全打在了共享仓库上，把它改成了 `core.bare=true`，所有 worktree 一起坏。修法是在起进程的边界清掉 `GIT_DIR` 和 `GIT_WORK_TREE`，设 `GIT_CEILING_DIRECTORIES`，作者身份用 `-c` 传而不是写配置。

对我们的意义：我们大量用 worktree，hub 起 git 时只设了 `GIT_INDEX_FILE`，没有清别的。hub 平时由桌面端拉起，环境里一般没有这两个变量，所以出事的概率低；会出事的场景是有人从 git hook 或 `git rebase --exec` 里起 hub 或跑测试。后果是整个仓库坏掉，修起来只是几行。属于便宜的保险，不急。

**T3 `@` 出来的是句柄，内容由 agent 用到时再取。** 来自 `agent-reference-mention-resolution.md`。

它的做法：输入框里 `@` 一组产物，消息里只放一个短链接（`mention://workspace-reference/<id>?source=app|task&workspaceId=…`），不展开成文件列表。agent 侧有一个注入的 `reference` 技能，用到时跑一条命令行拿到当前的文件清单，只读和请求有关的。几条规矩值得照搬：链接上的显示名和数量只管显示，不作为解析依据；解析是实时的，发消息之后新增或删掉的产物都算数；引用是只读的，引用一个任务不等于授权去执行它；解析失败就报失败，不许凭旧清单猜路径。`@` 的种类是分开的：被动引用、调用应用、操作任务、交给另一个 agent 或会话。

对我们的意义：现在 `@` 只能点名协作者或模型（`apps/gui/src/mention.tsx`），选中后是纯文本，含义写在 agent 的指引里。人想让一个会话接着另一个会话、某个任务或某一轮 run 的结果做，只能自己复制。可以给 `@` 菜单加三类：会话、任务、run，插入一个短记号，agent 用已有的工具按 id 去读。省下的是人的复制，以及整段贴进来的上下文。

先要确认的：这件事在 run 里用处不大，lead 派活本来就带交接卡；有用的是 run 之外的普通对话。人平时跨会话复制得多不多，我没有数据，要问人。

**T4（先核对再说）等人处理的事放在一处。** 来自 README 的 Control Center。

我们已经有「等你回答」卡片（`apps/gui/src/questions.tsx`，首页和会话页都有），收的是 agent 提的问题、通知、超时按默认处理的回答。没核对的是另外三类在不在同一处：工具权限的批准、等人评审的任务、等人落地的 run。如果不在，补进同一个区域即可，不必另做一页。

### 2.3 记下，不排期

**run 级的独立评审。** 它的目标评审有两种模式：`self` 是叫醒主 agent 自己核对，`independent` 是另起一个隐藏的、严格只读的评审会话，给出「目标达成 / 还要干活 / 说不准」。我们现在是 lead 自审加项目检查，相当于它的 `self`。

不排的理由和 §二十五 的跨厂商顾问一样：这是拿额度换质量，而「lead 自审放过了没做完的 run」这件事发生得多不多没有量过。任务级的评审闭环 0.2.12 才上线，先看它的数据。

哪天要做，它的四条规矩值得照搬：评审会话只读，能力用白名单逐项列出，不是「禁用几样」；评审意见只是建议，决定权仍在 lead；lead 推翻「还要干活」的意见时必须留一条理由，可以事后查；评审自己失败了不自动退回自审，而是报出来等人。

**Side 对话。** 在一个会话上开一个临时分支问点别的：不写进正式历史，不出现在会话列表，进程没了就过期。Codex 上用 `thread/fork`（`ephemeral: true`、`excludeTurns: true`）加 `thread/inject_items` 实现。不排：只有 Codex 有现成的协议支持，我们已经有插话和中断，没听人提过这个需要。

**几条小规矩**，来自 `troubleshooting/issue-execution.md`，以后改 lead 工具时可以参考：

- 拒绝一条命令时给稳定的原因码（它用 `stale_graph_revision`、`missing_agent_target`、`dependency_unsatisfied` 这类），不要只给一句话。模型拿到原因码才知道是该重读状态再试，还是该换做法。
- 叫醒 lead 的提示里带精确的命令示例，并写明恢复的上限（它定的是「重读一次、重试一次」），再不行就报给人。
- 返工时只给了新的标题和内容，没给的字段（agent、模型、权限、目录、依赖）从原任务继承，不要因为缺字段拒绝。我们的返工是退回同一个任务，不涉及；`task_assign` 换人时要不要继承没看。
- 可重入死锁的回归测试，要让被 mock 的取消操作在返回之前就同步发布结算事件。结算晚一拍的 mock 复现不了这类死锁。

### 2.4 持有 run 锁时调了什么（只是记一笔）

它的规则是绝不在持有任务锁时调用 agent 宿主或 git worktree 操作，因为这些调用可能反过来触发要拿同一把锁的结算。我们的 `continue_run` 一进来就拿 run 锁，之后在锁里调 `host.notify` 投递叫醒。`notify` 如果只是往待投递表写一行就没事；如果它会等 agent 的进程响应，就有它说的那种风险。这一点没读 `notify` 的实现，不下结论。§二十四 的「应用到原目录后的复跑同步阻塞」是同一类毛病已经露头的一处。碰到卡死时先往这个方向查。

### 2.5 不学的

- checkpoint、任务图版本号、叫醒租约这整套围栏。理由在 §2.1 表后。
- 应用中心（图像、原型、文档、幻灯片）和连接器市场。`connector.md` 光是清单版本、签名、授权、安装状态机就写了近五万字，是另一个产品的体量。我们的扩展（§二十六）够用。
- 多人房间、云端共享状态、借用别人的 agent、卖模型额度。
- 产品自己把目标拆成任务。我们由 lead 来拆，拆得不好可以换 lead 的模型，不必在 hub 里再放一套。
- 替用户安装和升级各家 CLI（`agent-provider-cli-updates.md`）。它为了不动用户自己装的那份，做了来源识别、隔离目录、所有权判断一整套。我们只用用户已装好的 CLI，`self_check` 记下哪个版本能用，已经够了。
- 强制升级和更新准入（`desktop-update-admission.md`）。它有云端服务要保版本一致，我们没有。
- 会话的可恢复删除（墓碑、按连通分量恢复）。我们有归档。
- 省额度模式 RTK（`agent-runtime-preparation.md`）：给每个会话的 PATH 前面插一个改写命令、压缩命令输出的程序。它减少的是进入上下文的工具输出。按 [USAGE-TRIM.md](../USAGE-TRIM.md) 的实测，我们 worker 的缓存命中 99%，把上下文压小只省约 7% 的 credit，这条不值得做。记在这里，省得以后再查一遍。

已经有的，不用再做：

- 事件游标用存储分配的单调序号，不用时间戳（ADR 0004 讲的是毫秒时间戳在同一毫秒内碰撞会丢消息）。我们的事件流用的是 `latest_event_seq`。
- 同一个状态目录只让一个服务进程用。我们有 `hub.lock`（`crates/hub/src/lib.rs`）。它踩的另一半坑，开发版服务裸跑时选中了正式版的状态目录，我们有没有同样的风险没核对。

## 3. nutthouse/tutti（附带，不是人问的那个）

第一遍没分清两个项目时看的。下面两条和 tutti-os 无关，去留由人定。我的意见：N1 对着的是真发生过的问题，值得留；N2 可有可无。

### 3.1 当时挑出来的

**N1 开工前先验一遍环境，包括在基线上跑一次验收。** 来自 `tt doctor` 和 `tt verify`。

它的做法：`tt doctor` 检查会话工具、账号配置、各家 CLI 在不在、配置里声明的命令和环境变量在不在，还探一下正在跑的 agent 登录是否有效；`--strict` 用在 CI 里。`tt run --dry-run --json` 把流程里要跑的命令原样列出来，供事先批准。

对我们的意义：我们有按插件的自检和派活前拒绝已下线模型，缺的是「这一轮的验收命令在基线上本来是不是绿的」。§28.8 列了四组偶发失败的测试，其中一条让一次验收白挂。基线不绿时，worker 会被自己没造成的失败退回，返工一轮就是一轮额度。做法是 run 开工时在基线提交上跑一次验收命令并记下结果：不绿就告诉 lead 和人，之后验收失败时拿来比，分得清是原有的还是新引入的。我在 `crates/orchestrator/src` 里搜「baseline / 基线」只看到落地时的基线分支，没看到这种检查。这是我从它的两条命令引申出来的，它自己没有这样用。

**N2 模板写角色，角色到模型的对应放在一处。** 来自 `[roles]`。

它的做法：agent 声明 `role = "reviewer"`，`[roles]` 表里写 `reviewer = "codex"`，换一行，所有用这个角色的 agent 一起换。

对我们的意义：§二十四 记着 Hazel 4 模板仍指向已下线的 `deepseek-v4.1-flash-sg`。模型下线会一再发生，每次都要逐个改模板。让模板指向一个档位名（比如「便宜的 worker」「评审」），档位到具体模型的对应放在设置里一处。模板现在怎么存模型我没有读，改动面要先看。

### 3.2 已经有的，或者不学的

| 它有的 | 我们的情况 |
| --- | --- |
| 上下文超过阈值自动生成交接包 | 已有：lead 每轮新会话、返工换会话（[USAGE-TRIM.md](../USAGE-TRIM.md) §8） |
| 每个 agent 一个 worktree、`tt land`、合并闸门 | 已有：任务 worktree、验收、落地 |
| `tt review` 发评审包给评审 agent | 已有：评审闭环（§二十八） |
| `[budget]` 按周 token 预警或拦截 | 已有任务预算，默认不开 |
| 从本地会话日志读用量 | 已有，CodeBuddy 的口径今天刚修过 |
| issue 认领租约 | 不需要：我们由一个 hub 派活，没有多个编排器抢同一件事 |
| tmux 里跑会话、靠屏幕输出判断空闲 | 不学：我们走各家 CLI 的结构化协议，比抓屏可靠 |
| 配置写死的流程（步骤、依赖、嵌套） | 不学：和 lead 临场决定是两条路，两套并存只会更乱 |
| 登录失效、限流时自动换账号 | 先不学：§28.9 记着 Cursor 的限额判定可能不准，判定不准时自动换只会乱换 |

## 4. 建议顺序

T2 → T5 → T6 → N1，T7 和 T4 先核对，其余记下不排。

- T2、T5、T6 排前面：都对着「人以为的状态和实际状态不一致」这一类问题（消息以为发了、lead 以为还在、以为停住了），改动都小，都是机械规则。T5 和 §28.5 一起做。
- N1 对着已经发生过的验收白挂。
- T7 先花半小时核对有没有审批卡挂住的情况，有再修。
- T1 收益大但要先定录像格式，值得单独开一轮出设计，不和上面几条抢。
- T8 是便宜的保险，哪次改到起 git 的地方顺手做。
- T3 等人说了平时跨会话复制得多不多再定。

## 5. 没核实的

- 两个项目都没运行过，上面写的都是它们文档里的说法。
- tutti-os 第一遍读的几篇长文档只读了开头，`connector.md` 读了约六分之一。Control Center 没有找到对应的架构文档，只看了 README 的介绍。没读的相关文档：`adr/0003`、`0005`、`0007`、`0008`、`0010`，`architecture/claude-code-sdk-requirements.md`、`claude-code-sdk-runtime.md`、`workspace-agents-and-automation.md`，`troubleshooting/agent-session-lifecycle.md`（很长）、`agent-approvals-subagents.md`，以及 `specs` 下的目标控制、原生子 agent、会话分叉几篇。
- CrossAgents 这边读了实现的：`continuation.rs` 全文、`api.rs` 的 `archive_agent`、`engine.rs` 里 lead 空闲时的那一支、`codex_native/wire.rs` 回答服务端请求的那一段。**都没有实测**，T5、T6 写的后果是从代码推出来的。
- CrossAgents 这边只搜了关键字、没读实现的：
  - 通知的待投递表（只看到 `list_pending_notices`、`wake_tries` 和一条测试名）
  - `serverRequest/resolved`（`crates` 里搜不到；回合结束时待批准项会不会被别的路径清掉没看）
  - hub 重启时挂着的审批怎么处理
  - 暂停的 run 会不会仍收到任务结算的通知
  - `host.notify` 会不会等 agent 响应
  - 开发版 hub 会不会误用正式版的状态目录
  - 基线验收（只搜了 orchestrator）
  - agent 能不能按 id 读另一个会话的转录（看到 `agent_get`、`history_task`、`agent_commit_diff` 等工具名，没逐个看）
  - 「等你回答」之外的三类待处理事项显示在哪
  - 模板怎么存模型
- 「TODO 里 33 行带实测」是按行数的字面统计，不等于 33 个独立条目，也不是每条都能靠回放解决。
