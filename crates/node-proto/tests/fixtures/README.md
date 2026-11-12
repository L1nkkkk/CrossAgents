# 报文样本（wire fixtures）

这里存的是**真实报文**：每一种跨线消息录一份，按协议主版本分目录。它们的作用是让
「在 core 或契约层改一个跨线字段的名字」从「只有另一台机器的节点连上来才发现」变成
「CI 当场变红」。

目录按协议主版本分（`docs/tasks/B1-NODE.md` §4「报文留档」）：

```
tests/fixtures/
  v1/                协议 1.x 的报文，每个消息/op/事件一份，一份一个文件
    register.json
    call_open.json
    call_prompt.json
    event_output.json
    ...
```

## 约定

1. **每次发版录一份当时的报文**，放进当前协议主版本的目录（`v<协议主版本>/`）。
   协议版本是 `PROTOCOL_V1` 的 `major`（当前 `1`）；主版本没变就录进 `v1/`，变了就
   新建 `v2/`，`v1/` 原样留着。
2. **老目录只增不改。** 已经录下的文件不修改、不删除、不「顺手更新一下」——改了就等于
   悄悄改了历史，跨版本解码测试就再也证明不了旧报文还能被新代码读。要让旧报文跟着新
   格式走，那是协议变更，该新开主版本目录。
3. 文件名就是消息/op 的名字（`register.json`、`call_set_model.json`、`event_usage.json`），
   与 `crates/node-proto/src/v1/` 的 DTO 一一对应。
4. 文件内容是**一行** JSON（和线上一样，以 `\n` 结尾），由代码编码得到，**不手写**。
5. 样本只放稳定的、与机器无关的值：固定的 UUID、固定时间戳、正斜杠路径。不要放本机
   用户名、随机 id 或当前时间，否则重录会无谓地产生 diff。

## 重新录制

样本由 `crates/node-proto/tests/fixtures_v1.rs` 里的 `cases()` 构造域对象、经 `Wire*`
DTO 编码得到。重录（会覆盖 `v1/` 下的文件）：

```text
cargo test -p crossagents-node-proto --test fixtures_v1 -- --ignored record_fixtures
```

只有**新增样本**或**协议主版本升级**时才重录。平时不要跑：测试会拿当前编码和磁盘上的
样本逐行比对，样本被无意改动就会红，这正是它要挡的。

## 测试怎么挡

`crates/node-proto/tests/fixtures_v1.rs` 里这几条：

- `fixtures_match_current_encoding` —— 域类型 → DTO → 行，和磁盘上的样本逐字节比对。
  跨线字段改名、变类型、被删掉，这里立刻红。
- `fixtures_decode_to_the_frames_that_were_recorded` —— 用当前代码解每一份样本，断言解
  出来的整帧就是录它时的那个 DTO。
- `frames_decode_to_their_recorded_meaning` / `events_decode_to_their_recorded_kind_and_content`
  —— 再显式断言关键字段：`seq`、`session`、`Call` 的 `id` 与 `op` 参数、事件的
  `kind` 和内容。不只是「没报错」。
- `every_case_has_exactly_one_sample_and_nothing_else` —— 目录里每个样本都对应一个用例，
  没有多余的旧文件；新增用例忘了录样本也会红。

## 新增一份样本

1. 在 `cases()` 里加一个 `Case { name: "...", frame: ... }`（用域对象构造，别手写 JSON）。
2. 跑上面的重录命令。
3. 跑 `cargo test -p crossagents-node-proto` 确认全绿。
4. 把新文件和代码一起提交。

## 跨版本

`v1/` 的样本会一直留着。等协议主版本升到 2，`v2/` 录新格式，测试同时读两个目录：
`v1/` 的样本必须还能被新代码解出同样的语义（宽容解码），`v2/` 的样本必须和新编码
逐字节一致。那时再在本文件里补一段说明。
