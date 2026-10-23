# B部分目标规划与实现情况

版本：0.1。日期：2026-10-23。维护方：B。

本文记录B部分的阶段目标、当前实现、与A的接入差异和待确认事项，供A与B对照进度。接口语义以TorseDesign中的`contracts/01-authority-io.md`与`contracts/authority_io.rs`为准，本文只记录实施安排和现状。

## 一、职责范围

| 模块 | B负责的内容 |
|---|---|
| crates/codec | `StorageCodec`、显式字段编码、记录帧与预算 |
| crates/storage | `CommitJournal`、`CheckpointStore`、恢复扫描与故障注入 |
| crates/protocol | 握手、输入、结果与复制消息的网络格式 |
| crates/net | 会话、请求序号、输入校验、传输队列与复制流程 |
| apps/server | 提交顺序、持久门槛、启动恢复与关闭流程的装配 |

A负责权威世界、模拟、生命周期、原子提交、恢复构建器和授权投影。torse-types由双方共用。B取得A的数据后不修改世界，恢复数据和外部请求通过A提供的入口接纳。

## 二、实现情况

### 2.1 B部分

| 模块 | 状态 | 内容 |
|---|---|---|
| torse-types | 已提交，与A共用 | 世界身份、提交与持久序号、会话与请求身份、兼容、恢复位置、预算、拒绝与故障码、载荷封装 |
| crates/codec | 基础已实现 | `StorageCodec`接口、`Encoder`未完成即回滚、`Decoder`有界读取、`BudgetGuard`、`CodecLimits`、长度前缀帧与字段 |
| codec场景测试 | 已实现 | `crates/codec/tests`下的无界面运行器，覆盖帧往返与预算回滚 |
| crates/storage | 未开始 | 仅有预留文件 |
| crates/protocol | 未开始 | 仅有预留文件 |
| crates/net | 未开始 | 仅有预留文件 |
| apps/server | 未开始 | 空入口 |

codec尚无具体领域载荷编码，也没有正式文件格式。记录帧还没有magic、版本和校验和。

### 2.2 A部分中与B相关的现状

以下根据2026-10-23的代码整理，以A的README为准。

| 项目 | 现状 | 与B的关系 |
|---|---|---|
| `CommitCoordinator` | 单协调器，校验读集后直接写入`SectionStore`，成功才分配`CommitSeq` | 提交记录的来源，见第三节差异 |
| `Committed` | 包含`cursor`、`edits`与`touched` | 将来`Changes`的雏形 |
| `Simulation` | 六阶段串行Tick驱动，每阶段末提交 | `ResumePoint`的阶段与步骤来源 |
| `Phase::index` | 0到5，类型为`usize` | 需要对应`ResumePoint.next_phase: u16` |
| torse-types新增 | 坐标、内容键、实体身份、`Revision`、`EventId`、`TransactionId`、`TransactionOutcome`、复制声明 | B编码时直接使用 |
| 快照与恢复入口 | 未实现 | `Authority::capture_snapshot`、`begin_restore`尚无实现 |

## 三、与契约的接入差异

1. **提交顺序**：契约3.3节要求`prepare_next -> try_stage -> publish -> accept -> flush_through`，准备阶段不改变权威状态。目前`CommitCoordinator::commit`在分配序号前已经写入`SectionStore`，日志无法在发布前拒绝。日志队列满、记录超限这两个场景（契约7.4节）因此无法满足。需要A把提交拆成准备和发布两步，或者另行约定B的介入位置。
2. **提交元数据**：`Committed`缺少契约`CommitMeta`中的`previous`、`compatibility`和`resume_at`。
3. **阶段编号**：`ResumePoint.next_phase`与`Phase::index`的对应关系、Tick内步骤的含义还没有定义。
4. **两种结果类型**：types中同时有契约的`Outcome<O>`（请求结果）和A新增的`TransactionOutcome<T>`（事务结果）。我理解两者用途不同，需要A确认。
5. **类型归属**：契约7.2节规定types只放身份、位置、摘要与预算。`AuthorityData`、`WorldSnapshot`、`CommitRecord`等载荷封装目前放在types里，`Authority`、`RecoveryTarget`、`ReplicaSource`、`CommitJournal`和`CheckpointStore`这几个trait还没有落到代码。

