# EXPERTISE 任务书：专长档案

> 依据：2026-11-02 讨论（经验系统记「出过什么错」，专长档案记「怎样算好」；照经验系统的做法：SQLite + FTS5 trigram + agent 工具）· 状态：E0 契约完成（2026-11-02，见 §9），exp-push / exp-tools / exp-review / exp-candidates / exp-maintain / exp-api / exp-gui 未开工 · 2026-11-02

## 1. 目标

run 里落地的代码，好不好靠人看 diff。看得多了会发现，同一个人的偏好是稳定的：错误处理怎么写、什么算过度设计、哪些「优化」不要做、什么情况下应该先问。这些今天只存在于人当时的 review 意见里，下一次派活时 worker 看不见，于是同一个毛病在第七次还是会出现。

要做到：

- **派活时推送**：任务交到 worker 手上时，把它所在项目和这个人的核心判断（Judgment）按 domain 分组、在预算内一起给出去；再按任务标题和目标搜出最相关的例子和边界，附上编号。
- **用得到就搜**：`expertise_search` / `expertise_get`，范围是调用者所在项目的档案 + 这个人的档案。
- **交活时说用了哪条**：`task_complete` 的 `used` 填编号；验收时把这些编号和任务结果对上，统计「一次合并率」。
- **reviewer 有清单**：把相关 Judgment 渲染成逐条 checklist，reviewer 对照 diff 判 pass/fail/na。
- **会自己长**：返工、一次合并、reviewer 判 fail，都生成候选（Candidate）；**候选只有人能接受**，agent 和 lead 只能提议、只能标 suggested。
- **会自己收**：每个 domain 的 Judgment 有约 1500 token 预算，超了提示合并；长期没被用到、或返工率变高的条目标黄；相互冲突的条目不自动覆盖，交人裁决。

和经验系统的关系：两套表、两套工具、互不影响。`lessons` 不动。

## 2. 与经验系统的分工

| | lessons（[LESSONS.md](LESSONS.md)） | expertise（本文） |
|---|---|---|
| 记什么 | 出过什么错：现象、关键词、处理方法、结果 | 怎样算好：判断、范例、边界 |
| 从哪来 | run 里随手记的 issue，收尾整理 | 人写的条目；返工 / 一次合并 / reviewer 判 fail 生成的候选 |
| 归属 | 只有 Project | Person + Project |
| 形态 | 一个 run 内自由写的 Markdown | 短条目，三类，判断有 token 预算 |
| 入口 | 主动搜；失败结果里附相关条目 | 派活时**推送** + 主动搜 + 交活时**回报** |
| 信任 | 参考，worker 自己判断 | 参考，不注入成规则；冲突由人裁决 |

两者都是参考资料，不写进 `CROSSAGENTS.md`，也不作为验收标准。区别只在「记坏」还是「记好」，以及专长档案要能算出「有没有用」。

## 3. 主流程

```
人写条目（或接受候选）
        ↓
worker 接任务：项目档案 + 人档案里 Active 的 Judgment（按 domain、预算内）+ 搜到的 3-5 条 Example/Limit，带编号
        ↓
worker 干活，需要时 expertise_search；交活时 task_complete.used 填编号
        ↓
验收：一次合并 → 记 MergedFirstTry；返工过的 → Reworked；丢弃 → Dropped；同时更新条目 stats
        ↓
返工 / 一次合并 / reviewer 的 fail → 候选（Pending，带 conflicts_with）
        ↓
人审：接受（有冲突先裁决）/ 拒绝；接受后条目进入档案
        ↓
专长卡：一次合并率、与 30 天前的比较、条目数、token 用量、黄/红计数
```

## 4. 设计

### 4.1 归属

一条目属于一个人或一个项目，二者取其一：

| `owner_kind` | `owner_id` | 存在哪 | 谁能查 |
|---|---|---|---|
| `person` | 固定 `"me"`（[`PERSON_OWNER_ID`](../../crates/core/src/expertise.rs)） | hub | 本机所有 agent、所有项目 |
| `project` | `Project.id`（[LESSONS.md](LESSONS.md) §4.2 的项目实体） | hub | 该项目里的 agent |

