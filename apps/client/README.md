# 客户端

包名：`torse-client`。提供本地交互窗口、离屏PNG输出，以及连接独立服务端的图形客户端。各模式共用场景、网格与绘制管线。

## 运行

在仓库根目录打开本地窗口：

```sh
cargo run -p torse-client -- --window
```

离屏模式输出一张PNG，仍需要GPU与可用的图形后端：

```sh
cargo run -p torse-client -- --capture out.png --width 960 --height 720 --radius 2
```

连接服务端时，先在另一终端创建演示世界：

```sh
cargo run -p torse-server -- .work/client-demo --listen 127.0.0.1:7777 --fill --editors
```

保持服务端运行，再连接客户端：

```sh
cargo run -p torse-client -- --window --connect 127.0.0.1:7777
```

`--fill`只适用于新世界，重开同一目录时需去掉。`--editors`授予演示订阅范围内的编辑权限；省略时以访客权限连接，编辑请求会被拒绝。客户端在联网模式中发送意图，服务端提交后通过授权增量更新画面。

## 参数与操作

| 参数 | 含义 |
|---|---|
| `--window` | 打开交互窗口 |
| `--capture FILE` | 离屏渲染一帧并写入PNG |
| `--width N`、`--height N` | 输出图像或初始窗口尺寸，默认960x540 |
| `--radius N` | 本地分区半径，离屏默认2，窗口默认4 |
| `--view overview`、`--view player` | 离屏相机，默认俯视全景 |
| `--frames N` | 窗口呈现指定帧数后退出，用于自动验证 |
| `--screenshot FILE` | 窗口正常退出前保存最后一帧 |
| `--autoplay` | 自动前进并周期性尝试破坏或放置方块，会修改世界 |
| `--walk` | 以行走模式启动，自动行走遇台阶时尝试跳跃 |
| `--connect ADDRESS` | 连接服务端并读取授权世界镜像 |
| `--probe-outside` | 联网后发送范围外请求，验证越界拒绝 |
| `--ui-preview hud`、`pause`、`machine` | 指定初始界面，窗口与离屏模式均可使用 |

WASD移动，按住鼠标左键或右键拖动转向，滚轮调整速度。飞行模式下空格上升、左Shift下降；F5切换行走，行走时空格跳跃。短按左键破坏目标方块，短按右键沿选中面放置；数字键1到3切换石头、玻璃半砖和植物。F12保存`torse-shot-N.png`，Esc打开或关闭菜单，E打开机器面板预览。

自动验证示例：

```sh
cargo run --release -p torse-client -- --window --autoplay --frames 2000 --radius 4
cargo run --release -p torse-client -- --window --walk --autoplay --frames 2400 --radius 3
```

退出汇总包含帧数、编辑结果、网络请求及帧时指标。GPU网格按分区上传，本地地形按距离排序、每帧限量生成，相关测量见[帧预算记录](../../benchmarks/M6-frame-budget.md)。这些数据取决于环境与场景，不代表通用性能保证。

## 界面与字体

窗口默认绘制准星、快捷栏、当前方块和连接状态。菜单提供返回、机器面板预览和退出。面板打开时停止本地移动与编辑输入，网络接收继续运行；联网世界不会因客户端菜单暂停。

机器面板明确使用示例数据，目前不读取或修改真实机器与库存。服务端已有机器状态与动作模型，图形客户端对该模型的消费和操作仍待接入。截图及验证见[UI基础](../../benchmarks/M1-ui.md)。

```sh
cargo run -p torse-client -- --window --ui-preview machine
cargo run -p torse-client -- --capture ui.png --width 960 --height 540 --ui-preview machine
```

界面按可用空间选择1到4倍整数缩放。PNG和文案位于[基础游戏资源](../../games/base/assets/README.md)，编译时嵌入；修改后需重新编译。字体从操作系统读取，可用`TORSE_UI_FONT`指定TTF、OTF或TTC。缺少中文字形时使用英文文案，没有可用字体则报告错误。

## 实现入口

| 文件 | 职责 |
|---|---|
| [scene.rs](src/scene.rs) | 内容与世界装配、流式分区、网格、动作提交与行走接入 |
| [renderer.rs](src/renderer.rs) | GPU管线、分区缓冲、离屏输出和截图 |
| [window.rs](src/window.rs) | 窗口、帧循环、输入调度与网络轮询 |
| [net.rs](src/net.rs) | 握手、订阅、授权镜像与编辑请求 |
| [input.rs](src/input.rs)、[camera.rs](src/camera.rs) | 输入聚合与相机 |
| [pick.rs](src/pick.rs) | 方块射线拾取与放置面计算 |
| [ui](src/ui/mod.rs) | 界面资源、文字、布局与模态输入 |

本地编辑经过`ActionService`授权和规划，再由`WorldAuthority`提交。行走复用`torse_sim::player::walker_step`，通过移动事务结算重力、跳跃与碰撞。联网编辑不直接修改镜像，最终状态以服务端授权批次为准。

## 当前限制

- 联网编辑已接通，订阅仍为固定演示范围，没有随玩家移动的订阅切换。行走只在客户端本地结算，服务端移动请求与校正尚未接入。
- 实体以色块呈现，行走玩家由相机表现，本地行走实体不持久化。出生点嵌入地形时可能冻结，可切回飞行后重新进入行走。
- 放置不消耗库存，破坏不生成掉落；机器面板仍为预览，真实库存图形操作尚未接入。
- 拾取把非空格按完整立方体处理，当前演示只装载`y = 0`分区层。
- 地形使用顶点颜色与方向光，没有纹理、阴影、透明排序、自动上台阶或水体物理。

CI执行编译和测试，窗口交互、输入及真实GPU画面仍需本地验证。
