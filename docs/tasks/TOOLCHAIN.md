# TOOLCHAIN 任务书：hub 核心不绑语言

> 依据：2026-10-28 讨论（hub 是通用的，不该只认 cargo）· 状态：T1 完成（2026-10-28，出入见 §10）· 2026-10-28

## 1. 目标与边界

CrossAgents 要能管任何语言的项目。边界分三层，从下往上覆盖：

| 层 | 内容 | 懂不懂语言 |
|---|---|---|
| hub 核心 | worktree、按 worktree 分的构建目录、占位符替换、检查命令的执行与结果、正在运行的命令、操作系统层面的“文件被占用” | 不懂 |
| 预设层（`orchestrator/src/presets.rs`，一张数据表） | 按根目录的标志文件命中，给出环境变量和诊断提示 | 懂少量工具链知识，只在这一处 |
| 项目配置（`crossagents.toml`） | 检查命令、环境变量、启用哪些预设 | 项目自己说了算，优先级最高 |

所以准确的说法是：**hub 核心不懂语言**，工具链知识集中在预设层，项目配置可以覆盖或关掉预设。不说“hub 完全不内置语言知识”。

## 2. 现状（已核对代码）

| 位置 | 绑了什么 | 处理 |
|---|---|---|
| `workspace.rs::worktree_env` | 给 run worktree 里的每个 agent（`hub/src/collab.rs::agent_env`）和 hub 的检查（`verify.rs`）都只注入 `CARGO_TARGET_DIR=<root>/<run>/targets/<tree>` | 改成按项目配置和预设算出一组变量（§3、§4） |
| `running.rs::in_use_failure` | `LNK1104`（MSVC 链接器）、`Blocking waiting for file lock`（cargo）和操作系统信号混在一起，命中任意一条就把失败说成“不是你代码的问题” | 分成确定占用和可能相关两级（§6） |
| `landing.rs::require_run_checks` | 没有 `crossagents.toml`、或者变更没碰到任何检查的 `paths` 时，直接放行，什么都不说；kickoff 也不提 | 明确报告“未配置检查 / 本次变更不涉及任何检查，未执行验证”（§5） |
| `landing.rs` 第 101–106 行和第 145 行的报错文案 | 字符串续行丢了 `\`，文案里夹着一长串空格和一个换行（b27499f 引入） | T1 顺手修 |
| `cli/src/cmd/hub.rs`（dev hub 的家目录默认放在 `$CARGO_TARGET_DIR/dev-hub`） | 这是 CrossAgents 自己的开发工具，不属于 hub 运行时的行为 | 不动 |

**Kai 那次 `tsc` 找不到，原因已确认：依赖还没装，不是没共享。** run checkout `27bd5f0e/integration/apps/gui/node_modules` 的时间戳是 22:34:34，是 run 进行中 agent 自己 `npm install` 装上的，之后 `tsc` 就能用了。新建的 worktree 本来就没有依赖。缺的是“新 worktree 怎么准备依赖”，这件事单独设计（§7），不靠共享目录解决。

## 3. 构建目录与环境变量

### 3.1 占位符

`crossagents.toml` 和预设里的环境变量值可以带下面的占位符。T1 只做第一个：

| 占位符 | 指向 | 生命周期 | 回收 | T1 |
|---|---|---|---|---|
| `{build}` | `<root>/<run>/targets/<tree>`（`<tree>` 是 `integration` 或 task key），同一 worktree 里 agent 和 hub 的检查共用 | 跟 worktree 走：worktree 删了它就删，run 删了整个 `targets` 都删。这套删除逻辑已有（`prune_run_targets` / `targets_in_use`） | 已有逻辑，跳过仍有 worktree 在用的目录 | ✓ |
| `{checkout}` | worktree 本身的绝对路径 | — | — | ✓ |
| `{cache}` | 跨 run 复用的目录，按 project_id 分 | 跨 run | 要另外定义：按大小回收时必须跳过正在使用的目录（以该项目有没有活跃 run 判断），还要考虑多个 run 并发写 | ✗ 暂不做 |

先不做 `{cache}` 的原因：常见包管理器的**下载缓存**默认就放在用户目录、跨 worktree 共用（`~/.npm`、pip cache、`~/.cargo/registry`、`~/.gradle/caches`），而且它们本身支持并发。hub 再造一层跨 run 缓存，只会多出一个需要回收的目录。真有项目需要的时候再加，到时把作用域和回收规则写进本节。

构建产物（`{build}`）和缓存（`{cache}`）不能混用：构建产物只在一个 worktree 内有意义，跟着 worktree 删；缓存跨 run，按大小和是否在用回收。

### 3.2 项目配置

```toml
[run]
# 不写：按标志文件自动命中预设；写了就只用列出的；[] 关掉全部预设
presets = ["rust"]

