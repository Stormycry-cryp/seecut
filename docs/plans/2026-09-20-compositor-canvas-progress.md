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

---

## 六、P8：调整图层 UI、.comp 工程、质感补遗（2026-09-21）

### 1. 调整图层弹层与参数编辑（全栈打通）
- GPU 侧本轮确认已完备：`gpu.rs` shader `adjust_one` 七种 pass_kind
  （Invert/Exposure/Levels/HueSat/GradientMap/Grain/Curves-LUT）+ CPU
  `apply_one` 对拍。缺的只有 UI 与接线，本轮补齐。
- `canvas.rs`：`CanvasMsg::AdjustmentAdd(kind)/AdjustmentParam(index, value)`；
  新调整插到活跃图层正上方（子数组 active 位置 +1，嵌套/无活跃则置顶），
  成为活跃行；绘画目标回落到最上层图像层。
- 种类编号即边界契约：1 Invert、2 Exposure、3 Levels、4 Hue & Saturation、
  5 Gradient map、6 Grain（7 Curves 文档可往返、弹层暂不出——曲线要手柄不要旋钮）。
- 参数发布：`adjustment_state() -> (kind, [(label, value, min, max)])`，
  Slint 侧 `CanvasAdjustmentParam` 行 + 小旋钮（Knob 26px），改完按索引写回
  并 clamp 到参数自身量程。
- UI：图层面板头部 sliders 按钮 → 弹出菜单（六类）；活跃行是调整图层时
  面板底部出现参数旋钮排。弹层开关是面板本地状态，外点关闭层在其下方。
- agent 接口同步补齐：`agent_adjustment_add / agent_adjustment_param /
  agent_adjustment_state / agent_adjustment_kinds`。

### 2. .comp 工程格式（Concat 自有格式 v1）
- 结构：`<name>.comp/manifest.json`（`concat-project:1` + 画布尺寸 +
  `ImageDocument` 原样 serde JSON，含全部 id/变换/调整参数）+
  `images/<pixel-id>.png`（图层与蒙版统一，按 `collect_pixels` 清单）。
- 保存：兄弟临时目录（`.tmp-<pid>`）暂存 → `replace_package` 三步换入
  （旧包先让位 `.old`，新包就位后才删旧包；中途失败旧包复位）。
- 打开：manifest 校验（版本/尺寸/`doc.validate()` 剪裁链）→ 逐 id 解码
  PNG → `PixelStore::restore`（新原语：按保存的 id 复位、计数器越过最大 id，
  再铸不撞车）→ 树与像素零重铸，往返后再保存的树字节一致。
- `PixelId` 补 `Ord/PartialOrd/as_u64()`（文件名与排序需要）。
- UI：面板底部 save 按钮（对话框），`CanvasMsg::SaveComp`；打开按路径分发
  （`.comp` 目录走 load_comp）。agent：`agent_save_comp / agent_open`。

### 3. 质感补遗
- **透明棋盘格**：`checker_image(w,h)` 按文档像素生成 16px 方格（(0,0) 亮，
  上限 2048 后拉伸），开图/载工程时生成一次随 publish 发布；stage 内
  image-fit:fill 铺在合成帧之下——方格随缩放与画面像素同步缩放，
  文档空间语义正确，零每帧成本。
- **蚂蚁线**：Slint Path 无虚线（只有 TextStrokeStyle），改用自适应周期
  短划线方案：`Ants` 组件四边 `for` 铺白色 dash（周期 = max(8, span/80)
  px，封顶每边 80 段），相位由 Rust `slint::Timer`（120ms）步进
  `Editor.canvas-ants` 0..3，每步 1/4 周期 → 无缝爬行。
- **压感：被上游阻塞**。核实 slint 1.17 `PointerEvent`（i-slint-common
  builtin_structs）仅 button/kind/modifiers/touch_finger_id，无 pressure。
  待上游暴露后接入笔刷直径/流量（任务 #17）。

### 4. 验证
- concat-canvas（gpu 变体）：**134 全绿**（+restore 原语 1 项）；
- concat（wgpu 变体）：**58 全绿**（+调整图层发布/写入 1 项、.comp 往返 1 项）；
- clippy `-D warnings` 双 crate 干净（is_multiple_of、nonminimal_bool 两处修正）；
- i18n：12 个新词条 × 13 个语言全量补齐（顺序保持，diff 每文件 +13 行），
  覆盖测试通过。

