# Torse

Torse使用Rust构建开放方块世界的通用内核与开发框架，并以基础游戏验证。

本仓库由项目成员共享。目录按架构总览建立完整骨架，覆盖应用、框架、标准玩法模块、基础游戏、Mod示例、场景验证与性能测试。当前Rust模块仅包含职责说明，各应用入口暂为空，尚无游戏、存档或网络功能。

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
    inventory/
    crafting/
    energy/
games/
    base/
examples/
    mods/
tests/
    scenarios/
    fixtures/
benchmarks/
```

| 位置 | 职责 |
|---|---|
| [apps](apps/README.md) | 客户端、服务端与工具入口 |
| [crates](crates/README.md) | 框架库与B部分的编码库 |
| [modules](modules/README.md) | 库存、配方与能量等可选标准玩法模块 |
| [games/base](games/base/README.md) | 基础游戏的模块、内容与规则组合 |
| [examples/mods](examples/mods/README.md) | 仅依赖公开SDK的Mod示例 |
| [tests](tests/README.md) | 跨模块场景、故障注入与测试数据 |
| [benchmarks](benchmarks/README.md) | 可复现工作负载与性能测量 |

apps、crates与标准玩法模块已注册为Cargo工作区成员。游戏定义、Mod示例和性能测试暂以README保留目录，具体格式与运行入口待对应契约确定。codec保留上一轮建立的存档编码职责，是总览中storage职责的细分；fixtures是测试数据的补充目录。

## 协作边界

A负责权威世界数据、模拟、生命周期和原子提交，保留最终写入权。B负责世界与磁盘、连接之间的数据交换，解码后的加载数据和外部请求通过A提供的入口接纳。

公共契约由用户指定。当前不定义世界类型、提交记录字段、恢复接口、动作接口或复制权限规则。各crate暂不声明彼此依赖，也不引入第三方库。

本轮补齐全部目录后，此聊天后续实现范围保持为B部分：apps/server、crates/codec、crates/storage、crates/protocol、crates/net，以及对应测试。A部分及其他模块的骨架供负责成员继续开发。公共契约、根工作区配置和跨模块接入变更需要与相关成员协调。

存档记录与网络消息分别编码。持久化确认、请求接纳和状态复制的具体接入方式在契约确定后实现。正式需求和架构材料在独立的[TorseDesign仓库](https://github.com/invefa/TorseDesign)维护。

## 检查

需要支持Rust2024 Edition的Rust工具链。当前骨架可使用以下命令检查：

```powershell
cargo fmt --all --check
cargo check --workspace --all-targets
```

这些检查仅验证骨架的格式和编译，不代表功能验收通过。工作区尚未锁定正式工具链版本。
