# 存档编码

包名：`torse-codec`。当前各模块仅预留文件，尚无编码格式或公共接口。

| 文件 | 预留职责 |
|---|---|
| [frame.rs](src/frame.rs) | 存储记录的版本、长度与完整性检查 |
| [journal.rs](src/journal.rs) | 完整提交记录的编码与解码 |
| [registry.rs](src/registry.rs) | 稳定内容映射与未知载荷保留 |
| [snapshot.rs](src/snapshot.rs) | 一致世界快照的编码与解码 |

根据用户指定的公共契约实现显式编码，不直接转储Rust内存布局。此crate负责存档格式，网络格式由protocol维护。
