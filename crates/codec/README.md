# 存档编码

包名：`torse-codec`。已实现编码原语、事务性回滚、预算检查、B1记录包裹与公共元数据编码，以及B5世界载荷编码。

| 文件 | 职责与状态 |
|---|---|
| [frame.rs](src/frame.rs) | B0长度前缀原语，不具备持久记录完整性检查 |
| [record.rs](src/record.rs) | B1包裹：magic、版本、类型、长度、头与载荷CRC32C、结束标记 |
| [metadata.rs](src/metadata.rs) | 五类公共元数据的定长小端编码，不依赖Rust内存布局 |
| [world.rs](src/world.rs) | `WorldCodec`：世界格式版本、元数据一致性与`StorageCodec`实现 |
| [snapshot.rs](src/snapshot.rs) | 快照主体：范围、状态表、分区与实体 |
| [journal.rs](src/journal.rs) | 变更集主体：块编辑、实体移动与组件编辑 |
| [value.rs](src/value.rs) | 两类载荷共用的内容键、计数、组件值与字段表 |
| [registry.rs](src/registry.rs) | 未知载荷保留，尚未实现 |

包裹布局、校验范围与固定字节样例见[格式说明](FORMAT.md)，世界载荷布局见[世界载荷格式](WORLD.md)。校验和依赖固定为`crc32c = "=0.6.8"`，锁文件保存解析结果。包裹版本与世界格式版本独立维护。

`WorldCodec`对任意快照与变更类型分别为`torse_world::Snapshot`与`ChangeSet`的权威模型实现`StorageCodec`，模型的输入、输出与复制类型由宿主定义。限制全部来自本地配置，`max_depth`不得小于`MIN_WORLD_DEPTH`。

世界格式版本2保存实体分配器水位，拒绝版本1临时存档。版本2仍是M1临时格式，未建立兼容承诺。未知内容在恢复时报错，原始载荷保留尚未实现。网络格式由protocol维护。
