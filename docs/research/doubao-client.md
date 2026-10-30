# 豆包桌面客户端接入调研

2026-10-29，本机实测（豆包 2.32.6，Windows，`%LOCALAPPDATA%\Doubao`）。上一份 [doubao.md](doubao.md) 讲的是 API 与 TRAE CLI；这份只讲那个桌面客户端。

## 结论

**能接，但方向是反的。**

客户端不能被 CrossAgents 当成 driver 驱动——它没有 CLI、没有 ACP、没有 stream-json、没有本地 API。但它有一个官方的**用户技能目录**（`workspace/.user_skills`），可以给它写一个 Skill，让它在需要时反过来调 CrossAgents 的 CLI。也就是：

> 人在客户端里说一句话 → 客户端照着 Skill 执行 `crossagents ...` → hub 派活给 Claude Code / Codex / TRAE → 结果回到对话里。

这不算"把豆包接进编排"，算"把编排接到豆包手上"。对"我想用客户端"这个诉求，它恰好是对的那一半。

## 本机实测

它本身就是一个完整的 agent harness，不是一个聊天窗口：

| 证据 | 说明 |
|---|---|
| `Application/app/agent_infra.dll`（10 MB） | agent 运行时 |
| `Application/app/terminal_conpty/`（`conpty.dll`） | 能开终端，跑真实命令 |
| `Application/app/sbox/sbox_sdk.dll` + `User Data/Default/sandbox_envs_dir/` | 有沙箱，按环境隔离 |
| `Application/app/mcp_helper.dll`（15 MB） | 有 MCP 能力，且在原生层 |
| `User Data/Default/.doubao/agent_mode/workspace/.skills/` | **99 个内置技能**：pdf、ppt、sheet、word、browser-use、computer-use-automation、lark-* 全家桶… |
| `…/workspace/.user_skills/` | **空目录，官方留给用户的位置** |
| 内置 `pdf/SKILL.md` 里出现 `create_agent` | 它自己会派子 agent |

技能格式就是 Anthropic 那套：`SKILL.md` + YAML frontmatter（`name`、`description`）+ 正文 + 可选 `scripts/`、`references/`。内置的 `skill-creator-for-work` 把落点写死了：

> every Skill created with this Skill must be created inside an existing directory whose path ends with `workspace/.user_skills`

**没有的东西**（这几条决定了它不能被驱动）：

- 安装目录里没有任何 `doubao-cli` / `dbcli` 之类的可执行入口，`Application/app` 下全是 GUI 进程与 DLL。
- `entry-main/main.js` 里 `acp`、`stream-json`、`conpty` 出现 0 次；`agent_mode` 2 次、`user_skills` 1 次。
- 运行时有 13 个进程、两个 `127.0.0.1` 端口（49853 回 404、52820 不回），都不是文档化接口，也没有 `mcpServers` 一类的本地配置。
- 唯一的外部入口是 `doubao://` 协议：`DoubaoLinkRouter.exe --single-argument %1`。只能打开 app，不能投喂消息。

## 三条接法

| 方式 | 谁调谁 | 可行性 | 用在哪 |
|---|---|---|---|
| **技能注入** | 客户端 → CrossAgents | 高。官方扩展点，零代码 | 人在客户端里当调度台 |
| **MCP** | 客户端 → CrossAgents | 中。`mcp_helper.dll` 说明能力在，但本机没找到配置，入口可能在设置页或云端 | 若设置里有"MCP / 连接器"，挂 `crossagents mcp` 最省事 |
| GUI 自动化 | CrossAgents → 客户端 | 低。UIA / CDP 都脆，且它自己就有 `computer-use-automation`，说明字节没打算让它被别的程序驱动 | 不建议 |

要"无人编排"，走 TRAE CLI（[doubao.md](doubao.md) 的 A 路线）或方舟 API（B 路线）。桌面客户端只适合"人 + 豆包"这一侧。

## 现成的 Skill

放到 `%LOCALAPPDATA%\Doubao\User Data\Default\.doubao\agent_mode\workspace\.user_skills\crossagents\SKILL.md`，重启客户端：

```markdown
---
name: crossagents
description: 把编码任务交给本机的 CrossAgents hub（多 agent 编排），并取回结果。当用户提到 CrossAgents、派活、多 agent、看进度、查用量，或要"让几个 agent 一起做"时使用。
---

# CrossAgents

本机跑着一个 CrossAgents hub，它下面挂着若干编码 agent（Claude Code、Codex、CodeBuddy 等）。
你负责把用户的话翻译成 `crossagents` 命令，再把结果讲回给用户。**不要自己改仓库文件**，改动交给 agent。

## 先确认 hub 在跑

```bash
crossagents agent ls
```

报错说明 hub 没起，让用户执行 `crossagents hub start`（或直接开 GUI），不要自己猜。

## 派活

```bash
# 给已有 agent 发一条消息，等它这一轮结束
crossagents send <agent> "把 README 的第一段改成一句话" --wait

# 派完就走，不阻塞
crossagents send <agent> "跑一遍 cargo test，失败就修"

# 看它在干什么
crossagents events
```

`--wait` 会一直等到这一轮结束，适合短任务；超过一两分钟的任务不要带，改成事后查。

## 开一个新 agent

```bash
crossagents agent add <name> --plugin claude --role worker --cwd <dir>
crossagents agent add <name> --plugin codex  --role planner --cwd <dir>
```

`--role` 必填：`planner` / `worker` / `reviewer`。

## 多 agent 跑一个目标

```bash
crossagents run "把设置页拆成三个组件并补测试" --planner <planner-name>
crossagents run ls                 # 看 run 和状态
crossagents run show <run> -v      # 看计划与每个任务
crossagents run approve <run>      # 计划要人批，批了才开始派发
```

`run` 会先出计划，**必须 approve 才动工**。除非用户明确说"批了"，否则停在计划这一步。

## 汇报规则

- 用中文，先说结论，再说改了哪些文件。
- 不要把命令原始输出整段贴出来；挑用户要的信息。
- 失败就直说失败在哪一步，不要编造成功。
```

## 验证清单

1. 放好目录、重启客户端，问它"用 crossagents 看看现在有哪些 agent"——期望它执行 `crossagents agent ls` 并把输出讲回来。
2. 再问"派一个 worker 把 README 第一段改短"——期望它 `send ... --wait`，然后贴 diff 摘要。
3. 若第 1 步它没找到技能：检查 `.user_skills/crossagents/SKILL.md` 的 frontmatter 是否被完整保留（`name` 与目录名一致）。
4. 顺便在客户端设置里找一遍"MCP / 连接器 / 扩展"——如果有入口，把 `crossagents mcp` 挂上去比 skill 更原生，这条待确认。
