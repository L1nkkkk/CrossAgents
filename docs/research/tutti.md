# Tutti 调研

> 2026-11-11 整理。只读了文档，没有安装、没有运行、没有读实现代码。
> 叫 Tutti 的项目有两个，互不相干，这里都看了：
>
> - [tutti-os/tutti](https://github.com/tutti-os/tutti)，核对时 `main` 在 [`207beee`](https://github.com/tutti-os/tutti/tree/207beeea554435618dffd3b579d0ffe112b065c4)。读了 `README.md`、`docs/README.md`、`docs/architecture/` 下的 `agent-reference-mention-resolution.md`、`agent-reference-sources.md`、`connector.md`、`workspace-issue-manager.md`、`agent-session-replay.md`、`agent-provider-cli-updates.md`、`agent-runtime-preparation.md`、`workspace-workflows.md`、`agent-activity-packages.md`。后几篇很长，每篇只读了开头 5000 到 8000 字。
> - [nutthouse/tutti](https://github.com/nutthouse/tutti)，核对时 `main` 在 [`6b86cca`](https://github.com/nutthouse/tutti/tree/6b86cca7457364888032e6ff9c04f2a87fc14cb2)。读了 `README.md` 全文、`docs/WHY_TUTTI.md` 全文、`docs/AGENT_INTEGRATION_CONTRACT.md` 前半。
>
> 下面写「CrossAgents 现在怎样」的地方，凡是只搜过关键字、没读实现的，都在 §5 列出。
> 结论落在 [TODO.md](../TODO.md) §三十二。

## 1. 两个项目各是什么

| | tutti-os/tutti | nutthouse/tutti |
| --- | --- | --- |
| 形态 | 桌面应用，Electron 加一个常驻的 Go 服务 `tuttid` | Rust 命令行 `tt`，加一个本地网页面板 |
| 一句话 | 多个 agent 共用一个工作区：对话、文件、任务、应用产物互相看得见 | 把 agent 团队和流程写成 `tutti.toml`，像基础设施配置一样版本化 |
| 接的 agent | Claude Code、Codex、Hermes，另有自带的 Tutti Agent；架构文档里还出现 Cursor、OpenCode、OpenClaw、Kimi | Claude Code、Codex、Aider、OpenClaw，另有直连模型 API 的一条路 |
| 谁来拆任务 | 产品把目标拆成子任务，人审过再分给 agent | 流程写死在配置里，步骤按依赖执行 |
| 隔离 | 每个会话一份私有运行目录 | 每个 agent 一个 git worktree，会话跑在 tmux 里 |
| 多人 | 开源版只有一个人。多人、跨设备、借用别人的 agent 在收费的 Tutti · VM 里 | 没有 |
| 许可 | Apache-2.0 | MIT |

和 CrossAgents 的关系：结构上 nutthouse 更像我们（worktree、评审、合并闸门、交接包、用量），只是它的流程是配置写死的，我们是 lead 临场决定。tutti-os 的方向不同，它重的是「人在各个 agent 之间搬东西」这件事，以及一批内置应用；我们重的是 lead 带 worker 把一件事做完。

## 2. tutti-os/tutti

### 2.1 建议借鉴的

**T1 录下真实 agent 的进出帧，测试里回放。** 来自 `agent-session-replay.md`。

它的做法：录一个会话的全过程存成 Cassette，回放时界面、服务、会话管理、各家适配器全用真的，只把「和 agent 进程通信」这一层换成按录像出帧。适配器必须先发出和录像一致的请求，录下的回复才放出来。录像带清单、哈希和大小上限，存盘前把账号、路径这类字段抹掉。文档自己承认两个代价：匹配很严，启动探测或消息顺序稍有变化录像就作废；它不验证真实服务的可用性、额度和模型行为。

对我们的意义：TODO 里有 33 行带「实测」二字，其中一类是「只有 mock 验证，真实 Claude / Codex / CodeBuddy 没跑过」。`crates/mock-agent` 的脚本是人照着协议写的，不是从真实 CLI 录的，所以协议有出入时 mock 测不出来。把真实 CLI 的输出录一次，交给 mock-agent 回放，这类条目跑过一次就变成可重复的测试，不再靠人记得去测，也不再每次花额度。CLI 升级后重录一份，和旧录像不一致的地方就是协议变了的地方，§五「每次升级后跑冒烟」可以落在这上面。

缩小的做法：只在驱动拉起子进程的那一层录和放，不做它那套经浏览器调试协议驱动界面的部分。匹配放宽到「消息类型和关键字段」，不逐字节比，避开它自己说的易碎。录的时候学 `self_check.rs`：空的临时目录、合成的小任务，录像里就没有真实代码和对话。

它管不了的：Mac 上的界面、SSH 隧道、打洞这些要真机的验收；额度和模型下线这类线上行为。

**T2 发送意图先落盘，再投递。** 来自 `workspace-issue-manager.md` 和 `workspace-workflows.md`。

它的做法：发起一次执行时，执行记录和「准备投递」的意图在同一个 SQLite 事务里写入，意图里带固定的会话标识、提交标识和提示词快照。投递前先租约；确认送到标为已投递；确认没开始就整体结清；结果不明就退回「准备投递」，启动时和定时的恢复流程用同一份内容、同一组标识重试。另有一张表把客户端提交标识对应到预先分配的回合 id，重试时对账，不会重复派发。

对我们的意义：§28.5 写着「排队中的消息只在内存里」，§28.3 写着 hub 重启前排队的消息，重启后跑出的回合没有来源。这两条是同一个缺口：消息从「hub 收下」到「agent 开始这一回合」之间没有落盘的记录。照它的做法加一张待投递表，重启后按表重放，这两条一起解决，Codex 插话的 `outcome_unknown` 也有了对账的依据。全是机械规则，不违反「hub 不思考」。

**T3 `@` 出来的是句柄，内容由 agent 用到时再取。** 来自 `agent-reference-mention-resolution.md`。

它的做法：输入框里 `@` 一组产物，消息里只放一个短链接（`mention://workspace-reference/<id>?source=app|task&workspaceId=…`），不展开成文件列表。agent 侧有一个注入的 `reference` 技能，用到时跑一条命令行拿到当前的文件清单，只读和请求有关的。几条规矩值得照搬：链接上的显示名和数量只管显示，不作为解析依据；解析是实时的，发消息之后新增或删掉的产物都算数；引用是只读的，引用一个任务不等于授权去执行它；解析失败就报失败，不许凭旧清单猜路径。`@` 的种类是分开的：被动引用、调用应用、操作任务、交给另一个 agent 或会话。

对我们的意义：现在 `@` 只能点名协作者或模型（`apps/gui/src/mention.tsx`），选中后是纯文本，含义写在 agent 的指引里。人想让一个会话接着另一个会话、某个任务或某一轮 run 的结果做，只能自己复制。可以给 `@` 菜单加三类：会话、任务、run，插入一个短记号，agent 用已有的工具按 id 去读。省下的是人的复制，以及整段贴进来的上下文。

先要确认的：这件事在 run 里用处不大，lead 派活本来就带交接卡；有用的是 run 之外的普通对话。人平时跨会话复制得多不多，我没有数据，要问人。

**T4（先核对再说）等人处理的事放在一处。** 来自 README 的 Control Center。

我们已经有「等你回答」卡片（`apps/gui/src/questions.tsx`，首页和会话页都有），收的是 agent 提的问题、通知、超时按默认处理的回答。没核对的是另外三类在不在同一处：工具权限的批准、等人评审的任务、等人落地的 run。如果不在，补进同一个区域即可，不必另做一页。

### 2.2 不学的

- 应用中心（图像、原型、文档、幻灯片）和连接器市场。`connector.md` 光是清单版本、签名、授权、安装状态机就写了近五万字，是另一个产品的体量。我们的扩展（§二十六）够用。
- 多人房间、云端共享状态、借用别人的 agent、卖模型额度。
- 产品自己把目标拆成任务。我们由 lead 来拆，拆得不好可以换 lead 的模型，不必在 hub 里再放一套。
- 替用户安装和升级各家 CLI（`agent-provider-cli-updates.md`）。它为了不动用户自己装的那份，做了来源识别、隔离目录、所有权判断一整套。我们只用用户已装好的 CLI，`self_check` 记下哪个版本能用，已经够了。
- 会话的可恢复删除（墓碑、按连通分量恢复）。我们有归档。
- 省额度模式 RTK（`agent-runtime-preparation.md`）：给每个会话的 PATH 前面插一个改写命令、压缩命令输出的程序。它减少的是进入上下文的工具输出。按 [USAGE-TRIM.md](../USAGE-TRIM.md) 的实测，我们 worker 的缓存命中 99%，把上下文压小只省约 7% 的 credit，这条不值得做。记在这里，省得以后再查一遍。

## 3. nutthouse/tutti

### 3.1 建议借鉴的

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

T2 → N1 → T1 → N2 → T3，T4 先核对。

T2 和 N1 排前面，因为它们对着的是已经发生过的问题（重启丢排队的消息、验收白挂），改动小。T1 收益最大但要先定录像格式，值得单独开一轮出设计。T3 等人说了平时复制得多不多再定。

## 5. 没核实的

- 两个项目都没运行过，上面写的都是它们文档里的说法。
- tutti-os 几篇长文档只读了开头，`connector.md` 读了约六分之一。Control Center 没有找到对应的架构文档，只看了 README 的介绍。
- CrossAgents 这边只搜了关键字、没读实现的：基线验收（只搜了 orchestrator）；agent 能不能按 id 读另一个会话的转录（看到 `agent_get`、`history_task`、`agent_commit_diff` 等工具名，没逐个看）；「等你回答」之外的三类待处理事项显示在哪；模板怎么存模型。
- 「TODO 里 33 行带实测」是按行数的字面统计，不等于 33 个独立条目，也不是每条都能靠回放解决。
