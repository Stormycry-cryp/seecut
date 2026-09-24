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

### 2026-09-24 v20 原生候选与交互复核

v20 中间候选包为 `/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0cd9d-dd88-7012-bd9e-8103e508d601/native-candidate-v20/Seecut.app` 和同目录的 `Seecut-macos.zip`，曾安装到 `/Applications/Seecut.app`。安装包可执行文件与候选包的 SHA-256 相同，`codesign --verify --deep --strict` 通过；`.icns` 解包可见 `icon_256x256.png`、`icon_256x256@2x.png` 等分辨率。系统图标使用原画的圆角版本和轻微阴影。v20 的 8 张浅／深色截图来自安装包的真实 macOS 窗口，带原生红黄绿窗口按钮和隔离 HOME 的公开演示素材；没有付费生成。此前采集时出现的 macOS 屏幕共享角标和导航 tooltip 在重拍后均已消失。

| 清单 | 当前证据与结果 | 范围界限 |
| --- | --- | --- |
| D01、D08、D09 | 个人资产和生成结果批量文件导出共用后台复制流程，独占创建输出名、按项记录进度和可重试结果，导出名取资产展示名。 | 大视频、慢盘、空间耗尽和断盘仍需隔离实测。 |
| D02、R03 | 剪辑导出采用独立临时文件、重名处理和作业身份隔离。v20 安装版导出至资产库后，本地库新增视频记录；成品为 1920 × 1080、30 fps、150 帧的 H.264，`ffmpeg` 全段解码通过。导出至其他文件夹前预置同名文件，成品自动使用 `(2)` 后缀；原文件 SHA-256 保持不变，新文件全段解码通过。 | 含音轨及取消临界时序未在 v20 原生包重跑。 |
| D03、D04、D05、D06 | 资产页选择随可见范围收敛；剪辑选择器按导入能力校验；素材准备失败保留弹层与选项；参考批量入口预校验。原生包实测：资产页筛到“音频”显示 0 项时，画布选择器仍显示两张图片，取消返回后原筛选仍生效。另在资产页选择一张图片，进入画布选择器切到“视频”显示 0 项，取消后返回资产页仍显示“已选 1 项”。 | 团队素材登录、下载失败和重新登录未实测。 |
| D07、R02 | 本地库对源文件内容变化和管理副本缺失区分处理；读改写加事务锁，剪辑导出登记也走同一库路径。 | 并发压力和真实大库未做原生压力测试。 |
| D10、R04、R05 | 多文件拖入画布走批量交接；弹层打开时系统拖入受统一边界限制；取消新建导入会清理所属等待项。 | Finder 混合文件拖入与 OS 弹窗竞态仍需人工覆盖。 |
| 01–08 | 生成底栏、参考入口、资产选择／收藏、独立文件夹滚动、失败表单、素材名和空状态均有对应修正；浅／深色原生截图及 1024／1280／1440 软件渲染图可复核布局。 | 重名和持久化失败表单的全部变体没有在本次原生窗口逐一重跑。 |
| 09、10 | `keyboard-validation.json` 通过画布／剪辑导出和交接弹层对 Delete、撤销、全选及最上层 Escape 的隔离；资产拖入文件夹按稳定 ID 移动，并校验已选／未选范围。 | 20+ 文件夹边缘滚动、存储失败和重启后的拖拽分类未逐项原生重跑。 |
| S01–S12、S14–S17 | v20 已修改主题 token、圆角、切换控件、素材选择器、参考识别、字号和部分反馈；软件渲染 fixture 及 8 张原生截图覆盖主要浅／深色和窄窗口状态。 | S09 通用弹窗标题、S12 结果卡键盘入口、S14 通用弹窗动效、S16 重复登录提示和 S17 提交区仍在修正；登录后真实结果、真实报价和部分瞬态仍由 fixture 表示。 |
| S13 | 最上层弹层关闭顺序和背景快捷键隔离在软件渲染 fixture 通过。 | 关闭后的来源控件焦点恢复尚未完整实现和验收，保持未完成。 |

