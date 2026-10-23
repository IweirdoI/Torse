# 标准玩法模块

| 目录 | 职责 |
|---|---|
| [inventory](inventory/src/lib.rs) | 物品堆叠、容量、端口与受控转移 |
| [crafting](crafting/src/lib.rs) | 配方描述与原子加工结算 |
| [energy](energy/src/lib.rs) | 能量单位、存储、端口与结算 |

当前各模块只有空库入口，待对应负责人实现。通过公开模块、内容、动作与模拟接口注册，世界内核不依赖具体玩法模块。GameDefinition选择启用哪些模块。
