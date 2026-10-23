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

M2已开始，当前覆盖模拟执行模型与实体存储：

- 固定步长时钟：按Tick推进，超预算只记录并报告，不跳Tick也不改步长。
- 阶段与系统排序：六个阶段按固定顺序执行，阶段内按依赖做拓扑排序，并列时按优先级与身份稳定排序，环与跨阶段依赖拒绝。
- 类型化事务：读取前置条件按格记录，写入成组校验后一次应用，重复目标与空事务拒绝。
- 提交拆分：准备阶段完成读集校验与写入校验，返回带`CommitMeta`的准备记录，世界不变；发布阶段才应用改动并分配提交序号。准备记录被丢弃时序号与世界都不变。
- 单协调器提交：按阶段、优先级、稳定任务键与局部序号仲裁，成功才分配提交序号，冲突不消耗序号。
- Tick驱动：系统只读上下文，事务在阶段末统一提交，阶段事务数设上限，系统不能跨阶段投放事务，每个阶段与每个Tick都会更新恢复位置。
- 实体存储：持久身份与运行时句柄分离，槽位复用会推进代数，空间索引随移动同步维护。
- 实体移动事务：准备阶段校验身份与位置，按碰撞结果生成记录，发布时成组更新位置和空间索引；可在发布前生成交换载荷，恢复直接重放已解析位置。
- 有界组件：组件值按模式校验字段、类型、必填项、字符串长度、列表长度与嵌套深度。
- 组件事务：检查模式所有者和精确前置字段，准备后可预留日志，发布时与块编辑、实体移动成组应用，恢复重放支持组件替换与删除。
- 动作与库存：受限只读视图、统一授权、可选放置破坏动作、库存查询与守恒转移；两个机器争用同一原料时只有一个成功。
- 权威装配与恢复：`WorldAuthority`统一持有存储、实体、注册表与协调器；`Recovery`在快照与日志尾都通过后才产出世界，续号与在线世界一致。
- 提交仲裁、Tick与交换场景：同格争用只有一个成功，准备项被拒时世界不变，实体跨界移动后索引正确，快照往返与变更重放一致。
- 世界载荷编码：快照与变更集按显式字段写入，读写两侧检查规范顺序、重复项、嵌套与预算；经文件日志与检查点保存后，可在新权威中恢复并续号。格式为M1临时版本，不承诺兼容。
- 服务端真实世界装配：WorldHost接入B4协调器，可信本地事务经日志预留、发布与刷盘后返回持久位置；支持有界完整快照、文件恢复和关闭。网络请求、持久事件及去重结果仍待B6后续。

M1尚未完成：GPU上传与可视客户端（窗口、相机、渲染管线）、可玩角色的输入装配。放置破坏已提供统一动作路径，实际UI与网络适配仍待装配。B6已接入可信本地事务，完整执行循环与请求恢复仍待后续。

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
cargo run -p torse-tools -- commit-scenario
cargo run -p torse-tools -- tick-scenario
cargo run -p torse-tools -- exchange-scenario
cargo run -p torse-tools -- recovery-scenario
cargo run -p torse-tools -- movement-scenario
cargo run -p torse-tools -- persistence-scenario
cargo run -p torse-server --example durable_world -- .work/local-world
```

重复运行durable_world会从同一目录恢复并切换一个方块，提交序号和运行代数连续递增。真实世界持久化的故障边界与范围见[验证记录](benchmarks/M3-local-world.md)。

技术实验需要显式开启组件：

```sh
cargo run -p torse-tools --features experiments -- ecs-probe
cargo run -p torse-tools --features experiments -- wasm-probe
cargo run -p torse-tools --features experiments -- gpu-probe
```

## 协作边界

A负责权威世界、模拟、生命周期和原子提交。B负责世界与磁盘、连接之间的数据交换，包括codec、storage、protocol、net和server装配。公共契约变更需要同步修改设计、类型、测试和受影响模块，不能在单个crate内悄悄改变语义。

正式需求与架构材料在独立的设计材料目录中维护。M0实验结果与M1场景记录都是功能证据，不是性能承诺。
