# Compositor 画布移植 · 完整进度文档（2026-09-21 更新）

分支：`codex/compositor-canvas`（仓库 `Concat-main/src`）
配套方案：`docs/plans/2026-09-20-compositor-canvas-port-plan.md`（架构依据与阶段拆分）
参考源码：`compositor-reference/`（robbietilton/Compositor，macOS/SwiftUI/MIT）

---

## 一、总览

目标：把 Compositor（macOS 独占的 Photoshop 替代品）完整移植到 SeeCut 技术栈
**Rust + Slint + wgpu**，支持 Windows，UI 质感复用现有设计系统，性能不降（CPU 合成 → GPU 合成）。

### 阶段状态总表

| 阶段 | 内容 | 状态 | 提交 |
|---|---|---|---|
| P1/P2 | 方案解剖与移植计划 | ✅ 已完成 | `8d04951` 前的规划文档 |
| P3a | 混合模式（blend.rs，13 种，PDF 32000 语义） | ✅ 已完成并测试 | 早期提交 |
| P3b | GPU 合成器（gpu.rs，全图层 fragment pass + CPU 对拍） | ✅ 已完成并测试 | `7a71f64` |
| P4a | 视口与手势（viewport.rs，Navigator 状态机） | ✅ 已完成并测试 | `b76c53d` |
| P4b | Slint 画布接入（CanvasPane 进工作区 seat） | ✅ 已完成并测试 | `3e70cb4` |
| P5a | 笔刷 CPU 引擎（brush.rs，光密度路径积分） | ✅ 已完成并测试 | `3e20e1e` |
| P5b | 笔刷 WGSL compute 版 + 逐字节对拍 | ✅ 已完成并测试 | `6098d0b` |
| P5c | 笔刷接入 canvas-pane（真实绘画） | 🔶 代码完成、编译通过，测试未跑完 | 工作区（未提交） |
| P6 | 选区与工具（矩形/椭圆/套索/魔棒/填充/吸管/移动） | 🔶 selection.rs 已写（9 测试），未注册未编译 | 工作区（未提交） |
| P7 | 图层面板 UI + 调整弹层 + 导出 + 完整集成 | ⬜ 未开始 | — |

测试基线：**concat-canvas 124 项**（gpu 变体）/ **105 项**（默认变体）全绿，clippy `-D warnings` 干净；
app crate（concat，wgpu 变体）编译通过。P5c/P6 新增测试见对应小节。

---

## 二、已完成阶段明细

### P3a 混合模式（blend.rs）
- 13 种混合模式统一按 PDF 32000 公式实现：可分离模式逐通道，非可分离
  （hue/saturation/color/luminosity）走 SetLum/SetSat。
- 上游为 colorBurn/colorDodge 单独做 Core Image 回退的坑（Core Graphics 忽略源 alpha），
  在统一实现下不存在。

### P3b GPU 合成器（gpu.rs，提交 `7a71f64`）
- 每图层一个 fragment pass 的 GPU 合成管线，常驻纹理缓存（`Resident`）+ mipmap，
  缩放平移不重传像素；`DirtyRect` 支持局部上传。
- 调整图层为片段 pass；`PixelStore::replace` 零拷贝交换。
- **CPU 始终是对拍 oracle**：每通道 ≤1 字节容差，11 项对拍测试。
- 期间修复：`set_sat` WGSL 中 `let t = order[j]` 被推断为 i32 致 Sint/Uint Add 校验失败
  （显式 `: u32`）；OVERLAY 分支误用 `hard_light(cb,cs)`，应为交换参数的 `hard_light(cs,cb)`。

### P4a 视口与手势（viewport.rs，提交 `b76c53d`）
- `CanvasViewport`：zoom 0.001..=32、fit 96pt 边距、缩放锚定指针；
  `document_point`/`view_point` 坐标互换（含设备缩放）。
- `Navigator`：跨事件手势状态机（PanPress/Move、ZoomPress/Move、Release{option}、
  Scroll、Pinch、Space、Fit），单参 `apply(NavInput)`；14 项测试。
- 修复：点击缩放静默失效——Release 测试漏传文档尺寸；重构为 Navigator 自持 `set_document`。

### P4b Slint 画布接入（提交 `3e70cb4`）
- `canvas-pane.slint` 复用 Pane/IconButton/Icon/Theme/I18n；右侧工具栏
  move/hand/zoom/fit，左侧缩放读数，空态"打开图像"。
