# SeeCut Figma 实现核对（2026-09-24）

Figma 与软件预览为 1× 的 1440 × 960（专业模式另有 1024 × 960）；原生截图为 Retina 2× 的 2800 × 1696，即 1400 × 848 逻辑像素。当前显示器报告 2560 × 1664 Retina，原生窗口无法容纳 1440 × 960 逻辑尺寸。下表的原生位置换算为逻辑像素。Figma 使用演示数据；原生测试使用隔离 HOME 中的两项 `opaque-square` 图片资产（1254 × 1254 与 1920 × 1080）。

截图根目录：

- Figma：`/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0ccdc-54d1-77d0-ae27-10bfe596a93d/`
- 软件预览：`/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0cd9d-dd88-7012-bd9e-8103e508d601/seecut-figma-candidate-v11/`
- 原生实屏：`/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0cd9d-dd88-7012-bd9e-8103e508d601/native-validation-v11/captures/`
- 视觉复核实屏：`/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0cd9d-dd88-7012-bd9e-8103e508d601/native-validation-v13/captures/`
- 最终交接复核实屏：`/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0cd9d-dd88-7012-bd9e-8103e508d601/native-validation-v14/captures/`

| 页面与 Figma 节点 | 对照截图 | 已核对的尺寸、状态、文案 | 仍有差异或未确认 |
| --- | --- | --- | --- |
| 图片专业 1024：[105:4269](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=105-4269) | `pro1024-mac-controls.png` ↔ `generation-pro-1024x960.png`；原生 `generation-pro-light.png` | 同为 1024 宽的软件预览中，导航 80px、创作栏右界 x=400、结果栏起点 x=433；参考缩略图上缘约 y=127（Figma y=125），提示词框上缘约 y=361（Figma y=363），模型框上缘约 y=563（Figma y=568）。“更多参数”可由键盘操作。原生未登录页不再出现不可用的“数量”提示。 | 软件预览的字体笔画与部分图标形状和 Figma 不同。原生为未登录空状态，无法与 Figma 的已登录 4 张结果逐项比较报价、结果卡和按钮按下态。 |
| 资产库浏览：[19:954](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=19-954) | `asset-normal-mac-controls.png` ↔ 原生 `02-assets-zh-light.png`、`14-assets-zh-dark.png` | 导航 80px、文件夹栏 192px，内容从 x=304 开始；搜索框 232px、类型筛选选中填充、排序为纯文字。两项资产卡正常显示尺寸，浅色截图无操作通知。深色实屏中“全部素材”选中背景限制在文件夹行内。 | 素材数量、文件夹和图片数据与 Figma 示例不同；交互瞬态另见下方抽检。 |
| 素材送入画布：[83:2093](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=83-2093) | `modal-asset-mac-controls.png` ↔ 原生 `handoff-canvas-light.png`；双行软件预览 `handoff-canvas-1440x960.png`；最终实屏 `canvas-existing-target-selected-zh-light.jpg`、`canvas-new-auto-open-zh-light.jpg`、`canvas-existing-auto-open-zh-light.jpg` | Figma 弹窗 x=360、y=235、720 × 490；原生 x≈340、y≈179、720 × 490，中心坐标随窗口宽少 40px、高少 112px 各减半。真实目标行含当前画布与历史项目；选中历史项目后“加入并打开”启用。新项目交接后直接进入画布，清单写入同名素材图层；历史项目交接后直接进入画布，项目图层由 2 层变为 3 层。取消交接返回资产库。 | 当前画布行显示占位缩略图；这轮未点击该行的交接分支。 |
| 画布图库：[19:528](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=19-528) | `project-canvas-mac-controls.png` ↔ `canvas-project-gallery-dark-1440x960.png`；原生 `08-canvas-gallery-zh-light.png`、`15-canvas-gallery-zh-dark.png` | 同宽预览中首卡 x≈112、y≈113、宽≈302；有搜索、排序、新建项目卡。v13 的两个 `opaque-square` 项目在浅色和深色图库中均可见；v14 新建第三个项目后，项目清单与图层文件也已核对。 | 示例卡数量与缩略图不同；卡片交互瞬态另见下方抽检。 |
| 画布导出：[83:2329](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=83-2329) | `export-canvas-mac-controls.png` ↔ 原生 `canvas-export-choices-light.png` | Figma 弹窗 x=448、y=373、544 × 214；原生 x≈428、y≈317、544 × 214，中心位移与窗口尺寸差一致。标题左对齐，右上关闭，按钮为“导出至资产库”“导出至其他文件夹”；前者实测使资产数从 1 变为 2。 | Figma 深色、原生浅色；同主题对照仍未完成。其他文件夹分支在旧包曾导出 PNG，新包只复核了入口文案。 |
| 剪辑编辑：[77:2001](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=77-2001) | `clip-mac-controls.png` ↔ 原生 `12-clip-media-bin-en-light.png`（设置前）、最终实屏 `clip-media-bin-zh-light.jpg`、`clip-media-bin-zh-dark.jpg` | v14 同一“简体中文”设置下，浅色和深色实屏的内部媒体/预览/详情与外层“已保存”均为中文。媒体区 1 项，名称 `opaque-square`，时间线 0 片段。名称沿用资产展示名，没有 UUID。 | `Timeline 1` 是测试项目中的数据名，未被此轮改名；没有在时间线上放置片段并导出视频。 |

原生测试还确认了窗口左上红、黄、绿三个按钮、源图与画布图层名称、剪辑媒体名称和画布保存。v14 用离线 Cargo 构建，macOS 包已装入 `SeeCut.icns`，`Info.plist` 指向该图标，`codesign --verify --deep --strict` 通过。登录后生成、真实报价与服务端结果、剪辑时间线输出不在这组原生证据内。当前 PR 保持 Draft。
