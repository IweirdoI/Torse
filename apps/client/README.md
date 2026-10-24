# 客户端

包名：`torse-client`。当前提供两种模式，共用同一场景、网格与管线：`--capture`离屏渲染一帧PNG；`--window`打开交互窗口，连续渲染、飞行输入、分区流式装载，并可在窗口内选中、放置与破坏方块。

## 运行

```sh
# 离屏单帧
cargo run -p torse-client -- --capture out.png --width 960 --height 720 --radius 2
# 交互窗口
cargo run -p torse-client -- --window --radius 4
# 自动飞行取证，呈现N帧后退出并可存最后一帧；飞行中每24帧交替破坏/放置准星目标
cargo run --release -p torse-client -- --window --autoplay --frames 4000 --radius 4
```

| 参数 | 含义 |
|---|---|
| `--capture` | 离屏模式，输出PNG路径 |
| `--window` | 窗口模式，未指定时按离屏处理 |
| `--width`、`--height` | 图像或初始窗口尺寸，默认960x540 |
| `--radius` | 分区半径，离屏默认2，窗口默认4 |
| `--view` | 离屏相机：`overview`或`player`，默认俯视全景 |
| `--frames` | 窗口模式呈现指定帧数后自动退出，供取证 |
| `--screenshot` | 窗口模式退出前保存最后一帧PNG |
| `--autoplay` | 窗口模式自动前向飞行并演示破坏/放置，供取证 |

窗口操作：WASD移动，空格与Shift升降，按住鼠标左键或右键拖动转向，滚轮调节速度；短按左键破坏准星方块，短按右键沿选中面放置，数字键1到3切换石头、玻璃半砖与植物；F12截图到`torse-shot-N.png`，Esc或关闭窗口退出。准星处绘制选中框线框。

需要可用的图形后端。本机实测走Vulkan，集显即可。CI不执行渲染，只做编译检查。

## 组成

| 文件 | 职责 |
|---|---|
| [scene.rs](src/scene.rs) | 装配`WorldAuthority`与动作服务、注入丘陵生成器、按半径装载并建网格、提交放置与破坏事务 |
| [renderer.rs](src/renderer.rs) | `GpuContext`地形与线框双管线、离屏目标及PNG写出的公共路径 |
| [window.rs](src/window.rs) | winit窗口、surface配置、帧循环、流式装载与动作调度、窗口截图 |
| [camera.rs](src/camera.rs) | 透视与视图矩阵；`FlyCamera`按偏航俯仰积分飞行 |
| [input.rs](src/input.rs) | 按键、鼠标与滚轮的帧级聚合，区分点击与拖动 |
| [pick.rs](src/pick.rs) | 体素DDA拾取射线，返回目标格与进入面 |
| [main.rs](src/main.rs) | 参数解析与两种模式的入口 |

顶点为位置、法线、颜色各三浮点，共36字节，按材质映射到一个扁平颜色，光照由法线与固定方向光决定。窗口surface格式由适配器决定（本机为BGRA），管线颜色目标按实际格式创建，截图读回时转换成RGBA。

点击先由拾取射线解析目标格与面，放置沿面偏移一格；每次编辑经`ActionService::plan`完成权限、版本与前置条件校验，再由`WorldAuthority::commit`提交，客户端不绕过动作服务直接写世界。

## 边界

- 实体不在画面中：未接入`Simulation`，实体渲染与网络联机随客户端后续任务补齐。
- 放置不消耗库存、破坏不产生掉落；命令解析与网络输入适配待后续。
- 拾取按完整立方体处理非空格；只装载`y = 0`分区层，编辑与拾取限于此层。
- 无纹理、阴影与透明排序；着色器与离屏路径相同。
- 代码不直接写世界，方块修改仅经动作服务产生的事务提交。

