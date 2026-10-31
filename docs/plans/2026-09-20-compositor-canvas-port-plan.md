# Compositor 画布移植方案（2026-09-20）

分支：`codex/compositor-canvas`（自 `codex/seecut-generation-accounts` @ 91d9a15 切出）
参考源码：`/Users/chenyunzhe/Documents/Codex_Project/SeeCut/compositor-reference`（robbietilton/Compositor @ main，MIT）

## 一、结论

**可以移植，且非常合适。** 但不是"嵌入"，而是"重写移植"：

- Compositor 是 SwiftUI + AppKit + Core Graphics 的 macOS 26 专属应用（92 个 Swift 文件、约 16.8k 行），
  UI 层（NSView/NSEvent/NSCursor/Core Image/vImage）完全不可跨平台复用；
- 但它的**文档模型、渲染算法、像素例程是纯逻辑**，与语言和平台无关；
- 它的架构哲学（规划与合成分离、CPU 参考实现 + GPU 快速实现）与 concat-render 现有设计**同构**，
  移植是"把一套成熟设计装进我们已有的骨架"，而非从零发明。

## 二、源方案架构解剖

### 2.1 分层（92 文件 / 16,755 行 Swift + C）

| 层 | 目录 | 内容 | 行数 |
|---|---|---|---|
| 画布视图 | Rendering/EditorCanvas.swift | 单文件 1,814 行：全部工具手势状态机 + drawRect 合成 | 1,814 |
| 文档模型 | Document/ | EditorSession（689）、BrushStroke（899）、图层/蒙版/选区/滤镜 | ~7,000 |
| 渲染管线 | Rendering/ | TiledLayerRenderer、DownsampleCache、LayerRenderer、MetalBrushCoverage | ~2,500 |
| 像素算法 | Rendering/*.c | AdjustPixels / HealPixels / WandPixels / ContentFill / NoisePixels / LevelsPixels / LensPixels / BrushPixels —— **纯 C，可直接编译** | ~1,500 |
| UI 面板 | UI/ | NativeLayerList（896 行）、各级 Sheet/Controls —— SwiftUI，需重做 | ~3,500 |
| IO | IO/ | ProjectStore（Codable 存档）、ImageImporter/Exporter | ~800 |

### 2.2 渲染管线（移植的核心资产）

1. **合成走 CPU Core Graphics**：`CanvasView.drawRect` 按 z 序把图层 CGImage 画进 CGContext，
   GPU 只用于一处 —— `MetalBrushCoverage`（compute shader 算笔刷覆盖率，tile 级存储，无整画布 GPU 分配）。
2. **DownsampleCache**：vImage Lanczos 半减链（每级严格 2×，最多 6 级，100M 像素 LRU 预算），
   保证缩小视图时锐利 —— 因为 CG 一步重采样在 4×~8× 缩小时会发软。
3. **TiledLayerRenderer**：笔刷编辑分块更新 —— 活笔划 256px 块、提交后 1024px 块，
   块对齐 halving 网格 + 边距，避免分块重采样的接缝与像素漂移。
4. **SeparableBlend**：Core Graphics 的 colorBurn/colorDodge 忽略源透明度（作者实测发现），
   所以这两个模式单独走 Core Image 回退。**移植时用 WGSL 统一实现正确的 PDF 语义。**
5. **混合模式 13 种**：normal/multiply/screen/overlay/darken/lighten/difference/colorDodge/colorBurn/
   hue/saturation/color/luminosity（后四种是非可分离模式，走 SetLum/SetSat 而非逐通道）。
   上游为 colorBurn/colorDodge 单独做了 Core Image 回退，因为它实测 Core Graphics 忽略源 alpha；
   移植时用统一的 PDF 32000 公式一次解决（`concat-canvas/src/blend.rs` 已实现）。
6. **笔刷不是简单叠加**（见上游 `docs/brush-performance.md`）：沿平滑指针路径扫连续圆头，
   对**光密度沿路径积分**再转覆盖率（早期版本取最大衰减，两条羽化边相交处会出现硬折痕）；
   软头间距 2.5% 直径、硬头 1.5%；永久密度存 float tile 缓冲，未定尾巴单独存放可整体替换；
   opacity 封顶整条笔划。抬笔同步提交不可变 RasterSnapshot（tile 共享 + 空间索引合并）。
   **这些细节必须原样移植，否则性能与观感都会退化。**

### 2.3 文档模型

- `EditorSession`（@Observable）持有 `CanvasDocument`：图层树（ImageLayer + LayerGroup 嵌套）、
  每层：像素（CGImage 或 RasterSnapshot 平铺快照）+ 非破坏变换（LayerTransform）+ 蒙版 + 裁剪蒙版 +
  调整图层参数（HueSat/Levels/Curves/Exposure/GradientMap/Grain）。
- `DocumentHistory`：值快照 undo（图层共享不可变 CGImage，零像素拷贝；100 条 / 256MB 上限）。
- 选区：`Selection`（矩形/椭圆/套索/多边形/魔棒）+ 浮动选区像素（FloatingSelection）。

### 2.4 上游存档格式（`.comp` v1–v6，P7 的移植清单）

上游 `docs/project-format.md` 记录了一份 package 格式，是移植持久化层时的权威参照：
`manifest.json` + `images/<layer UUID>.png`，sRGB 工作空间，图层自下而上排列，变换与像素分离存储
（嵌入 PNG 保留原始像素，变换只是数字），因此项目不依赖原始素材文件是否还在。
版本演进即功能演进史：v2 分组（`parentID`/`isGroup`，子节点引用已存在分组，数组顺序=兄弟顺序，
分组无图）、v3 每层 `opacity`/`blendMode`、v4 图层蒙版（8 位灰度**无 alpha**，白显黑隐，
归一化范围与图层矩形一致，允许 1×1 均匀蒙版）、v5 `maskSourceID`（即裁剪蒙版：引用另一图层的
活 alpha，"Option 点击取下方兄弟为基"，多个从属层可共享一个基）、v6 分组蒙版
（分组是 pass-through，分组蒙版与每个后代自身蒙版及外层蒙版相乘；裁剪蒙版覆盖率不受影响）。
上限：单边 30,000 px、源像素总量 1 亿、10,000 图层、manifest 4 MiB、单资源 512 MiB；
undo 历史与视口是 session-only，不存入文件。
移植取舍：JSON 结构用 serde 重写并保留我们的 id 语义（见 `concat-canvas/src/document.rs`），
不做 `.comp` 兼容读取；上表是**字段与校验规则的清单**，P7 逐条对齐。

### 2.5 功能清单（移植验收范围）

图层与组、混合模式×13、不透明度、图层蒙版（画/填/反/羽化/模糊/独立变换）、裁剪蒙版、组蒙版、
调整图层×6、合并/盖印、非破坏变换（移动/缩放/旋转/翻转/自由扭曲）、多选区工具、内容识别填充、
画笔（大小/硬度/不透明度/Shift 直线）、修复画笔、仿制图章、模糊工具、渐变、形状、吸管、色板、
色阶/曲线/色相饱和度/曝光/渐变映射/颗粒/反相、高斯与运动模糊、添加噪点、镜头校正、背景移除、
裁剪、画布大小、图像大小、多项目标签、JPEG/PNG/HEIC/TIFF 导入、JPEG 导出（带实时预览）。

## 三、目标技术栈现状（已具备的地基）

| 能力 | 现状 | 差距 |
|---|---|---|
| GPU 设备共享 | `concat/src/gpu.rs`：Slint 与引擎共享一个 wgpu 29 设备，monitor 帧零拷贝纹理（Metal/DX12/Vulkan 三分支已处理） | ✅ 直接复用 |
| 合成器骨架 | `concat-render`：plan/compositor 分离，`CpuCompositor` 参考 + `WgpuCompositor`，Layer 有 opacity/blend/placement/passes | 混合模式仅 6 种；无蒙版、无组嵌套、无裁剪蒙版 |
| 图像解码 | `concat-media`：FFmpeg（PNG/JPEG/TIFF/WebP，HEIC 取决于构建） | ✅ 基本够用 |
| 效果目录 | `concat-effects`：ShaderPass + 效果清单 | 与 Compositor 滤镜集合部分重叠，可互补 |
| AI 抠图 | `concat-vision`：ONNX cutout（DirectML/CoreML/NNAPI 加速） | 对标 Compositor 的 Remove Background/SubjectRemoval，已更强 |
| UI 设计系统 | Slint 1.17：theme/dark+light、primitives（knob/panel/segmented-control/number-field…） | 画布工具面板需新建，但质感基建已在 |
| 跨平台 | Windows DX12 / macOS Metal / Linux Vulkan 均为一等公民，Windows CI 已在跑 | ✅ Windows 适配是栈的固有属性 |

## 四、移植设计

### 4.1 新 crate：`concat-canvas`

```
crates/concat-canvas/
  src/
    document.rs     # ImageDocument / LayerNode(组+图层) / 蒙版 / 调整图层参数 —— 纯数据
    history.rs      # 值快照 undo（对齐 DocumentHistory：条目上限+字节预算+挂起合并）
    blend.rs        # 13 种混合模式：CPU 参考实现 + 对拍常量（已落地）
    pyramid.rs      # mipmap 半减链（GPU 原生 mipmap 替代 vImage Lanczos 链）
    tiles.rs        # 笔刷分块更新（256 活块 / 1024 提交块，对齐 halving 网格）
    brush.rs        # 笔刷覆盖 compute（WGSL 翻译 MetalBrushCoverage 的 continuousBrush）
    selection.rs    # 选区模型（路径 + 蒙版位图 + 蚂蚁线状态）
    pixel/          # 直接编译源方案 C 文件（cc crate），Rust 安全封装
      adjust.rs wand.rs heal.rs fill.rs noise.rs levels.rs lens.rs
```

关键决策：
- **GPU 合成替代 CPU 合成**。原方案 drawRect/CPU CGContext 是它的性能瓶颈所在；我们沿用 concat-render
  的双实现模式：CPU 参考实现保证正确性（`cargo test` 对拍），WgpuCompositor 扩展为支持
  蒙版/组/裁剪/13 种混合的画布合成器，全部走 fragment/compute shader。**性能预期优于原方案**
  （原方案只有笔刷覆盖率在 GPU）。
- **mipmap 替代 DownsampleCache**。GPU 原生 mipmap 生成即"每级严格 2×"的半减链，
  语义与 TiledLayerRenderer 的网格对齐约定天然一致；锐利缩小用线性 mipmap 采样 +
  最后两级间的各向异性过滤。wgpu 的 `Texture::create_view` + 一次 blit 即生成，比 vImage 链更省。
- **笔刷覆盖 WGSL 化**。MetalBrushCoverage 的 compute shader（SIMD uniforms、tile 级 buffer、
  无整画布分配）逐段翻译为 WGSL compute，语义 1:1。
- **蒙版 = 第二张纹理**。图层蒙版/裁剪蒙版/组蒙版在合成 shader 里作为采样纹理参与，
  CPU 参考实现同步实现以对拍。

### 4.2 UI 层（Slint）

```
concat/ui/canvas/
  canvas-pane.slint      # 画布视图：缩放/平移/网格/参考线；指针手势状态机
  tools.slint            # 工具栏：选择/移动/裁剪/吸管/画笔/修复/仿制/模糊/渐变/形状/魔棒/文字占位
  layers-panel.slint     # 图层面板（缩略图、混合模式、不透明度、蒙版指示、拖拽排序）
  adjust-sheets.slint    # 色阶/曲线/色相饱和度/曝光/渐变映射/颗粒 弹层（实时预览）
  brush-controls.slint   # 笔刷大小/硬度/不透明度
  export-sheet.slint     # JPEG 导出（质量滑杆 + 实时预览）
```

- 手势状态机是最大单块工作：原 EditorCanvas.swift 1,814 行（工具分发 + 修饰键 + 自动滚动 +
  光标管理 + 蚂蚁线定时器）。Slint TouchArea + pointer 事件可表达全部所需原语；
  自绘指针光标（原方案大量使用自绘 NSCursor）在 Slint 里用画布内 overlay 而非系统光标。
- 快捷键：Photoshop 约定，Cmd→Ctrl 在 Windows/Linux 自动映射（Slint 的 `.accent` 修饰键天然跨平台）。
- 质感：复用现有 dark.slint 主题与 primitives（knob/panel/segmented-control），与剪辑工作台同语言；
  浮动面板参考原方案 FloatingPanel 的收纳逻辑但用我们自己的 panel 样式。

### 4.3 与剪辑工作台的集成

- 新"画布"页签/模式，位于生成→剪辑流之间：生成结果/素材箱图片可"在画布中编辑"，
  编辑结果作为图层或盖印图回写时间线素材箱。
- 文档持久化：`ProjectStore` 的 JSON 文档格式重新设计为 Rust 侧 serde 模型
  （不做 .compositor 兼容，源格式是内部 Codable，无生态价值）。
- 复用 personal_library 的资产索引与缩略图管线。

### 4.4 Windows 适配清单

| 项 | 处理 |
|---|---|
| GPU | DX12 分支已在 gpu.rs 处理；WGSL 全平台一致，无 shader 分叉 |
| 光标 | 不用系统光标 API，画布内自绘 overlay（macOS 同样路径，少一套平台代码） |
| 快捷键 | Slint 修饰键抽象；文案显示 Ctrl/Cmd 按平台切换（i18n 已有机制） |
| 字体 | 现有 Synonym/Helvetica 栈已解决回退 |
| HEIC | FFmpeg 构建带 libheif 则支持，否则隐藏入口（能力探测） |
| 高 DPI | Slint/wgpu 均为 logical px，已有 monitor 先例 |

## 五、性能论证（不输原方案的依据）

**上游自报的实测基线**（`docs/brush-performance.md`，2026-09-12，4000×4000 文档、800 px 笔刷、
0% 硬度、100% 不透明度、原生 1000×1000 窗口 fit 缩放，Debug 构建，同步 CPU 计时）：

| 场景 | 改进前 | 改进后（上游现状） |
|---|---:|---:|
| 指针更新中位数 / p95（空白绘制层） | 6.54 / 11.98 ms | 2.64 / 3.70 ms |
| 抬笔（首笔 / 次笔） | 1058 / 1002 ms | 8.71 / 8.14 ms |
| 指针更新中位数 / p95（已有不透明 4K 层） | — | 2.80 / 5.38 ms，抬笔 5.2–6.2 ms |
| 40 px 笔刷 | — | 0.36–0.47 ms 中位数，抬笔 1.3–4.8 ms |

**这组数字就是验收线**：我们的移植若在同一场景下明显更慢，即为回归。

逐项依据：

1. 合成路径：原 = CPU CGContext 逐层 drawRect（每帧全画布 CPU 混合）；
   新 = GPU fragment shader 逐层混合 + 蒙版采样，显示器速率下 GPU 占用远低于 CPU。**优于原方案。**
2. 缩小显示：原 = vImage Lanczos 链（CPU，100M 像素缓存预算）；
   新 = GPU mipmap（显存，硬件生成）。质量同级（各向异性过滤下更好），成本更低。**持平或优。**
3. 笔刷：原 = Metal compute 覆盖率 + CPU tile 重组；新 = 同一算法的 WGSL 翻译 + 同样 tile 尺寸
   （256 活块 / 1024 提交块），密度积分与尾巴替换语义保持一致。**目标持平**，
   P5 的出口判据直接采用上表数字。
4. 像素滤镜（色阶/曲线/魔棒/修复）：原 = C 循环；新 = 同一批 C 文件直接编译（cc crate），
   热路径逐步迁 WGSL compute（魔棒/内容填充首迁，天然并行）。**起步持平，终点更优。**
5. CPU 参考实现只跑测试，不进发布路径（与 concat-render 现状一致）。

## 六、阶段划分

| 阶段 | 内容 | 状态与出口判据 |
|---|---|---|
| P1 文档模型 | document.rs + history.rs + pixels.rs + serde | ✅ 已落地：42 项单测（undo/redo/嵌套组移动/裁剪链接校验/序列化往返/PixelStore 回收） |
| P2 混合模式 | blend.rs：CPU 参考 13 种 ✅ + WGSL（P3） | ✅ CPU 参考落地：15 项测试含 colorBurn/Dodge alpha 语义、非可分离模式与规范勘误 |
| P3 GPU 合成 | 蒙版/裁剪/组/调整图层进 shader；mipmap 金字塔 | ✅ 已落地（`gpu.rs`，feature="gpu"）：ping-pong 逐层 fragment pass，11 项与 CPU 参考对拍 ≤1 字节；脏矩形上传 + Frame::id 常驻纹理 + mipmap（对应用户提出的四个性能问题）。踩坑记录：uniform u32 必须按原生字节写（`as f32` 位型损坏全部 gated 行为）；Overlay 分支按源色；SetSat 需写回原通道位。 |
| P4 画布交互 | 视口/导航 + Slint 画布 + 工具手势 + 蚂蚁线 | 🔶 大头已落地：P4a `viewport.rs`（CanvasViewport + Navigator，14 项单测）；P4b studio 集成（`canvas-pane.slint` + `panes/canvas.rs`，PaneKind.canvas，TouchArea→NavInput 单手势控制器跨平台，CanvasGpu 走 monitor 共享 wgpu 设备零拷贝上屏，工具栏 move/hand/zoom/fit + 缩放读数 + 空态打开图像）。剩余：变换 overlay + 蚂蚁线 + 工具内的绘制/选区手势（随 P5/P6） |
| P5 绘画引擎 | 笔刷 WGSL 覆盖 + tile 管线 + 仿制/修复/模糊 | 🔶 大头已落地：P5a CPU 参考（`brush.rs`）：连续光密度积分（8 点 Gauss–Legendre、coverage=1−exp(−density) 封顶 20）、永久密度浮点 tile 缓冲 + preview-only tail、向心 Catmull–Rom 自适应细分、硬头抗锯齿轮廓、source-over/擦除合成。P5b GPU 笔刷（`brush_gpu.rs`，feature="gpu"）：integrate/preview 两个 compute pass 复刻密度积分与 8-bit 预览，路径状态机（`PathState`）CPU/GPU 共用，与 CPU 参考逐字节对拍 ≤1（锁步 append/flush、跨 tile 跑动、硬头、tail 换段、flush 幂等、合成对照 7 项对拍全绿）。修复 P5a 潜伏 bug：update 的 tile 集合 settled/tail 合并后未去重，软头在 tail 与 settled 同 tile 时密度翻倍。P5c 已接线：canvas-pane 笔刷/橡皮工具（tool 3/4）手势→BrushStroke→变更 tile 从 stroke 前基线重拷+重印→GPU 脏矩形上传（无 GPU 回退 store 替换），像素级撤销/重做（Arc 快照零拷贝），托盘含尺寸/不透明度/硬度旋钮 + 8 色色板。剩余：异步读回（tile 管线）、仿制/修复/模糊 |
| P6 选区与工具 | 选区 5 种 + 魔棒/内容填充（C 编译进）+ 形状/渐变/吸管/裁剪 | 🔶 核心已落地：`selection.rs`——8-bit 覆盖 Mask（2×2 超采样光栅化矩形/椭圆/多边形套索）、魔棒（颜色距离容差 flood fill，连续/全局）、布尔运算（加/交/减/反选/全选/空）、bounds、fill（coverage 加权 source-over）/erase/move_region、吸管。9 项单测。UI 已接线：选框工具拖拽矩形选区（叠加层高亮）、魔棒点击、托盘"填充选区/清除选区/取消选择"按钮（有选区时出现），选区操作带像素撤销。剩余：套索/椭圆工具手势、渐变、裁剪、内容识别填充 |
| P7 UI 与集成 | 图层面板/调整弹层/导出；素材箱↔画布↔时间线流转；按 §2.4 对齐存档字段 | 🔶 图层面板已落地：canvas-pane 右侧 172px 面板——图层列表（walk 顺序 front-to-back 发布为 CanvasLayerData 模型）、眼睛可见性切换、行内不透明度旋钮、点选激活（工具作用层）、新建/删除/上移/下移；导出：合成帧 PNG 编码（png crate）+ 平台保存对话框（`platform::save_file`，rfd）。剩余：调整弹层、混合模式选择、图层分组 UI、素材箱流转、.comp 存档 |

每阶段独立可合并，CPU 参考实现对拍是每阶段的硬门槛（沿用 concat-render 的测试纪律）。

## 七、许可与合规

- Compositor 是 MIT（版权方 Wonder Assembly LLC）：并入 AGPL-3.0 单向兼容。
  完整 MIT 文本已登记在仓库根 `THIRD_PARTY_NOTICES.md`（"Compositor — the image canvas, ported"），
  每个源文件头的 `Ported from …` 注记标明出处。
- compositor-reference 目录仅为参考阅读，**不进入**仓库；移植产物全部是 Rust/Slint/WGSL 重写。

## 八、风险

1. **EditorCanvas 手势状态机**（1,814 行）是最大翻译块 —— Slint 事件模型表达力足够，但
   边缘案例（自动滚动、修饰键中途切换、光标锁）需要逐条对照移植，建议 P4 单独一轮。
2. 曲线/色相饱和度的 GPU 实现与 CPU 对拍可能有浮点容差 —— 参考实现容忍 1/255 级差。
3. Slint 大画布重绘成本：画布用独立 wgpu 纹理 + Image 元素呈现，避免走 Slint 场景图
   （monitor 已验证此路径可行）。
4. 工作量估计：P1–P7 全量约 15k–20k 行 Rust+Slint，多轮完成。
   已落地 P1 + P2 的 CPU 参考（`src/crates/concat-canvas`，约 1.3k 行含测试）。
