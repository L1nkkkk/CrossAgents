# CrossAgents 项目规则

本文件由 CrossAgents 自动注入到各厂商 Agent（Codex / With 原生读取，其余由 Hub 注入），请所有在本仓库工作的 Agent 遵守。

## 1. 分层测试策略（避免频繁改动浪费时间）

本仓库 `crates/mock-agent/tests/*` 包含 30+ 个进程级端到端集成测试，全量 `cargo test --workspace` 耗时约 13~15 分钟（其中 92% 耗在 E2E）。**日常开发不要每次改动都跑全量 E2E**，按改动范围分层验证：

1. **第一档：日常增量开发（3~15 秒）** — 只跑受影响的包/目标：
   - 前端 GUI：`npm --prefix apps/gui run typecheck && npm --prefix apps/gui test`
   - Rust 单元逻辑：`cargo test -p <改动的crate> --lib -j 4`
   - Hub HTTP API：`cargo test -p crossagents-hub --test api -j 4`
   - Tauri 桌面壳：`cargo test --manifest-path apps/gui/src-tauri/Cargo.toml -j 4`
2. **第二档：提交前快检（~1 分钟）**：
   - `cargo test --workspace --lib -j 4`（覆盖全部 1,290+ Rust 单元测试，跳过重型 E2E）
   - 仅补跑与改动直接相关的 1~2 个 `crates/mock-agent` 集成测试文件（如 `cargo test -p crossagents-mock-agent --test delivery -j 4`）
   - 若改动涉及前端，再补 `npm --prefix apps/gui run build`
3. **第三档：发版前或触及底层核心时**（修改进程托管、ConPTY、编排器状态机、SQLite 迁移等）：
   - 才跑全量 `cargo test --workspace --no-fail-fast -j 4`。

## 2. Windows Cargo 构建注意事项

- 在 Windows 本机运行 `cargo` 时**始终传 `-j 4`**，且**不要并行启动两个 `cargo` 构建**，避免 `rustc` 虚拟内存提交 OOM（`0xc0000409`）。
- 修改 `crates/core` 或 `crates/api-types` 中导出到前端的类型（`#[derive(TS)]`）后，务必执行 `cargo test -p crossagents-api-types -j 4` 重新生成 `bindings/*.ts`，并跑前端 `typecheck` 和 `test` 确认对齐。

## 3. Tauri 桌面壳约束

- `apps/gui/src-tauri/src/` 的生产代码禁止使用 `WebviewWindow` / `get_webview_window`（一律使用 `Window` / `app.get_window("main")`），否则内嵌浏览器子 WebView 存在时会破坏主窗口控制（由 `desktop::tests` 守卫）。

## 4. 对话中的文件引用规范

- 在回复中引用仓库文件时，优先使用**工作区相对路径 + 正斜杠**（如 `crates/hub/src/api.rs:12` 或 `[api.rs](crates/hub/src/api.rs:12)`），避免拼带绝对路径的 `file:///` 链接。

## 5. 发版流程

- 先将功能与修复改动单独提交到 `main`，确保工作区完全干净；
- 发版统一执行 `node scripts/release.mjs`（脚本会自动递增版本号、构建 release、提交 `chore: 发布 x.y.z`、打 tag、打包签名并上传到 `L1nkkkk/CrossAgents-releases`，核对完整后才 `git push origin main vX.Y.Z`）。
