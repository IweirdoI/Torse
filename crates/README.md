# 框架库

2026-10-24补充：B6已接入真实世界本地持久事务与阶段恢复；A7已交付`sim::replica::ReplicaSource`及授权复制载荷，B6持久执行与B7协议、会话、内存/TCP传输已接线。客户端已有离屏单帧、交互窗口连续渲染与联网镜像：飞行与行走输入、分区流式装载、窗口内放置破坏、A4行走事务和TCP连接消费A7投影已接通。下面的模块划分继续适用。

下表对应架构总览的模块职责。M0已实现公共类型、内容契约、内容与模块编译、动作声明、codec原语和三组技术探针。M1补上块状态编号、Section存储与生命周期、确定性生成、AABB碰撞和CPU网格，客户端已接入离屏绘制与交互窗口。M2已有固定步长时钟、阶段排序、类型化事务与单协调器提交、实体和基础动作/库存。B1到B5补上有界日志、检查点、恢复扫描与世界载荷编码，B6已接入真实世界本地持久事务；ExecutionHost已提供持久阶段循环，NetworkHub提供授权协议和连接。

| 目录 | 职责 |
|---|---|
| [types](types/src/lib.rs) | 身份、坐标、时间与通用错误等公共基础类型 |
| [runtime](runtime/src/lib.rs) | CPU任务、I/O、执行预算与ECS探针 |
| [content](content/src/lib.rs) | 内容契约、编译、摘要与M0夹具 |
| [world](world/src/lib.rs) | 块状态注册、Section存储、分区生命周期与票据、实体存储与空间索引、有界组件、交换载荷 |
| [sim](sim/src/lib.rs) | 固定步长时钟、阶段与系统排序、事务、Tick驱动、变更载荷、权威装配与恢复、有界授权复制源 |
| [actions](actions/src/lib.rs) | 动作声明、命令冲突与注册检查 |
| [modules](modules/src/lib.rs) | 模块依赖、服务与注册解析 |
| [world_services](world_services/src/lib.rs) | 确定性地形生成与AABB碰撞；光照与寻路待实现 |
| [mod_api](mod_api/src/lib.rs) | SDK契约、可传输类型与接口描述 |
| [mod_host](mod_host/src/lib.rs) | Mod宿主骨架与Wasmtime探针 |
| [codec](codec/README.md) | 存档快照、记录与内容映射的显式编码，B部分 |
| [storage](storage/README.md) | 日志、检查点、恢复与迁移，B部分 |
| [protocol](protocol/README.md) | 泛型消息版本1、兼容握手、输入/结果和授权复制表示 |
| [net](net/README.md) | 会话与速率、内存/非阻塞TCP、有界队列及原子客户端镜像 |
| [render](render/src/lib.rs) | CPU网格生成、遮挡剔除与网格缓存；wgpu探针仍为初始化骨架 |
| [assets](assets/src/lib.rs) | 资源定位、解码与格式适配 |
| [diagnostics](diagnostics/src/lib.rs) | 指标、跟踪与故障关联 |

crates/modules负责通用装配，根目录modules中的库负责具体可选玩法。codec细分总览中storage的编码职责。

依赖方向遵循架构总览，后续按实际接口添加依赖。world不得调用render或net，render不得直接修改权威world。提交记录通过公共契约交付，避免sim与storage互相依赖。


