# 离线归档格式

归档是显式交换载荷，与现有世界检查点、执行记录及日志布局独立。wire外层版本保持1，所有整数、容器和字符串沿用wire原语及预算检查。

| 载荷 | 内层顺序 |
|---|---|
| SnapshotArchive | 八字节ASCII魔数TORSEAR1、origin兼容信息、ArchiveManifest、世界格式3的Snapshot |
| ExecutionArchive | 八字节ASCII魔数TORSEEX1、SnapshotArchive、RuntimeState |
| ArchiveManifest | blocks映射、components映射 |
| blocks条目 | BlockStateId键、definition_version、模式ContentKey、schema_version |
| components条目 | 模式ContentKey键、所有者ContentKey、版本 |

映射按键严格递增。方块版本按归档中的状态ID记录，同一方块的不同状态可在迁移途中保留不同版本。组件版本按模式记录。origin记录最初来源，当前候选兼容信息使用Snapshot.meta；两者在迁移后可以不同。

归档自身没有新增文件锁、校验和或原子替换语义。应用负责文件发布；正常文件世界继续使用现有存储层。解码成功只证明载荷结构合法，不能代替内容可用性、版本和领域恢复验证。

未知ContentKey、有限状态值与字段可以保留在归档中。激活前调用validate_for_restore并经过正常Recovery或ExecutionRecovery.finish。原执行历史不能换兼容信息后继续追加；文件导入须显式fork_for_import到新的世界身份和空目录。