- 人档案只有一个（单机阶段就是这台机器的主人）。多用户留到以后：`owner_id` 已经是字符串，不改成别的主键。
- 项目档案跟着 `Project.id` 走，不跟路径走：同一个仓库在不同节点上是同一份档案，别的项目查不到。
- 查询永远限定在「调用者所在项目 + 人」这两个 owner 内，跨项目查不到。

### 4.2 一条条目

| 字段 | 用途 |
|---|---|
| `id`、`owner_kind`、`owner_id`、`domain` | `domain` 是 `rust`、`gui`、`windows`、`testing` 这类粗分类，预算按它算 |
| `kind` | `judgment` / `example` / `limit` |
| `title` | 一行：判断本身、案例名，或者边界 |
| `text` | 正文：为什么成立、怎么用 |
| `example` | 仅 `example` 有：`situation`（情况）、`rejected`（被否方案）、`rejected_why`（否掉原因）、`chosen`（最终方案） |
| `applies_when` | 适用条件：仓库区域、工具及版本。空表示哪里都适用 |
| `evidence` | 证据数：条目上次修改后，有多少个 run 确认过它 |
| `last_confirmed_at` | 最近一次被确认的时间 |
| `status` | `active` / `stale` / `review` / `superseded` |
| `superseded_by` | 被哪条替代（仅 `superseded`） |
| `stats` | `used` / `merged_first_try` / `reworked` / `dropped` |
| `sources` | 从哪来：`run` / `task` / `message`（[`SourceRef`](../../crates/core/src/expertise.rs)） |
| `created_at`、`updated_at` | |

三类各自的写法：

- **Judgment**：一句话能背下来的判断，`text` 说为什么。例：标题「先写会失败的测试」，正文「新增行为先补一个会红的用例，再实现；只有改错字或纯重命名可以跳」。
- **Example**：一个真发生过的取舍。`situation` 说当时的情况，`rejected` + `rejected_why` 是被否的方案和原因，`chosen` 是最终方案。推送时这四段一起给，因为它教的是「为什么不那样」。
- **Limit**：判断失效的地方，或者明确不要做的事。例：「Windows 上不要用软链，需要管理员权限」，或「不要为了复用抽第三个调用点的抽象」。

状态：

| 状态 | 含义 | 怎么进来 |
|---|---|---|
| `active` | 可以照着做 | 人写、人接受候选 |
| `stale` | 长期没确认，或结果变差 | `stale_scan` 的建议，人确认后改（见 §4.4） |
| `review` | 出过返工或 reviewer 判 fail，先看清楚再用 | 同上；推送时标黄 |
| `superseded` | 已被替代，留作历史 | 人接受一个替代它的候选时 |

### 4.3 预算：Judgment 约 1500 token/domain

推送的目的不是把所有条目都给出去，是让 worker 开场就知道这个 domain 的底线。所以：

- 每个 `domain` 的 **Judgment** 合计约 1500 token（`estimate_tokens`：中文按字，英文约 4 字符/token；[`crates/core/src/expertise.rs`](../../crates/core/src/expertise.rs)）。
- 超了不是硬截断：`budget_report` 给出这个 domain 的 token 数、超出多少、以及建议合并的条目（同一个 domain、`applies_when` 有交集、标题或正文用词相近的成对条目）。合并由人或 lead 做，结果是候选。
- Example / Limit 不算进 Judgment 预算：它们按任务检索后才推，一次 3–5 条。
- 推送本身还有一层上限：整段不超过约 2000 token，超了先丢最不相关的 Example/Limit。

### 4.4 冲突与标黄

- 候选带 `conflicts_with: Vec<String>`（同一 owner 下相似或可能矛盾的已有条目 id）。**冲突不自动覆盖**：接受候选时，人必须对每个冲突给出裁决——`keep_old`、`replace`、`merge`、`both`；缺了裁决 API 返回 400，候选保持 Pending。
- `stale_scan` 只给建议，不改状态：60 天没被用到、或 `stats.reworked / used` 偏高的条目，建议改成 `stale`；`reworked` 且最近有失败证据的建议 `review`。改状态是人（或 GUI）的动作。
- 条目本身不做版本历史：`superseded` 指向替代者，旧条目留着；不删，避免历史里的 run 引用断掉。

