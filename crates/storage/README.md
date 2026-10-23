# 持久化

包名：`torse-storage`。当前各模块仅预留文件，尚无磁盘读写功能。

| 文件 | 预留职责 |
|---|---|
| [journal.rs](src/journal.rs) | 有界日志追加、刷盘与持久位置反馈 |
| [checkpoint.rs](src/checkpoint.rs) | 检查点写入、发布与旧数据保留 |
| [recovery.rs](src/recovery.rs) | 恢复扫描与完整恢复数据交付 |

A提供一致快照与完整提交记录，B保存并反馈持久化进度。恢复数据通过A指定的入口接纳。容量预留、故障反馈、日志连续性和检查点截止位置的语义待公共契约确定。
