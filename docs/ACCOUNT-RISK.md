# 账号风险机制

账号是 CrossAgents 里最贵的东西：一个登录被封，损失的是一段时间的额度和一个真实身份。
这份文档收拢围绕账号的几套机制，每节各自独立、由不同任务填充；没写的那节就是还没做。

## 出口策略

每个账号有一条出口策略（`egress`），决定它能不能在代理走不通时改用裸 IP 直连。

### 取值

- `proxy_only`：必须配置并使用代理；没有配置代理时直接失败。代理本身连不上时直接失败；代理连得上但到目标 host 的路由
  是死的，也直接失败，**不**尝试直连、**不**缓存直连结果、**不**给 CLI 发 `NO_PROXY`
  豁免。失败信息会写明是哪个 host、并要求修好代理节点，或把该账号改成 `allow_direct`。
- `allow_direct`：保持改动前的行为。代理到目标 host 不通而直连能通时，为这次启动豁免
  该 host（写 `NO_PROXY`），并记住它 10 分钟，让后续启动不再等探测超时。

取值写在账号 JSON 的 `egress` 字段里（`"proxy_only"` / `"allow_direct"`）；老账号没有
这个字段，按 provider 推导：`claude`、`codex` 为 `proxy_only`，其余为 `allow_direct`。
没有绑定账号的会话（直接用本机登录）按它驱动的 CLI 取同样的默认值。

### 配置项

`config.toml` 的 `[accounts]` 段，按下面的优先级从高到低生效：按账号 id 的覆盖 > 账号
JSON 里的 `egress` > 按 provider 的覆盖 > `egress_default` > provider 默认值。越细粒度
的键越优先：provider 表和默认值只负责兜底，不会推翻某个账号显式写的选择。

- `[accounts] egress_default = "proxy_only"` — 所有没特别指定的账号用哪个策略（不写则按
  provider 推导）。
- `[accounts.egress_by_provider]` — 按 provider 覆盖，例如
  `codebuddy = "allow_direct"`。
- `[accounts.egress_by_account]` — 按账号 id 覆盖，例如
  `claude-work = "allow_direct"`。

使用自定义 `ANTHROPIC_BASE_URL` 连接国内中转网关的 claude 账号也继承 `proxy_only`；若该网关直连不会额外暴露账号，可在 `[accounts.egress_by_account]` 将该账号设为 `allow_direct`。

另外，Windows 的系统代理现在每 45 秒最多读一次注册表（原来整个 hub 生命周期只读一次），
在系统里换节点后不必重启 hub。`CROSSAGENTS_EGRESS_POLICY` 这个变量由 hub 自己设置：项目
`crossagents.toml` 的 `[run] env` 里写同名变量会被忽略，也不会被传给厂家的 CLI。

### 这套机制能做什么，不能做什么

它只做三件事，而且这三件事分在不同的小节里：

1. 不让 CrossAgents 自己把账号卖了——代理不通时不许它偷偷用裸 IP 直连（本节）。
2. 出口落地变了，当天通知到人（见「出口指纹」）。
3. 出事时立刻停手（见「风控熔断」）。

它**防不住** vendor 改风控阈值：什么行为算异常、什么时候封号，由对方决定，这里不做预测。
它**也防不住**同一个出口 IP 上别的用户出事把你连坐：我们只能保证自己不主动暴露，管不了
同节点的陌生人。别把这几条读成「账号安全了」——它们只降低由 CrossAgents 自己引起的风险。

## 出口指纹

会话启动前，用合并后的账号和项目环境选出实际代理，通过它访问用户配置的 echo 端点；不会使用内置第三方服务。端点返回纯文本 IP，或 JSON `{"ip":"...","asn":"AS...","country":"US"}`；ASN 和国家可缺省。当前支持 HTTP 端点和不带认证的 HTTP 代理，其他情况及超时、无效响应均视为 unknown，不拦启动。未配置端点时 IP 也为 unknown。

`allow_direct` 可能回退直连，`NO_PROXY` 也可能让目标绕过代理；在这些情况下无法确认代理出口是会话出口，指纹记为 unknown。

每次观察仅保存账号、IP、ASN、国家和时间。新 IP 通知但不阻断；已知国家发生变化时默认先询问，选择「允许」后重试启动。unknown 不触发国家变化判定；未配置代理时仍按 `proxy_only` 拒绝启动。

库里只记录真实观察；完全采不到指纹时不写入，因此界面显示的是最后一次能确认的出口，可能来自上一次启动，而非此刻的出口。

ASN 缺失时国家也按 unknown 处理，避免不完整的采集结果阻断启动或覆盖上次已知国家。

- `[accounts] egress_echo_url`：自选 HTTP echo 地址，不配置则不请求任何端点。
- `[accounts] egress_history_days`：判定新 IP 时检查的历史天数，默认 `0` 表示全部历史。
- `[accounts] egress_block_country_change`：已知国家变化是否要求确认，默认 `true`。

## 风控熔断

账号级熔断：一旦 vendor 开始怀疑这个登录，继续跑只会让画像更难看。命中后立刻停手，
**不是**重试——这条是故意的，代码里没有任何自动重试、退避重连或换账号再试的路径。

### 命中哪些信号

- vendor 返回的拒绝里带 KYC、identity verification / identity check、captcha、
  human verification / human check / verify you are human 字样；
- 异常的 401 / 403：登录不再被接受，不是额度问题；
- 登录态从 `logged_in: true` 突然变成 `false`。

### 阈值

在一个窗口内（`WINDOW_DAYS = 7` 天），风险信号至少 `MIN_SIGNALS = 2` 个，且占该账号
窗口内被观察到的调用至少 `SIGNAL_PERCENT = 50%` 就命中。观察按账号记录在
`crossagents_core::Health` 里：`ok` 的一次是没有信号的调用，`!ok` 的一次是一个风险信号。

### 命中之后

- 该账号上绑定的所有 agent 立即置为 `Stopped`；
- 账号被 pause，`risk_reason` 保留原因给人读（GUI 由 T5 显示）；
- `fit` / `weigh` 对该账号返回 at-risk，路由把它**排除**在挑选之外——不是降权，也不是
  排到最后当备选；
- 不重试：账号 pause 后 `risk::record` 不再计数，路由也不会再探测它。

### 谁能重新 arm

只有人。`Accounts::clear_risk(id)` 是唯一的清除入口；没有冷却时间，也没有任何自动恢复。

### 配置项

本节没有新增配置项；窗口、最小信号数和占比是代码里的常量
（`WINDOW_DAYS` / `MIN_SIGNALS` / `SIGNAL_PERCENT`，见 `crates/orchestrator/src/risk.rs`）。

## 并发与作息

（待填）

## GUI 显示

（待填）