- `TouchArea → NavInput` 映射；`slint::Image::try_from(wgpu::Texture)` 零拷贝上屏，
  复用 monitor 的共享 device/queue（`CanvasGpu::with_device`）。
- `PaneKind` 增 `canvas`；studio.rs/lib.rs 完成 Msg 分发与 publish。

### P5a 笔刷 CPU 引擎（brush.rs，提交 `3e20e1e`）
- `BrushSettings{diameter,opacity,hardness,color,erasing}` + `BrushStroke`：
  - 软头沿路径**光密度积分**（8 点 Gauss–Legendre，coverage = 1−exp(−density)，封顶 20），
    间距 2.5% 直径（硬头 1.5%），稀疏/稠密采样等价；
  - 256px tile 浮点永久密度缓冲 + provisional tail（只预览不累积，抬笔即换）；
  - 向心 Catmull–Rom + 0.2px 自适应细分；硬头抗锯齿轮廓（max 覆盖率）；
  - source-over 绘画 / 擦除缩 alpha，opacity 封顶整条笔划。15 项测试。
- 修复：composite 误用 `tile.x` 作 tile 索引致偏移 tile 全跳过（改 `tile.x / TILE_SIZE`）。

### P5b 笔刷 WGSL compute + 对拍（brush_gpu.rs，提交 `6098d0b`）
- CPU/GPU 共用 `PathState`（samples→settled/tail 状态机，`take_settled` 保证每段恰好积分一次）。
- GPU 两个 compute pass：**integrate**（settled 段积分进全层 f32 密度平面）+
  **preview**（settled∪tail∪旧 tail tile 并集重写 8-bit 覆盖，u32 槽/像素读回转 u8）。
- 段缓冲 16B/vec4 按需扩容；层上限 32M 像素（两块 128MiB storage buffer）。
- **7 项锁步对拍**（每步 append/flush 后逐字节 ≤1）：软头单击、跨 tile 弯线、稀疏/稠密等价、
  硬头、tail 换段、flush 幂等、composite 对照。
- **对拍揪出 P5a 潜伏 bug**：CPU `update()` 把 settled/tail/旧 tail 的 tile 合并后没去重，
  软头在 tail 与 settled 共用 tile 时密度翻倍（硬头用 max 无感，故旧测试全绿）。已修（sort+dedup）。
- 排查方法论（值得复用）：先用 `bitcast<u32>` 把 shader 内 `seg_density` 原始 f32 读回对比——
  完全一致排除 shader 数学；再单像素插桩暴露双积分。**对拍失败先别怀疑 GPU 端，
  参考实现自己的隐藏 bug 同样表现为"对不齐"。**

### P5c 笔刷接入 canvas-pane（工作区，待测试收尾）
**设计**（Slint 传视口坐标，Rust 经 `document_point` 转文档像素——与 nav 手势同一模式）：
- 工具扩为 5 个：move / hand / zoom / **brush(3)** / **eraser(4)**；eraser = brush + erasing 位。
- 笔刷流程（`panes/canvas.rs`）：
  - `BrushPress`：快照层像素（`Arc<Frame>` 廉价克隆）→ 建 `BrushStroke` → 一次性拷贝 scratch 帧
    → push 撤销栈 → 首点落笔；
  - `BrushMove/Release`：每事件只对**变更 tile** 做「从 pre-stroke 基准重拷 → composite_tiles 盖章」
    （覆盖率是累积量，tile 必须从基准重建而非叠盖，否则二次上色），GPU 路径逐 tile
    `upload(DirtyRect)` 脏矩形、无 GPU 回退路径整帧进 store；
  - 撤销/重做：像素级栈（undo_stack / redo_stack 存 `Arc<Frame>`），undo 全量重传一次可负担；
    `store.replace` 零拷贝换入。
- UI（canvas-pane.slint）：托盘加 brush/eraser 按钮、**笔刷选项条**（tool≥3 时显示：
  尺寸/不透明度/硬度三个 Knob + 8 色 Palette 色板，`Palette` 全局与 Rust `PALETTE` 同序同色）、
  undo/redo 按钮（icons.slint 新增 lucide undo-2/redo-2）；手势层 painting 状态分支
  （左键 tool≥3 → brush-press/move/release，中键仍平移，双击仍 fit）。
- 新增回调链：canvas-pane → seat → Editor 全局 → lib.rs `on_canvas_*` → CanvasMsg → pane。
- locales：en/zh-Hans 增 "Brush tool"/"Eraser tool"（"Undo"/"Redo" 已有）。
- 新增测试 6 项：单击落墨（中心饱和）、undo/redo 往返、擦除清 alpha、画布外点击不起笔、
  视口坐标转换（200% 缩放锚定）、工具夹取。
