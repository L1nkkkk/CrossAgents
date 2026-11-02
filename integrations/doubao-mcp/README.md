# doubao-mcp

把本机 **豆包桌面版** 的生图能力，接成 CrossAgents agent 能直接调用的 MCP 工具。

豆包同一个对话里，图片有两个 CDN 版本：聊天流挂的是 384×216 带水印缩略图，点开预览才挂 2848×1600 的无水印原图。本工具拿的是后者——不是去水印，是根本不去取那张带水印的。

实现上它只是 [dsh-doubao-painter](https://github.com/liangzz1998/dsh-doubao-painter)（MIT）里 `tools/paint.mjs` 的一层壳：CDP 驱动、DOM 选择器、CDN 模板名都在那边，这里只认它命令行的契约。

## 前置

1. **豆包已登录**，并且能以调试端口启动。从托盘完全退出豆包，然后：

   ```bat
   "%LOCALAPPDATA%\Doubao\Application\app\Doubao.exe" --remote-debugging-port=9225
   ```

   注意是 `Application\app\Doubao.exe`，不是 `Application\Doubao.exe`（那是启动器）。

2. **拿到插件**（只需要它的 `tools/paint.mjs`）：

   ```bash
   git clone https://github.com/liangzz1998/dsh-doubao-painter D:/tools/dsh-doubao-painter
   ```

3. **Node ≥ 22**（CrossAgents 运行时自带的那个就够）。

## 接进 CrossAgents

`~/.crossagents/config.toml`：

```toml
[mcp.doubao]
command = ["node", "C:/Users/Link/Dev/CrossAgents/integrations/doubao-mcp/server.mjs"]
env = { DOUBAO_PAINTER_HOME = "D:/tools/dsh-doubao-painter" }
```

改完重启 hub。`[mcp.<name>]` 的 `command` 是「程序 + 参数」，和 `[plugins.<name>]` 一个写法；`env` 会原样给到 MCP 进程。

每个 agent 会话都会挂上它，和 `crossagents` 那个协作 MCP 并列。名字不能重复——重名时 hub 的协作工具优先。

## 验证

```bash
crossagents agent ls                 # hub 起着
crossagents send <agent> "用 doubao_paint 画一只戴墨镜的橘猫，赛博朋克街头" --wait
```

agent 应该调到 `doubao_paint`，20~30 秒后拿回 `files` 里的本地路径。失败就先调 `doubao_status`，它会直接告诉你缺哪一步。

也可以不经过 CrossAgents 单测：

```bash
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"doubao_status","arguments":{}}}' \
  | node integrations/doubao-mcp/server.mjs
```

## 工具

| 工具 | 参数 | 返回 |
|---|---|---|
| `doubao_paint` | `prompt` 或 `prompts[]`；可选 `ref_images[]`（图生图）、`out`（输出目录） | `{ files: [...], seconds }`；批量时逐张串行，中途失败会把已出的图一起交回 |
| `doubao_status` | 无 | 插件脚本在不在、调试端口通不通，不通时给出启动命令 |

默认输出到当前工作区的 `doubao-images/`（agent 的 cwd 就是它的工作区）。

## 限制

- **串行**：豆包只有一个窗口，出图期间会开新对话、点开预览。别和人同时操作豆包，也别指望多个 worker 并发出图。
- **要重启豆包**：正常启动的实例没有调试端口。要么常驻调试模式，要么每次用前重启一次。
- **调试端口是后门**：CDP 能完全控制那个已登录的豆包，`127.0.0.1` 挡不住本机进程。只在需要时开。
- **会随豆包改版失效**：DOM 选择器和 CDN 模板名都是私有实现。坏了先 `node tools/paint.mjs "test" --json` 看报错。
- **吃每日免费额度**，用完就失败，`doubao_status` 不会报这一条（只有出图时会知道）。
- 对外发布的图仍要按《人工智能生成合成内容标识办法》标识，这里解决的只是「素材被平台水印污染」。
