# 连接与状态复制

包名：`torse-net`。当前各模块仅预留文件，尚无传输库、连接或复制实现。

| 文件 | 预留职责 |
|---|---|
| [transport.rs](src/transport.rs) | 内存与外部传输适配、有界队列 |
| [session.rs](src/session.rs) | 宿主管理的会话、请求序号与连接配额 |
| [ingress.rs](src/ingress.rs) | 输入检查与带宿主身份的世界请求交付 |
| [replication.rs](src/replication.rs) | 已授权快照、按序增量与重连流程 |

首版计划固定内容包，先使用内存传输验证，再接入真实连接。输入交给A裁定，状态复制遵守契约提供的可见性与授权结果。动作去重的最终结果需要与权威提交和恢复保持一致。
