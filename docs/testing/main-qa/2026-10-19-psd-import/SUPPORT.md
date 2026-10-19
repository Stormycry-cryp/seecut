# C04 PSD 分层导入范围与实现审计

本实现依据 [Adobe Photoshop File Formats Specification，2019-11](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) 独立编写。没有引入 PSD 解析依赖，也没有复制第三方解析器代码。依据格式段长度和类型读取，输出原有 ImageDocument / PixelStore / LayerGroup。

| 项目 | 当前行为 |
| --- | --- |
| 文件 | PSD v1，8 位 RGB，3/4 个文件通道；每层 RGB、可选 alpha 与单个位图蒙版 |
| 压缩 | Raw 与逐行 PackBits RLE；图层及合成图段均校验。ZIP、ZIP prediction、PSB、16/32 位和非 RGB 拒绝 |
| 图层 | 保存原始矩形对应的位置，无自动居中/缩放；保持背到前顺序、opacity、隐藏状态和 Unicode 名称；缺少 Unicode 块的非 UTF-8 旧名字拒绝 |
| 组 | `lsct`/`lsdk` 普通嵌套隔离组，严格配对；组记录自身须无栅格内容；组显隐、opacity、受支持 blend 与基本位图蒙版保留 |
| 混合 | Normal / Multiply / Screen / Overlay / Darken / Lighten / Difference / Color Dodge / Color Burn / Hue / Saturation / Color / Luminosity，逐个显式映射 |
| 位图蒙版 | 基本 20 字节 mask 记录、黑/白默认值、禁用和相对矩形；保留画布外样本、规范为文档坐标，建立 layer anchor，导入后随图层移动；可绘制、启停或移除 |
| 原生保存 | 含带边界位图蒙版时写 `.comp` v7，RGB 与覆盖位图存 PNG；v1–v6 继续可读。带新语义但伪装旧版本的文件拒绝 |
| 导出 | 使用已有 CanvasExportJob 生成 PNG 并登记全局资产库；同一登记 ID 重试去重 |
| 明确拒绝 | Pass Through、未知 blend、剪贴图层、非默认 Blend If、非默认填充 opacity/挖空/特殊混合、调整层、文字、形状、矢量蒙版、智能对象、图层效果、蒙版密度/羽化/反相、全局蒙版、未知附加语义块 |
| 非渲染元数据 | 识别并跳过图层 ID、标签、名称来源、版本、metadata、参考点、编辑保护；不承诺保留 Photoshop 编辑器私有设置 |
| 色彩管理 | 按存储 RGB 数据进入现有合成器；不应用 ICC 转换。成功后的折叠详情说明限制，检测到 ICC 时另行提示；不宣称 Photoshop 色彩管理或任意 PSD 的像素保真 |
| 扁平化/写回 | 没有实现，也不以合成预览冒充分层导入。PSD 合成图只验证完整性，不用于替代图层 |

## 候选实现审计

审阅 [chinedufn/psd](https://github.com/chinedufn/psd/tree/28357a29414f940b6272b71831b3273f57f267bd)，固定 SHA `28357a29414f940b6272b71831b3273f57f267bd`（MIT / Apache-2.0，MIT copyright 2018 Chinedu Francis Nwafili）。`src/sections/layer_and_mask_information_section/mod.rs` 397–399 行跳过 mask data，290 行 ZIP 进入 `unimplemented!`；未作为依赖接入。该源码仅保留在本地审计目录，未进入产品。

[psd-tools](https://github.com/psd-tools/psd-tools/tree/b1b454baa5fdc4d72e26b9b4bd29b2e8ad1fb93b) 固定 SHA `b1b454baa5fdc4d72e26b9b4bd29b2e8ad1fb93b`，MIT。审阅 `src/psd_tools/api/psd_image.py` 的 `_init`：从 bounding divider 建组，再由 folder record 结束/命名，确认记录到背到前树的映射。只复用两份测试 PSD，源码不进入产品；fixture 目录保留来源、许可和摘要。

## 资源与事务

读取上限 64 MiB，先解析记录并计算预算，再分配通道/像素。限制 512 条记录、每层 256 个附加块、16 层组深度、1000 张位图、GPU texture limit 与 PSD 30000 尺寸上限；坐标、段长、通道 ID、RLE 每行和整数运算均检查。峰值按输入 + 累计解压通道 + 各位图 RGBA/上传副本 + `canvas_pixels × 64 × (max_group_depth + 2)` 预留，超过 512 MiB 拒绝。这是保守的单次导入预算，旧画布仍由已有预算管理；没有宣称整个应用内存不超过 512 MiB。原生读取队列串行解析，取消令牌在排队、逐记录和 RLE 行之间检查。

现有脏画布确认通过后才创建后台任务。临时结果绑定唯一操作 ID、Studio session generation、canvas generation、revision、单调 revision clock、binding epoch 和工程路径。完成时再次检查未完成编辑/待打开状态；取消、新操作、切工程或编辑后的结果不替换当前文档。正常 UI 撤销/重做导致 revision 变化时也取消任务。全部通过后一次替换文档与 PixelStore，失败仅更新错误提示。

界面入口复用“打开图像”文件选择器并增加 `.psd`；后台导入卡提供取消，错误/成功详情默认折叠并限制滚动区域。无新前台测试或 UI 截图。
