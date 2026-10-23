# 存档编码

包名：`torse-codec`。已定义`StorageCodec`接口、显式长度字段、预算守卫、事务性`Encoder`、有界`Decoder`和长度前缀帧。尚无具体领域载荷编码或正式文件格式。

| 文件 | 预留职责 |
|---|---|
| [frame.rs](src/frame.rs) | 单记录长度边界检查 |
| [journal.rs](src/journal.rs) | 完整提交记录的编码与解码 |
| [registry.rs](src/registry.rs) | 稳定内容映射与未知载荷保留 |
| [snapshot.rs](src/snapshot.rs) | 一致世界快照的编码与解码 |

根据用户指定的公共契约实现显式编码，不直接转储Rust内存布局。此crate负责存档格式，网络格式由protocol维护。
