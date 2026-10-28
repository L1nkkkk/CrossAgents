# LESSONS 任务书：项目排障记录

> 依据：2026-10-28 讨论（run 里随手记问题和绕法，收尾统一整理；按项目共享、跨项目隔离；按需查，不当规则注入）· 状态：设计定稿，未开工 · 2026-10-28

## 1. 目标

worker 做任务时总会遇到问题，大多数都用别的办法绕过去了。这些「问题和解法」今天只留在那一次会话的记录里，下一个 worker 碰到同样的问题还要从头排查。

要做到：
- **随手记**：run 进行中就记，不等 run 成功；同一 run 的其他 worker 马上能查到。
- **收尾整理**：run 结束时（成功、失败、取消都算），把有复用价值的内容整理进项目的排障记录（下称 lessons），并关联原始 run。
- **按需查**：lessons 是参考资料，不是规则。查到以后由 worker 判断是否适用；发现方法失效时，补充新结果，或者把它标成已被替代。
- **按项目共享，跨项目隔离**：同一项目在不同节点上运行都能查到；其他项目默认看不见。

这是 CrossAgents 自我迭代成本最低的一种：不改代码、不过验收，只积累排障资料。

## 2. 现状（已核对代码）

| 已有 | 现状 | 对本设计的影响 |
|---|---|---|
| run 笔记（`note_write` / `note_read` / `note_list`，`crates/orchestrator/src/notes.rs`） | 范围有 `run` 和 `team` 两种。key 长 1–120 字符，按 key 覆盖写，正文最多 16 KB，只有作者或主导能删，每次写都记 `NoteWritten` 事件 | run 中的采集可以复用（§4.1）。key 要带 uuid 才不会被覆盖 |
| run 笔记的读取范围 | `note_run` 按「调用者当前所在的 run」找笔记；run 结束后，没人能再用工具读到它，原主导也不行 | 给已中断的 run 补整理时，不能靠笔记工具，hub 要提供按 run id 读取的专门通道（§4.3） |
| `team` 笔记 | 跨 run 保留，但不分项目、不注入、没有结构；实际几乎没人用 | 前车之鉴：只有能力、没有入口，就没人用（§4.5） |
| 项目身份 | hub 里没有「项目」实体。GUI 的工作区是前端按路径拼出来的（`apps/gui/src/workspace.ts`），共享编译目录的 `repo_key` 也按本机路径算 | `project_id` 要新建（§4.2）。按路径算的话，同一个仓库在两台机器上会被当成两个项目 |
| 存储 | sqlx 用的是内置打包的 SQLite（`libsqlite3-sys` 的 `bundled`），编译时开了 `SQLITE_ENABLE_FTS5` | 直接用 FTS5 建搜索索引，不需要新依赖 |
| 项目规则（DOGFOOD §6，`CROSSAGENTS.md`） | 开会话时注入给所有 agent | lessons 不进规则文件。要升格成规则，由人自己抄进去（§6） |

## 3. 主流程

```
Worker 随手记录 issue（run 笔记 issue/<task_id>/<uuid>）
        ↓
run 内持久化保存，同 run 的 worker 可查；失败或取消也保留
        ↓
收尾整理：逐条处理（新增 / 合并 / 仅留在 run）；没处理完的显示成待办
        ↓
项目 lessons 表 + FTS5 搜索索引，关联原始 issue
        ↓
开场给出入口 + 失败结果附相关条目 + 主动搜索
        ↓
Worker 判断是否适用，并补充新的验证结果
```

## 4. 设计

### 4.1 run 中：记录事实

worker 复用 `note_write`，范围是 `run`，key 用 `issue/<task_id>/<uuid>`。用 uuid 而不是序号，是为了避免重试或并发时互相覆盖。不在任务里的记录（比如主导自己遇到的），`task_id` 写 `lead`。

正文按约定写成一小段 Markdown，至少包含：

```markdown
问题：<现象，原样带上关键报错>
环境：<系统、工具及版本、节点；与问题相关的才写>
尝试：<试过什么，哪些没用>
处理：<最后怎么做的>
结果：已解决 | 临时绕过 | 未解决
相关文件：<路径，可选>
```

- hub 不解析正文，只靠 key 前缀 `issue/` 识别这是一条问题记录。格式靠 `note_write` 的工具说明约定。
- 记录在写入时就已保存，和 run 最后成不成功无关。
- 同一 run 的 worker 用 `note_list { prefix: "issue/" }` 查，接手任务前先看一眼，避免重复排查。

**不进 lessons 的东西：**
- 环境状态，比如 Python 路径、工具版本、代理通不通：检查起来便宜的，就现场检查，同一 run 内复用结果（这和 [SELF.md](SELF.md) B4「开场说明带本机工具链」是同一件事）。
- 只有排查过程本身有复用价值时，才写成 issue。

### 4.2 项目身份

lessons 归属于 hub 里的一个项目，用稳定的 `project_id`，不用路径，也不直接用 remote URL：

