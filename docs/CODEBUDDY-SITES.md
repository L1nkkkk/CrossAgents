# 同时使用 CodeBuddy 国内站和国际站

在「订阅与额度」中分别添加两个 CodeBuddy 账号，选择「新的独立登录」，站点分别选国内站、国际站。建议命名为 `codebuddy-cn` 和 `codebuddy-intl`。在各自打开的官方登录页选择对应站点，完成本人扫码或授权；添加账号并不代表已经登录。之后给不同队友选择对应账号，即可同时使用两边的模型。

CLI 的对应操作：

```sh
crossagents account add codebuddy-cn --provider codebuddy --codebuddy-site china --label "CodeBuddy 国内站"
crossagents account add codebuddy-intl --provider codebuddy --codebuddy-site international --label "CodeBuddy 国际站"
crossagents account login codebuddy-cn
crossagents account login codebuddy-intl
```

新账号创建 API 的可选字段为 `codebuddy_site: "china" | "international"`。指定此字段必须使用 `provider: "codebuddy"`，不能同时导入默认登录，也不能复用本机默认配置目录或已经登记的账号目录。旧账号缺少这个字段时，保留已有的登录、设置和运行行为。

## 为什么要额外隔离登录

CodeBuddy CLI 的 `CODEBUDDY_CONFIG_DIR` 控制配置和会话目录，但在本机 2.178.1 中，文件登录凭据由 FileAuthenticationStorage 写到用户数据目录的 `CodeBuddyExtension/Data/Public/auth/`，因此只分配置目录不能保证两份凭据独立。

明确指定站点的新账号会同时获得：

- 自己的 CodeBuddy 配置和会话目录。
- `<账号目录>/profile` 下的子进程用户资料目录，登录、状态探测和 agent 运行都使用相同环境。只改变子进程的 `HOME`、`USERPROFILE`、AppData 和 XDG 数据目录，不改变 hub 或系统用户的环境。
- 指向对应站点的 CodeBuddy 环境选择，并移除子进程继承的 `CODEBUDDY_API_KEY` / `CODEBUDDY_AUTH_TOKEN`，避免父进程的身份覆盖独立登录。不能把这两个变量设为空字符串：本机 CLI 会将空的认证覆盖当作显式凭据，出现账号探测已登录、实际请求却提示 `Authentication required` 的情况。登录服务、状态探测和 agent 启动共用同一套移除处理。

CLI 启动的 shell 工具也会看到这个独立资料目录，原用户目录中的 Git、SSH 等配置不会自动复制进来；需要时显式配置。这个机制用于分开 CLI 的资料和登录，不是文件访问沙箱。

两个账号的状态和模型分别缓存。国内站沿用现有余额查询；国际站尚未接入余额 API，界面会说明，不能把没有余额数据当作余额为零。国际站凭据不会发送到国内余额端点。站点标签用于区分账号；登录页面中仍需本人选择对应站点并完成认证。

## 验证范围

本机安装的 CodeBuddy 2.178.1，使用两个全新的临时资料目录并发执行真实 `initialize`：两个子进程的用户目录均正确隔离，均返回未登录，国内站返回 17 个模型条目，国际站返回 21 个。此结果证明没有借用当前机器的登录，并证明两个站点的模型列表可以分别读取；模型条目不等于账号已获使用权限。

自动测试覆盖两站点的登录/探测/agent 环境一致、父进程身份覆盖、站点对应的网络探测主机、账号站点持久化，以及禁止导入默认登录或复用目录。未替用户完成两次真实扫码授权，也未进行付费模型调用。

官方参考：[CLI 安装与配置目录](https://www.codebuddy.cn/docs/cli/installation)、[登录方式](https://www.codebuddy.ai/docs/cli/quickstart)。配置目录与凭据位置的区别依据本机 CLI 实现核对，不能仅从配置目录文档推断。
