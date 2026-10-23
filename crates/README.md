# 框架库

下表对应架构总览的模块职责。M0已实现公共类型、内容契约、内容与模块编译、动作声明、codec原语和三组技术探针。M1补上块状态编号、Section存储与生命周期、确定性生成、AABB碰撞和CPU网格。M2从模拟执行模型开始，已有固定步长时钟、阶段内系统排序、类型化事务与单协调器提交。B1、B2补上记录包裹、元数据编码、有界日志与恢复扫描；领域存档接入、检查点、协议和网络尚未完成。

| 目录 | 职责 |
|---|---|
| [types](types/src/lib.rs) | 身份、坐标、时间与通用错误等公共基础类型 |
| [runtime](runtime/src/lib.rs) | CPU任务、I/O、执行预算与ECS探针 |
| [content](content/src/lib.rs) | 内容契约、编译、摘要与M0夹具 |
| [world](world/src/lib.rs) | 块状态注册、Section存储、分区生命周期与票据、实体存储与空间索引、有界组件 |
| [sim](sim/src/lib.rs) | 固定步长时钟、阶段与系统排序、事务前置条件、准备与发布两步提交、Tick驱动 |
| [actions](actions/src/lib.rs) | 动作声明、命令冲突与注册检查 |
| [modules](modules/src/lib.rs) | 模块依赖、服务与注册解析 |
| [world_services](world_services/src/lib.rs) | 确定性地形生成与AABB碰撞；光照与寻路待实现 |
| [mod_api](mod_api/src/lib.rs) | SDK契约、可传输类型与接口描述 |
| [mod_host](mod_host/src/lib.rs) | Mod宿主骨架与Wasmtime探针 |
| [codec](codec/README.md) | 存档快照、记录与内容映射的显式编码，B部分 |
| [storage](storage/README.md) | 日志、检查点、恢复与迁移，B部分 |
| [protocol](protocol/README.md) | 网络消息、协商与编码约束，B部分 |
| [net](net/README.md) | 会话、输入、复制与背压，B部分 |
| [render](render/src/lib.rs) | CPU网格生成、遮挡剔除与网格缓存；wgpu探针仍为初始化骨架 |
| [assets](assets/src/lib.rs) | 资源定位、解码与格式适配 |
| [diagnostics](diagnostics/src/lib.rs) | 指标、跟踪与故障关联 |

crates/modules负责通用装配，根目录modules中的库负责具体可选玩法。codec细分总览中storage的编码职责。

依赖方向遵循架构总览，后续按实际接口添加依赖。world不得调用render或net，render不得直接修改权威world。提交记录通过公共契约交付，避免sim与storage互相依赖。