v20 使用 `SEECUT_UI_PREVIEW_DIR` 完整生成软件渲染图，`keyboard-validation.json` 返回 `passed`；真实安装包打开了资产、画布、剪辑和生成页面，并在隔离 HOME 中打开已有画布和 5 秒剪辑项目。上述两次 v20 原生导出覆盖本轮输出路径；v16 的编码结果仅为历史证据。macOS 图标尺寸、签名和安装路径已在 v20 包检查。Windows、登录后团队资产、付费生成和服务端计费仍未验收。

### v21 历史本地候选（2026-09-24）

从本分支最终源码离线构建并安装 `/Applications/Seecut.app`，候选包位于 `native-candidate-v21/Seecut.app`，同目录有 `Seecut-macos.zip`。安装版与候选包的可执行文件 SHA-256 均为 `4a47b2118faf904b0e90e308830d2a23044613f89c148aba12dfbfbcd0a74d01`；`codesign --verify --deep --strict` 通过。系统图标包含 256 × 256 和 256@2x 等尺寸，图标底部的轻微阴影保留。v20 候选目录已在确认 v21 安装后清理，上一节的路径为历史记录。

仓库 `docs/screenshots/` 的 8 张浅／深色图片均重新从 v21 安装版原生窗口采集，覆盖生成、资产库、画布和剪辑。采集使用隔离 HOME 中的两张公开演示图和一个 5 秒离线剪辑项目；没有登录或付费生成。当前 macOS 截屏会在窗口左上显示屏幕共享标记，v21 截图保留了系统实际显示的标记；窗口按钮此前在 v20 的原生截图和 v21 的窗口无障碍树中核对过。

v21 完成 S09 的通用弹窗标题对齐与圆角，S12 的结果卡键盘操作栏可见性，S14 的弹窗进入／退出动效，S16 的生成登录提示合并与术语整理，S17 的提交区间距，以及 S13 的部分来源控件焦点恢复。焦点恢复覆盖画布、剪辑、设置和已接入的生成／个人／团队卡片及选择器；图库中的其他动态卡片和系统文件对话框返回焦点未逐项原生验证。`SEECUT_UI_PREVIEW_DIR` 从最终二进制生成 97 张软件渲染图，`keyboard-validation.json` 为 `passed`，验证了弹层隔离及最上层 Escape；登录后的结果卡和计费只由 fixture 表示。

离线 `cargo test -p concat --lib --no-default-features --features wgpu`、`cargo fmt --all --check`、`scripts/locales.py --check` 和 `git diff --check` 均通过；语言文件检查为 13 种语言每种 718 行、0 missing、0 stale。v20 安装版的双目的地真实 H.264 导出仍是本次输出路径的原生证据；v21 的导出业务代码与 v20 相同，未重复编码。Windows、登录后团队资产、真实服务端生成和计费仍未验收。

### v23 当前本地候选（2026-09-24）

本分支以 `--offline --no-default-features --features wgpu`、无调试符号配置重建，并安装到 `/Applications/Seecut.app`。候选包为 `native-candidate-v23/Seecut.app` 和 `Seecut-macos.zip`；安装版与候选包可执行文件 SHA-256 同为 `28a0c9e11bf8499bd6172a433c990155bf2eeca882634144601bf75af8a3dae8`，严格代码签名验证通过。v21 候选目录和旧安装备份在确认 v23 可打开后已清理。系统图标仍由 1024px 源图生成，含 256px 与 256@2x 尺寸及轻微阴影。

左侧导航顶部品牌图已移除，四个工作区图标上移；未登录账号的 `?` 默认头像改为圆形品牌图。`docs/screenshots/` 的 8 张浅／深色原生窗口截图已从 v23 安装版重新采集，覆盖生成、资产库、画布和剪辑。资产页截图的两个“拖拽验收”文件夹是隔离 HOME 的测试数据。屏幕共享角标属于当前 macOS 截屏状态，截图保留实屏画面。

个人素材拖拽图改为使用资产卡缩略图，单项大小为 160 × 100，多选时叠放显示。代码编译并随 v23 安装；自动化原生拖动尝试未使素材进入目标文件夹，也未捕获到拖动中的系统浮层，因此缩略图的实际拖动态与文件夹移动仍需人工实测，不能记为通过。个人素材移动范围的稳定 ID 逻辑已有对应单元测试通过。