### 4.5 三个入口

只有能力、没有入口就没人用（[LESSONS.md](LESSONS.md) §4.5 的教训）。三个入口：

1. **派活时推送**（`expertise_brief`）：接任务时把项目 + 人档案里 `active` 的 Judgment 按 domain 分组给出，再附上按任务标题/目标检索到的 3–5 条 Example/Limit。每条带编号（`[e1]` 这样的短号即条目 id 前 8 位），末尾一行说明可以用 `expertise_search` 再搜、交活时在 `task_complete.used` 里填编号。没有条目时整段不出现。
2. **主动搜**：`expertise_search { query, domain?, kind?, limit? }` 和 `expertise_get { id }`。搜索范围是「本项目 + 人」，命中的条目只给标题、domain、状态、`applies_when` 和正文第一段（至多 300 字节，截断标 `truncated`）；全文用 `expertise_get`。
3. **交活回报**：`task_complete` 的 `used: [entry_id]`，可以不填。填了就在验收时对上结果。

推送、拉取、`used` 都写一条 `UsageRecord`（`source` 分别是 `pushed` / `pulled` / `declared`），带 run、task、agent 和时间。

### 4.6 结果与统计

验收时把 usage 的 `outcome` 补上，同时更新条目 `stats`：

| 事件 | usage 的 outcome | 条目 stats |
|---|---|---|
| `task_accept`，该任务从没返工 | `merged_first_try` | `used +1`、`merged_first_try +1` |
| `task_accept`，该任务返工过（`report.rework` 非空或 `attempts > 1`） | `reworked` | `used +1`、`reworked +1` |
| `task_drop`（不论是否 requeue） | `dropped` | `used +1`、`dropped +1` |

- 只统计这条任务上**有记录**的条目；同一条目在一项任务上被推了两次只算一次（`usage_set_outcome` 只补 `outcome` 为空的记录，重复调用返回 0）。
- 一次合并 / 返工也让 **证据数 +1**、`last_confirmed_at = 现在`：这条判断在真实任务上又对了一次或错了一次。
- `stats.used` 是「被用到的次数」，不是 `usage` 行数：推送但任务没落地（被取消）不算。

### 4.7 候选

| 来源 `origin` | 什么时候生成 | 提议内容 |
|---|---|---|
| `rework` | `task_rework`，用返工 notes | 一条 Judgment 或 Limit 草稿，标题取 notes 第一行，正文附 notes 全文 |
| `review` | `review_submit` 里 checklist 判 `fail` | 那条 Judgment 的替代稿，附 reviewer 的 note |
| `merged_first_try` | `task_accept` 且从没返工 | 任务摘要 + 目标，作为一条 Example 草稿（情况=目标，chosen=摘要） |
| `manual` | 人直接写 | 人自己填 |

- `status`：`pending` / `accepted` / `rejected`。新建的一律 `pending`。
- 生成时用 `expertise_search` 在同一 owner 下找相似条目，填 `conflicts_with`。
- **只有人能接受**：HTTP `accept` / `reject`。lead 的 `expertise_triage` 只能把候选标成 `suggested_by_lead = true` 并附理由；试图接受返回错误。
- 接受时：proposed 条目的 `id` 由 hub 生成，`sources` 合并候选的来源，冲突按人的裁决处理；拒绝只改状态，条目不动。

### 4.8 指标：一次合并率

专长卡（`expertise_card`）：

- **一次合并率** = `stats.merged_first_try / (merged_first_try + reworked + dropped)`，按 owner 汇总；没有结果的 usage 时给 `null`，不显示 0。
- **与 30 天前比较**：同一算法只看 30 天前就已有的 usage（`at < now - 30d`），两者相减给百分点。样本少于 10 次时照给数字，但卡上标「样本少」。
- 另有：条目数（按 kind）、Judgment token 用量与预算、`stale` / `review` 计数、超预算的 domain 数。

指标只用来发现问题（哪条判断老是被返工），不挂到 agent 的绩效上，也不自动降级任何条目。

### 4.9 存储契约