### 5. 新增/改动文件
- `concat-canvas/src/pixels.rs`（restore + Ord/as_u64 + 测试）
- `concat/src/panes/canvas.rs`（调整图层全套、.comp 存取、checker、agent、测试）
- `concat/src/studio.rs`（publish checker/adjustment）
- `concat/src/lib.rs`（3 个回调注册 + 蚂蚁线 Timer）
- `concat/ui/icons.slint`（sliders/save 两个 lucide glyph）
- `concat/ui/workspace/canvas-pane.slint`（CanvasAdjustmentParam、弹层、旋钮排、
  checker 图层、Ants 组件）
- `concat/ui/editor.slint`、`workspace/seat.slint`（全局与转发）
- `concat/locales/*.json` ×13（12 词条）

---

## 七、P9：笔刷光标、图层分组、Curves 编辑器 + 全量校验（2026-09-21）

### 1. 笔刷光标预览
- 手势层内嵌跟随指针的圆环：直径 = 笔刷尺寸 × zoom，亮环叠深环，
  任何底色可辨；仅 brush/eraser 且有文档且指针悬停时显示。
  零往返——圆环直接绑定 TouchArea 的 mouse-x/mouse-y，不经过 Rust。

### 2. 图层分组 UI（全栈）
- 引擎早已支持嵌套（new_group/move_node/find），本轮补 UI 与接线：
  - `rows()`：面板行的单一事实来源——深度优先反转遍历 + 折叠集合过滤，
    带 `(node, depth)`。所有面板索引（pick/visibility/opacity/fold/
    move/delete）统一切到该列表，**顺带修复旧代码面板行
    （root.children）与 walk()（含根）索引空间错位的隐患**。
  - 行模型扩为 8 元组 `LayerRow`（+depth/expanded/group），面板按
    depth×12px 缩进，组行显示折叠 chevron；折叠状态是窗格工作区状态
    （HashSet<LayerId>），不入 .comp（与 Photoshop 一致）。
  - CanvasMsg::LayerAddGroup / LayerFold；移动收敛为
    `move_within_container`（组内上/下，UI 与 agent 共用）。
  - agent 新增 `agent_layer_group / agent_layer_fold /
    agent_layer_move_into`（移入组/移回根，走引擎 move_node 的
    子树防环校验）。UI 拖拽入组留待后续（引擎已就绪）。

### 3. Curves 调整图层（第 7 类，全栈）
- 默认曲线 = 恒等 (0,0)-(1,1)；每通道独立点集，按输入排序，
  上限 16 点/通道。
- 编辑协议：`CurveSet(channel,index,x,y)`（端点只给输出、中间点
  clamp 在邻点间 ±0.001 防交叉）、`CurveAdd`（按输入排序插入）、
  `CurveRemove`（角点拒绝删除，长度守卫先行防空表下溢）。
- UI：R/G/B 通道页签 + 方形曲线编辑器（CurveEditor 组件）——
  拖柄改点、点击加点、双击删点；线段几何（两端点，y 已翻转到屏幕
  轴）由 Rust 预计算发布（`curve_segments`，因 Slint 无法按下标
  索引模型/Rectangle 无 rotation），Path 动态 commands 串绘制。
- GPU/CPU 合成引擎无需改动——Curves LUT 通道本已完备。

### 4. 审查发现与修复
- **下溢隐患**：`remove_curve_point` 原守卫顺序在空点表（畸形
  .comp 可构造 `Curves { red: [] }`）时 `len()-1` 下溢 panic——
  改为长度检查先行。
- **索引错位**：agent 层操作原先用 `document.walk()`（其文档注释
  声称含根但实际不含，而面板行是 root.children——两层不一致），
  全部统一到 `rows()`。
- 5 处 clippy（2×collapsible_if 收敛进 move_within_container、
  2×type_complexity 引入 LayerRow 别名、1×rows 内 if 合并）。

### 5. 验证（全量回归）
- concat-canvas（gpu）：**134 全绿**；
- concat（wgpu）：**60 全绿**（+分组折叠行序 1 项、曲线协议往返
  1 项，含"曲线抬升红通道"的合成级断言）；
- clippy `-D warnings` 双 crate 干净；
- i18n：6 个新词条 × 13 语言全量补齐（Add group / Fold group /
  Unfold group / Group {0} / Curves / 曲线编辑器提示语）。
- agent 接口清单核对：35 个 `agent_*` 方法（本轮 +6），UI 与
  agent 共用同一私有路径，无旁路。

### 6. 剩余待办
- 图层面板拖拽入组（引擎 move_node 已就绪，缺 UI 手势）
- GradientMap 双色 / 蒙版绘制 UI、压感（Slint 上游）
- 真机全链路手测
