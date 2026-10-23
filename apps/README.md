# 应用入口

| 目录 | 职责 | 当前状态 |
|---|---|---|
| [client](client/src/main.rs) | 窗口、输入、预测、表现与UI的应用装配 | 空入口，待对应负责人实现 |
| [server](server/README.md) | 权威模拟与存储、连接服务的应用装配 | B部分，空入口 |
| [tools](tools/src/main.rs) | 内容检查、M1场景与技术实验 | `content-fixture`、`m1-scenario`、`ecs-probe`、`wasm-probe`和`gpu-probe` |

各应用由组合入口连接服务，底层库不反向依赖应用。无界面服务端不得依赖客户端窗口、GPU或纹理解码初始化。