新表（`crates/store-sqlite/migrations/0012_expertise.sql`）：`expertise_entries`、`expertise_fts`（trigram，索引 `title` / `domain` / `applies_when` / `text`）、`expertise_candidates`、`expertise_usage`。整条记录存 `data` JSON，旁边的列只放过滤用的（owner、domain、kind、status、时间、task）。

[`Store`](../../crates/core/src/store.rs) 上的方法（默认实现返回空或 `Unsupported`，`store-sqlite` 实现）：

```rust
async fn expertise_put(&self, entry: &ExpertiseEntry) -> Result<(), CoreError>;
async fn expertise_get(&self, id: &str) -> Result<ExpertiseEntry, CoreError>;
async fn expertise_list(&self, owner_kind: OwnerKind, owner_id: &str, domain: Option<&str>)
    -> Result<Vec<ExpertiseEntry>, CoreError>;
/// `owners` 里的条目，命中 `terms` 中任意一个，`bm25` 排序；superseded 不参与。
async fn expertise_search(&self, owners: &[(OwnerKind, String)], terms: &[String], limit: usize)
    -> Result<Vec<ExpertiseEntry>, CoreError>;
async fn expertise_delete(&self, id: &str) -> Result<(), CoreError>;

async fn candidate_put(&self, candidate: &Candidate) -> Result<(), CoreError>;
async fn candidate_list(&self, owner: Option<(OwnerKind, String)>, status: Option<CandidateStatus>)
    -> Result<Vec<Candidate>, CoreError>;
async fn candidate_set_status(&self, id: &str, status: CandidateStatus) -> Result<(), CoreError>;

async fn usage_record(&self, usage: &UsageRecord) -> Result<(), CoreError>;
/// 给这条任务上还没有结果的 usage 补 outcome，返回补了几条（重试得 0）。
async fn usage_set_outcome(&self, task: TaskId, outcome: UsageOutcome) -> Result<usize, CoreError>;
async fn usage_list(&self, entry: Option<&str>, task: Option<TaskId>)
    -> Result<Vec<UsageRecord>, CoreError>;
```

搜索沿用 lessons 的做法：三个字以上走 FTS5 trigram，不到三个字的词（如「代理」）用 `LIKE` 兜底；查询里的引号和 FTS 语法一律按文本处理。

## 5. 分阶段

第一版就要把归属、三类条目、usage/结果、候选和冲突裁决定下来：这几样后补都要迁移数据。CLI、导出、统计的细节可以后加。

| 阶段 | 内容 | 验收 |
|---|---|---|
| E0 | 设计；`crates/core/src/expertise.rs` 类型；`estimate_tokens`；`0012` 迁移；`Store` 契约及单测（含中文 trigram）；`expertise/` 模块骨架 | `cargo test -p crossagents-core -p crossagents-store-sqlite`、`cargo build --workspace` |
| E1 | 推送（exp-push）、工具与结果（exp-tools） | 单测；mock-agent 走通「推送 → 搜 → `used` → accept → stats」 |
| E2 | reviewer 清单（exp-review）、候选（exp-candidates） | 单测；run_finish 报候选数 |
| E3 | 维护与指标（exp-maintain）、API/CLI（exp-api）、GUI（exp-gui） | 纯函数单测；hub 路由测试；GUI 明暗两套截图 |

## 6. 各任务分工

`crates/orchestrator/src/expertise/mod.rs` 与四个空子模块已建好（E0），后面各任务只在 mod.rs 里加一行 `pub mod …;`，不再互相改同一个文件。**钩子一律只加几行**：调一个函数、塞一个字段、拼一段文字；要改结构说明契约有问题，先问 lead。

### 6.1 exp-push — 派活时推送

**负责**：`crates/orchestrator/src/expertise/push.rs`（全部逻辑）。

**钩子**（只许加几行）：

| 位置 | 加什么 |
|---|---|
| `crates/orchestrator/src/tasks.rs:472`（`task_assign` 里 `self.relay(worker.id, …, brief::handover(&task, &card), …)`） | 在 `brief::handover` 的结果后面接 `self.expertise_brief(&worker, &task).await`（`Some` 才接） |
| `crates/orchestrator/src/brief.rs:275`（`handover`，及它调用的 `assignment`） | 若选择把编号说明写进 brief，只在末尾追加一段，不动已有字段 |

