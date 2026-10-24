# Execution载荷版本1

这是B6执行模型和B7应用载荷的显式格式登记。实现真源为src/wire.rs的Wire实现及fields列表；不使用Rust内存布局。外层日志包裹仍按FORMAT.md的版本1，原WorldCodec的WORLD.md不改变。

所有整数小端。每个独立wire载荷以u16版本1开头，随后写入类型约定的字段，不含Rust类型名。Option为u8标签0/1，非法标签拒绝。Vec为u32项数后逐项编码，Map为u32项数后按ContentKey严格升序写入键/值。数量在分配前检查最小输入宽度、条目与拥有的字节预算。序列次序有语义，不自动排序输入。

ContentKey、ComponentValue与嵌套组件字段沿用WORLD.md。位置和位移为三个u64位模式；元数据布局沿用FORMAT.md。i32优先级以u32二进制补码表示。RejectCode标签按src/wire.rs的0至9固定映射；ReplyStatus为0已提交、1只读、2拒绝后跟RejectCode。

| 类型 | 按顺序的字段 |
|---|---|
| Call | action、version:u32、target、parameters |
| target | 0 + BlockPos，或1 + EntityId |
| Reply | status、fields、at:CommitCursor |
| Receipt | id:RequestId、input:Call、reply |
| Window | epoch、session、high:u64、receipts |
| EventDraft | tick、rule、payload:ComponentFields |
| TimedEvent | id:EventId、event:EventDraft |
| StableCell | pos、content（0空，1方块键与u16值列表） |
| StableMove | id、previous:[u64;3]、delta:[u64;3]、half:[u64;3] |
| StableSpawn | position:[u64;3]、components:Map<ContentKey,ComponentFields> |
| StoredTransaction | id、source、phase:u16、priority:i32、task_key:u64、local:u32、reads、writes、moves、components、spawns、despawns |
| Accepted | id:RequestId、input:Call |
| FrozenTask | transaction:Option、request:Option、consume:Option<EventId>、schedule、cancel、status、output |
| PhaseWork | tasks、next:u32 |
| SystemSpec | id、phase:u16、rule_priority:i32、before、after |
| RuntimeState | rules、next_event:u128、events、windows、work:Option<PhaseWork> |
| ExecutionSnapshot | world:世界布局3的快照正文、runtime |
| ExecutionChanges | world:世界布局3的变更正文、runtime、outcome:Option<ReplyStatus>、cancelled:Option<Accepted> |

组件替换ComponentEdit和销毁EntityState沿用世界领域字段顺序，字段值保留嵌套List/Record。运行句柄和进程状态编号不进入StoredTransaction，块前置条件也使用稳定内容。

WorldSnapshot包裹先写SnapshotMeta，再写ExecutionSnapshot；CommitRecord先写CommitMeta，再写ExecutionChanges。包裹与world子体元数据必须一致。世界子体没有再次添加世界版本前缀，执行版本1固定解释为布局3。

授权复制使用相同基础词汇，ReplicaSeed按binding、subscription、baseline、state，ReplicaBatch按binding、subscription、seq、through、changes。ReplicaSnapshot和ReplicaChanges字段顺序按A7登记；Dense在此使用带计数的符号列表，因此网络编码条目预算可能比A7内存预检更严格。客户端必须通过state表解析符号，不能直接使用远端运行编号。

decode要求消耗完整输入，拒绝未知版本、标签、超额集合、非法内容键和尾随字节。StorageCodec执行包裹元数据核对，ExecutionRecovery执行规则、阶段、事件、结果和世界领域校验。不能把编码成功当作授权或持久确认。
