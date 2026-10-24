# 应用协议版本1

每个TCP帧为u32小端正文长度 + 正文。零长度或超出本地frame上限拒绝。正文使用codec/wire版本u16=1，再跟u8消息标签和以下字段。TCP分段不等于应用分片，正文未收齐不解码。

| 客户端标签 | 字段顺序 |
|---|---|
| 0 Hello | protocol_version:u16=1、Compatibility |
| 1 Request | RequestSeq:u64、泛型输入I |
| 2 Subscribe | SectionPos的有界Vec |
| 3 Unsubscribe | SubscriptionId:u128 |
| 4 Close | 无 |

| 服务端标签 | 字段顺序 |
|---|---|
| 0 Welcome | Binding、SessionId:u128 |
| 1 Admission | RequestSeq、接纳状态 |
| 2 Result | RequestSeq、泛型结果O |
| 3 Seed | ReplicaSeed<S> |
| 4 Batch | ReplicaBatch<C> |
| 5 Rejected | RejectCode |

接纳状态为0 + target_tick:u64、1表示Duplicate、2 + RejectCode表示Rejected。元数据和容器按codec显式基础编码，应用层本版绑定I=Call、O=Reply、S=ReplicaSnapshot、C=ReplicaChanges，见../codec/EXECUTION.md。

客户端不能提供可信actor、管理员标记或世界绑定。连接的世界、运行代数、session与actor来自宿主已认证记录。Hello必须先成功，未知版本、内容/规则摘要不兼容或非法消息拒绝。新请求序号从1连续增长，重发沿用原值，旧序号由权威窗口判断Duplicate、DuplicateMismatch或SequenceExpired。

Queued不表示动作成功。Result、Seed、Batch都要求其观察/提交位置不超过DurableSeq。只有A7授权源产生复制载荷，不发送完整存档。ReplicaSeq按订阅连续，CommitSeq可跳过隐藏变化。失效控制清空未发队列并关闭连接，客户端丢弃该连接镜像后重建。

首版只支持完整有界消息；没有压缩、任意大分片、登录协议或TLS消息。传输上限、速率、会话、订阅和输出队列策略在本地配置，未知字段或尾随字节不被忽略。
