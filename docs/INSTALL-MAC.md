# Mac 安装说明

Mac 上装的是一个**不带 hub** 的桌面 GUI：hub 和 agent 跑在另一台电脑上（下面叫「中心」，例如 Windows 那台），Mac 用中心给的配对码经内置打洞连过去（推荐，4.0），或经 SSH 隧道连过去（4.1 起），在 Mac 上看对话、发消息、管 run。

先说清楚几点：

- 发布页（[CrossAgents-releases](https://github.com/L1nkkkk/CrossAgents-releases/releases)）有 Apple 芯片的 Mac 包（`CrossAgents-<版本>-arm64.dmg` / `.zip`，推 tag 时由 GitHub Actions 打）。Apple 芯片的 Mac 直接下载，跳过第 1、2 节；Intel Mac 没有现成的包，按第 1、2 节自己打。
- 包是 ad-hoc 签名（没有 Apple 开发者证书），不能自动更新。更新就是重新下载（或重新打包）、替换，见第 5 节。
- 包里没有 hub，也不连这台 Mac 上另外运行的 hub，只能连另一台电脑上的 hub。
- 标了「要实测」的地方还没在 Mac 上跑过；实际结果不一样的，记下来改这份文档。

## 1. 准备

Mac 上：

| 需要 | 说明 |
|---|---|
| git | `xcode-select --install` 会装上 |
| Node.js 22.12 或更新（带 npm） | Vite 8 的要求；20.x 要 20.19 以上。Windows 那台用的是 24 |
| Rust（rustup） | `curl https://sh.rustup.rs -sSf | sh`。打包脚本先 `cargo build --release -p crossagents-link`：包里要带打洞程序 crossagents-link（4.0）。不编 hub |
| 一把 SSH 密钥 | 只有走 SSH（4.1 起）才要；没有就 `ssh-keygen -t ed25519` |

中心上：hub 在跑（装了 Windows 版 CrossAgents 就会起；关窗口只是收进托盘，hub 照常跑）。用配对码时中心上什么都不用装；走 SSH 时 Mac 要能 SSH 登录它（4.1）。

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

不确定是哪种芯片：终端跑 `uname -m`，`arm64` 是 Apple 芯片，`x86_64` 是 Intel。配置里两种架构都打；在一台 Mac 上能不能把另一种也打出来，要实测。**只装和这台 Mac 芯片相同的那个**：包里的 crossagents-link 是 `cargo build` 按本机架构编的，另一种架构的包里那个跑不起来，配对码就用不了（SSH 不受影响）。

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

### 4.0 用配对码（推荐：不用 SSH、不开端口、不装 Tailscale）

两台电脑先经中继找到对方，再尽量直接连通（打洞）；打不通也能经中继用。原理和取舍见 [MULTI-NODE.md](MULTI-NODE.md) §14「内置打洞」。

1. **中心上**：打开 CrossAgents → 设置 →「连接」→「让别的电脑连进来」。
   - 第 1 步点「开启」。状态先是「连接中继中」，变成「在线」就好了（国内网络一般要十几秒，且要代理，见下面）。
   - 第 2 步点「生成配对码」，第 3 步出现配对码，点「复制」。码只能用一次，30 分钟内有效，中心的 hub 重启后作废。
   - 中心没有界面（例如 Linux 服务器）时，在它的终端里跑 `crossagents link on`，再跑 `crossagents link invite`，打印出来的就是配对码。
2. 把配对码发到 Mac 上（自己的聊天软件、备忘录同步都行；它等于一把一次性的钥匙，别发到群里）。
3. **Mac 上**：设置 →「连接」→「连到别的电脑上的中心」→「配对码（推荐）」，粘贴。下面马上写出中心的名字、还剩多久过期。填个设备名，点「连接」。三步走完（打洞连到中心换取设备 token → 在本机起转发 → 检查 token），窗口整页重新加载，侧栏写着「远程 <中心机器名>」。
4. 「现在连的是」那一行旁边写着「经中继」或「直连」：刚连上一般经中继，几秒后打通就变「直连」。经中继也能正常用，只是延迟高一些。

**代理。** n0 的公共中继在海外，国内网络两边常常都要代理才连得上：

- 中心的 hub 用它自己环境里的 `HTTPS_PROXY`，没有就用 Windows 的系统代理（和它给 CLI 用的是同一个）。一直停在「连接中继中」，就打开系统代理后点「重新开启」。
- Mac 上的 GUI 起 crossagents-link 时按 macOS 的系统代理设置走（Chromium 对中继地址给出的代理）。
- 也可以在国内的云服务器上跑一个 iroh-relay，在中心的「高级：自建中继」里填它的地址；生成的配对码会带上它，Mac 也走它。
- 两台在同一个局域网时，配对码里带着中心的局域网地址，不经中继也能连。

**连不上时**：「连接」页按原因给出下一步。码过期或用过了，在中心重新生成一个；「连不上中心」先看中心那边是不是「在线」，再看两边的代理。

**吊销**：配对上的 Mac 出现在中心的「设备」页，在那里吊销它，它的连接立刻被拒绝，Mac 上会显示「请重新配对」。在中心点「关闭」，不会再有新的电脑连进来，没用掉的配对码也作废。

这一节在 Windows 上用真程序跑通过（两个进程都在这台 Windows 上，经代理连 n0 中继，几秒后变直连，吊销后转发退出），还没在 Mac 上实测，见 [TODO.md](TODO.md) §八「内置打洞」。

下面 4.1 到 4.4 是 SSH 方式：中心开着 OpenSSH Server 时可以用，也是配对码用不了时的退路。

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

### 4.4 用 Tailscale

Mac 和中心不在一个网络里时，两台都装 Tailscale（Mac 装 Tailscale.app，中心装 Windows 版），登录同一个账号，两边都显示在线。装好之后就不用手抄中心的地址了：

1. 设置 →「连接」最上面是「Tailscale」一节，列出同一账号下的机器。在线的排在前面，每台机器旁边是系统图标和在线点；离线的也列出来（灰显，写着最后在线的时间），选了也连不上，要等它在线。
2. 点要连的那台机器右边的「用这个地址」：它把 MagicDNS 名（没有就用 `100.x` 地址）填进下面的「主机」栏。
3. 接着照 4.1 到 4.3 做。中心上的 OpenSSH、公钥、用户名都还要，Tailscale 只解决连得到，不替代 SSH 登录。

这一节读的是本机已装的 Tailscale 命令（`tailscale status --json`）：先找 PATH，再找默认安装位置。没装、没登录、没在运行，各有一句提示；装好或登录之后点「刷新」重读。

服务器开发同理：中心放在一台 Linux 服务器上（或另一台 Windows 机器上）时，服务器也装 Tailscale 并登录同一个账号，在列表里选它的地址，SSH 那套照做。

这一节还没在真机上实测，见 [TODO.md](TODO.md) §八。

## 5. 更新

Mac 版不能自动更新（Squirrel.Mac 不接受 ad-hoc 签名的包），设置里的更新卡片也是这么写的，托盘里没有「检查更新」。中心升级以后，Apple 芯片的 Mac 到发布页下载同版本的 arm64 包；自己打包的，重新打包：

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
| 配对码：「配对码不能用了」 | 用过了、过了 30 分钟，或中心的 hub 重启过：在中心重新生成一个 |
| 配对码：「连不上中心」 | 中心不在线（看中心「连接」页是不是「在线」），或两边连不上中继：开系统代理，或用自建中继；同一局域网时直接能连 |
| 配对码：「这个安装包缺少打洞程序」 | 打包时没编 crossagents-link：先 `cargo build --release -p crossagents-link` 再打包（第 1、2 节） |

更多背景：打包配置和还没实测的项见 [tasks/MACOS.md](tasks/MACOS.md) §8；SSH 隧道怎么配对、怎么重连见 [MULTI-NODE.md](MULTI-NODE.md) §4.3、§14。
