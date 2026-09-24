# SeeCut Figma 实现核对（2026-09-24）

Figma 与软件预览为 1× 的 1440 × 960（专业模式另有 1024 × 960）；原生截图为 Retina 2× 的 2800 × 1696，即 1400 × 848 逻辑像素。当前显示器报告 2560 × 1664 Retina，原生窗口无法容纳 1440 × 960 逻辑尺寸。下表的原生位置换算为逻辑像素。Figma 使用演示数据；原生测试使用隔离 HOME 中的两项 `opaque-square` 图片资产（1254 × 1254 与 1920 × 1080）。

截图根目录：

- Figma：`/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0ccdc-54d1-77d0-ae27-10bfe596a93d/`
- 软件预览：`/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0cd9d-dd88-7012-bd9e-8103e508d601/seecut-figma-candidate-v11/`
- 原生实屏：`/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0cd9d-dd88-7012-bd9e-8103e508d601/native-validation-v11/captures/`
- 视觉复核实屏：`/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0cd9d-dd88-7012-bd9e-8103e508d601/native-validation-v13/captures/`
- 当前画布交接实屏：`/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0cd9d-dd88-7012-bd9e-8103e508d601/native-validation-v14/captures/`
- v14 原包 Slint 软件渲染 fixture：`/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0cd9d-dd88-7012-bd9e-8103e508d601/native-validation-v14/fixture-render/`
- v16 原生实屏：`/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0cd9d-dd88-7012-bd9e-8103e508d601/native-validation-v16/captures/`
- v17 剪辑导出实屏：`/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0cd9d-dd88-7012-bd9e-8103e508d601/native-validation-v17/captures/`
- v18 品牌与安装实屏：`/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0cd9d-dd88-7012-bd9e-8103e508d601/native-validation-v18/captures/`
- 最终 Slint 软件渲染 fixture：`/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0cd9d-dd88-7012-bd9e-8103e508d601/native-validation-v16/fixture-render/`