**签名**：

```rust
/// 这个 worker 接手这项任务时要看到的条目，渲染好的段落；一条都没有时 `None`。
pub(crate) async fn expertise_brief(&self, worker: &Agent, task: &Task) -> Option<String>;

/// 纯函数：把已选好的条目渲染成带编号的段落；空切片返回 `None`。
/// 分组顺序 Judgment → Example → Limit；同组内按 domain。
pub fn render_brief(entries: &[ExpertiseEntry]) -> Option<String>;

/// 预算：每个 domain 的 Judgment 合计上限（`estimate_tokens`）。
pub const JUDGMENT_BUDGET: usize = 1500;
```

- 取数：`store.expertise_list(Project, run 的项目 id, None)` + `store.expertise_list(Person, "me", None)` 过滤 `active`；再用 `search_terms(&format!("{} {}", task.title, task.spec.goal))` 走 `expertise_search` 取 3–5 条 Example/Limit。
- 每条推出去的条目写 `UsageRecord { source: Pushed, entry_id, run_id, task_id: Some(task.id), agent: Some(worker.id), outcome: None }`，`id` 用 uuid v4。
- 单测：无条目 → `None`；超过 `JUDGMENT_BUDGET` 时截断且不切在字中间；编号出现且与 `used` 对得上。

### 6.2 exp-tools — 工具、`used`、accept/drop 记结果

**负责**：`crates/orchestrator/src/expertise/tools.rs`；`crates/mock-agent/tests/expertise.rs`。

**钩子**：

| 位置 | 加什么 |
|---|---|
| `crates/orchestrator/src/expertise/mod.rs` | 一行 `pub mod tools;` |
| `crates/orchestrator/src/tools.rs:318` 附近（`lesson_search` 的 `ToolDef` 之后） | 两个 `ToolDef`：`expertise_search`、`expertise_get` |
| `crates/orchestrator/src/tools.rs:786`（`lesson_search` 的分发行之后） | 两行分发 |
| `crates/orchestrator/src/tools.rs:674`、`:852`（`task_complete` 的 schema 与分发） | schema 加 `used`，handler 里记 declared usage |
| `crates/orchestrator/src/tasks.rs:1198`（`task_accept` 合并成功后、返回前） | `self.settle_usage(&task, UsageOutcome::MergedFirstTry /* 或 Reworked */).await?;` |
| `crates/orchestrator/src/tasks.rs:1439`（`task_drop` 改完状态后） | `self.settle_usage(&task, UsageOutcome::Dropped).await?;` |

**签名**：

```rust
pub(crate) async fn expertise_search(&self, caller: &Agent, input: Value) -> Result<Value, CoreError>;
pub(crate) async fn expertise_get(&self, caller: &Agent, input: Value) -> Result<Value, CoreError>;

/// 这条任务的结果落到它用过的条目上：补 usage 的 outcome，更新 stats 与证据数。
/// 一次合并 → `MergedFirstTry`；返工过（`report.rework` 非空或 `attempts > 1`）→ `Reworked`。
pub(crate) async fn settle_usage(&self, task: &Task, outcome: UsageOutcome) -> Result<(), CoreError>;
```

- `task_complete.used` 里的 id 校验：只接受 `expertise_get` 能取到、且属于「本项目 + 人」的条目；不存在的条目忽略，不报错（worker 可能在别处看到过旧编号）。
- 一条任务重复 accept（`repeated`）不能再记一次结果：`usage_set_outcome` 返回 0 时不动 stats。
- 验收：`cargo test -p crossagents-mock-agent --test expertise -- --test-threads=4`；集成测试走「预置条目 → search → `used` → accept → 断言 usage 与 `stats.merged_first_try`」。

### 6.3 exp-review — reviewer 清单

**负责**：`crates/orchestrator/src/expertise/review.rs`。

**钩子**：

