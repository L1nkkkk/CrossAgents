# Mac 安装说明

Mac 上装的是一个**不带 hub** 的桌面 GUI：hub 和 agent 跑在另一台电脑上（下面叫「中心」，例如 Windows 那台），Mac 经 SSH 隧道连过去，在 Mac 上看对话、发消息、管 run。

先说清楚几点：

- 发布页（[CrossAgents-releases](https://github.com/L1nkkkk/CrossAgents-releases/releases)）只有 Windows 安装包。Mac 版要在 Mac 上自己打包。
- 包是 ad-hoc 签名（没有 Apple 开发者证书），不能自动更新。更新就是重新打包、替换，见第 5 节。
- 包里没有 hub，也不连这台 Mac 上另外运行的 hub，只能连另一台电脑上的 hub。
- 标了「要实测」的地方还没在 Mac 上跑过；实际结果不一样的，记下来改这份文档。

## 1. 准备

Mac 上：

| 需要 | 说明 |
|---|---|
| git | `xcode-select --install` 会装上。主仓库是私有的，clone 前先用有权限的账号登录 GitHub（例如 `gh auth login`） |
| Node.js 22.12 或更新（带 npm） | Vite 8 的要求；20.x 要 20.19 以上。Windows 那台用的是 24 |
| 一把 SSH 密钥 | 没有就 `ssh-keygen -t ed25519` |

不需要 Rust，也不用 `cargo build`：Mac 的打包脚本不带 hub，不检查 hub 二进制。

中心上：hub 在跑（装了 Windows 版 CrossAgents 就会起；关窗口只是收进托盘，hub 照常跑），并且 Mac 能 SSH 登录它（第 4 节）。

## 2. 打包

```sh
git clone https://github.com/L1nkkkk/CrossAgents.git
cd CrossAgents
npm --prefix apps/gui ci
npm --prefix apps/gui run electron:build:mac
```

产物在 `apps/gui/release/`：

| 文件 | 给哪种 Mac |
|---|---|
| `CrossAgents-<版本>-arm64.dmg`、`CrossAgents-<版本>-arm64.zip` | Apple 芯片（M1 及以后） |
| `CrossAgents-<版本>-x64.dmg`、`CrossAgents-<版本>-x64.zip` | Intel 芯片 |

不确定是哪种芯片：终端跑 `uname -m`，`arm64` 是 Apple 芯片，`x86_64` 是 Intel。配置里两种架构都打；在一台 Mac 上能不能把另一种也打出来，要实测。

## 3. 安装与首次打开

1. 双击 dmg，把 CrossAgents 拖进「应用程序」；或者解压 zip，把 `CrossAgents.app` 拖进「应用程序」。
2. 从「应用程序」打开。在这台 Mac 上自己打的包一般不会被拦（要实测）。从别处拷来的（浏览器下载、AirDrop、聊天软件）会被 Gatekeeper 拦下，两种办法任选一种：
   - 系统设置 → 隐私与安全性 → 找到 CrossAgents 那一条 →「仍要打开」；
   - 终端里跑 `xattr -dr com.apple.quarantine /Applications/CrossAgents.app`，再打开。
3. 打开后侧栏写着「本机 · 不带 hub」，这是正常的：还没连中心。接着做第 4 节。

想核对签名（可选）：

```sh
codesign -dv --verbose=2 /Applications/CrossAgents.app          # 应看到 Signature=adhoc
codesign --verify --deep --strict /Applications/CrossAgents.app   # 没有输出就是通过
```

## 4. 连上中心的 hub

GUI 用系统自带的 `ssh` 登录中心，在中心上给这台 Mac 签发一个设备 token，再把中心 hub 的端口转到 Mac 本机。hub 仍只听中心的 `127.0.0.1`，不对外开端口。GUI 登录时不问口令、也不问主机密钥（`BatchMode=yes`），所以先在终端里把 SSH 调通。

Mac 要连得到中心：同一个局域网就用中心的局域网地址；不在一个网络里，推荐两边都装 Tailscale 这类组网工具，用它给的地址（几种办法的对比见 [MULTI-NODE.md](MULTI-NODE.md) §14）。

### 4.1 中心（Windows）上，做一次

1. 打开 OpenSSH 服务器。用管理员 PowerShell：

   ```powershell
   Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
   Start-Service sshd
   Set-Service -Name sshd -StartupType Automatic
   ```

   也可以在 设置 → 系统 → 可选功能 里添加「OpenSSH 服务器」，再用后两条命令启动并设成开机自启。

2. 把 Mac 的公钥（Mac 上 `~/.ssh/id_ed25519.pub` 的内容）加到中心：
   - 中心上的账户是管理员：加到 `C:\ProgramData\ssh\administrators_authorized_keys`。这个文件只能由 Administrators 和 SYSTEM 写，权限不对 sshd 不认，用管理员 PowerShell 改：

     ```powershell
     icacls.exe "C:\ProgramData\ssh\administrators_authorized_keys" /inheritance:r /grant "Administrators:F" /grant "SYSTEM:F"
     ```

   - 普通账户：加到 `C:\Users\<用户名>\.ssh\authorized_keys`。

3. 确认中心上的 hub 在跑：打开 Windows 版 CrossAgents 即可。

### 4.2 Mac 上先用终端试通

```sh
ssh <用户名>@<中心地址> 'type "%USERPROFILE%\.crossagents\hub.json"'
```

- 第一次连会问是否信任主机密钥，答 `yes`。这一步必须在终端里做一次：主机密钥还不认识时，GUI 直接失败，不会替你答。
- 不应该再问口令。问了就是公钥没装对，回到 4.1 第 2 步。密钥带口令的，先放进 ssh-agent：`ssh-add --apple-use-keychain ~/.ssh/id_ed25519`。
- 输出一段 JSON、里面有 `port`，就说明 hub 在跑，GUI 也读得到。

常连的话可以在 Mac 的 `~/.ssh/config` 里起个别名，GUI 里「主机」直接填别名：

```
Host centre
  HostName 192.168.1.20
  User <中心上的用户名>
```

### 4.3 在 GUI 里配对

设置 →「连接」→「配对一台远程中心」：

| 字段 | 填什么 |
|---|---|
| 主机（必填） | 中心的地址，或 `~/.ssh/config` 里的别名 |
| 用户名 | 中心上的 Windows 用户名；不填就用 ssh 的默认用户（或别名里的 `User`） |
| SSH 端口 | 不填是 22 |
| 设备名 | 在中心的「设备」页里认这台 Mac 用的名字 |
| 高级 → 中心上的 crossagents 命令 | 一般留空：GUI 先用中心 `hub.json` 里记的 hub 程序，没有才找中心 PATH 上的 `crossagents` |

点「配对并连接」。配对要登录中心几次，可能要十几秒。成功后侧栏标出远程主机；在 Mac 和中心各发一条消息，两边的对话应该一致。

- 配对拿到的是 write（读写）设备：它能让 agent 在中心上执行命令，所以只配对自己的机器。开终端、批准扩展、改 agent 权限要 admin 设备，在 Mac 上点这些会被 hub 拒绝。
- 第一次保存配对时，macOS 钥匙串可能会问，选「始终允许」（要实测）。拒绝的话保存的配对读不出来，连接页会显示「请重新配对；不会再自动重连」。
- 断网或隧道断了，会按 1、2、5、10、30 秒自动重连，断线期间的事件接着补上，不重放更早的历史。在中心的「设备」页吊销这台 Mac 之后，它不再重连，要重新配对。

## 5. 更新

Mac 版不能自动更新（Squirrel.Mac 不接受 ad-hoc 签名的包），设置里的更新卡片也是这么写的，托盘里没有「检查更新」。中心升级以后，Mac 上重新打包：

```sh
cd CrossAgents
git pull
npm --prefix apps/gui ci
npm --prefix apps/gui run electron:build:mac
```

然后退出 CrossAgents（菜单栏图标 →「退出界面」），用新的 `CrossAgents.app` 替换「应用程序」里的旧版。配对信息不在 app 里，替换后还在；签名变了，钥匙串可能再问一次，选「始终允许」（要实测）。

中心和 Mac 的接口版本对不上时，GUI 会显示说明页，按上面重新打包就好。

## 6. 卸载

把「应用程序」里的 CrossAgents 拖进废纸篓。配对信息在 `~/Library/Application Support/crossagents-gui/remote.json`（目录名按 Windows 上的 `%APPDATA%\crossagents-gui` 推断，要实测），要清干净就把这个目录也删掉；再到中心的「设备」页吊销这台 Mac。

## 7. 连不上时

ssh 自己的报错原样显示在「连接」页上。

| 看到的 | 原因与办法 |
|---|---|
| `Permission denied (publickey…)` | 公钥没装对，或带口令的密钥不在 ssh-agent 里：见 4.1 第 2 步、4.2 |
| `Host key verification failed` | 还没认过中心的主机密钥：先在终端里 `ssh` 一次，答 `yes` |
| `Connection timed out` / `Connection refused` | Mac 连不到中心的 SSH 端口：不在一个网络、sshd 没启动，或防火墙拦了 |
| 读不到 `hub.json` | 中心上的 hub 没在跑：在中心打开 CrossAgents |
| 「请重新配对；不会再自动重连」 | 这台设备被吊销了，或钥匙串拒绝了读取：重新配对 |

更多背景：打包配置和还没实测的项见 [tasks/MACOS.md](tasks/MACOS.md) §8；SSH 隧道怎么配对、怎么重连见 [MULTI-NODE.md](MULTI-NODE.md) §4.3、§14。
