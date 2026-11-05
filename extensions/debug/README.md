# 事件流扩展（试点）

此目录是普通扩展 `debug`，不包含在发布版安装包中。它只声明 `read` 和 GUI 入口，不提供工具、提示词、钩子或服务。

## 安装

1. 从本仓库取得完整的 `extensions/debug/` 目录（包含 `extension.toml`、`gui/index.js` 和本文件）。发布版用户没有仓库时，可以从 [CrossAgents 源码仓库](https://github.com/L1nkkkk/CrossAgents)下载包含该目录的源码压缩包，或克隆仓库后复制该目录；要选择实际包含此扩展的版本。
2. 把整个目录复制为 `<home>/extensions/debug/`。`<home>` 是 `CROSSAGENTS_HOME`，未设置时为 `~/.crossagents`。无需重新启动 hub；它每两秒检查源目录。
3. 在 GUI「扩展」页检查待批准的文件差异和权限，再批准该指纹。工具列提供「事件流」标签；选择一个会话即可查看。更新文件后也要检查并批准新版本。

## 与内置面板的差异

- 异步加载并订阅之前的事件不会出现；停用后重新启用，以及批准新版本，都会丢失扩展缓存中的历史。
- 只跟随扩展 API 的 `follow`，不能使用内置面板的 `safeTarget?.agent` 覆盖。扩展按现有 `/v1/agents` 只读接口解析会话名称和 ID。
- 发布版启用后也可见；内置事件面板仅在开发版或 mock 中可见。未安装时没有此扩展标签。
- 每会话保留最近 1000 条，过滤后最多展示最近 300 条。关闭和重开面板保留缓存。重连使用宿主的 Last-Event-ID，不自行回放；hub 按大于上次序号发送，并过滤重放与实时流的重叠。

## 验收状态

2026-11-05：在 Windows 的真实 Electron 44.6.0 中，以隔离的 `CROSSAGENTS_HOME`、`port = 0` 和独立 user-data-dir 验收。GUI 使用 `vite build` 的 `file://.../dist/index.html`，没有 `--dev-server-url`；hub 从本工作树构建。通过 CDP 检查并截图：

- 未安装时，独立会话的标签及添加菜单均没有事件流（`12-uninstalled.png`）；复制后 hub 发现待批准版本，在扩展页检查差异并批准（`02-pending.png`、`03-diff.png`）。
- 启用后生产页面出现标签，收到事件 18；关闭并重新添加标签后事件 18 仍在（`04-event.png`、`05-reopen.png`）。断开 hub，在 GUI 离线时重启并写入事件，重连后事件 23 到达，且只显示一次（`06-reconnect.png`）。
- 停用后标签消失（`07-disabled.png`、`08-tab-gone.png`）。为验证订阅清理，仅在隔离目录的测试副本加入计数器：停用后再发事件，旧计数保持 3；重新启用并切换到另一个计数版本后，旧计数保持 8、新计数递增，切换前事件 36 不再出现，新事件 41 出现（`09-before-switch.png`、`10-after-switch.png`）。
- 隔离测试副本注入渲染异常后，仅扩展区域显示错误及「停用」，会话内容和其他标签仍可用，点击「停用」生效（`11-error.png`）。展开 JSON 和过滤也检查过（`14-filter.png`、`16-final-json.png`）。计数和异常注入没有进入仓库扩展。

截图位于本机临时目录 `%TEMP%/ca-events-pilot-attempt2/`。`npm run build`、`npm run typecheck` 和 `npm test` 通过；单测覆盖分组和 1000 条缓存。