```
projects
  project_id            ← lessons 的实际归属
  ├─ 已绑定的 git remote  ← 跨节点识别同一个项目
  ├─ 节点 A 的本地仓库路径
  └─ 节点 B 的本地仓库路径
```

- **remote 要规范化**：`git@github.com:o/r.git` 和 `https://github.com/o/r` 视为同一个。具体做法：只留主机名加路径，去掉 `.git` 后缀和末尾的 `/`，主机名统一成小写。
- **首次接入**：一个仓库路径第一次出现（建 run、建 agent 时），用它的 remote 匹配已有项目。
  - 唯一命中：自动绑定，记下「节点 + 路径」。
  - 没有命中：新建一个项目。
  - 命中多个，或者 remote 有冲突：先新建，在 GUI 里让人明确绑定或合并。
- **没有 remote 的仓库**：先按「节点 + 路径」新建项目，以后可以由人手动绑定到已有项目。
- **fork 不自动合并**：remote 不同就是不同的项目，即使提交历史相同。两个独立项目的记录不能混在一起。
- **worktree** 按 `.git` 文件找到主仓库，与 `workspace::repo_key` 的做法相同，再按主仓库路径找项目。
- **remote 改了**：迁移或改名后，一个已绑定的路径出现了新的 remote。这时把新 remote 也绑到同一个项目上，不新建。

单机阶段，「节点」就是本机。多节点（[MULTI-NODE.md](../MULTI-NODE.md)）之后，节点 id 用节点注册时的 id。

### 4.3 收尾：整理与待办

整理按「每条 issue 是否已处理」来跟踪，而不是只记「整个 run 是否整理过」。每条 issue 的处理结果是以下三种之一：

| 结果 | 含义 |
|---|---|
| `lesson` | 新增了一条 lesson |
| `merged` | 合并进了已有的某条 lesson（补充了一次验证结果，或者改进了处理方法） |
| `kept` | 只留在 run 里，不值得提炼 |

三种都算处理完成，否则没有复用价值的记录会永远挂在待办里。

**整理工具（只开放给负责整理的 agent）：**
- `issue_pending { run }`：列出某个 run 里还没处理的 issue，读取时按 run id 走专门通道，所以已结束的 run 也能读（§2）。
- `lesson_create { title, symptom, keywords, conditions, resolution, outcome, sources: [issue…] }`：新增一条 lesson，并把 `sources` 里的 issue 标成 `lesson`。
- `lesson_merge { lesson, update?, sources }`：合并进已有的 lesson，标成 `merged`。
- `issue_keep { run, issues, why? }`：标成 `kept`。

这几个工具都按 issue 记录处理结果。整理到一半中断后，换一个 agent 重做，看到的只剩没处理完的那些，不会重复生成。

**谁来整理：**
- 正常收尾：主导在 `run_finish` 前整理。`run_finish` 的工具说明写明这一步。还有未处理 issue 时，`run_finish` 的结果里会给出未处理的数量，但不拦截收尾：整理是锦上添花，不能卡住落地。
- 失败、取消、主导中途掉线：issue 留着，run 页和「需要你」条显示「N 条问题记录未整理」。人点一下，交给原主导，或者任意一个同项目的 agent 去整理（发一条带 run id 的消息；hub 只附结构化字段，不写提示文字，见 [H1.md](H1.md) §2）。
- 默认不需要人逐条审批：lessons 不会作为指令注入（§4.5），写错了的代价是「查到一条不适用的参考」，worker 会自己判断。

### 4.4 一条 lesson

| 字段 | 用途 |
|---|---|
| `id`、`project_id` | |
| `title` | 一句话，搜索结果和开场提示里显示的就是它 |
| `symptom`、`keywords` | 问题现象和报错关键词，供搜索使用 |
| `conditions` | 适用条件：系统、工具及版本、节点、相关文件路径 |
| `resolution` | 处理方法 |
| `outcome` | `resolved`（已解决）、`workaround`（临时绕过）、`unresolved`（未解决） |
| `status` | `active`（有效参考）、`review`（待复核）、`superseded`（已被替代，并指向替代它的那条） |
| `evidence` | 验证记录列表，每次一条：哪个 run、哪个 task、哪个节点、什么时间、HEAD commit、工作区有没有未提交的改动（有 diff 或运行快照时关联上），以及这次的结果 |
| `sources` | 由哪些 issue 整理而来（run id 加 note key） |
| `created_at`、`updated_at` | |

- `evidence` 是追加写入的：后来的 worker 再次验证有效，或者发现方法失效，就补一条新的验证记录。失效时把 `status` 改成 `review` 或 `superseded`，不删历史。
- commit 和文件路径只作为复核线索：文件变了不代表经验失效，文件没变也不代表环境没变。hub 不根据它们自动判定失效，最多在搜索结果里标注「相关文件在此 commit 之后有改动」。
- 存储：新表 `lessons`、`lesson_evidence`、`issue_triage`（每条 issue 的处理结果），外加 FTS5 虚表索引 `title`、`symptom`、`keywords`、`resolution`。每次写入都记事件。

### 4.5 查询入口