软件渲染 fixture 从 v23 二进制完整生成 98 张图。`result-keyboard-validation.json` 返回 `passed`：Tab 27 次到第一张结果卡的隐藏入口，Enter 聚焦其“作为参考”，鼠标移至相邻卡片后再次 Enter，仍向第一张卡片发送 `task-reference`。原生 v23 中，画布“打开”菜单按 Escape 后关闭，焦点回到“打开”按钮；v21 原生包中画布／剪辑导出、剪辑导入和个人资产卡关闭预览后的焦点返回已记录，相关逻辑未在 v23 改动。`cargo fmt --all --check`、`scripts/locales.py --check` 与 `git diff --check` 通过；全量 111 项库测试为 v21 源码阶段的结果，v23 的新增 UI 与缩略图代码由构建及 fixture 覆盖。画布图片选择／移动／变换和调整滑杆问题单列后续讨论与实现，未归入本候选的已完成项。

完成截图与候选包后，检查目标目录没有打开句柄，再对本 checkout 执行 `cargo clean`，清理 3.5 GiB 可再生构建产物；旧 v21 候选和安装备份经路径与句柄核对后清理。fresh `df -kP /System/Volumes/Data` 显示可用 16,141,968 KiB，约 15.39 GiB。保留 v23 候选包与已安装应用，后续构建需要重新生成 `src/target`。

### v24 素材拖拽复核进行中（2026-09-24）

Slint 软件渲染 fixture 使用连续 `PointerPressed`、`PointerMoved`、`PointerReleased`，从资产卡拖到左侧“灵感参考”文件夹，恰好产生一次 `personal-drop-folder(fixture-0:3)`；拖到“全部素材”和空白位置均未产生移动回调。`native-ui-v24-drag-final-fixture/personal-drag-validation.json` 明确标注这是软件窗口的合成指针事件，仅证明 DragArea → DropArea → UI action。`personal-drag-thumbnail.png` 来自与正式 `Payload.preview` 相同的位图缩略图函数，尺寸 160 × 100；多选 2 项和 20 项的叠放图及数量反馈也在该轮 fixture 中生成。软件快照未显示系统拖拽浮层，不能据此判定原生鼠标拖动态通过。

定点库测试命令为 `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_DEV_INCREMENTAL=false SHERPA_ONNX_LIB_DIR=/Users/chenyunzhe/Documents/Codex_Project/SeeCut/Concat-main/src/target/sherpa-onnx-prebuilt/sherpa-onnx-v1.13.7-osx-arm64-static-lib/lib cargo test -p concat --lib --offline --no-default-features --features wgpu folder`。本任务执行会话 `43235` 返回 2 passed、0 failed、109 filtered：`cloud::reference_tests::personal_folder_drop_uses_selected_group_only_when_source_is_selected` 覆盖选中组、拖未选项单独移动、无效／无变化目标；`personal_library::tests::folder_move_persists_ids_skips_noop_and_rolls_back_failed_save` 覆盖稳定 ID、重载后分类、无变化跳过与保存失败回滚。执行输出仅存于本任务工具记录，没有独立磁盘日志。真实安装版从拖入文件夹到 `library.json` 更新及重启后保留，仍需原生验收。

后续源码已将多选数量角标改为应用现有的 Helvetica Neue SVG 文字，并使用 `Theme.accent`／`Theme.on-accent` 随浅深主题切换。此改动尚未完成构建和截图复核；`native-ui-v24-drag-final-fixture` 是此前位图数字角标的阶段证据，不能代表最新源码。重编译时 fresh `df` 最低降至 3,928,024 KiB，按低于 5 GiB 的门槛停止；在 `lsof` 无句柄后精确清理本轮旧二进制和测试产物。完整构建曾观察到约 8.4 GiB 的可用空间摆幅，恢复前需 fresh `df` 约 15 GiB 并核对活跃写入。v23 安装版与候选包继续保留，PR 保持 Draft。

### v26 当前 macOS 候选与原生拖拽验收（2026-09-25）