| 位置 | 加什么 |
|---|---|
| `crates/orchestrator/src/tasks.rs:1139`（`task_review` 里给 reviewer 的 `relay` 文本，`brief::task(&task)`） | 在 `brief::task` 后接 `render_checklist(...)` 的段落 |
| `crates/orchestrator/src/tools.rs:696`（`review_submit` 的 schema） | 加可选 `checklist` |
| `crates/orchestrator/src/tools.rs:885`（`review_submit` 的分支） | 解析 `checklist` 并调用 `review_candidates` |
| `crates/orchestrator/src/review.rs:84`（`review_task`，人给的 remarks） | 只在字面上说明 checklist 也可以从这里来，不改结构 |

**签名**：

```rust
/// 纯函数：相关 Judgment 渲染成逐条编号的 checklist；没有条目时 `None`（整段不出现）。
pub fn render_checklist(entries: &[ExpertiseEntry]) -> Option<String>;

/// reviewer 的每条裁决：编号、pass/fail/na、说明。
#[derive(Deserialize)]
pub struct ChecklistItem {
    pub entry_id: String,
    pub verdict: Verdict, // Pass | Fail | Na
    #[serde(default)]
    pub note: String,
}

/// 判 fail 的每条生成一个 Candidate(origin = Review, status = Pending)，引用该任务。
pub(crate) async fn review_candidates(
    &self,
    reviewer: &Agent,
    task: &Task,
    checklist: &[ChecklistItem],
) -> Result<Vec<Candidate>, CoreError>;
```

- 清单只取 `active` 的 Judgment，按 `domain` 分组，`applies_when` 与任务涉及的文件有交集（例如 glob 前缀）的排前面；总预算同样 1500 token。
- `entry_id` 不在本项目/人档案里的裁决忽略，不报错。

### 6.4 exp-candidates — 候选与 lead 工具

**负责**：`crates/orchestrator/src/expertise/candidates.rs`；`crates/mock-agent/tests/expertise_candidates.rs`。

**钩子**：

| 位置 | 加什么 |
|---|---|
| `crates/orchestrator/src/tasks.rs:1378`（`task_rework` 写完 notes 后） | `self.candidate_from_rework(&task, &notes).await?;` |
| `crates/orchestrator/src/tasks.rs:1198`（`task_accept` 成功后） | 从没返工 → `self.candidate_from_first_try(&task).await?;` |
| `crates/orchestrator/src/tools.rs`（`TOOLS` 数组与分发） | 两个 lead 工具：`expertise_pending`、`expertise_triage` |
| `crates/orchestrator/src/landing.rs:599`（`run_finish_tool` 的返回 json，照 `unsorted_issues`） | 加字段 `pending_expertise: self.pending_candidates(&project_id).await` |

**签名**：

```rust
pub(crate) async fn candidate_from_rework(&self, task: &Task, notes: &str) -> Result<Candidate, CoreError>;
pub(crate) async fn candidate_from_first_try(&self, task: &Task) -> Result<Candidate, CoreError>;

/// 用同一 owner 下的相似条目填 `conflicts_with`（不改别的字段）。
pub(crate) async fn fill_conflicts(&self, candidate: &mut Candidate) -> Result<(), CoreError>;

pub(crate) async fn expertise_pending(&self, caller: &Agent) -> Result<Value, CoreError>;
/// 只能 `suggested_by_lead = true` + 理由；请求里出现 accept / reject → `CoreError::Conflict`。
pub(crate) async fn expertise_triage(&self, caller: &Agent, input: Value) -> Result<Value, CoreError>;

/// `run_finish` 报数用：本项目 + 人里 Pending 的候选数。
pub(crate) async fn pending_candidates(&self, project_id: &str) -> Result<usize, CoreError>;
```

- 生成的候选 `owner_kind`/`owner_id` 跟任务所属项目一致（Example 优先挂项目；判断人偏好的 Limit 也可以挂人——第一版一律挂项目，减少判断）。
- `task_accept` 里 exp-tools 和本任务各加一行（先记结果、再生成候选）。两条都放在合并成功后的返回前；合并顺序 exp-tools 在先，若起冲突，保留两者、只调顺序，不要删任何一行的调用。
- `run_finish` 只报数，不拦截收尾（与 `unsorted_issues` 一致）。

### 6.5 exp-maintain — 纯函数

