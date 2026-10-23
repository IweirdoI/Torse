# 框架库

下表对应架构总览的模块职责。当前各库仅有骨架，公共类型与依赖接入待契约确定。

| 目录 | 职责 |
|---|---|
| [types](types/src/lib.rs) | 身份、坐标、时间与通用错误等公共基础类型 |
| [runtime](runtime/src/lib.rs) | CPU任务、I/O与执行预算 |
| [content](content/src/lib.rs) | 内容模式、注册、编译与不可变描述 |
| [world](world/src/lib.rs) | 地形、实体、索引、生命周期与受控访问 |
| [sim](sim/src/lib.rs) | Tick阶段、模拟任务、事务与事件 |
| [actions](actions/src/lib.rs) | 类型化动作、授权入口与结果 |
| [modules](modules/src/lib.rs) | 模块依赖解析与生命周期 |
| [world_services](world_services/src/lib.rs) | 碰撞、光照、生成与寻路等世界服务 |
| [mod_api](mod_api/src/lib.rs) | SDK契约、可传输类型与接口描述 |
| [mod_host](mod_host/src/lib.rs) | Mod加载、执行、权限与配额 |
| [codec](codec/README.md) | 存档快照、记录与内容映射的显式编码，B部分 |
| [storage](storage/README.md) | 日志、检查点、恢复与迁移，B部分 |
| [protocol](protocol/README.md) | 网络消息、协商与编码约束，B部分 |
| [net](net/README.md) | 会话、输入、复制与背压，B部分 |
| [render](render/src/lib.rs) | 可视状态、网格、材质与GPU资源 |
| [assets](assets/src/lib.rs) | 资源定位、解码与格式适配 |
| [diagnostics](diagnostics/src/lib.rs) | 指标、跟踪与故障关联 |

crates/modules负责通用装配，根目录modules中的库负责具体可选玩法。codec细分总览中storage的编码职责。

依赖方向遵循架构总览，后续按实际接口添加依赖。world不得调用render或net，render不得直接修改权威world。提交记录通过公共契约交付，避免sim与storage互相依赖。
