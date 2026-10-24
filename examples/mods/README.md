# Mod示例与工具

Torse通过Wasm组件执行行为Mod。Mod返回世界修改、组件更新、库存转移和唤醒计划，由宿主检查权限、准备并提交。包清单与内容使用TOML，行为边界定义于[behavior.wit](../../crates/mod_api/wit/behavior.wit)。

| 示例 | 用途 |
|---|---|
| [roundtrip](roundtrip/src/lib.rs) | 验证WIT查询、计划返回和组件调用边界 |
| [machine](machine/mod.toml) | 可装载的机器包，包含内容定义、四步加工逻辑与无界面场景 |

SDK与工具尚未提供独立发布安装布局，以下命令使用本仓库的Cargo工作区。SDK和示例遵循仓库的[GPL-3.0-only许可](../../LICENSE)。

## 运行机器示例

先在仓库根目录安装Wasm目标：

```sh
rustup target add wasm32-unknown-unknown
```

检查包，再构建行为代码并运行场景：

```sh
cargo run -p torse-mod-host --features package --bin torse-mod -- check examples/mods/machine
cargo run -p torse-mod-host --features package --bin torse-mod -- test examples/mods/machine --scenario examples/mods/machine/scenario.toml
```

成功时会输出各包的派发、提交、拒绝与陷阱统计，并以`scenario passed`结束。场景读取[scenario.toml](machine/scenario.toml)中的世界、实体、初始库存、唤醒和断言。测试模式授予清单申请的权限，实际服务端仍由宿主策略决定授权。

生成目录内容锁文件并验证：

```sh
cargo run -p torse-mod-host --features package --bin torse-mod -- pack examples/mods/machine -o .work/machine.lock
cargo run -p torse-mod-host --features package --bin torse-mod -- check examples/mods/machine --lock .work/machine.lock
```

`pack`当前生成锁文件，记录目录文件摘要，不生成安装压缩包。包检查与锁定工具的实现见[torse_mod.rs](../../crates/mod_host/src/bin/torse_mod.rs)。

## 创建自己的包

`new`可生成清单、组件内容、行为代码和场景。当前模板需要显式设置SDK与WIT的路径；将包放在`examples/mods/my-machine`时，PowerShell示例如下：

```powershell
$env:TORSE_SDK_PATH = "../../../crates/mod_sdk"
$env:TORSE_WIT_PATH = "../../../crates/mod_api/wit"
cargo run -p torse-mod-host --features package --bin torse-mod -- new examples/mods/my-machine
Remove-Item Env:TORSE_SDK_PATH, Env:TORSE_WIT_PATH
```

其他Shell设置同名环境变量即可，路径相对于生成的Cargo.toml与源码所在crate解释。生成后将`examples/mods/my-machine`加入根Cargo.toml的`workspace.members`；清单中的`build.workspace = "../../../"`指向仓库根目录。使用其他目录时，需同步调整这些路径。

随后按机器示例的方式执行`check`与`test`，将包目录和场景路径替换为新目录。提交贡献前补充包用途、授权需求与验证结果，要求见[贡献指南](../../CONTRIBUTING.md)。

## 接入服务端

启用`mods`特性后可加载机器包：

```sh
cargo run -p torse-server --features mods -- .work/mod-demo --mod examples/mods/machine --machine-operators
```

保持服务端运行，在另一终端连接匹配内容的无GPU客户端：

```sh
cargo run -p torse-server --features mods --example headless_client -- 127.0.0.1:7777 --mod examples/mods/machine
```

`--machine-operators`是显式演示策略，授予机器及输入、输出端口的读取和库存转移权限；默认不授予。创建与重开世界时需使用相同内容。现有图形客户端仍使用基础内容装配，尚未加载匹配Mod内容或接入真实机器面板。

机器进度、库存计划与新唤醒在持久任务中共同结算。包可声明绑定方块行为；动作成功时，放置与破坏通知和世界修改共同持久化。机器UI公共模型位于[mod_api](../../crates/mod_api/src/lib.rs)，图形显示与操作仍待接入。

## 验证与限制

```sh
cargo test -p torse-mod-host --features package --all-targets --locked
cargo test -p torse-server --features mods --all-targets --locked
```

包格式、跨包依赖、Merkle锁与工具验证见[包工具记录](../../benchmarks/M4-mod-package.md)；持久唤醒与恢复见[持久Mod执行](../../benchmarks/M4-mod-durable.md)，跨模块共同提交见[Mod原子执行](../../benchmarks/M4-mod-atomic.md)。这些记录描述各次验证时的范围，当前实现以代码和本说明为准。

包API、WIT与内容格式仍在演进，没有通用兼容承诺。依赖包需要一同提供给装载器，尚无远程包发现或注册表。SDK通过仓库路径引用，模板暂不能直接作为独立安装包使用。
