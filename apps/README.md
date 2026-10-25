# 应用入口

| 目录 | 职责 | 运行方式 |
|---|---|---|
| [client](client/README.md) | 图形渲染、本地交互、联网镜像与编辑请求 | `--window`打开窗口，`--capture`输出PNG，`--connect`连接服务端 |
| [server](server/README.md) | 持久世界、阶段执行、授权网络与可选Mod装配 | 指定世界目录启动；支持恢复、有限Tick运行及无GPU客户端示例 |
| [tools](tools/src/main.rs) | 内容检查、世界场景、提交与恢复验证、技术实验 | 使用`content-fixture`、`m1-scenario`等子命令 |

底层库不依赖应用入口。服务端可在无图形环境中运行；图形客户端由外部项目实现，通过领域模型与公共接口与本仓库装配。

开发工具还提供`commit-scenario`、`tick-scenario`、`exchange-scenario`、`recovery-scenario`、`movement-scenario`与`persistence-scenario`。`ecs-probe`、`wasm-probe`和`gpu-probe`需要显式启用`experiments`特性。

```sh
cargo run -p torse-tools -- m1-scenario
cargo run -p torse-tools --features experiments -- wasm-probe
```
