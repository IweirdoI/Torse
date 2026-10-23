# Torse

Torse使用Rust构建开放方块世界的通用内核与开发框架，并以基础游戏验证。

## 当前状态

M0公共契约与实验框架已完成，M1推进到无界面的可编辑世界层。

M0交付：

- 有符号块坐标、Section转换、内容命名空间和运行时句柄失效。
- 提交序号、持久序号、对象与事件身份、事务结果和复制声明。
- `BlockDefinition`、`StateSchema`、`CapabilityDescriptor`和`ModuleDescriptor`四项内容契约。
- 最小内容编译器和模块解析器，包含顺序无关摘要及可定位错误。
- 石头、玻璃半砖和植物内容夹具。
- 统一动作声明、命令冲突检查和模块服务注册校验。
- codec预算、事务性编码回滚和场景指标框架。
- 可复跑的ECS、Wasmtime和wgpu/winit技术实验。

M1已实现：

- 块状态的紧凑编号，含空占用哨兵、有限状态展开与默认状态。
- Section存储：统一值与稠密两种表示，负坐标按欧几里得除法分区。
- 分区生命周期：请求、装载、就绪、活动四个阶段与四类票据配额，未装载区域不返回空气。
- 确定性地形生成：平坦地形与种子丘陵地形，生成结果与调度顺序无关。
- AABB碰撞扫掠：轴分离、子步防穿透、未装载区域阻挡移动。
- CPU网格生成：按遮挡掩码剔除面并保留未加载边界面，网格带依赖修订戳，旧结果不得覆盖新状态。
- 无界面M1场景：生成、行走、挖补、网格化与卸载的端到端验收。

M1尚未完成：GPU上传与可视客户端（窗口、相机、渲染管线）、实体的存储与移动、放置破坏走统一动作服务、临时快照的保存读取。方块编辑当前通过`SectionStore::edit`直接作用于权威数据，动作接入属于M2。

## 目录

```text
apps/
    client/
    server/
    tools/
crates/
    types/
    runtime/
    content/
    world/
    sim/
    actions/
    modules/
    world_services/
    mod_api/
    mod_host/
    codec/
    storage/
    protocol/
    net/
    render/
    assets/
    diagnostics/
modules/
games/
examples/
tests/
benchmarks/
```

| 位置 | 职责 |
|---|---|
| [apps](apps/README.md) | 客户端、服务端与开发工具入口 |
| [crates](crates/README.md) | 框架库与B部分编码库 |
| [modules](modules/README.md) | 库存、配方与能量等可选标准玩法模块 |
| [games/base](games/base/README.md) | 基础游戏的模块、内容与规则组合 |
| [examples/mods](examples/mods/README.md) | 仅依赖公开SDK的Mod示例 |
| [tests](tests/README.md) | 跨模块场景、故障注入与测试数据 |
| [benchmarks](benchmarks/README.md) | 工作负载、性能测量与[M0实验记录](benchmarks/M0-experiments.md) |

## 检查

工作区固定使用Rust 1.93.1，依赖版本记录在`Cargo.lock`。

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo run -p torse-tools -- content-fixture
cargo run -p torse-tools -- m1-scenario
```

技术实验需要显式开启组件：

```sh
cargo run -p torse-tools --features experiments -- ecs-probe
cargo run -p torse-tools --features experiments -- wasm-probe
cargo run -p torse-tools --features experiments -- gpu-probe
```

## 协作边界

A负责权威世界、模拟、生命周期和原子提交。B负责世界与磁盘、连接之间的数据交换，包括codec、storage、protocol、net和server装配。公共契约变更需要同步修改设计、类型、测试和受影响模块，不能在单个crate内悄悄改变语义。

正式需求与架构材料在独立的设计材料目录中维护。M0实验结果与M1场景记录都是功能证据，不是性能承诺。
