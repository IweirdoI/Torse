# 应用入口

| 目录 | 职责 | 当前状态 |
|---|---|---|
| [client](client/README.md) | 世界装配、网格上传与离屏渲染；窗口与输入待接 | `capture`输出PNG，见[说明](client/README.md) |
| [server](server/README.md) | 持久权威、事件、请求与授权网络装配 | 主程序支持创建/恢复、非阻塞TCP和有限Tick关闭，含headless_client示例 |
| [tools](tools/src/main.rs) | 内容检查、世界与提交场景、技术实验 | `content-fixture`、`m1-scenario`、`commit-scenario`、`tick-scenario`、`exchange-scenario`、`recovery-scenario`、`movement-scenario`、`persistence-scenario`、`ecs-probe`、`wasm-probe`和`gpu-probe` |

各应用由组合入口连接服务，底层库不反向依赖应用。无界面服务端不得依赖客户端窗口、GPU或纹理解码初始化。