**负责**：`crates/orchestrator/src/expertise/maintain.rs`（不碰 store）。

**签名**：

```rust
/// 每个 domain 的 Judgment token 用量、预算、是否超出、建议合并的条目 id 对。
pub fn budget_report(entries: &[ExpertiseEntry]) -> Vec<DomainBudget>;

/// 长期（60 天）没用到、或返工偏高的条目：建议的 status 与原因。不改任何东西。
pub fn stale_scan(
    entries: &[ExpertiseEntry],
    usages: &[UsageRecord],
    now: DateTime<Utc>,
) -> Vec<StaleHint>;

/// 带 conflicts_with 的待裁决候选。
pub fn conflict_list(candidates: &[Candidate]) -> Vec<ConflictPair>;

/// 专长卡：一次合并率、30 天前的同口径值、条目数、token 用量、黄/红计数。
pub fn expertise_card(
    entries: &[ExpertiseEntry],
    usages: &[UsageRecord],
    now: DateTime<Utc>,
) -> ExpertiseCard;

/// 按 domain、三类分节的 Markdown。
pub fn export_markdown(entries: &[ExpertiseEntry]) -> String;
```

结果类型（`DomainBudget`、`StaleHint`、`ConflictPair`、`ExpertiseCard`）都 `derive(Serialize)`，字段见 §4.3 / §4.4 / §4.8；`expertise_card.first_try_rate: Option<f64>`（0–1），`first_try_rate_30d_ago: Option<f64>`，`delta_points: Option<f64>`。

### 6.6 exp-api — hub 路由、CLI、ts 绑定

**负责**：`crates/hub/src/api_expertise.rs`（新建）、`crates/api-types/**`、`bindings/**`、`crates/cli/src/main.rs`。

**钩子**：

| 位置 | 加什么 |
|---|---|
| `crates/hub/src/api.rs:99`（`let authed = Router::new()` 那串末尾） | `.merge(crate::api_expertise::router())` 一行 |
| `crates/hub/src/lib.rs` | 一行 `mod api_expertise;` |
| `crates/api-types/src/lib.rs:1575`（`export_core_bindings`） | `crossagents_core::ExpertiseEntry::export_all(&cfg)` 与 `Candidate` 两行 |
| `crates/cli/src/main.rs:30`（`Command`） | 一个 `Expertise` 子命令 |

**路由**（都要 `AppState` 的 store，鉴权同现有 `authed`）：

```
GET    /v1/expertise?owner_kind=&owner_id=&domain=
POST   /v1/expertise                     创建/替换条目
GET    /v1/expertise/{id}
DELETE /v1/expertise/{id}
GET    /v1/expertise/search?q=&owner_kind=&owner_id=&limit=
GET    /v1/expertise/candidates?status=
POST   /v1/expertise/candidates/{id}/accept   body: { decisions: [{ entry_id, verdict }] }
POST   /v1/expertise/candidates/{id}/reject
GET    /v1/expertise/card
GET    /v1/expertise/maintenance         预算 + 标黄建议 + 冲突
GET    /v1/expertise/export              文本 Markdown（Content-Type: text/markdown）
GET    /v1/projects                      项目 id 与它的 checkout 路径：客户端从工作目录找到项目档案
```

- 项目档案的 `owner_id` 是 `Project.id`，不是目录路径（§4.1）。客户端在一个目录里工作，所以先查 `/v1/projects` 的 `paths` 找到项目，再用它的 `id` 查专长档案；页面在 `Project` 还不存在时退回目录路径（此时档案本来就是空的）。

- `accept` 且有 `conflicts_with` 时，`decisions` 必须覆盖每一个冲突 id，`verdict` ∈ `keep_old` / `replace` / `merge` / `both`；缺一个返回 400（`CoreError` → 现有错误形状），候选不变。
- 请求/响应类型放 `crates/api-types`；`cargo test -p crossagents-api-types` 重新生成 `bindings/`。
- `maintain` 的三个视图调 exp-maintain 的纯函数；若它还没合并，在本文件内放同签名的临时实现并注明，合并后删。

### 6.7 exp-gui — 「专长」页