**只有能力、没有入口，就没人用。** `team` 笔记就是例子。所以要主动给出三个入口，但都不注入正文：

1. **开场一行**：项目有 lessons 时，开场说明里加一行「本项目有 N 条排障记录，遇到报错可以用 `lesson_search` 查」。不放正文，只告诉 agent 有这个东西、怎么查。这一行必须有，因为 worker 自己跑命令时出的报错不经过 hub，只能靠 agent 主动去查。
2. **失败时附上相关记录**：hub 能看到报错的那些失败，结果里多一个结构化字段 `related_lessons`，每项包含 id、title、匹配到的词。这些失败包括：
   - run 检查（`require_run_checks`）失败；
   - `task_fail`；
   - `task_complete` 的验收命令失败。

   规则：
   - 最多 3 条。
   - 只拿报错输出里像关键词的片段去搜：错误码、异常名、`error:` 后面的那一行。BM25 分数低于阈值的丢掉。
   - 没有可靠命中就返回空列表。
   - 字段说明里写清楚：命中只表示「可能相关」，不代表历史解法已确认适用。
3. **主动搜索**：
   - `lesson_search { query, limit? }`：搜本项目的 `active` 和 `review`；带 `include_superseded` 时，也包括已被替代的。
   - `lesson_get { id }`：返回全文，包括 evidence。
   - `lesson_report { id, outcome, note }`：worker 用了某条 lesson 以后回报结果，追加一条 evidence；失效时把状态改成 `review`。

所有查询都限定在调用者所属的项目内，跨项目查不到。

### 4.6 接口与 GUI

- REST：
  - `GET /v1/projects`、`POST /v1/projects/{id}/bind`（绑定 remote 或路径）、`POST /v1/projects/merge`
  - `GET /v1/projects/{id}/lessons?q=&status=`、`GET /v1/lessons/{id}`、`PATCH /v1/lessons/{id}`（人直接改字或改状态）
  - `GET /v1/runs/{key}/issues`（含处理结果）
  - `GET /v1/projects/{id}/lessons/export`（导出 Markdown）
- 事件：`issue.recorded`（由 `NoteWritten` 按前缀派生）、`issue.triaged`、`lesson.created`、`lesson.updated`。
- CLI：`crossagents lessons ls | show | search | export`，`crossagents project ls | bind | merge`。
- GUI：
  - 项目页加一个「排障记录」标签：搜索框、按状态筛选，每条可以展开看 evidence 和来源 run。
  - run 页显示本 run 的 issue 和处理结果，有未整理的就给一个「交给…整理」按钮。
  - 「需要你」条显示未整理数量，以及待绑定的项目。

## 5. 分阶段

第一版就要把项目身份、来源、整理状态这几样定下来，后补会牵扯数据迁移。搜索排序、环境变化判断可以以后慢慢完善。

| 阶段 | 内容 | 验收 |
|---|---|---|
| L1 | `projects` 表及其绑定（remote 规范化、首次接入时匹配、worktree 归属）；`issue/` 前缀的约定；`issue_triage`、`lessons`、`lesson_evidence`、FTS5；整理工具；按 run id 读 issue；`lesson_search` / `lesson_get` / `lesson_report`；开场那一行 | 单测：两种写法的 remote 归到同一个项目，fork 不归入；同一仓库的两个路径共用 lessons，别的项目查不到；整理中断后重做不重复生成；已取消的 run 仍能整理。mock-agent 集成测试走一遍「记录 → 整理 → 新 run 搜到」 |
| L2 | 失败结果附上 `related_lessons`；GUI 的排障记录页、run 页、「需要你」入口；CLI；Markdown 导出 | 单测：检查失败的输出命中关键词时附上记录，无关输出返回空列表、最多 3 条；mock 场景截图 |
| L3 | 「相关文件在此 commit 之后有改动」的复核标注；项目绑定、合并的 GUI；接入 [SELF.md](SELF.md) C1 复盘（复盘结论可以直接走整理工具） | 单测：标注正确；合并项目后 lessons 和 evidence 都并入 |

## 6. 不做

- 不把 lessons 当规则注入，也不让 agent 改 `CROSSAGENTS.md`。某条经验值得成为约定时，由人确认后写进项目规则或检查脚本，通过 git 维护。
- 第一版不自动向仓库提交 lessons。Markdown 导出只用于查看、备份和迁移。
- 不根据 commit 或文件变化自动判定 lesson 失效（§4.4）。
- hub 不在任务失败时发消息催 agent 记录或整理（[H1.md](H1.md) §2）。入口就是工具说明、开场那一行和结构化字段。
- 不做跨项目共享。机器相关的经验（比如 python 占位、代理）现场检测，不靠 lessons 跨项目传播。

## 7. 待定

- 节点断线时的工作方式：在节点本地暂存 issue、缓存本项目的 lessons，恢复连接后补传。等多节点 B 阶段协议定下来再做。
- 「像关键词的片段」怎么抽取：先用正则（错误码、异常类名、`error:` 行），跑一阵再看命中质量。
- 现有 `team` 笔记里那几条「坑」，要不要用整理工具迁移进 lessons。
