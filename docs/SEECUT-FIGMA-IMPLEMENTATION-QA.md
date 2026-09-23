# SeeCut Figma 实现核对（2026-09-24）

Figma 与软件预览为 1× 的 1440 × 960（专业模式另有 1024 × 960）；原生截图为 Retina 2× 的 2800 × 1696，即 1400 × 848 逻辑像素。下表的原生位置换算为逻辑像素。Figma 使用演示数据；原生测试使用隔离账户中的 `opaque-square.png`（1254 × 1254）。

截图根目录：

- Figma：`/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0ccdc-54d1-77d0-ae27-10bfe596a93d/`
- 软件预览：`/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0cd9d-dd88-7012-bd9e-8103e508d601/seecut-figma-candidate-v11/`
- 原生实屏：`/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0cd9d-dd88-7012-bd9e-8103e508d601/native-validation-v11/captures/`

| 页面与 Figma 节点 | 对照截图 | 已核对的尺寸、状态、文案 | 仍有差异或未确认 |
| --- | --- | --- | --- |
| 图片专业 1024：[105:4269](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=105-4269) | `pro1024-mac-controls.png` ↔ `generation-pro-1024x960.png`；原生 `generation-pro-light.png` | 同为 1024 宽的软件预览中，导航 80px、创作栏右界 x=400、结果栏起点 x=433；参考缩略图上缘约 y=127（Figma y=125），提示词框上缘约 y=361（Figma y=363），模型框上缘约 y=563（Figma y=568）。“更多参数”可由键盘操作。原生未登录页不再出现不可用的“数量”提示。 | 软件预览的字体笔画与部分图标形状和 Figma 不同。原生为未登录空状态，无法与 Figma 的已登录 4 张结果逐项比较报价、结果卡和按钮按下态。 |
| 资产库浏览：[19:954](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=19-954) | `asset-normal-mac-controls.png` ↔ 原生 `asset-light.png` | 导航 80px、文件夹栏 192px，内容从 x=304 开始；搜索框 232px、类型筛选选中填充、排序为纯文字。原生导入后卡片显示 `opaque-square` 和 `1254 × 1254`，图片圆角由 GPU 裁切。 | 原生截图含“已导入个人资产库”的 42px 顶部通知，内容整体下移；素材数量、文件夹和图片数据与 Figma 示例不同。深色文件夹选中背景已在源码调整，未取得新版原生深色实屏；hover、focus、pressed 未逐态截图。 |
| 素材送入画布：[83:2093](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=83-2093) | `modal-asset-mac-controls.png` ↔ 原生 `handoff-canvas-light.png`；双行软件预览 `handoff-canvas-1440x960.png` | Figma 弹窗 x=360、y=235、720 × 490；原生 x≈340、y≈179、720 × 490，中心坐标随窗口宽少 40px、高少 112px 各减半。双行预览中缩略图、名称、日期、箭头按行居中。 | 原生只有 1 项素材且当时无历史项目，未在真实数据中验证目标行的选中与箭头状态。点击“新建画布项目”后，项目与同名图层已写入；“加入并打开”后的自动跳转在当次短时观察中未确认。 |
| 画布图库：[19:528](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=19-528) | `project-canvas-mac-controls.png` ↔ `canvas-project-gallery-dark-1440x960.png`；原生图库由访问树确认 | 同宽预览中首卡 x≈112、y≈113、宽≈302；有搜索、排序、新建项目卡。原生新建 `opaque-square` 项目后，图库可找到并重新打开，图层保留同名。 | 原生图库未保存新版深色截图；示例卡数量与缩略图不同。暗色卡片的 hover/focus/pressed 仍需实屏逐态确认。 |
| 画布导出：[83:2329](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=83-2329) | `export-canvas-mac-controls.png` ↔ 原生 `canvas-export-choices-light.png` | Figma 弹窗 x=448、y=373、544 × 214；原生 x≈428、y≈317、544 × 214，中心位移与窗口尺寸差一致。标题左对齐，右上关闭，按钮为“导出至资产库”“导出至其他文件夹”；前者实测使资产数从 1 变为 2。 | Figma 深色、原生浅色；同主题对照仍未完成。其他文件夹分支在旧包曾导出 PNG，新包只复核了入口文案。 |
| 剪辑编辑：[77:2001](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=77-2001) | `clip-mac-controls.png` ↔ 原生 `clip-import-light.png` | 资产导入后媒体区 1 项，名称 `opaque-square`，时间线 0 片段；保存后项目状态为 `saved`。名称沿用资产展示名，没有 UUID。 | 此隔离测试会话的应用语言为 English，Figma 为中文；`saved` 的中文词条为“已保存”，源码已调用翻译函数，但新版原生会话未切到中文复核。`Timeline 1` 是测试项目中的数据名。 |

原生测试还确认了窗口左上红、黄、绿三个按钮、源图与画布图层名称、剪辑媒体名称和画布保存。登录后生成、真实报价与服务端结果、剪辑时间线输出、深色及各交互瞬态不在这组原生证据内。当前 PR 保持 Draft。
