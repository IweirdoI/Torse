# 数据编码

包名：`torse-codec`。提供有预算限制的显式字段编码、编码失败回滚、记录完整性校验，以及世界、执行状态和授权网络载荷的编码。持久格式不依赖Rust内存布局。

| 文件 | 职责 |
|---|---|
| [frame.rs](src/frame.rs) | 长度前缀原语，不承担持久记录完整性检查 |
| [record.rs](src/record.rs) | 记录包裹、版本、类型、长度、CRC32C与结束标记 |
| [metadata.rs](src/metadata.rs) | 公共元数据的定长小端编码 |
| [world.rs](src/world.rs) | 世界格式版本检查与`WorldCodec`实现 |
| [snapshot.rs](src/snapshot.rs) | 快照中的范围、状态表、分区与实体 |
| [journal.rs](src/journal.rs) | 块编辑、实体移动、组件编辑与生命周期变更 |
| [lifecycle.rs](src/lifecycle.rs) | 实体生成、销毁与分配器前置条件 |
| [value.rs](src/value.rs) | 内容键、计数、组件值与字段表 |
| [wire.rs](src/wire.rs) | 持久任务、事件、请求结果、授权复制与协议载荷 |
| [registry.rs](src/registry.rs) | 未知内容原始载荷保留的预留入口，尚未实现 |

记录包裹见[FORMAT.md](FORMAT.md)，世界快照与变更见[WORLD.md](WORLD.md)，执行状态与网络载荷见[EXECUTION.md](EXECUTION.md)。这些格式分别维护版本，不能只修改其中一个版本号来代替兼容处理。

`WorldCodec`为使用`torse_world::Snapshot`与`ChangeSet`的权威模型实现`StorageCodec`。新写入采用世界格式3，读取继续接受版本2，拒绝缺少实体分配历史的版本1。执行宿主使用独立的`wire::ExecutionCodec`，两种世界目录不会自动迁移。

领域载荷的条目数、字节与嵌套深度均受本地配置限制，`max_depth`不得小于`MIN_WORLD_DEPTH`。未知内容会在世界恢复时被拒绝，尚不支持保留其原始载荷后继续运行。当前格式仍在演进，不承诺未经说明的向前或向后兼容。