[run.env]
# 值里可以用 {build}、{checkout}。项目里写的变量覆盖预设里的同名变量
CARGO_TARGET_DIR = "{build}/cargo"
PYTHONDONTWRITEBYTECODE = "1"
# false：删掉某个预设给的变量（不传，不是传空串）
SOME_PRESET_VAR = false
```

规则：
- 只在 run 的 worktree 里生效（integration 和 task 的 checkout）。和今天的 `worktree_env` 范围一样：in-place run、在用户自己仓库里开的聊天都不注入。
- **agent 和 hub 的检查必须拿到同一组变量**，否则会一边编译进 A 目录、一边编译进 B 目录。两边都调用同一个函数（§8），从该 worktree 里的 `crossagents.toml` 读配置。
- 什么时候生效：agent 的变量在会话启动时算，检查的变量在每次跑检查前算。run 进行中改了 `[run.env]`，之后新开的会话才会拿到新值。kickoff 会把最终生效的变量列出来（§5），可以对照。
- 账号环境（代理、API key，见 `accounts.agent_env`）优先：项目配置和预设不能覆盖账号已经设置的变量。冲突时以账号为准，并记一条 warn。
- `crossagents.toml` 会提交进仓库，不能放密钥。文档里写清楚，代码不做检查。
- 值必须是字符串或 `false`，其他类型（数字、表）按配置错误处理。这和今天的规则一致：`crossagents.toml` 写错了，收尾就会被拒，不会悄悄失效。
- hub 只负责创建 `{build}` 目录本身，其中的子目录由工具链自己创建。

## 4. 预设

### 4.1 表的形状

```rust
pub struct Preset {
    pub name: &'static str,
    /// 任意一个文件出现在 checkout 根目录就命中
    pub markers: &'static [&'static str],
    pub env: &'static [(&'static str, &'static str)],
    /// 失败输出里出现时，标注“可能相关”（§6），不改变结论
    pub hints: &'static [&'static str],
}
```

T1 只有一条：

| name | markers | env | hints |
|---|---|---|---|
| `rust` | `Cargo.toml` | `CARGO_TARGET_DIR = "{build}/cargo"` | `LNK1104`、`Blocking waiting for file lock` |

Node 和 Python 在 T1 里不加预设：它们按 worktree 各装各的依赖、共用全局下载缓存，已经是对的默认行为，没有需要注入的变量。一条空预设只会让人误以为 hub 做了什么。等 §7 依赖准备的设计定下来，再决定它们要不要预设。

`rust` 的构建目录从 `{build}` 本身改成 `{build}/cargo`。这样以后别的预设各用自己的子目录，不会嵌套进 cargo 的 target 目录。代价是升级后正在跑的 run 要重新编译一次，可以接受。

### 4.2 组合、覆盖与冲突

- **组合**：`presets` 不写时，所有命中的预设都启用。仓库根目录同时有 `Cargo.toml` 和 `package.json`，就两个都启用。只看根目录：子目录里的标志文件（比如 `apps/gui/package.json`）不算，让结果可以预测。项目要启用子目录里的工具链，就在 `[run.env]` 里写或者用 `presets` 列出来。
- **覆盖**：优先级从低到高是 预设 < `[run.env]` < 账号环境。`[run.env]` 里写 `KEY = false` 可以删掉预设给的变量。
- **冲突**：两个启用的预设给同一个变量设了不同的值时，这个变量**两边都不用**，记 warn，并在 kickoff 的环境行里写明冲突。如果项目在 `[run.env]` 里给了这个变量，就以项目为准，冲突消失。不按名字顺序挑一个：挑错了比不设更难排查。
- **关掉**：`presets = []` 关掉全部预设；`presets = ["rust"]` 只用列出的，列出的名字在表里不存在时按配置错误处理。
- 预设表在编译时就定了，T1 不支持用户自定义预设。项目有特殊需要就写 `[run.env]`。

## 5. 没有检查时明确说出来

不猜命令，也不把“没检查”显示成“检查通过”。分四种状态：

| 状态 | 条件 | kickoff | `run_finish` / `run_land` 返回 & run 页面 |
|---|---|---|---|
| 未配置 | 没有 `crossagents.toml` 或 `checks` 为空 | `checks: none declared in crossagents.toml; the hub will not verify this run` | `checks: not configured — nothing was verified` |
| 不涉及 | 有检查，但这次的变更没碰到任何检查的 `paths` | 照常列出检查 | `checks: none apply to this change (<n> declared) — nothing was verified` |
| 通过 / 失败 | 跑了 | 照常 | 照常（`2/2 passed`，或者第一个失败） |

具体做法：
- 在 run 上存一个状态 `checks_state: NotConfigured | NotApplicable | Ran`，和 `checks` 结果放在一起。“没有结果”不能等于“通过”。
- GUI 的 run 页面在“未配置”和“不涉及”时显示灰色的「未验证」，不显示绿色。
- kickoff 加一行生效的环境（来自哪个预设或项目配置），例如：`build env: CARGO_TARGET_DIR=…\targets\integration\cargo (preset rust)`。

这些都是 hub 知道的事实，加成结构化的行，不替 agent 写怎么做，符合 H1。

## 6. 占用识别分两级

| 级别 | 信号 | 效果 |
|---|---|---|
| 确定占用（操作系统层面，不分语言，写死在核心里） | `os error 32`（Windows `ERROR_SHARING_VIOLATION`）、`os error 33`（`ERROR_LOCK_VIOLATION`）、`being used by another process`、`另一个程序正在使用此文件`、`Text file busy`（ETXTBSY）、`Device or resource busy` / `EBUSY` | 报错说成环境冲突：“有文件被另一个进程占用，不像是你代码的问题，确认没有别的构建或测试在跑，再调用一次” |
| 可能相关（来自启用的预设的 `hints`） | `LNK1104`（只说明打不开文件，原因不一定是占用）、`Blocking waiting for file lock`（只说明在等锁，等完可能照样成功） | 结论仍是“检查失败”，后面附一行 `possibly related: <hint>, which can come from another build running at the same time` |

两级都**不会**触发自动重试或终止，只影响报错的措辞。真正防撞的机制是 b27499f 的“检查前看 checkout 里有没有命令在跑”（`refuse_while_running`），这部分已经不分语言。

只检查失败的那条命令的输出：通过的检查里出现 `Blocking waiting for file lock` 是正常的，不做任何处理。

## 7. 依赖目录（单独设计，不在 T1）

`node_modules`、`.venv` 这类目录**不默认共享**。它们不是普通缓存：内容取决于锁文件、平台和 Python/Node 版本，里面还有绝对路径（`.venv` 的 `pyvenv.cfg`、各种 shim）。几个 worktree 共用一份时，一个 worktree 里 `npm install` 会改掉另一个正在用的依赖。

先记下方向，到时再写成正式设计：
1. 默认：每个 worktree 各装各的依赖，靠包管理器的全局下载缓存省时间（`npm ci` 有缓存时很快）。
2. 由项目声明 worktree 的准备命令，例如 `[run] setup = [{ command = "npm ci", dir = "apps/gui", paths = [...] }]`。hub 在建 worktree 之后、交给 agent 之前跑一次，失败了在 kickoff 里如实写出来。这比让 agent 自己摸索（Kai 那次就是这样）更快，也更稳定。
3. 显式共享（`shared = ["apps/gui/node_modules"]`）可以以后支持，但只在项目自己写了的时候生效，任何预设都不默认开。

SELF A1 里“照 `CARGO_TARGET_DIR` 共享一份 `node_modules`”的写法要按这里改。

## 8. T1 实现

| 文件 | 改什么 |
|---|---|
| `orchestrator/src/presets.rs`（新） | `Preset` 表，以及 `detect(dir)` |
| `orchestrator/src/project.rs` | 解析 `[run] presets` 和 `[run.env]`（字符串或 `false`），保持 `deny_unknown_fields`；新增 `RunEnv { vars, sources, conflicts }` 和 `run_env(dir) -> Result<RunEnv, String>` |
| `orchestrator/src/workspace.rs` | `worktree_env(root, cwd)` 改成返回 `Vec<(String, String)>`：先用原来的路径判断找到 run/tree，再读 `run_env(cwd 所在的 checkout)` 替换占位符；不在 run worktree 里就返回空。函数里不再出现 `CARGO_TARGET_DIR` |
| `hub/src/collab.rs` | `agent_env` 按账号优先合并 |
| `orchestrator/src/verify.rs` | 检查用同一个 `worktree_env` |
| `orchestrator/src/running.rs` | `in_use_failure` 只留操作系统信号；新增 `hints(output, presets)` |
| `orchestrator/src/landing.rs` | 存三种检查状态；按两级识别生成报错；修第 101–106、145 行的文案 |
| `orchestrator/src/brief.rs` | kickoff 加“未配置检查”行和生效环境行 |
| core `Run` + migration + GUI run 页面 | `checks_state` 字段与「未验证」显示 |
| `crossagents.toml`（本仓库） | 不用改：根目录有 `Cargo.toml`，自动命中 `rust` |

## 9. 验收

1. 根目录有 `Cargo.toml`、没写 `[run.env]`：agent（`[env CARGO_TARGET_DIR]`）和 hub 的检查（`echo`）拿到同一个 `…/targets/<tree>/cargo`。
2. 只有 `package.json` 的项目：agent 环境里没有 `CARGO_TARGET_DIR`。
3. `Cargo.toml` 和 `package.json` 都有：命中 `rust`。`[run.env]` 覆盖生效；`KEY = false` 删掉变量；`presets = []` 全部关掉；`presets = ["nope"]` 报配置错误。
4. 两个合成预设设了同一个变量：两边都不用，kickoff 写明冲突；项目在 `[run.env]` 里写了它之后，以项目为准。
5. 账号环境里的变量不会被项目配置覆盖。
6. 没有 `crossagents.toml`：kickoff 写“未配置”，`run_finish` 的返回和存下的 run 都是“未验证”，不是“通过”；变更没碰到任何检查的 `paths` 时同理，显示“不涉及”。
7. 检查失败输出含 `os error 32`：报成环境冲突。只含 `LNK1104`：报成检查失败，并附“可能相关”一行。都不自动重试。
8. 现有的测试（`checks_in_a_run_build_in_its_own_worktrees_target`、`the_checks_wait_for_the_leads_own_build`、collab 的 env 测试）改成断言 `…/cargo` 之后全部通过。

## 10. T1 实现记录

- 和设计的出入：
  - GUI 的 run 页面今天不展示 run 级检查结果，只有结束时给人的通知里有「验收」一行。所以「未验证」写在通知里（“没有声明检查，未执行验证” / “这次的改动不涉及……未执行验证”），没有另做灰色标记。run 上的 `checks_state` 已经进了 API（`bindings/ChecksState.ts`），以后页面要展示时可以直接用。
  - `{build}/`、`{checkout}/` 后面紧跟的 `/` 会换成平台的分隔符，所以 Windows 上得到的是 `…\targets\integration\cargo`；更深层的 `/` 原样保留。
  - `crossagents.toml` 写错时，agent 和检查的环境退回只用预设（记 warn），错误本身仍由收尾时的检查报出来。
- 代码：`presets.rs`（表）、`project.rs::run_env`（解析、组合、冲突、占位符校验、`fill`）、`workspace.rs::worktree_env` / `worktree_run_env` / `worktree_dirs`、`collab.rs::with_project_env`（账号优先）、`running.rs::in_use_failure` / `hints`、`landing.rs::record_checks`、`brief.rs::verification`、core `ChecksState`。
- 测试：`project` 两条（预设与覆盖、冲突）、`running` 一条（两级）、`verify` 与 `collab` 的环境测试（含非 Rust 项目没有 `CARGO_TARGET_DIR`、账号优先）、`lead.rs` 两条端到端（没配检查报未验证；Rust 项目 kickoff 带环境行，检查跑过记为 `ran`）。
