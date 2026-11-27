# qoder-cli 记录

Qoder CLI（`@qoder-ai/qodercli`，可执行程序名 `qodercli`）是阿里云 Qoder 的终端编码 Agent。

本记录对应 `@qoder-ai/qodercli@1.1.99`（2026-11 校验，直接核对了官方发布包中的 `bundle/qodercli.js`、`package.json`、`README.md` 与 `qoder-npm-dispatcher.cjs`，并实测了无登录状态下的 CLI 参数校验、子命令与退出行为）。**尚未**使用已登录的 Qoder 账号跑通真实对话轮次，因此 `crates/drivers/tests/qoder_smoke.rs` 为 `#[ignore]`，协议链路通过 mock-CLI 集成测试与纯协议单元测试覆盖。

## 结论

- **原生协议与 Claude Code stream-json 高度一致**：`qodercli -p --input-format stream-json --output-format stream-json --include-partial-messages --permission-prompt-tool stdio --replay-user-messages --strict-mcp-config` 完整实现 `control_request` / `control_response`、`initialize`、`can_use_tool`、`interrupt`、`set_model`、`apply_flag_settings`、`get_context_usage`、`get_usage_info`、`--session-id`、`--resume`、`--fork-session`、`--resume-session-at`、`--setting-sources`、`--settings '{"disableAllHooks":true}'`、`--append-system-prompt`。
- 因此 `qoder-native` 直接复用 `claude-native` 的会话引擎（`Flavor::Qoder`），能力与 `claude-native` / `codebuddy-native` 对齐：`streaming` / `interrupt` / `mcp` / `resume` / `move_cwd` / `fork` / `set_model` / `usage` / `rate_limits` / `permission_callback` / `compaction` / `attachments: ALL` 全部开启；`result` 不带 `user_message_uuid`，`steer` 保守保持 `false`。同时在 `qoder.toml` 中保留 `qodercli --acp` 作为 `acp-generic` 兜底。

## 与 Claude Code 的关键差异（实测 / 源码核对）

1. **必须传 `QODER_EXPOSE_TOKEN_USAGE=1`**：`qodercli.js` 的 `RY()` / `zEA()` 会检查 `QODER_EXPOSE_TOKEN_USAGE`（`1`/`true`/`yes`）；**不设时所有 stream-json 的 `input_tokens` / `output_tokens` / `cache_*` 会被全部归零**。驱动启动会话时默认注入 `QODER_EXPOSE_TOKEN_USAGE=1`（仍允许用户显式 env 覆盖）。
2. **不接受 `--verbose`**：启动参数中省略 `--verbose`（保留 `--include-partial-messages`）。
3. **推理强度参数是 `--reasoning-effort`**（不是 `--effort`），支持 `none | low | medium | high | xhigh | max`。会话中切换：
   - 同时切模型与强度：`{ "subtype": "set_model", "model": "<id>", "reasoning_effort": "<level>" }`；
   - 仅改强度：`{ "subtype": "apply_flag_settings", "settings": { "model": { "reasoningEffort": "<level>" } } }`。
4. **`--permission-mode` 为 snake_case**：接受 `default`、`accept_edits`、`bypass_permissions`、`dont_ask`、`auto`；驱动自动把 Claude 风格的 camelCase（`acceptEdits` / `bypassPermissions` / `dontAsk`）规范化过去。
5. **上下文窗口大小（`modelUsage[*].contextWindow == 0`）**：`Q_o()` 在 `result.modelUsage` 中把 `contextWindow` 硬编码为 `0`，但 `Vyn()` / `hue()` 会在 `message_start`、`message_delta`、`result.usage` 中写出 `context_usage_ratio = input_tokens / contextWindow`（0..1）。驱动通过 `round(raw_input / context_usage_ratio)` 还原窗口大小，保证 GUI 进度条与 `get_context_usage` 拆解正常显示。
6. **Token 口径（网关 vs Anthropic BYOK）**：
   - Qoder 网关（`Z2a`，OpenAI 风格）把 `cached_tokens` 包含在 `input_tokens` 内；
   - Anthropic 兼容 BYOK（`u7r`）把 `input_tokens` 与 `cache_read_input_tokens` / `cache_creation_input_tokens` 分开上报。
   - 驱动规则：当 `cache_read + cache_creation > input_tokens` 时按 Anthropic 拆开相加，否则直接取 `input_tokens`（与 `hue()` 一致，不重复计算缓存）。
