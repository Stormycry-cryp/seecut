# 画布移植代码审查

审查基线：`71a6181`，2026-09-21。修复状态单独记录，不能把本清单当作已修复证明。

## 2026-09-21 修复候选状态

以下均为当前工作区候选，尚未经过原生客户端验收。`cargo check --release -p concat --no-default-features --features wgpu` 已通过。冻结点 release 测试为 `concat` 79/79、`concat-canvas` 137/137；画笔坐标/选区/基线共享/最终曲线、未保存打开、魔棒文档坐标、单边纹理限制、蒙版 CPU/GPU 一致性等 8 个定点用例也逐项确认实际运行并通过。GPU live resident 与停用蒙版测试在本机可用 adapter 上运行通过。最新 `rustfmt` 与 `git diff --check` 通过。证据日志位于 `outputs/validation/concat-frozen-canvas-targeted.log`、`concat-frozen-full-opt1-release.log`、`concat-canvas-frozen-full-release.log` 和 `concat-canvas-frozen-gpu-live.log`。

- 画笔释放会先 flush 并提交最终 tile，再把按下时锁定的 PixelId 落库；蒙版绘制不再回写图层 PixelId。蒙版像素统一为灰度 coverage + opaque alpha，橡皮与删除选区会把 coverage 清零。回归覆盖最终曲线对拍、蒙版前后原图 RGBA、CPU/GPU mask 结果和笔画期间基线强引用数。
- GPU layer resident 增加 live stroke 状态，重合成忽略 PixelStore 中仍为笔画前版本的旧 Frame id；释放时同一工作 Frame id 进入 store 后关闭 live 状态。蒙版改为复用纹理并按脏矩形更新。
- GPU 读取蒙版前检查 `enabled`；新增 CPU/GPU 对拍候选。
- 新开图片重建 PixelStore，开工程与新图均清空 GPU 文档缓存；相同 PixelId 不再沿用前一文档纹理。
- 画笔输出按当前选区 coverage 裁切。滚轮、平移、缩放、fit 只同步视口字段，不重新合成图层。
- Agent 画笔和魔棒统一接收文档像素；UI 指针单独从视口换算。偏移缩放视口回归用于防止再次混用坐标空间。
- 文档和像素进入同一顺序历史；UI 命令与公开 `agent_*` 写接口均已静态复核。Undo/Redo 后重新 render，连续同类滑杆值合并，历史限制为 100 项和 256 MiB retained frame 预算。
- `move_node` 在目标不是 Group 时会在 take 前拒绝，避免节点丢失。
- PNG 解码启用 palette expand 和 16-bit strip；image crate 启用 JPEG、WebP、BMP。`.comp` 对 manifest、bitmap 数量、单文件及总像素设置上限。图片与工程在分配像素前检查画布单边尺寸，UI 路径使用当前 WGPU device 的 `max_texture_dimension_2d`；render 再次检查并用可见错误返回，避免 `create_texture` panic。
- 存档 staging 与 backup 使用进程内唯一旁路名，中途失败清理 staging；旧包只在新包完整写完后交换。
- 新图/工程打开增加未保存保护：保存并打开、放弃并打开、取消。保存取消或失败保留 pending path 与当前文档；PNG 导出不清除工程 modified，成功后派发个人资产登记 action。

仍需验证或保留为已知边界：

- `concat` release check 与冻结点 Rust 测试已通过；Slint 原生包和可见交互仍未运行。
- 新增 GPU-only 回归已改为 adapter 缺失即失败，本机 adapter 运行已通过；Windows GPU 仍无证据。
- CPU fallback 绘画为让 CPU 合成看到当前画面，每个 move 仍会复制 working frame；GPU 路径为每个 stroke 一次整帧复制。大图耗时与峰值内存仍需实测。
- PNG 编码、`.comp` 保存和图片解码仍在 UI 事件线程同步执行，可能阻塞界面；迁移到后台任务仍待实现。
- `.comp` 仍是 `concat-project:1` 私有目录格式，不兼容上游 `com.compositor.project` v1–6。
- 组仍按隔离组合成，蒙版仍按画布坐标采样；这与上游 pass-through 组和局部变换蒙版存在语义差异。
- Windows 实机、跨平台文件对话框、GPU 大图性能及个人资产页面即时可见结果仍无当前证据。

## 必须修复的正确性问题

1. `canvas.rs::brush_release` 将蒙版 scratch 写入图层 PixelId；同时先移走 stroke/base/scratch，再提交 flush 变化，最终尾段无法落地。需要原图 RGBA 不变及最终曲线回归。
2. `CanvasMsg::Undo/Redo` 没有重合成。当前历史仅记录像素，文档 history 未接入；删除、调整、图层排序无法通过同一撤销链恢复。像素历史没有容量限制。
3. `open/agent_open` 继续向旧 PixelStore 插入；`load_comp` 替换 PixelStore 时未同步清理 GPU 按 PixelId 缓存的蒙版。重复打开与跨项目相同 ID 要验证。
4. `gpu.rs::mask_texture` 未判断 enabled，各调用者也直接传入；CPU 与 GPU 的停用蒙版行为不一致。旧测试需要确认是否实际覆盖 GPU。
5. PNG 解码未启用调色板展开与 16 位转 8 位。文件选择器宣称支持的格式应逐一核对实际 Cargo feature。
6. 项目临时目录仅 PID 唯一，备份固定 `.old`；旧临时数据可能混入存档，旁边已有同名文件可能受影响。

## 性能与资源

- UI 画笔使用 CPU BrushStroke，GPU compute 画笔核心存在但未见应用调用，不能宣称已接入 GPU 绘制。
- 画布平移缩放部分事件重新执行全图合成，导航应只更新视口。
- 蒙版每个输入样本复制整幅像素并创建/上传整幅纹理；大图需要复用纹理与脏区更新。
- 解码、PNG 编码、项目写入均同步执行；应将耗时 IO 与 CPU 编码移出 UI 事件回调。
- 撤销保留整帧且无限增长；需要条目和内存预算。
- GPU 合成虽有脏区上传，合成仍走全画布；不可把注释中的“脏瓦片合成”作为实测结论。

## 交互与样式

- 工具栏固定高度且容纳大量固定宽度控件，窄窗溢出；工具设置应随工具切换并分行布局。
- tool >= 3 让框选与魔棒显示画笔参数；无文档事件未全禁用。
- 画布键盘撤销应路由画布，不能落到时间线；历史按钮要反映可用性。
- 独立画布工作区替代默认剪辑布局中的预览上下分割。
- 新文档/开图覆盖当前文档前应处理未保存修改，保存成功状态应可辨识。

## 移植兼容性

- 当前 `.comp` 使用私有 `concat-project:1` 嵌套 Rust schema。上游使用 `com.compositor.project` v1–6。后缀相同不代表可以互开，需要导入兼容或者明确格式边界。
- 上游 v6 组为 pass-through，当前 CPU/GPU 都将组隔离合成；上游 mask 使用图层局部变换，当前 mask 在画布坐标采样。CPU/GPU 对拍只能证明二者一致，不能证明与原产品一致。
- Windows 已有构建/安装器基础，但缺少本次画布的实机结果。性能和可见交互仍需独立验收。

## 验收门槛

先证明数据不被破坏、撤销与存档能恢复，再验收四项导航与资产流转；最后核对原生包、双平台结果及大图耗时。不能用测试计数替代这几条用户路径。