`native-candidate-v26/Seecut.app` 和同目录 `Seecut-macos.zip` 已从本分支离线构建、签名并打包；`/Applications/Seecut.app` 与候选可执行文件 SHA-256 均为 `b3fed073b2d438ea11d751777f24290d4ec73994a1116d0970fe1487c193ed8e`，`codesign --verify --deep --strict` 通过。系统图标与 v24 候选的 `.icns` SHA-256 相同；解包实查含 `icon_256x256.png` 与 `icon_256x256@2x.png`，继续保留轻微阴影。左侧导航顶端 Logo 已移除，工作区图标上移，未登录账户显示圆形品牌头像。v25 是本轮拖拽逻辑的验收包，v26 只补 macOS Cmd+Q 退出时的临时缩略图清理；两者的拖拽绘制代码相同。

原生实屏捕获证实 v24 的 160 × 100 拖拽浮层是纯色矩形（`native-v24-drag-second-inflight.png`）；同一回调输出的照片位图有 11,529 种颜色。Slint 1.17.1 的 FemtoVG 直接拖拽绘制对动态内存图没有可复用缓存键，本地对照把相同像素从文件加载后，原生浮层显示了照片。v26 因此将素材位图和同一拖拽路径上的通用 SVG 写入会话临时目录，再作为路径图像交给浮层；退出时清理。v26 的 `native-v26-drag-single-light-inflight.png` 再次截到了真实拖动中的照片缩略图，没有纯色色块。v26 的 Cmd+Q 退出后，实查本次 `.tmpxxc9u4` 拖拽图目录已消失。

| 原生手势 | 实际结果与证据 |
| --- | --- |
| 单项拖入文件夹 | v26 安装版将 `海边小屋.png` 拖到“拖拽验收 B”，截图为 `native-v26-drag-single-light-inflight.png`；隔离 HOME 的 `library.json` 写入 B 的稳定 ID `e6e9f877-46aa-41d4-b2ae-28cda91210be`，其他两项未分类。 |
| 选中组与数量角标 | v25 安装版选中视频与图片两项后拖到 A，`native-v25-drag-group-two-inflight.png` 显示照片叠放和蓝色 `2`；`library.json` 两项均写入 A。深色主题再选两张图片拖到 B，`native-v25-drag-group-two-dark-inflight.png` 显示橙色 `2`。 |
| 拖动未选项 | v25 在选中两项时拖动未选中的 `海边小屋.png`，`native-v25-drag-unselected-inflight.png` 为无数量角标的单项照片；库文件只移动该素材，选中视频仍保留在 B。 |
| 无效目标、取消与重启 | 拖到“全部素材”及内容空白处前后，`library.json` SHA-256 均为 `8310e4866141364901472d9f43c15b4b9ae2d3bf32b20b73f723468876b693ee`。重启 v25 后，原生 `native-v25-restart-folder-a-dark.png` 显示 A 为 2 项，`native-v25-restart-folder-b-dark.png` 显示 B 为 1 项，与库文件一致。 |

以上截图位于 `/Users/chenyunzhe/.codex/visualizations/2026/09/23/01a0cd9d-dd88-7012-bd9e-8103e508d601/`，均来自隔离 HOME 的公开演示素材。屏幕上的桌面人物浮窗属于其他应用，随系统区域截图进入画面，与 SeeCut 卡片或拖拽浮层无关；不将这些带浮窗的画面替换仓库 README 截图。v25 二进制的软件渲染 fixture 目录为 `native-ui-v25-theme-fixture/`，生成 103 张 PNG；`personal-drag-validation.json` 为 `passed`，覆盖单项像素细节、2／20 项和浅深角标，并明确只代表合成指针事件。此前定点库测试为 2 passed，覆盖稳定 ID、选中范围、无效目标及保存失败回滚，业务代码在 v26 未改动。v26 `cargo fmt --all --check`、`git diff --check`、离线构建和安装包原生单项手势均通过。

本轮未实测团队素材登录、真实生成与报价、Finder 混合文件拖入、大视频／慢盘／断盘、含音轨导出及 Windows 包；相关 D/R/S 结论沿用上文标出的证据与边界。画布图片选择／变换及调整滑杆留待单独的画布任务。当前 PR 保持 Draft，未合并或发布。
