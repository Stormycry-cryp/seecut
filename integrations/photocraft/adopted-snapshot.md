# 画布采用帧快照合同

`seecut.snapshot` 读取完整编辑器实际提交绘制的文档材料，供后续 SeeCut Agent 看图适配。它是内部只读协议，尚未接入模型工具、父级 MCP 或 Task。权限、图片预算和交付前的新鲜度检查仍由 SeeCut 原有链路负责。

## 来源与身份

请求严格拒绝未知字段：

```json
{
  "requestId": "photo-read-1",
  "expected": {
    "hostInstanceId": "8e88a522-e405-40cd-9ef0-56b530ec0c21",
    "docId": 1,
    "documentGeneration": 1,
    "revision": 1
  },
  "maxEdge": 256
}
```

请求 ID 为 1..128 个 ASCII 字母、数字或 `-_.:`；`maxEdge` 为 1..256。文档身份沿用实时 Session 的 host、DocId、generation 和 revision，原生整数不经过 JavaScript Number。

实际 CPU/GPU 主画布绘制分支交出其使用的不可变 Document Arc；在同一分支冻结身份、帧号、视口、画布、显示来源、几何、活动图层与选择。无法取得已完成且一致的材料时明确拒绝，不额外读取另一次文档状态拼成图片。普通悬停可以采样，输入、释放、滚轮、缩放、临时编辑、预览、后台重绘和动画帧会被拒绝。

成功结果为 `kind=adoptedDocumentComposite`、`adoptionStage=uiPaintSubmitted`。图片是该采用帧材料的透明底全图合成缩图，含 sRGB ICC；网格、标尺、对话框及选框不属于图片。它不提供窗口截图、GPU present 或用户已看到的证明。返回明确标记 `returnedHistoricalFrame=true`，调用方须在交付和修改前检查当前身份，不能把历史材料当作当前授权。

## 几何、选择与像素

结果保留实际 `viewportId`、`canvasId`、`capturedFrame`、绘制矩形与 pixels-per-point、缩放、旋转、翻转、宽高比、显示/目标相机和精确可视四边形。四边形的 AABB 与裁剪到文档后的 AABB 单独返回，不用渲染损伤区域冒充可视边界。

选区覆盖率、活动图层、有序已选图层从同一材料计算；`coverageHash` 区分 None 与 Some，`selectionStateId` 同时绑定图层选择。返回 `snapshotId`、`viewStateId`、像素/PNG/ICC 哈希。文档、层、视口及画布的 u64 值须在父级适配时映射成无损字符串或受约束的不透明标识。

PNG 的 `pixels.kind=documentComposite`，输出为 straight-alpha RGBA8；`documentPixelsPerOutputPixel` 用最终尺寸计算。它描述文档坐标中的像素，不能作为显示器色彩转换后的像素使用。

## 首版范围与资源限制

首版支持 RGB/U8、默认 sRGB、普通 raster 与有限 Normal/pass-through group。复杂图层、效果、智能对象、蒙版、特殊通道、proof/HDR、artboard、pattern/crop 显示及无法证明颜色来源的模式会拒绝；编辑器原有功能保留，采样不会为获得图片关闭这些模式或扁平化文档。

源图最多 4,194,304 像素，单边最多 4096；最多 64 层、8 层嵌套、32 个 surface，引用 tile 预算 32 MiB，每个 surface 最多 128 个 tile；源面积乘 raster 数最多 8,388,608。PNG 最多 512 KiB，完整回复小于 1 MiB，编码前后均检查大小。

UI 线程只冻结 Arc 与小型材料。首次有效请求才创建一个单线程 Rayon CPU worker 和一个期限线程；合成、选区扫描及 PNG 编码在后台进行。全局只允许一个等待或计算中的请求，没有请求队列或按请求新增线程。

等待采用帧最多 2 秒，从 UI 入队计时；总期限最多 5 秒，从真实控制套接字读完有界请求行、解析 JSON 前开始，跨派发队列保留同一截止时间。普通控制请求原来的 60 秒期限保持不变。回复中的 `capturedAfterUiEnqueueMs` 和 `captureAgeAtReplyMs` 明确各自时间基准，不能推算未经记录的 present 时间。

取消、过期或断开后不交付图片。已经开始的 CPU 合成无法安全强杀；它在真正结束前继续占用唯一计算槽，后续请求返回 busy，避免超时后不断新增 worker。上述输入与并发上限不构成复杂合成的硬 CPU 终止保证。

## 验证记录

2026-10-23，主代理在固定五补丁源码上实际执行 Rust 1.95 library 回归：引擎 1 项、UI 38 项通过，0 失败、0 忽略；包括真实 CPU/GPU 离屏绘制、文档材料与身份一致性、选择、拒绝条件、期限和单 worker 生命周期。GPU 用例显式包含在执行中，没有仅保留 ignored 状态。

两项真实控制套接字回归也已通过：UI 未消费请求时在 5 秒到期并保持帧配对，UI 响应端断开时返回 unavailable。完整 desktop/CLI 重建和 Linux Xvfb 进程联测另行记录。通过离屏回归不代表本机窗口交互、模型看图、父级 MCP/Task 或正式分发已通过。

## 父级交图前的剩余条件

现有 `seecut.state` 不能确认历史快照仍对应当前观看内容：View 序列化没有实际 display camera，控制回调发生在布局和输入前；平滑滚动还可能在没有新 raw event 的帧改变画面。frame 增长与否、相同文档版本或目标相机相同都不能代替新鲜度证明。

下一步需要复用真实绘制的 FrozenGeometry 和原文档/选择依据，为原 capture 保留短期有界观察；验证等待一次真实绘制，并在该帧相关输入和面板操作结束后结算。相同内容的空闲重绘可以通过，相关状态改变、过期或无法证明一致时不发布图片。该观察只用于读取核验，不创建权限或持久账本。

发送时沿原 inputId 冻结账户、许可、文档和人类来源；异步采样不能换用后来活动的文档或倒填成点击时的画面。父级 MCP 返回图片前、runtime 准备模型输入前分别复核原绑定和读取许可；失效时均不交付 image。此层尚未实现，当前不宣称 Photo 图片已能交给模型使用。
