# 客户端

包名：`torse-client`。当前提供两种模式，共用同一场景、网格与管线：`--capture`离屏渲染一帧PNG；`--window`打开交互窗口，连续渲染、飞行输入与分区流式装载。

## 运行

```sh
# 离屏单帧
cargo run -p torse-client -- --capture out.png --width 960 --height 720 --radius 2
# 交互窗口
cargo run -p torse-client -- --window --radius 4
# 自动飞行取证，呈现N帧后退出并可存最后一帧
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
| `--autoplay` | 窗口模式自动前向飞行，供取证 |

窗口操作：WASD移动，空格与Shift升降，按住鼠标左键或右键拖动转向，滚轮调节速度，F12截图到`torse-shot-N.png`，Esc或关闭窗口退出。

需要可用的图形后端。本机实测走Vulkan，集显即可。CI不执行渲染，只做编译检查。

## 组成

| 文件 | 职责 |
|---|---|
| [scene.rs](src/scene.rs) | 装配`WorldAuthority`、注入丘陵生成器、按半径装载并建网格、汇总顶点 |
| [renderer.rs](src/renderer.rs) | `GpuContext`管线与缓冲、离屏目标及PNG写出的公共路径 |
| [window.rs](src/window.rs) | winit窗口、surface配置、帧循环、流式装载调度与窗口截图 |
| [camera.rs](src/camera.rs) | 透视与视图矩阵；`FlyCamera`按偏航俯仰积分飞行 |
| [input.rs](src/input.rs) | 按键、鼠标与滚轮的帧级聚合 |
| [main.rs](src/main.rs) | 参数解析与两种模式的入口 |

顶点为位置、法线、颜色各三浮点，共36字节，按材质映射到一个扁平颜色，光照由法线与固定方向光决定。窗口surface格式由适配器决定（本机为BGRA），管线颜色目标按实际格式创建，截图读回时转换成RGBA。

## 边界

- 实体不在画面中：未接入`Simulation`，实体渲染与网络联机随客户端后续任务补齐。
- 无纹理、阴影与透明排序；着色器与离屏路径相同。
- 分区固定`y = 0`层，半径外直接卸载；窗口尺寸变化已重配置surface与深度。
- 代码不写世界，仅通过只读查询与网格任务读取。
