# 账号风险机制

账号是 CrossAgents 里最贵的东西：一个登录被封，损失的是一段时间的额度和一个真实身份。
这份文档收拢围绕账号的几套机制，每节各自独立、由不同任务填充；没写的那节就是还没做。

## 出口策略

每个账号有一条出口策略（`egress`），决定它能不能在代理走不通时改用裸 IP 直连。

### 取值

- `proxy_only`：只走代理。代理本身连不上时直接失败；代理连得上但到目标 host 的路由
  是死的，也直接失败，**不**尝试直连、**不**缓存直连结果、**不**给 CLI 发 `NO_PROXY`
  豁免。失败信息会写明是哪个 host、并要求修好代理节点，或把该账号改成 `allow_direct`。
- `allow_direct`：保持改动前的行为。代理到目标 host 不通而直连能通时，为这次启动豁免
  该 host（写 `NO_PROXY`），并记住它 10 分钟，让后续启动不再等探测超时。

取值写在账号 JSON 的 `egress` 字段里（`"proxy_only"` / `"allow_direct"`）；老账号没有
这个字段，按 provider 推导：`claude`、`codex` 为 `proxy_only`，其余为 `allow_direct`。
没有绑定账号的会话（直接用本机登录）按它驱动的 CLI 取同样的默认值。

### 配置项

`config.toml` 的 `[accounts]` 段，按下面的优先级从高到低生效：按账号 id 的覆盖 > 按
provider 的覆盖 > 账号 JSON 里的 `egress` > `egress_default` > provider 默认值。配置
压过账号自己的值，是为了在默认值判断错时不用逐个改账号。

- `[accounts] egress_default = "proxy_only"` — 所有没特别指定的账号用哪个策略（不写则按
  provider 推导）。
- `[accounts.egress_by_provider]` — 按 provider 覆盖，例如
  `codebuddy = "allow_direct"`。
- `[accounts.egress_by_account]` — 按账号 id 覆盖，例如
  `claude-work = "allow_direct"`。

另外，Windows 的系统代理现在每 45 秒最多读一次注册表（原来整个 hub 生命周期只读一次），
在系统里换节点后不必重启 hub。

### 这套机制能做什么，不能做什么

它只做三件事，而且这三件事分在不同的小节里：

1. 不让 CrossAgents 自己把账号卖了——代理不通时不许它偷偷用裸 IP 直连（本节）。
2. 出口落地变了，当天通知到人（见「出口指纹」）。
3. 出事时立刻停手（见「风控熔断」）。

它**防不住** vendor 改风控阈值：什么行为算异常、什么时候封号，由对方决定，这里不做预测。
它**也防不住**同一个出口 IP 上别的用户出事把你连坐：我们只能保证自己不主动暴露，管不了
同节点的陌生人。别把这几条读成「账号安全了」——它们只降低由 CrossAgents 自己引起的风险。

## 出口指纹

（待填）

## 风控熔断

（待填）

## 并发与作息

（待填）

## GUI 显示

（待填）
