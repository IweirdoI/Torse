# 库存标准模块

可选包torse-inventory。模块提供物品目录、固定容量槽位、查询与原子转移。世界内核只处理通用组件、身份、模式和提交，未依赖库存模块。

## 装配与执行

先调用register_content注册模式、动作和item_port能力，编译内容后创建ItemCatalog，再把handlers交给torse_sim::action::ActionService::bind。缺失处理器、重复绑定、所有者错误或模式缺失会在绑定阶段失败。

ActorContext由宿主根据认证身份、可达范围和容器访问规则建立。请求只能指定意图，不能自报权限。处理器通过ActionView读取获准对象，输出事务计划，交由WorldAuthority准备和发布。计划成功不代表已提交或已持久化。

交互动作在Interaction阶段执行。原生机器通过plan_transfer和Transfer直接调用同一操作，允许Interaction或Simulation阶段，仍检查权限、目标范围、容量和物品守恒。组件事务的Tick入口为Simulation::tick_with_components。

## 组件与规则

inventory:schema/container包含inventory:field/slots列表，长度为1到64。空槽为一个空Record；非空槽恰含inventory:field/item（Content）和inventory:field/count（U32），数量必须为正且不超过ItemCatalog中该物品的上限。

转移检查源槽数量、目标槽物品类型、槽位范围和堆叠上限。源与目标组件在同一事务中替换，原字段作为精确前置条件。同容器不同槽位只产生一次组件替换。同槽转移拒绝。

通用边界限制每对象64个组件，每事务256项组件编辑，每实例4096个值节点、65536预算字节及8层嵌套。预算字节是保守运行期估算，领域字节格式仍由B5定义。

## 验证入口

```sh
cargo run -p torse-inventory --example competition --locked
cargo test -p torse-inventory --all-targets --locked
```

场景验证四种来源得到相同计划、丢弃准备项不改世界、两机器争抢只有一个成功、物品总量不变，以及组件与块编辑记录恢复后继续提交。查询、授权、容量、错误绑定和非法处理器输出另有专项测试。

## 后续工作

配方、能量、物品附加数据、实体生成与删除事务仍待实现。命令入口当前接收已类型化参数，文本解析、实际UI、网络接入与持久确认留给后续装配。ItemCatalog由游戏配置冻结，宿主应将其规则纳入兼容摘要；真实文件恢复还需在模块激活时验证库存语义与目录兼容性。