**负责**：`apps/gui/**`（`src/lib/expertise.ts`、`src/lib/expertise.test.ts`、`src/views/Expertise.tsx`、`src/views/Expertise.mock.ts`、`src/dev/scenarios.ts` 的一个场景、导航入口）。

- 类型先手写在 `src/lib/expertise.ts`，字段对齐 `crates/core/src/expertise.rs`（§4.2、§4.7）；exp-api 生成 bindings 后切换。
- 页面：
  - 档案列表：Person / Project 切换，按 domain 分组，三类分节；每个 domain 显示 `token 用量 / 1500`，超了标红；`stale` / `review` 标黄；返工率高的条目旁写原因。
  - 候选：多选接受/拒绝；有冲突时新旧条目并排，每个冲突必须选裁决才能提交；显示 lead 的 suggested 标记和理由。
  - 专长卡：一次合并率及与 30 天前的变化、条目数、`used` / 一次合并 / 返工 / 丢弃、黄红计数。
  - 导出 Markdown（预览 + 复制）。
- mock 场景：派活推送 → 交活填 `used` → 返工生成候选 → 人接受 → 专长卡统计变化。
- 验收：`npm.cmd --prefix apps/gui run typecheck`、`npm.cmd --prefix apps/gui test`，明暗两套主题各截图一张自查。

## 7. 不做

- **不做 fork 聚合**：`fork` 的项目就是另一个项目（[LESSONS.md](LESSONS.md) §4.2），专长档案不跨 fork 合并，也不因为提交历史相同就共享。
- 不做跨项目共享 Person 以外的档案；项目档案只给本项目。
- 不把条目注入成规则，也不让 agent 改 `CROSSAGENTS.md`；要变成约定，由人写进项目规则或检查脚本。
- 不自动接受、自动拒绝、自动改状态：候选和标黄都只到「建议」为止。
- 不按 commit 或文件改动自动判定条目失效。
- 不做条目的版本历史与回滚；`superseded` 已经是够用的「旧版还在」。
- 不统计到 agent 个人（不给 agent 排名）：一次合并率只到 owner 级。

## 8. 待定

- 推送进开场说明的整体顺序与长度（现在排在 task 正文之后、checks 之前），等 E1 跑几个 run 看 worker 会不会读。
- 「相似」的判定：先用 FTS + domain 相同 + 标题词重合，是否够用要等候选攒起来再看。
- 30 天对比的窗口是否要可配。
- 人档案在多人协作下怎么分（现在固定 `me`）。

## 9. E0 实现记录（2026-11-02）

| 部分 | 位置 |
|---|---|
| 类型：`OwnerKind`、`EntryKind`、`EntryStatus`、`EntryStats`、`Example`、`ExpertiseEntry`、`SourceRef`、`Candidate`、`CandidateOrigin`、`CandidateStatus`、`UsageRecord`、`UsageSource`、`UsageOutcome`、`estimate_tokens` | `crates/core/src/expertise.rs`（`lib.rs` 导出） |
| 表与 FTS5（trigram） | `crates/store-sqlite/migrations/0012_expertise.sql` |
| `Store` 方法：`expertise_*`、`candidate_*`、`usage_*` | `crates/core/src/store.rs`（契约，带默认实现）、`crates/store-sqlite/src/lib.rs`（实现 + 单测） |
| 模块骨架 | `crates/orchestrator/src/expertise/mod.rs` 及 `push.rs`、`review.rs`、`candidates.rs`、`maintain.rs`（只有 doc 注释） |

实现时的几个定论：

- `UsageRecord.id` 是 uuid，`usage_set_outcome` 靠它逐条更新 `data` 与 `outcome` 列；`task_id` 为空时存空串，便于按任务查。
- `usage_set_outcome` 返回补了几条：重复调用（同一条任务再 accept 一次）返回 0，调用方据此不重复加 stats。
- `expertise_search` 不返回 `superseded`（历史用 `expertise_list` 看），其余状态都返回，由调用方按 `active` 过滤。
- 中文短词（不到 3 字）走 `LIKE` 兜底，与 lessons 一致；FTS 语法按文本处理，已有单测。
- `estimate_tokens` 把中文标点和全角字符也算一个字，英文按其字符数除以 4 向上取整，空串为 0。
