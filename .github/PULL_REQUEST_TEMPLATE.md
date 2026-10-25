<!--
  一次PR处理一个问题。新功能、公共接口以及存档、网络格式的变更，请先在Issue确认范围。
  验证要求见 CONTRIBUTING.md。
-->

## 说明

## 关联Issue

Fixes #

## 验证

<!--
  说明怎么验证的：复现命令、覆盖到的拒绝与失败路径；
  图形改动附截图；涉及持久状态的行为说明保存与恢复的验证。
-->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --all-targets --locked`
- [ ] 新增二进制素材已在说明中登记来源，或本PR未新增素材

## 场景记录

<!--
  新增或更新了 benchmarks/ 下的记录时在此列出。
-->
