# 存档编码

包名：`torse-codec`。已实现编码原语、事务性回滚、预算检查，以及B1记录包裹与公共元数据编码。A的具体领域载荷尚未登记。

| 文件 | 职责与状态 |
|---|---|
| [frame.rs](src/frame.rs) | B0长度前缀原语，不具备持久记录完整性检查 |
| [record.rs](src/record.rs) | B1包裹：magic、版本、类型、长度、头与载荷CRC32C、结束标记 |
| [metadata.rs](src/metadata.rs) | 五类公共元数据的定长小端编码，不依赖Rust内存布局 |
| [journal.rs](src/journal.rs) | 领域提交载荷的具体编码待B5实现 |
| [registry.rs](src/registry.rs) | 稳定内容映射与未知载荷保留 |
| [snapshot.rs](src/snapshot.rs) | 领域快照载荷的具体编码待B5实现 |

包裹布局、校验范围与固定字节样例见[格式说明](FORMAT.md)。校验和依赖固定为`crc32c = "=0.6.8"`，锁文件保存解析结果。包裹版本与领域模式版本独立维护。

`StorageCodec`的具体实现负责元数据与领域字段、未知载荷往返、解码完整消费和分配预算。B1只提供公共编码函数，未定义真实世界存档的兼容承诺。网络格式由protocol维护。
