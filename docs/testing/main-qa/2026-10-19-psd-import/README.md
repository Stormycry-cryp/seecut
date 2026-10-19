# C04 分层 PSD 导入（42.21）

基线 `742f4f8b62b62b9d6f490fb5d9cee163bcae76cb`（PR #19），功能源码 **`023c2f9088bddeb84b9ee6474cc750ba596b2179`**。确切支持范围、拒绝项、许可与内存计算见 [SUPPORT.md](SUPPORT.md)。本包未新增通用 PSD 引擎、云服务或生产部署。

打开受支持 PSD 后，画布得到可独立编辑的栅格层、嵌套隔离组与基本位图蒙版。层序、中文/Unicode 名称、原始位置、显隐、opacity 和 13 种混合模式进入已有文档模型；不做普通图片的自动缩放居中。不支持的图层语义会拒绝整个导入，并保留原画布。没有扁平化替代或 PSD 写回。

后台任务先构造完整临时文档，再核验操作 ID、会话、工程和文档版本，一次替换。取消、编辑、撤销/重做、换项目或后发导入使旧结果失效。现有脏画布确认仍在导入开始前执行。界面提供取消与折叠详情；详情区域可滚动。本轮没有启动前台窗口，因此这些按钮、布局和真实拖动均未做实窗验收。

PSD mask 的相对矩形转换为文档坐标，保留负坐标与画布外样本、默认黑白、disabled 和 anchor。新增 `LayerMask.bitmap_bounds`，CPU / GPU / 命中 / 剪贴及笔刷映射使用相同覆盖率规则；旧 bitmap 和 vector mask 默认语义不变。实际 GPU 测试发现并修复黑色外部覆盖率仍被视为白色的问题。带边界位图蒙版用 `.comp` v7 保存，旧 v1–v6 继续可读；旧版本伪装新字段会拒绝。

## 定向验证

最终 **107 项通过**：引擎 92、原生 15（含显式运行的隔离资产链）。Rust / Slint 原生测试构建通过，最后构建耗时 4 分 11 秒。

准确通过数、二进制和冻结源码摘要、原生构建耗时见 [verification.json](verification.json)，日志为该文件列出的最终运行。初轮失败或补边界前的运行不混算为最终通过。

- 规范字节 fixture：Raw/RLE、13 个显式 blend、嵌套组、组 mask/opacity/disabled、中文名、负坐标、黑白默认值、相对矩形、移动/旋转、全部前缀截断、错误 RLE/ZIP/格式/组深度/尺寸/峰值、256 附加块上限及 2048 次确定性变异。新 GPU 测试要求实际适配器，不以缺失适配器跳过计成功。
- 原生事务：取消、后发导入、revision clock、文档 generation、binding epoch、工程路径、Studio session 和解析失败的原子拒绝；成功后仍为未保存的可编辑画布。位图蒙版移动、笔刷、撤销重做、v7 往返和错误版本拒绝。复用受影响旧蒙版、矢量蒙版、变换和保存用例。
- 完整隔离链：复制测试二进制，在相邻空 `portable/` 中执行，先断言数据根目录后才登记资产；合成和外部样本分别导入、编辑、历史、保存重开、CPU/GPU 对照，真实 CanvasExportJob 导出 PNG 并登记，重试保持同一资产，最终恰好 2 项。无用户资产、账号登录、供应商或付费调用。

两份固定 MIT 外部 PSD 验证真实第三方字节读取。其中 `2layers.psd` 的未编辑导入渲染，与 Pillow 独立解码的原 PSD 合成图比较：101×55，22220 个通道值中 1278 个有差异，每通道最大 **1/255**，并非逐字节相同。详情见 [external-fidelity.json](external-fidelity.json)。样本是简单 GIMP 栅格图，不据此宣称任意 Photoshop 文件或 ICC 色彩保真。自造 fixture 的合成预览是黑色占位，不用作正确性 oracle。

## 样例与复现

- `samples/synthetic-editable.comp.zip`：含中文名、两层嵌套组和位图蒙版的可编辑工程（8×6 技术 fixture）。
- `samples/external-editable.comp.zip`：由外部 101×55 PSD 导入并编辑的工程。附 MIT 许可。
- `samples/*-original.png` 是导入后未编辑的合成图；`*-render.png` 是编辑、保存重开和全局资产登记后的图。**全部是后台渲染证据，不是 UI 截图。**

在仓库 `src/` 中设置 `CARGO_TARGET_DIR="$PWD/../.toonflow/native-target"`，执行 `cargo test -p concat-canvas --features gpu --offline --no-run` 和 `cargo test -p concat --lib --offline --no-run`。按 `verification.json` 的过滤器运行相应测试二进制，传 `--test-threads=1`。完整链默认 ignored：把原生测试二进制复制到新的专用目录，旁边创建空 `portable/`，令 `SEECUT_PSD_FIXTURE_DIR` 指向该目录，再运行精确过滤器 `panes::canvas::psd::tests::psd_complete_edit_save_gpu_png_global_library_chain --exact --ignored --test-threads=1`。不要对真实用户资产目录执行。

共享桌面禁令保持：未注入鼠键、激活/移动窗口、切换输入源或启动 QA 应用。没有新版应用安装包；旧安装包不代表本包源码。实际文件选择器/取消交互、窄窗/主题、更多 Adobe Photoshop 外部样本及 ICC 色彩管理仍未验。没有修改生产、持久凭据、私有运行时或旧 WIP，也未唤醒旧写入者或 Astra。