## 四、阶段规划

阶段编号B0到B7是B内部的顺序，对应关系见"对应里程碑"一列。B1到B4不依赖A的具体载荷，先用测试用的假模型实现`AuthorityData`。

| 阶段 | 目标 | 依赖 | 对应里程碑 | 状态 |
|---|---|---|---|---|
| B0 | 公共类型、codec原语、场景运行器 | 契约草案 | M0 | 完成 |
| B1 | 记录帧格式与元数据编码 | 校验和方案 | M0收尾 | 待开始 |
| B2 | 内存版与文件版`CommitJournal`、恢复扫描、故障注入 | B1 | M3前置 | 待开始 |
| B3 | `CheckpointStore`与manifest发布 | B1、B2 | M3前置 | 待开始 |
| B4 | server协调器骨架与世界目录单写者锁 | B2、B3，第三节第1项 | M3前置 | 待开始 |
| B5 | A载荷的显式编码与M1临时快照 | A登记`Snapshot`与`Changes` | M1 | 等待A |
| B6 | 接入A的提交与恢复，通过契约7.4节的持久化场景 | B4、B5，A的恢复构建器 | M3 | 等待A |
| B7 | 协议、内存传输、会话、去重窗口与复制 | A的`ReplicaSource` | M5 | 等待A |

### 4.1 各阶段要点

- **B1**

  帧包含magic、包裹版本、长度和校验和，能区分尾部未写完的记录和中间损坏。`CommitMeta`、`SnapshotMeta`、`Binding`、`Compatibility`和`ResumePoint`按字段显式编码，附往返测试与固定字节样例。

- **B2**

  `try_stage`完成编码、长度检查和容量预留，返回不可复制的`Staged`令牌，令牌绑定日志实例与`Binding`。`accept`只接收已预留的令牌。`flush_through`返回`Durability`，写入失败后进入停止状态。恢复扫描忽略最后一条未完成记录，对跳号和中间损坏报错。故障注入覆盖截断、跳号、位翻转和写入失败。

- **B3**

  先写入并校验新一代快照，再发布manifest。新检查点发布完成前，旧检查点和其后的日志保持可恢复。首次创建世界时先保存位置0的检查点。

- **B4**

  用假`Authority`驱动契约3.3节的执行顺序，覆盖启动恢复、停止接纳和关闭前检查点。M1临时快照（B5）不必等B4。

- **B5**

  M1尚未完成的"临时快照保存读取"属于B5。格式标为临时，M3前迁移或弃用。

- **B7**

  先用内存传输验证消息语义，再接入真实传输库。

## 五、需要A提供或确认

| 事项 | 用于 | 对应 |
|---|---|---|
| 提交拆分为准备与发布，或约定日志介入点 | B4、B6 | 第三节第1项 |
| `CommitMeta`字段的来源 | B1、B6 | 第三节第2项 |
| `ResumePoint`阶段与步骤的定义 | B1、B6 | 第三节第3项 |
| `Snapshot`与`Changes`字段登记：类型、范围、规范化顺序、模式版本、未知字段策略与样例 | B5 | 契约2.3节 |
| 载荷封装与几个trait的归属crate | B2起 | 第三节第5项 |
| `RequestSeq`去重窗口的配置方 | B7 | 契约5.1节 |

## 六、需要用户决定

| 事项 | 选项 |
|---|---|
| 记录校验和 | 自行实现CRC32C，或引入第三方依赖 |
| B方开发日志的存放位置 | 目前在工作区`.work`下，按规范交付前应清理 |

## 七、更新方式

B每完成一个阶段或发现新的接入差异时更新第二到第五节，并修改文首日期。A的现状以A的README和代码为准，本文只摘录与B相关的部分。差异解决后从第三节移除，并在对应阶段注明。
