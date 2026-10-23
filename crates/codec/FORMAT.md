# 记录包裹与元数据

版本：1。实施阶段：B1。该版本只规定外层包裹与公共元数据，具体世界载荷由A登记后在B5实现。

## 包裹布局

整数均为小端，`N`为载荷字节数，完整长度为`N + 28`。

| 偏移 | 字节数 | 内容 |
|---|---|---|
| 0 | 8 | magic，ASCII文本`TORSEFRM` |
| 8 | 2 | 包裹版本，当前为1 |
| 10 | 2 | 类型，1为提交，2为快照 |
| 12 | 4 | 载荷长度N，不含头尾 |
| 16 | 4 | 头CRC32C，覆盖偏移0到15 |
| 20 | N | 载荷，由具体`StorageCodec`显式编码 |
| 20 + N | 4 | 载荷CRC32C，只覆盖载荷 |
| 24 + N | 4 | 结束标记，ASCII文本`TEND` |

CRC32C调用固定版本的[crc32c库](https://docs.rs/crc32c/0.6.8/crc32c/)，参考校验值为`CRC32C("123456789") = 0xe3069283`，空载荷为0。CRC值本身也用小端保存。

头校验覆盖长度字段。完整头校验通过后才读取长度、核对本地上限并分配载荷缓冲。包裹版本和类型未知时拒绝读取，不猜测其他格式。

`read_record`借用首条完整记录并返回其总长度，后续字节属于下一条。合法前缀缺少完整头、载荷或尾部时返回`Incomplete`。完整记录的校验或结束标记错误始终报损坏，包括文件最后一条。恢复扫描只在物理EOF处容许不完整尾部，I/O错误直接失败。扫描不寻找后续magic来跳过损坏。

`write_record`在写入前检查全部长度、字节预算和一个记录条目预算，失败时不改变输出。B0的`frame::write_frame`与`read_frame`继续作为长度前缀原语，持久日志使用本节的记录包裹。

## 元数据布局

以下字段按表内顺序连续编码。摘要直接保存32字节，结构体填充字节不进入格式。

| 类型 | 字节数 | 字段顺序 |
|---|---|---|
| Compatibility | 68 | authority_schema:u32、rules_digest:32字节、content_digest:32字节 |
| Binding | 100 | world:u128、epoch:u128、compatibility |
| ResumePoint | 18 | tick:u64、next_phase:u16、next_step:u64 |
| SnapshotMeta | 110 | world:u128、seq:u64、compatibility、resume_at |
| CommitMeta | 118 | world:u128、seq:u64、previous:u64、compatibility、resume_at |

提交序号必须恰好为`previous + 1`，重复、跳号及溢出均拒绝。快照允许位置0。ResumePoint的阶段和步骤由A的规则图解释，编码层保存原值。持久元数据不保存运行代数；Binding供运行实例与令牌绑定使用。

元数据函数不计逻辑条目、不分配内存。外层codec负责记录条目与解码持有数据的预算。读取失败可以消费部分输入，调用方需丢弃本次完整解码结果。

## 固定样例

下列样例保存空格分隔的十六进制字节，由[格式测试](tests/record_format.rs)核对。测试同时覆盖往返、全部截断位置、预算不足和单比特翻转。

| 文件 | 样例 |
|---|---|
| [record.hex](tests/fixtures/record.hex) | 提交类型包裹，载荷为ASCII文本`abc` |
| [compatibility.hex](tests/fixtures/compatibility.hex) | 模式3，规则摘要填充0x11，内容摘要填充0x22 |
| [binding.hex](tests/fixtures/binding.hex) | 世界1，运行代数7，兼容信息同上 |
| [resume.hex](tests/fixtures/resume.hex) | Tick为4，阶段5，步骤6 |
| [snapshot.hex](tests/fixtures/snapshot.hex) | 世界1，截止位置2，兼容信息与恢复位置同上 |
| [commit.hex](tests/fixtures/commit.hex) | 世界1，位置2，前序1，兼容信息与恢复位置同上 |