7. **`get_context_usage` 返回百分比拆解**：
   - 形状为 `{ model, contextWindow: { usedPercentage }, categories: [{ type, percentage }], autoCompact: { enabled, thresholdPercentage } }`；
   - 类别为 `system_prompt`、`system_tools`、`skills`、`messages`、`other`、`free_space`、`auto_compact`。驱动结合已还原的 `context_size` 换算为 `ContextPart` 列表。
8. **`get_usage_info` 返回套餐额度**：
   - 形状为 `{ usage: { userId, userType, totalUsagePercentage, expiresAt, isQuotaExceeded, userQuota, addOnQuota, orgResourcePackage }, session: { total_credits, model_usage }, usage_error? }`；
   - `userQuota` / `addOnQuota` 带 `{ total, used, remaining, percentage, unit }`，`orgResourcePackage` 带 `{ used, cap, remaining, percentage, available, unit }`。

## 配置、MCP 与跨目录恢复

- 全局配置目录：`QODER_CONFIG_DIR`（默认 `~/.qoder`）。
- 用户级 MCP 配置：`<QODER_CONFIG_DIR>/settings.json` 中的 `mcpServers`。因为会话以 `--strict-mcp-config` 启动，驱动将用户 `settings.json` 中的 `mcpServers` 合并到 `--mcp-config` 下层（hub 的协作 MCP 在上层覆盖重名项）。
- 会话历史：`<QODER_CONFIG_DIR>/projects/<tf(cwd)>/<session_id>.jsonl`，其中 `tf(cwd)` 把所有非 `[a-zA-Z0-9]` 字符替换为 `-`，长度超过 200 时截断到 200 并追加 `-{djb2_base36}`。跨目录恢复 / fork 前由 `qoder_native::adopt_transcript` 复制到新目录对应的项目文件夹。
- 环境变量隔离：从另一个 Qoder 终端启动 hub 时，屏蔽继承的 `QODER_*` / `QODERCLI_*` / `QODERCN_*` 会话变量，仅保留凭据与区域（`QODER_PERSONAL_ACCESS_TOKEN`、`QODER_SERVICE_ACCOUNT_KEY`、`QODERCN_SERVICE_ACCOUNT_KEY`、`QODER_SDK_ACCESS_TOKEN`、`QODER_REGION`）。

## 账号、登录与模型发现

- **状态查询**：`qodercli status -o json` 输出 `{ logged_in, version, auth_source, account_type, login_method, username, email, user_type, plan, org_id, auth_error_reason, auth_error_message }`。
- **模型列表**：`qodercli --list-models` 输出 `MODEL\n<displayName>\n<displayName> (<key>)\n...`；内置档位为 `auto`、`ultimate`、`performance`、`efficient`、`lite`。未登录时退出码 1，驱动与账号页退回 `qoder.toml` 中的 fallback 列表。
- **登录**：`qodercli login` 配合 `NO_BROWSER=1` 会向 stdout 打印浏览器 OAuth 链接并等待回调完成后以 0 退出，直接复用 CrossAgents 的标准登录流程，无需额外登录助手子命令。
- **API 域名与代理探测**：默认 `api.qoder.sh`；`QODER_REGION=cn` 时为 `gateway.qoder.com.cn`；若设了 `ANTHROPIC_BASE_URL` 则优先探网关域名。

## 安装与查找路径

- 优先调用直接二进制 `qodercli`（不走可能转发到 IDE 的 `qoder`），支持环境变量 `QODER_CLI` 覆盖。
- 原生安装器（`Mil()` / `QEs`）：
  - Windows：`irm https://qoder.com/install.ps1 | iex`，落到 `~/.qoder/bin/qodercli/qodercli.exe`；
  - macOS / Linux：`curl -fsSL https://qoder.com/install | bash`，落到 `~/.local/bin/qodercli`（亦查 `~/.qoder/bin/qodercli/qodercli`、Homebrew 路径）；
  - 升级：`qodercli update`。
- npm 安装：`npm install -g @qoder-ai/qodercli`（`shim = "node-script"` 拆掉 `.cmd` 包装直接调 `node`，原生 `.exe` 不受影响）。
