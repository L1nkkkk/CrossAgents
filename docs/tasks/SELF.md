# SELF 任务书：交给 CrossAgents 自己改自己的任务

> 来源：2026-10-26。两次自举实测（[DOGFOOD.md](DOGFOOD.md) §9、§10）之后，从实测遗留问题和 [TODO.md](../TODO.md)
> 未完成项里挑出适合用 Work 交给 agent 做的任务。目标是从「能改自己」走到「能自己找该改什么」。

## 选任务的标准

- 改动集中在一两个 crate 或 `apps/gui` 的一块，范围说得清。
- hub 能自动验收（`crossagents.toml` 的 checks、单测、`gui_screenshot`），不靠人复核。
- 改坏了不会弄挂正在跑的 hub：先在 `hub sandbox` 上跑，验证过再落地。

顺序：A → B → C。A 不做完不要放开 B、C。现在 `crossagents.toml` 只验 `cargo test --workspace`，
GUI 改动的对错靠 agent 自述，在这种状态下加速自改等于在没测试的代码上加速。

## A. 补验收（先做）

| # | 任务 | 改哪里 | 验收 |
|---|---|---|---|
| A1 | （待按 [TOOLCHAIN.md](TOOLCHAIN.md) §7 改：依赖目录不默认共享，改为 worktree 准备命令）同一 run 共用一份 `node_modules`（照 `CARGO_TARGET_DIR` 按 run 共享），把 GUI 的 `tsc --noEmit` 和 `node --test` 加进 `crossagents.toml` checks（DOGFOOD §10 第 4 条） | `crates/hub` 拼 worker 环境与建 / 删 worktree 处；`crossagents.toml` | sandbox 里只改 GUI 的 run 收尾显示「验收 3/3 通过」；故意写类型错误时 `run_finish` 被拒 |
| A2 | mock 预览里无对话的 agent 也按插件给出能力（§10 第 5 条） | `apps/gui` mock 数据 | `gui_screenshot` 截出 codex agent 的能力提示 |
| A3 | worker 克隆在任务合并 / 取消后归档而不删，run 追溯里留下它最后几轮（§10 第 3 条） | `crates/hub` run / task 收尾、`trace.rs` | 单测：任务合并后 `/v1/runs/{key}/trace` 仍能查到该 worker 最后一轮和出错 |

A3 是 C1 复盘的数据来源。

## B. 修自举时踩过的坑

小而独立，适合强模型当主导、DeepSeek 当 worker；顺带验证修好权限后 CodeBuddy worker 能完整走「改、测、上报」（§10 未覆盖项）。

| # | 任务 | 来源 | 验收 |
|---|---|---|---|
| B1 | 压缩合并用主导 `run_finish` 给的 `message`，不再写死 `crossagents: <标题>` | §9 第 6 条 | 单测检查落地提交消息 |
| B2 | 落地提交加 `Co-authored-by` 或可选人的 git 身份 | §9 第 5 条 | 单测检查提交 trailer |
| B3 | run 页只显示本 run 的「告诉你」通知 | §9 第 4 条 | mock 场景加跨 run 通知，截图确认不出现 |
| B4 | 开场说明带本机可用工具链（python 是否 Store 占位、node / cargo 版本） | §9 第 3 条 | 单测注入假 `Machine` 检查开场说明 |
| B5 | hooks 默认关闭，需显式打开 | TODO §四 | 单测检查默认配置；mock 设置页截图 |
| B6 | 附件路径不能读到工作目录之外 | TODO §七 | 单测覆盖 `../`、符号链接、绝对路径三种越界 |

B6 碰安全边界，计划由人亲自批，不托管。

## C. 走向自我进化

| # | 任务 | 做法 |
|---|---|---|
| C1 | 复盘 run | 新增协作工具或 CLI 子命令：读已结束 run 的 trace，产出 DOGFOOD §9 式「现象 / 影响 / 方向」表，写进 `docs/tasks/RETRO-<run>.md`；只写文档不改代码 |
| C2 | 从复盘里挑一条修 | 主导读 C1 产出，挑「影响大、改动小」的一条开新 run；计划仍要人批 |
| C3 | 改 hub 后在 sandbox 自验 | 改到 hub 的任务，验收加一步：`hub sandbox start` 起刚编的 hub，跑脚本化冒烟（建 agent、发消息、收事件），再 `sandbox stop` |

C1 + C2 是最小进化循环：跑任务 → 复盘 → 挑一条修 → 再跑。复盘结论和挑选都给人看，
连续几次挑得对再考虑把 C2 的计划改成自动批。

## 不交给 agent 的

- 守卫审批的危险动作判定（DOGFOOD §7 / P1-7）：不让它放宽自己的约束。
- 完工监视器与本地部署（`~/.crossagents/finish-watcher`）：那是它被部署的通道，改坏了回不来，且不在仓库里、验收管不到。
- 多节点 B 阶段（节点协议、自升级）：设计未定，交出去等于让它替人做架构决定。

## A1 的请求原文

在 sandbox hub 上跑，托管选「我审批」：

```text
目标：让 GUI 改动也能被 hub 自动验收（docs/tasks/DOGFOOD.md §10 第 4 条）。

1. 同一个 run 的 worktree 共用一份 node_modules，做法参照 CARGO_TARGET_DIR 按 run 共享（DOGFOOD §1）：
   放在 <worktrees>/<run>/ 下，不进任何 checkout、不会被提交；run 的集成目录删除时一起删。
   第一次需要时在集成目录里跑 npm ci，之后的任务复用。
2. crossagents.toml 的 [run] checks 增加 apps/gui 的 tsc --noEmit
   和 node --test src/lib/time.test.ts src/lib/i18n.test.ts。
3. 不改 checks 的执行与拒绝逻辑，也不改 API。

验收：
- cargo test --workspace 通过，并新增单测，覆盖共享目录的路径与清理。
- 在 hub sandbox 里跑一个只改 apps/gui 的小 run，收尾通知显示 3/3 通过。
- 再故意写一个类型错误，确认 run_finish 被拒，失败输出里能看到 tsc 的报错。
- 报告写清 node_modules 实际放在哪里、第二个任务有没有复用上、占了多少磁盘。

不要 git push，不要改 .github/，不要动 ~/.crossagents/finish-watcher。
```