| 页面与 Figma 节点 | 对照截图 | 已核对的尺寸、状态、文案 | 仍有差异或未确认 |
| --- | --- | --- | --- |
| 图片专业 1024：[105:4269](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=105-4269) | `pro1024-mac-controls.png` ↔ `generation-pro-1024x960.png`；原生 `generation-pro-light.png` | 同为 1024 宽的软件预览中，导航 80px、创作栏右界 x=400、结果栏起点 x=433；参考缩略图上缘约 y=127（Figma y=125），提示词框上缘约 y=361（Figma y=363），模型框上缘约 y=563（Figma y=568）。“更多参数”可由键盘操作。原生未登录页不再出现不可用的“数量”提示。登录后的结果与报价视觉 fixture 另见下表。 | 软件预览的字体笔画与部分图标形状和 Figma 不同；真实登录、服务端任务与报价仍未验收。 |
| 资产库浏览：[19:954](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=19-954) | `asset-normal-mac-controls.png` ↔ 原生 `02-assets-zh-light.png`、`14-assets-zh-dark.png` | 导航 80px、文件夹栏 192px，内容从 x=304 开始；搜索框 232px、类型筛选选中填充、排序为纯文字。两项资产卡正常显示尺寸，浅色截图无操作通知。深色实屏中“全部素材”选中背景限制在文件夹行内。 | 素材数量、文件夹和图片数据与 Figma 示例不同；资产卡的按下与键盘焦点未逐项抽检。 |
| 素材送入画布：[83:2093](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=83-2093) | `modal-asset-mac-controls.png` ↔ 原生 `handoff-canvas-light.png`；双行软件预览 `handoff-canvas-1440x960.png`；最终实屏 `canvas-existing-target-selected-zh-light.jpg`、`canvas-new-auto-open-zh-light.jpg`、`canvas-existing-auto-open-zh-light.jpg`、`current-handoff-target-selected.jpg`、`current-handoff-cancel-source-retained.jpg`、`current-handoff-auto-open.jpg` | Figma 弹窗 x=360、y=235、720 × 490；原生 x≈340、y≈179、720 × 490。真实目标行含当前画布与历史项目。新项目、历史项目和当前画布交接后均直接进入画布；当前画布的图层从 3 层增至 4 层。取消交接返回资产库，仍显示所选 1 项。 | 当前画布行显示占位缩略图；来源素材和项目使用隔离测试数据。 |
| 画布图库：[19:528](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=19-528) | `project-canvas-mac-controls.png` ↔ `canvas-project-gallery-dark-1440x960.png`；原生 `08-canvas-gallery-zh-light.png`、`15-canvas-gallery-zh-dark.png` | 同宽预览中首卡 x≈112、y≈113、宽≈302；有搜索、排序、新建项目卡。v13 的两个 `opaque-square` 项目在浅色和深色图库中均可见；v14 新建第三个项目后，项目清单与图层文件也已核对。 | 示例卡数量与缩略图不同；项目卡的按下与键盘焦点未逐项抽检。 |
| 画布导出：[83:2329](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=83-2329) | `export-canvas-mac-controls.png` ↔ v16 原生 `canvas-export-zh-dark.jpg`、fixture `canvas-export-dark-1440x960.png` | 同为深色主题时，弹窗宽约 544px、高约 214px；标题左对齐、右上关闭。v16 次按钮仍无描边，文字使用正常前景色；关闭图形放大、提亮，点击热区保持 26 × 26px。旧包导出至资产库曾使资产数从 1 变为 2。 | 背景画布为隔离测试杯图，与 Figma 演示建筑图不同；其他文件夹分支在旧包曾导出 PNG，此轮只复核入口。 |
| 剪辑编辑：[77:2001](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=77-2001) | `clip-mac-controls.png` ↔ 原生 `clip-media-bin-zh-light.jpg`、`clip-media-bin-zh-dark.jpg`、v16 `clip-timeline-five-seconds-zh-dark.jpg` | 简体中文设置下，浅色和深色实屏的内部媒体/预览/详情均为中文。隔离项目 `测试剪辑` 用一项本地图片素材在时间线建立 5 秒片段，随后完成两种目的地的视频导出；详见下方离线验收。 | `Timeline 1` 是测试项目中的数据名，未被此轮改名；测试源图与 Figma 演示素材不同。 |
| 剪辑导出：[83:2396](https://www.figma.com/design/SCaIOer6VgtRok8iqj9XI9/SeeCut?node-id=83-2396) | `export-clip-mac-controls.png` ↔ v17 原生 `clip-export-destinations-zh-dark.png`、`clip-export-library-ready-zh-dark.png` | v17 目的地弹窗为 544 × 214px，标题左对齐、右上关闭；“导出至资产库”为描边按钮，“导出至其他文件夹”为无描边按钮。进入资产库导出设置后，“内容”显示“1 个片段”。关闭目的地弹窗或导出设置后没有新增 MP4；其他文件夹按钮打开系统文件夹选择器，取消后回到剪辑页。 | Figma 为浅色演示素材，原生截图为深色隔离杯图；此轮没有重复执行视频编码，复用 v16 的双目的地全段解码证据。 |

### 生成区视觉 fixture

`SEECUT_UI_PREVIEW_DIR` 使 v16 包使用同一 Slint 界面组件和软件渲染器，在隔离目录渲染 97 张确定性截图；图片借用本地 Figma 素材区域，任务状态与积分文字为 fixture 数据。此路径没有登录、生成请求或真实报价。

| 核对点 | Figma ↔ v16 fixture | 观察与边界 |
| --- | --- | --- |
| 结果卡与报价区 | `quote-mac-controls.png` ↔ `generation-pro-1440x960.png` | 相同 1440 × 960 视口下，左侧创作栏右界 x≈496、结果区起点 x≈529，四张结果卡按三列加一列换行；fixture 底部显示“本次 8 积分”。Figma 是“待估算／估算中”状态，不能用 fixture 数字证明服务端报价。 |
| 批次结果状态 | `partial-light-mac-controls.png` ↔ `generation-results-narrow.png`、`generation-video-nine-wide.png` | fixture 实屏中同一批次有已完成、生成中和生成失败卡；失败态为红色文字，处理中为中性色占位。Figma 示例为四项中的三项成功、一项失败，数量与素材不同。 |

### 控件瞬态抽检

| 控件 | 证据与观察 |
| --- | --- |
| 交接目标行 | v16 fixture `fixture-handoff-disabled-1440x960.png`、`fixture-handoff-target-hover-1440x960.png`、`fixture-handoff-target-pressed-1440x960.png` 显示目标行填充逐级变化；原生 `handoff-current-keyboard-focus-zh-dark.jpg` 显示 Tab 焦点环在行内。回车选择后，AX 中“加入并打开”由 disabled 变为 enabled。 |
| 共用按钮 | v16 fixture `fixture-handoff-button-hover-1440x960.png` 与 `fixture-handoff-button-pressed-1440x960.png` 的内容不同，变化范围 x=953–1055、y=662–700，仅在按钮内部；v14 原生 `app-button-keyboard-focus-zh-dark.jpg`、`handoff-button-disabled-zh-dark.jpg` 覆盖键盘焦点和禁用态。 |
| 图标按钮 | v16 fixture `fixture-icon-hover-1440x960.png` 与 `fixture-icon-pressed-1440x960.png` 有可见差异，变化范围 x=1320–1360、y=24–64；v14 原生 `icon-button-keyboard-focus-zh-dark.jpg` 覆盖键盘焦点。 |

### 剪辑导出离线验收

在 v16 原生包的隔离 HOME 中，双击素材箱中的 `opaque-square` 图片，时间线与项目 `concat.json` 均显示 1 个片段、时长 5 秒。原生截图：`clip-timeline-five-seconds-zh-dark.jpg`。

| 路径 | 实测结果 |
| --- | --- |
| 保存到个人资产 | `clip-export-library-ready-zh-dark.jpg` 显示 1920 × 1080、30 fps、H.264、0:05、1 个片段；完成后界面提示“视频已导出并加入资产库”。`clip-exports/fbd8c752-f63e-4d6f-b244-4994c79a39c6.mp4` 可整段解码。资产库 `library.json` 新增名为“测试剪辑”的视频；`clip-export-reimported-media-bin-zh-dark.jpg` 和项目文件确认再次导入剪辑素材箱后，媒体由 1 项变 2 项，时间线仍为 1 片段。 |
| 保存到其他文件夹 | 选择 `native-validation-v16/exports-other/`，`clip-export-other-ready-zh-dark.jpg` 显示目标 `测试剪辑.mp4`。完成后 `clip-export-other-done-zh-dark.jpg` 显示导出成功，文件可整段解码。第 1 秒解码画面见 `clip-export-other-frame-1s.png`，内容与源杯图一致；资产库仍为 3 项，没有把这次本地文件夹导出重复入库。 |
| 取消或关闭 | 目的地弹层关闭后，`clip-exports` 目录未创建、没有 MP4；其他文件夹的导出设置在点击“导出”前关闭后，目标目录仍为空。 |

两个 MP4 的 `ffprobe` 结果均为 H.264、1920 × 1080、30 fps、150 帧、4.967 秒；`ffmpeg` 全段解码均无错误。界面中的“约 5 MB”是导出前估计，实际静态杯图高度可压缩，单个文件为 73,496 字节。此验证只覆盖本地图片素材构成的短视频，没有覆盖音频、多片段特效或真实服务端生成。

### 品牌与安装（v18）

`seecut-astronaut.png` 原画的颜色和人物比例保留，应用内派生图采用 19% 圆角，在导航栏保持 36 × 36px。系统图标从同一派生图生成，在 1024px 画布四周留 7% 透明边距，并加入轻微下投影；256px 图标主体的可见范围为 220 × 220px，与本机 Finder/Safari 图标的测量宽度一致。16、32、128、256、512 及 2× 尺寸均由这一画布生成。`seecut-in-app-logo.png` 与 `seecut-finder-applications.png` 分别记录了安装后的应用内 Logo 和 Finder 中的图标、`Seecut.app` 名称及周围应用的尺寸对照。

`/Applications/Seecut.app` 已安装；`CFBundleName`、`CFBundleDisplayName` 和窗口/菜单栏显示为 `Seecut`，可执行文件名为 `seecut`，`CFBundleIconFile` 为 `Seecut.icns`。旧 `/Applications/SeeCut Preview.app` 已移到精确备份路径，应用程序目录只有一个可见入口。Bundle identifier 仍为 `cloud.stormycry.seecut.preview`，项目和账号数据目录未迁移。v18 用离线 Cargo 构建，并复用了本机同版本、同架构的 Sherpa ONNX 静态库；应用签名通过 `codesign --verify --deep --strict`。原生测试还确认了窗口左上红、黄、绿三个按钮、源图与画布图层名称、剪辑媒体名称和画布保存。登录后生成、真实报价与服务端结果不在这组原生证据内。当前 PR 保持 Draft。
