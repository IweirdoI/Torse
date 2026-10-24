# 测试与场景

可执行测试随实现保存在各包的单元测试与`tests/`目录中，本目录维护跨模块场景和固定数据的入口说明。基础检查与可选特性要求见[贡献指南](../CONTRIBUTING.md)。

| 位置 | 验证范围 |
|---|---|
| [crates/sim/tests](../crates/sim/tests) | 事务、跨模块提交、移动领域逻辑、范围授权与复制 |
| [crates/codec/tests](../crates/codec/tests) | 字段编码、固定字节、世界载荷、版本与生命周期往返 |
| [crates/storage/tests](../crates/storage/tests) | 日志、检查点、故障注入、恢复扫描与进程退出 |
| [crates/net/tests](../crates/net/tests) | 有界传输、部分读写、背压和连接关闭 |
| [crates/mod_host/tests](../crates/mod_host/tests) | Wasm边界、权威桥与包工具，需启用对应特性 |
| [apps/server/tests](../apps/server/tests) | 文件世界、持久执行、授权网络与Mod恢复 |
| [apps/tools](../apps/tools/src/main.rs) | 可独立运行的内容、世界、提交、移动与恢复场景 |

运行默认工作区测试：

```sh
cargo test --workspace --all-targets --locked
```

[场景索引](scenarios/README.md)按行为列出执行入口，[测试数据说明](fixtures/README.md)说明固定字节与动态故障数据的存放方式。窗口与GPU画面需要本地运行验证，复现条件和结果保存在[benchmarks](../benchmarks/README.md)。
