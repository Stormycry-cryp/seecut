# 当前剪辑监看帧读取协议 v1

状态：候选接线与验证中。此协议读取原生 Monitor 已采用的图像，服务 Agent 对当前内容的判断。它与 Canvas 的 preview、视频导出独立，不修改工程、播放或选择，也不产生业务 operation。

## 请求与上下文

原生能力为 clipPreviewProtocolVersion:1。仅 Clip 文档范围提供 clip_preview；模型不支持图片时隐藏该工具。旧宿主缺能力或缺 adoptedFrame 时不能取得替代图。

请求为封闭对象：projectId、documentSessionId、revision、contextRevision、frameId，以及可选 maxEdge。maxEdge 为 1..256 整数，默认 256。frameId 只能引用当前 Context 中的已就绪帧，形如 clip-frame-v1- 后接 64 位小写十六进制摘要；不能传时间、路径、效果、图像字节或来源声明。

Context 的 view.clip.preview.adoptedFrame 使用以下两种封闭形式：

- ready：state、version:1、source:adoptedMonitorFrame、frameId、generation、revision、timelineId、renderedFrameTimeSeconds、sourceWidth、sourceHeight。时间来自实际 PreviewSources 的量化结果，尺寸来自该次已采用图像；不以当前播放头补造。
- unavailable：state:unavailable、version:1、reason；reason 仅为 playing、busy、pending、noFrame 或 unsupported。未知、尚未采用和明确不可读取分别表达。

ready 要求暂停、空闲、无待处理帧、原文档及代次/时间线/渲染输入相同，并与当前 Context 的版本和图像尺寸一致。完整帧元信息进入原生 viewStateId。播放期间可继续显示同来源的较早画面，但 Context 始终 unavailable，不为 Agent 自动暂停。

## 捕获和返回

GPU 路径在可信 UI 回调内，用原 Device/Queue 立即提交对当前最终纹理的整帧缩略采样和独立 staging 复制；随后原 ring 纹理被覆盖也不能改变此次结果。每个实际 Device 由 Host 持有唯一长期 reader，单件未物理回收读取。UI 仅非阻塞 poll，PNG 编码在 worker 完成。

取消或应用 2 秒期限结束后不得交付晚到像素；物理槽仍等实际队列完成再释放，不能反复重建 reader 绕过。该期限不承诺中断驱动调用。CPU 路径仅读取原先保留的 RGBA，源像素最多 4194304；GPU 源边长最多 16384。GPU 不可用时不重新合成 CPU 图代替。

成功仍使用 status:ok/data 的原 Response 外层。data 包含 version、tool、source、appInstanceId、clientId、projectId、documentSessionId、revision、contextRevision、selectionRevision、viewStateId、frame、playheadSeconds、width、height、mimeType、byteLength、pngSha256、capturedAtUnixMs、viewportCapture:false、includesOverlays:false。frame 与 Context 的 ready 完整相同，playheadSeconds 与实际帧时间分别保留。

输出为整帧等比例缩略，使用整数向下取整且每边至少 1，不放大。RGBA8、非交错 PNG 最多 512KiB，以适应既有 1MiB IPC 消息上限。base64 仅存在于原生 IPC 中；正常 MCP CLI 从 text 和 structuredContent 删除该字段，转成恰好一个 image/png content。错误不保留原 data 或 image。

## 授权和运行时核对

原生捕获与编码后交付均核读取及媒体授权、账户、会话、实例、原文档、版本、选择、view 和 frame；发生变化则丢弃字节，不要求写权限或命名写许可。

运行时先读取当前 Context、核请求和视觉能力、协商协议，再次读取 Context 后发送原 frame 查询。返回后核封闭元信息、原身份、完整 frame/view、PNG 字节数及 SHA256、真实格式和尺寸、原始 chunk 名称/CRC、完整 IDAT 与有界解压；随后重新核视觉能力、授权与 Context，再交给模型。原 Canvas preview 和 media-store 行为保留，仅此工具增加严格 PNG 校验。正常图沿既有 media-store 保存和恢复，不在文本或会话存储重复保留 base64。

运行期替换 LUT 或效果 Catalogue 时，现有 Monitor 计划和 ReaderPool 可能仍保留旧资源。本批保存原 Host 的初始 Catalogue 身份，替换后明确 unsupported，重启 Host 后恢复；不把更新后的来源戳贴到旧缓存结果上。活动抠像或画笔任务未结束时也拒读。

## 验收边界

该图只代表该次已采用的 Monitor 合成结果，不含 Slint 选框、面板或视口裁切，也不证明操作系统呈现、色彩管理、完整成片或模型已观看。协议/runtime 合成测试、真实离屏 GPU、原生无窗口测试、真实窗口和模型验收分别记录。普通 App 页的 filteredList 仍不等于屏幕可见卡片；本批不扩展该能力。
