# 基础游戏地形贴图

本目录保存基础游戏的地形图集。图集由[make-textures.py](make-textures.py)用固定种子程序生成，全部像素为自绘，不包含第三方美术资源；重新运行脚本即可得到同一份结果。

## 图集布局

图集为64x16的RGBA图像，四段内容各占16x16：

| 区域 | 起始坐标 | 内容 |
|---|---|---|
| 石头 | (0, 0) | 灰色噪点表面，带少量暗斑 |
| 玻璃 | (16, 0) | 浅蓝半透明边框与高光，用于`blend`模式 |
| 植物 | (32, 0) | 透明背景上的绿色叶片，用于`mask`模式 |
| 兜底 | (48, 0) | 1x1白色像素，供没有图集条目的材质采样 |

## 与内容的绑定

客户端在[texture.rs](../../../apps/client/src/texture.rs)的`TexturePack::embedded`中把内容材质键映射到图集矩形：

| 材质键 | 图集矩形 |
|---|---|
| `base:material/stone` | (0, 0, 16, 16) |
| `base:material/glass_slab` | (16, 0, 16, 16) |
| `base:material/plant` | (32, 0, 16, 16) |

混合模式来自内容定义`RenderProfile::alpha_mode`，不在图集里声明：石头不透明，玻璃半透明，植物二值透明。没有图集条目的材质使用白色兜底像素与灰色色调，保证新内容在补图前仍可显示。

## 再生

修改脚本来调整图案；运行前需要Python与Pillow：

```sh
python games/base/assets/textures/make-textures.py
```

图集在编译期嵌入客户端，重新生成后需要重新编译。新增材质时同时更新`TexturePack::embedded`的条目与本节表格。