- **状态**：`cargo check`（wgpu 变体）通过；测试运行时磁盘写满中断，**尚未跑完**——
  这是当前第一件待办。

### P6 选区与工具（工作区，未注册未编译）
`concat-canvas/src/selection.rs` 已写完：
- `Mask`：8-bit 覆盖选区蒙版（软边支持）；矩形/椭圆/套索多边形以 2×2 超采样栅格化，
  feather 走 smoothstep；`from_magic_wand` 颜色距离容差泛洪（contiguous/全局两种）；
  `bounds()` 紧包围盒（编辑成本的 guard）；add/intersect/subtract/invert 布尔运算。
- 编辑：`fill_region`（按覆盖度 source-over，边缘半混）、`erase_region`（按覆盖度缩 alpha）、
  `move_region`（先 lift 后落，防自覆盖；出画即丢）、`pick_color` 吸管。
- 测试 9 项：矩形裁剪、椭圆中心/角、套索、布尔组合、魔棒连续/全局、填充半覆盖混合、
  擦除、移动、吸管。
- **待办**：注册进 lib.rs、编译过测；pane 接线（marquee/wand 工具手势、选区可视化描边、
  Fill/Delete/移动操作的消息流）。

---

## 三、验证与环境约束

### 验证命令（环境限制下的一套）
```bash
# concat-canvas（skia 因网络不可构建，统一走 wgpu 变体）
cargo check  -p concat-canvas --no-default-features --features gpu
cargo test   -p concat-canvas --no-default-features --features gpu
cargo clippy -p concat-canvas --no-default-features --features gpu --all-targets -- -D warnings
# app crate（需要 pkg-config：ffmpeg 依赖）
PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH" \
PKG_CONFIG_PATH="/opt/homebrew/lib/pkgconfig" \
cargo check -p concat --no-default-features --features wgpu
```

### 环境坑（备忘）
1. **skia-bindings 源码构建需连 googlesource**（网络不可达），预编译源也不可达——
   默认 skia 构建保持原样未动，验证一律走特性变体。
2. **pkg-config 不在默认 PATH**：app crate 构建必须带 `/opt/homebrew/bin` 与
   `PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig`。
3. **磁盘反复告急**（本轮 98% → rustc SIGBUS 一次）：`target/debug/incremental`
   与过期 hash 副本可随时清（registry 完整可离线重建）；**构建前先 `df`**；
   当前可用约 11GB，target 约 5.6GB。
4. **同文件并行 Edit 会静默丢失**（本轮又丢 3 处：mod 声明、字段、测试行）——
   务必串行编辑、改后 grep 复核。
5. locale JSON 重排时注意：`json.dump(..., sort_keys)` 会整文件重排（本轮 diff 70 行/文件，
   净增仅 4 条，可接受但需知情）。

---

## 四、剩余工作（按序）

1. **P5c 收尾**：跑完 app crate 测试（canvas 6 项 + 回归）→ clippy → 提交。
2. **P6 完成**：selection.rs 注册编译过测 → pane 接线选区工具（工具按钮、手势、
   选区蚂蚁线/描边可视化、Fill/Delete/吸管/移动消息流）→ 提交。
3. **P7**：
   - 图层面板 UI（列表/可见性/不透明度/混合模式/重排/分组），接 `DocumentHistory`
     结构快照与像素快照统一的历史栈；
   - 调整图层弹层（HueSat/Levels/Curves/Exposure/GradientMap/Grain，GPU 片段 pass 已就绪）；
   - 导出（PNG/JPEG；`.comp` package 格式按上游 `docs/project-format.md` 移植：
     manifest.json + images/<UUID>.png，sRGB，变换与像素分离）；
   - 完整集成回归（开图→绘画→选区→调整→导出全链路）。
4. **质感补遗**：透明棋盘格背景（slint 一层 pattern）、笔刷光标预览、HiDPI/压感
   （PointerEvent 压力字段）接入笔刷直径。

## 五、风险与注意

- **性能目标**（上游实测抬笔 <20ms）依赖 tile 级脏矩形上传，P5c 已按此实现；
  正式数字要等真机手测。
- `move_region`/浮选区是简化版（无自由变换插值），自由变换（旋转/缩放采样）在 P7
  结合 `LayerTransform` 决定是否上 GPU。
- 内容填充（ContentFill C 算法）与仿制/修复/模糊属 P6/P7 边缘，当前排期为 P7 可选项。
