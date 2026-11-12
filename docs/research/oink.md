# Oink 调研

> 2026-11-13 整理。只读了 [README](https://github.com/oinkvm/oink)、GitHub 上的 crate 列表、`crates/oink-protocol` 的文件树和 `tests/v1_golden.rs` 全文，没有安装、没有运行、没有读业务实现。核对在 [`c15f275`](https://github.com/oinkvm/oink/tree/c15f2754f0469a25920eb389c97eacbe9b2c529a)（提交信息是 Oink 0.3.6，2026-11-12）。
> 本文只回答一个问题：它的线上契约怎么组织，哪一处值得抄进 B1。不评判 KVM 功能。结论落在 [tasks/B1-NODE.md](../tasks/B1-NODE.md) §3 第 4 条、§4。

## 1. 它是什么

[Oink](https://github.com/oinkvm/oink)（Oink Is Not only a KVM）是 Rust + Tauri 的跨 Windows / macOS 对等软件 KVM：一套键鼠控制多台机器，剪贴板、拖拽、音频跟着走。GPL-3.0-only，带附加条款（分发衍生品不得用 Oink 的名义推广）。

| | Oink | CrossAgents |
|---|---|---|
| 拓扑 | 无中心对称 P2P。README 明说不是 client-server，不需要指定一台当 controller | 固定中心 hub + 节点（[MULTI-NODE.md](../MULTI-NODE.md) §2） |
| 线上传什么 | 键鼠输入、剪贴板、拖拽、音频、设备发现 | 会话事件、驱动调用、权限询问、终端数据 |
| 契约 crate | `oink-protocol`：canonical CBOR、schema 注册表、`v1/dto/` 按主题分 | `crates/node-proto`：JSON 行、`v1/` 按主题分 |
| 兼容性怎么验 | 存档报文（fixtures）逐字节解码再编码 | 待建，约定见 [B1-NODE.md](../tasks/B1-NODE.md) §4 |

两边的共同点只有一个：线上契约是一份独立的、被测试钉住的类型集合。这一处正好是我们这次改的东西。

## 2. 值得抄的一处：线上契约单独一个 crate

它的做法（`crates/oink-protocol`）：

- **只放契约。** `src/lib.rs` 的开头一句：「Typed v1 wire contract, canonical CBOR profile, framing and admission rules. Business owners convert their local domain values to the retained DTOs in `v1`.」业务侧把本地的域类型显式转成 `v1` 的 DTO。`v1/` 下除 `dto/` 外按职责分文件：`cbor.rs`、`frame.rs`、`limits.rs`、`admission.rs`、`error.rs`、`layout.rs`、`retained.rs` 等。
- **schema 与注册表成对。** `schema.rs` / `schema_entries.rs`、`registry.rs` / `registry_entries.rs`。`registry::MESSAGES` 分配消息 id，有 tombstone（删掉的 id 不复用），能力名要在 `CAPABILITIES` 里登记。测试侧另有一份 Python 的 `schema_compatibility.py`。
- **fixtures 存旧报文，测试要求逐字节稳定。** `tests/fixtures/v1/` 里一份 `manifest.json` 加每个报文一个 `.hex`；`v1_golden.rs` 的 `every_checked_in_vector_decodes_and_reencodes_identically` 把每个报文解码再编码，要求与存档字节相同，且 `manifest.json` 钉着 `schema_sha256`。磁盘上有、清单里没列的 fixture 也会让测试红（两边集合必须相等），防止有人加了录像却没接进测试。`fixtures/v1/schema.json` 是审过的 schema 快照。

我们抄的：

- `crates/node-proto` 单独成 crate（`171b2a0`），`v1/` 按主题分 `frames`、`ops`、`session`、`agent`、`cli`、`error`、`reply`；与 `core` 的域类型显式双向转换，事件路径已合（`69da2b9`）。
- fixtures 的约定写进 [B1-NODE.md](../tasks/B1-NODE.md) §4：每次发版录一份当时的报文到 `crates/node-proto/tests/fixtures/v<协议主版本>/`，老目录只增不改；跨版本解码测试照它的 golden 做法，格式变了就红。

不抄的：**CBOR**。我们的通道是 JSON 行，人能直接看一条流里的报文；canonical 编码的收益在 KVM 的高频输入帧上，会话事件用不上。schema 注册表那一套（消息 id、tombstone、schema 快照）也比我们现在的规模重，先不引入；等契约大到需要按 id 分配时才谈。

## 3. 不适用的：整体架构

- **无中心对称 P2P，分区后合并成员名单。** 仓库里有 `group_merge.rs`、`pair_merge.rs`、`roster.rs`、`group_state.rs`、`pair_journal.rs`，从文件名看成员名单是对等复制、断连后合并的。这一套不适用于我们：编排要常在线、历史只有一份、客户端下线不影响别人，MULTI-NODE §2 比较过「每台都是 hub，互联」并放弃。成员合并只在多主写入时才需要，hub 是唯一写入者。
- **设备发现、QoS 队列、指针槽位**（`discovery/`、`qos/`、`pointer_slots.rs`）是按 KVM 的流量特征做的，和会话事件无关。只记文件名，没有读实现。
- **不引入它的协议对端兼容范围。** 版本协商在 `v1_negotiation.rs`（没读）。我们的规则更简单：协议主版本决定能不能连，次版本与能力差异降级，见 B1-NODE §4。

## 4. 本 Work 不做，以后可能用到

这两处 Oink 有、我们不抄，各自等到自己的前置条件：

- **按流量类型分流**（`oink-network` 的 `qos/` 与 `qos.rs`）：从模块名看，是把不同用途的流量分到不同的队列或通道、各有优先级。我们的控制流目前一条，终端数据已经计划走单独的流（B1.7）。真出现大块事件堵住控制流时再做，排在 B1.7 和 B3（截图、产物回传）之后。分成哪几类、怎么排队，没读实现。
- **配对改 PAKE**（`group_pake.rs`、`pace.rs`、`pairing.rs`）：配对走口令认证的密钥交换。我们的配对码流程在 [MULTI-NODE.md](../MULTI-NODE.md) §10 的 A3（开端口、TLS、短码比对），那时再评估要不要换成 PAKE。是否还显示短码、具体是哪种构造，没读实现。

## 5. 没核实的

- 没有安装、没有运行。除 `v1_golden.rs` 全文和 `lib.rs` 的开头，其余按文件树、README 和提交信息判断，没有读实现。
- 版本号 0.3.6 取自最新提交信息，没有核对 Releases 页。
- `v1_negotiation.rs`、`qos.rs`、`group_pake.rs`、`pace.rs` 的内容都没读，上面只写了它们的职责。
- 「分区后合并成员名单」是从文件名和 README 的 P2P 定位推出来的，没有读合并算法的实现。
