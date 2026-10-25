# 验证场景

现有场景分布在工作区测试与开发工具中。本目录不包含独立测试程序，下表列出主要入口。

| 场景 | 执行入口 |
|---|---|
| 编码往返、版本与固定字节 | `cargo test -p torse-codec --all-targets --locked` |
| 日志、检查点、损坏与恢复 | `cargo test -p torse-storage --all-targets --locked` |
| 跨模块事务、移动与授权复制 | `cargo test -p torse-sim --all-targets --locked` |
| 库存争用与物品守恒 | `cargo run -p torse-inventory --example competition --locked` |
| 持久执行、重复请求、两客户端与进程退出 | `cargo test -p torse-server --all-targets --locked` |
| Mod组件、包工具与场景 | `cargo test -p torse-mod-host --features package --all-targets --locked` |
| 持久Mod任务与恢复 | `cargo test -p torse-server --features mods --all-targets --locked` |

开发工具提供`m1-scenario`、`commit-scenario`、`tick-scenario`、`exchange-scenario`、`movement-scenario`、`recovery-scenario`和`persistence-scenario`。例如：

```sh
cargo run -p torse-tools -- recovery-scenario
```

Mod测试需要`wasm32-unknown-unknown`目标。窗口编辑、行走与联机画面的验证记录见[验证与测量记录](../../benchmarks/README.md)。未知内容原始载荷保留仍未实现，不列入已通过的能力。

运行产生的数据放入`.work/`，复现存档问题时使用副本，避免覆盖人工存档。
