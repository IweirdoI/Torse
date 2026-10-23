# 网络协议

包名：`torse-protocol`。当前各模块仅预留文件，尚无消息模式或协议版本。

| 文件 | 预留职责 |
|---|---|
| [frame.rs](src/frame.rs) | 网络消息边界、长度检查与解码 |
| [handshake.rs](src/handshake.rs) | 协议与锁定内容兼容协商消息 |
| [input.rs](src/input.rs) | 输入请求与关联结果的消息编码 |
| [replication.rs](src/replication.rs) | 已授权初始快照与原子增量批次的消息编码 |

此crate定义网络表示，连接和复制流程由net组织。消息模式、版本、稳定身份和序号规则按用户指定的契约实现，不将存档原始记录直接作为客户端消息。
