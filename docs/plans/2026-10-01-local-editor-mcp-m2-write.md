# SeeCut MCP M2 阶段 3 准备稿

状态：阶段 2 PR #6 固定 HEAD `fcaed7b832ef61b065050332bf4e07c07e16d991` 的完整 Engine CI job 109950541665 success：App 144、API 27、host 46、Linux OS suite 16 项通过（子进程入口 1 项忽略），新增 Canvas 5 项/API 所有权 9 项全部 ok；wasm、Android、iOS 均 success。主审已接受阶段 2 的源码和自动测试工程基础，不等于实际交互/视觉或整体产品接受。已从此提交创建本地 `codex/seecut-editor-mcp-m2-write` 并授权阶段 3 实现；M1 ref05dcef、M2 ref/PR6 均保持。2026-10-01 主审已审内部接口，授权接入薄协议与可信授权 UI 源码；现有运行环境不自动授写权限，源码完成集中交接后推进独立 PR/CI 与候选验证；未验证原生闭环。主审继续负责产品、技术决策和阶段接受。

## 目标与前置门槛

在当前 App 持有的画布，通过可信 UI 的具名临时授权，移动当前单选图像层，X/Y 一次提交且一次 undo 可恢复。阶段 2 的 OS 锁 helper 证据不能替代完整现有 writer 测试；现有 writer 完整测试与工程基础已由主审接受，继续阶段 3 集成。透明图像层只要稳定 ID 和像素源有效，也允许平移。

## 已确定的参数、兼容和事务规则

- 每轴 `|dx|, |dy| ≤ 32768` 文档像素；结果每轴 `|x|, |y| ≤ 1_000_000`。输入和 f64 求和先验证有限及范围，再转 f32 并验证完整结果。结果与 raw old 的实际 f32 坐标相同则 `unchanged`；回包返回实际提交坐标。
- 从 raw old 拷贝 next，仅更新 X/Y，原旋转、缩放、翻转字段保持。允许有限负缩放、绝对值 0.001..1000，仅在副本 `canonical_for_edit().is_valid_edit()` 检查等价有效性，不写回 canonical 化的缩放或翻转。零缩放、NaN/Inf 或范围外缩放拒绝，保持文档不变。
- linked mask 首次 anchor 保存 raw old；已有 anchor 不变。所依赖 anchor 也必须有限可逆，校验失败不修改。单个图像层移动不复用依赖 alpha bounds 的 `active_geometry`，防止透明图层误报 objectGone。`ImageDocument::validate` 只验证 alpha 链，不能代替 transform 校验。
- 稳定 image layer ID 必须仍为当前单选目标；拒绝像素选区、多选、mask 绘画目标、组/调整层、缺失像素源、所有活跃手势、待确认变换、pending_history 和相关模态状态。实例、文档会话、revision、selectionRevision、owner 必须在同一 UI 提交点核对。
- 在原 document 上只读算完 next 后，才一次 begin_history → 按现有语义设置必要 anchor → 赋完整 next → commit_history，再 render/publish。失败、实际无变化不能创建 history、anchor、dirty 或 revision；不能依次发送两次 TransformSet。

## 可信授权与重试

- 租约为 5 分钟，以服务单调时钟计时；每工程最多一个客户端。可信 UI 显示客户端、工程、动作“移动已选图像层”和有效期，可撤销；续期再次明确 UI 授权，调用不能自动续期。R/R+M 不升级，App 重启或会话重绑失效。
- 按 M0 的认证客户端 ID、documentSessionId、clientSequence 确定 operationId，保留最近 1024 个结果、最多 24 小时，并保留本会话 highestSequence。
- 缓存重试先核当前授权、实例和会话，再识别原操作，返回原结果；不能用当前 revision/selection 先拒绝自己已成功操作的旧 expectedRevision。只有新操作才检查新鲜版本。同 ID 异参数拒绝；已过序号但结果淘汰返回 outcomeUnknown，不重做。撤权或租约到期后缓存不能绕过授权。

## Canvas owner 的只读检查

仅已验支持平台、当前 App 真实 Canvas 文档会话可进入写路径；未适配平台不能因未保存文档例外开放 MCP。owner 校验集中在 Canvas 的只读薄入口，桥接不拼私有字段、不构造 Session、不在写动作中隐式保存或取得磁盘目标。

| project_path | owner | autosave_inflight | 判定 |
| --- | --- | --- | --- |
| Some | Some | 有或无 | validate 原 guard 并匹配当前绑定；失效、不匹配明确拒绝 |
| Some | None | 有或无 | 拒绝，不降级为内存会话 |
| None | Some | Some | 首次自动保存候选：validate 原 guard，核其 target 与 inflight path 及当前 document_generation 一致；不依赖目标是否已存在 |
| None | None | None | 仅当前文档会话稳定、没有重绑/关闭待决时，按 App 持有的未保存文档身份允许；不创建 sidecar |
| None | Some/None | 与上述候选不一致 | 有 guard 无当前候选、无 guard 有 inflight、候选 path/generation 不一致均拒绝 |

定点测试：已有绑定缺 guard、stale/mismatched guard；候选目标尚不存在但合法 guard/代次一致；候选 stale sidecar、缺 inflight、path/代次不符；稳定未绑定文档允许且不创建 sidecar；inflight 无 owner 拒绝；不支持平台、关闭/重绑待决拒绝。检查前后无文件创建、Session 构造或文档/历史修改。首次自动绑定完成改变 binding/session 后，旧租约和请求失效，不续授权。

## 同一 UI 回调与超时结果

M1 的只读 `on_ui` 750ms 超时语义保持。阶段 3 的写请求采用独立、可证明的排队/开始/完成握手；不扩展为全局请求取消或文档撤销机制。

同一次 UI callback 持真实 Studio 互斥借用；bridge 只在短作用域核授权、身份和去重，然后释放 RefMut，再调用 Canvas 只读预检及一次提交。最后提交点核租约期限、owner、会话和版本；全程不 await、二次 invoke 或 UI 重入，不公开或跨 callback 转移 permit。结果立即记入同一 UI 会话的 dedup，再释放 bridge 借用后 render/publish，避免 settings.data 重借 bridge 时 panic，以及显示异常丢失已编辑的结果。

| 共享请求状态 | 超时/调度行为 | 编辑保证 |
| --- | --- | --- |
| Queued | UI 只能成功原子 claim Queued→Running 后执行；等待方超时可原子 Queued→Cancelled 并回 busy | 取消获胜后 callback 零编辑，不消费 clientSequence 或落 history |
| Running | 750ms 超时或调用方掉线回 outcomeUnknown，不能以 busy/appUnavailable 表示未执行 | 不补执行，不换序号；后续按原 operationId 查缓存 |
| Completed(result) | 完成先赢或完成/超时竞争中读到此状态则回已存原结果 | 结果与实际同一次提交一致 |
| Cancelled | 后续排队闭包不能成功 claim | 保持零编辑 |
| 从未启动且调度失败 | appUnavailable | 保持零编辑 |

状态锁仅保护状态/结果的短读写，不能持锁执行 UI 动作或渲染，也不能使超时线程阻塞等待整次提交。已 Running 后 channel 断开同样保守 unknown；完成结果仍须保存于原会话 dedup。

定点测试使用 barrier/channel 控制顺序：queued 取消后再放行仍零编辑；UI claim 获胜时 timeout→unknown 且最终只编辑一次；完成先赢返回原结果；调用方断开后缓存保留；render/publish 重借 bridge 不 panic；一次有效 XY 动作只需一次 undo。不得以 sleep 猜测竞争时序。

## 实施范围与顺序

| 里程碑 | 组件与责任 | 接受证据 |
| --- | --- | --- |
| 类型化画布动作 | `src/crates/concat/src/panes/canvas.rs`；原始 transform/选择/busy/owner 检查、一次历史提交、实际坐标结果 | 有效/无效 legacy、透明层、linked mask、原字段保持、失败零修改、实际 f32 unchanged、恰好一次 undo |
| 可信租约和重试状态 | `src/crates/concat/src/editor_mcp.rs`；单调期限、单客户端、会话撤销、bounded cache/highestSequence、写请求 UI claim/timeout 状态 | 过期/撤销/冲突/旧会话零编辑，成功同参数重试一次编辑，异参数和淘汰不重放；取消/开始/完成竞争的 barrier 测试 |
| UI 授权及 IPC/MCP 薄边界 | `concat/ui/dialogs/settings.slint`、`concat/ui/app.slint`、`concat/src/lib.rs`、`concat/src/panes/settings.rs`、`concat/locales/*.json`、`concat-editor-mcp/src/lib.rs` 与 `main.rs` | trusted UI 能授权/撤销，写 schema 拒无关字段及非法数值；13 语言库存、严格 Clippy、协议与相关 App 测试；已有四读工具继续可用 |
| 实际闭环 | 版本归属明确的候选、隔离 portable、小型画布、真实 stdio | 可信 UI grant → 明确 ID/小幅 dx/dy → App 实际位置和回包 → 一次 UI undo 同时恢复 XY → 再次平移、保存、关闭重开 → 新会话拒旧授权/请求 |
| 视觉与样式 | 主审负责设计与视觉判定，执行方实现并提供版本明确的实际窗口证据 | 独立检查布局层级、间距、字号/行高、文字与图标对齐、控件尺寸及状态、选中/禁用/错误/授权反馈、弹窗与窗口尺寸适配；实际效果的问题须修正后复看 |

实现单元可按稳定契约分工，主代理串行整合。结果若需要跨项目环境变更或新增外部副作用，先交主审确认，本文不引入此类动作。

## 独立视觉与样式验收门槛

原“画布交互与 UI 全面完善”目标持续有效。功能、协议、CI 与视觉分别验收；新增 MCP 授权入口、授权/撤销反馈及其涉及的画布状态，必须在实际窗口观察，源码或构建通过不能证明样式接受。

- 延续现有紧凑风格，授权 UI 显示必要的客户端、工程、动作、期限与授权/撤销状态，不堆说明。主审负责设计和视觉判定。
- 实际证据记录候选 commit/版本、窗口尺寸、操作路径和观察到的状态。覆盖布局层级、间距、字号/行高、文字与图标对齐、控件尺寸、默认/选中/禁用/错误/授权反馈，以及弹窗和窗口尺寸适配。
- 复用版本归属明确、仍有效的已验旧 UI 证据；不重复打开已充分验收的旧缺陷。本轮实际变化与仍未验视觉项各自列明，不能因 MCP/CI 工作遗漏。
- 候选可用后检查真实效果，发现问题即修正并复看。既有 CUA 故障豁免继续生效，不反复重连；缺少观察渠道时，只能记录具体未验证项及所需实际输入，不能删除视觉门槛或宣称产品最终接受。

最终交接清单独立列出：代码与平台检查、真实交互、保存/重开产物、视觉与样式、主审接受。视觉记录按“已验证 / 推断 / 未验证”区分；本轮授权 UI 源码已接入、当前候选实际样式尚未验证。

当前视觉待验清单（工程 CI 不关闭这些项目）：

| 范围 | 实际窗口覆盖 | 当前证据状态 |
| --- | --- | --- |
| 阶段 2 现有 writer 受影响路径 | 工程已被独立 writer 持有、身份变化、打开/保存失败时的错误反馈；较长路径/错误文本的布局；失败后当前画布、选中状态与编辑状态的呈现；首次自动保存绑定后的状态显示 | 源码与测试可验证行为，当前候选实际窗口及样式未验证 |
| 阶段 3 新增授权 UI | 必要授权信息、有效/失效/撤销及冲突状态，默认/禁用/错误反馈，与现有设置页的布局层级、文字/图标/控件尺度及间距一致 | 源码已接入，实际样式未验证 |
| 本轮变更涉及的弹窗与窗口适配 | 版本明确的实际候选，在实际使用窗口及较小窗口观察文字、控件、遮挡/裁切和操作可达性；问题修正后复看 | 待候选和实际观察渠道 |

旧 UI 只有既有证据明确覆盖的版本、路径和状态可复用；不把旧稳定预览 PNG 或旧授权功能证据扩展为当前全部样式通过。

## 主审授权 UI 设计（随后接入，当前不是视觉通过证据）

- 局部复用现有设置 tab4 的 DialogSection / Field / Button / Theme 字体和间距，不新增大卡片；节标题为“本地 Agent 权限”。保留客户端与连接字段，长实例 ID / token / 路径可选取复制且不撑宽面板。当前工程用项目名作主要信息，路径次级可换行。
- 同节按纵向两个小组显示读取与临时编辑。读取按钮用“允许读取”“读取与预览”，状态说明实际范围。写租约标题“移动已选图像层”，提示“仅移动当前选中的图像层，有效 5 分钟。”不加入协议、锁、历史机制说明。
- 点击前显示将获权的客户端和当前工程，不能用过时 client draft 冒充已授权对象。未授权操作“允许移动 · 5 分钟”；有效状态“已允许 · 剩余 X 分钟”，操作“续期 5 分钟”“撤销编辑授权”；到期显示“已到期”，重新授权须点击；会话切换显示未授权。读取撤销与编辑撤销区分，不重复弹第二次确认。
- 无画布、平台不支持、所有权不可用等不可授权状态，禁用按钮并在附近提供一条具体原因。失败/冲突就近反馈，保留当前客户端、工程及既有授权状态，不以大模态覆盖。按钮间距沿现有 6px / Theme 口径，窄窗口长译文可分行，不能裁掉撤销或遮住当前对象。
- 实际验收覆盖中文/英文、浅/深色、窄/宽窗口、无工程/未授权/有效/到期/撤销/冲突状态、Tab 焦点环和禁用可辨识。倒计时不逐秒抢屏幕阅读器焦点。候选须交代表状态截图或可操作窗口给主审观察；只调整局部授权区。

## 待确认的实际输入

- 主审/执行方：阶段 2 完整 CI 和 writer 行为证据，原生 UI 不与源码或 CI 混记。
- 执行方：按上表落地 Canvas owner 只读薄入口与直接行为测试；现有 writer 的所有权不能被新增桥接绕过。
- 主审/执行方：新候选的资源合规获取路径，以及可操作 CUA 或用户实际操作输入。目前本机禁止大型构建/下载，既有 CUA 首次失败豁免保留；不能注入 App grant 或把 unit fixture 当原生授权。

## 回退与证据边界

阶段 3 失败时停止新增写 capabilities/授权入口，保留已接入的现有 writer 保护及用户未保存状态。只回退功能分支的相关更改；不解除运行中 owner、不合并共享分支。协议测试、源码、平台构建、原生动作和实际保存产物分别记账，完整 M2 接受仍由主审完成。

## 2026-10-01 阶段 3 源码集中交接

本地功能分支 `codex/seecut-editor-mcp-m2-write` 在固定阶段 2 `fcaed7b832ef61b065050332bf4e07c07e16d991` 上实现；M1/阶段 2 refs 与 Draft PR #5/#6 不变。集中源码审查已完成并授权提交独立 Draft PR/CI；现有运行环境未自动开放写授权。

- Canvas typed move 已接入一次 XY history，最终提交前复核现有 owner 和调用方租约/会话/版本。单选稳定图像 ID、透明像素源、legacy raw transform、linked mask anchor、f64/f32 数值边界和失败零历史测试已写。
- UI callback 原子 claim 后才能访问 registry；先授权和 scope、再缓存去重，新请求才核 revision/selection。记录实际结果并完成请求状态后才 render/publish；已有只读 750ms 语义保持。
- `SEECUT_MCP_WRITE_TOKEN` 独立于读 token；可信设置页 grant/renew/revoke 绑定实际 client 与当前画布。到期保留实际 owner 以供显式续期或撤销；撤销后才能按新 draft 授权。离开画布工作区撤销 E，同时保留同文档会话 highestSequence/结果，返回后不重放旧序号。绑定更换清租约和不可达旧会话记录。
- 授权界面复用现有主题和控件，显示实际工程、获权对象与分钟状态，禁用/错误在局部呈现；读写技术字段分别标明环境变量。局部按钮支持文字换行与键盘焦点源码，13 语言库存同步。
- 协议细节见 [M2 移动协议](../SEECUT-MCP-M2-WRITE-PROTOCOL.md)。无额外 grant/renew MCP tool，静态 tool 列表不能代表已授权。

已执行：租约 registry 11 项、请求状态 13 项；协议精确源码 11 项、stdio adapter 3 项直接复用现成 Rust 1.93 依赖编译测试通过；协议及 adapter 严格 Clippy/missing_docs 通过。现成 Slint 1.17.1 编译器对 SettingsDialog 及 app.slint 全树检查退出 0、无诊断（不嵌资源，约 817 KB 临时产物）；此项仅证明 Slint 语法及类型，不证明 Rust/App 集成或视觉。Rustfmt、项目 locales.py 库存检查和差异空白检查通过。未通过本地 Cargo 构建依赖或完整 App。

已写但未执行：Canvas 16 项和 App 桥接 4 项。App/跨字段集成实际编译与全部适用 CI 待独立 PR；原生授权、移动/undo、保存重开和视觉样式待版本明确的资源合规候选及实际观察，绝不能以以上局部证据代替。

证据目录：`/private/tmp/sc-m2-stage2-mjbnrul2`（registry/request）、`/private/tmp/sc-m2-protocol-jec3i7hi`（协议与 adapter 编译、测试、Clippy）。本机 Data 仍超过项目 256 GB 上限，仅小型源码/harness 写入；本地大型 App 构建、下载与打包未执行。

主审指出三处技术 Field 原先可编辑且聚焦时保留旧 draft，现已最小扩展 Field 默认 `read-only: false`。仅实例 ID/读取 token/写 token 启用只读：使用直接绑定 `value` 的只读 TextInput，原可编辑输入的 focused draft 逻辑保留；两个输入互斥 visible/enabled。按钮按权限区实际容器宽度在正常尺寸同行、较窄纵向，保留 6px 与换行。修正后的 app.slint 全树再次经 Slint 编译器检查退出 0、无诊断；真实复制、焦点时轮换/撤销同步与默认编辑行为仍未运行验证。未找到可复用的小型运行 harness，不为此扩展大型本地链接。

## 2026-10-01 编译检查点与直接控件回归

独立 Draft PR #7 的首个 head `c4ad161e7aa622bd23321c78ac472fa47aa00dec`：CI `36754060750` 的 wasm 与完整 workspace 严格 Clippy 已成功；Phones `36754060528` 的 Android 已成功。workspace 测试与 iOS 在此记录时仍运行，不把以上结果扩展为完整 CI 或实际窗口验收。

新增 `src/crates/concat/src/field_tests.rs`，由现有 workspace 测试入口执行。测试导入产品 Field，使用真实 Slint pointer/key 事件、虚拟时钟、软件渲染和测试 Platform 的内存剪贴板：覆盖默认可编辑字段在聚焦时保留已输入草稿、只读字段拒绝编辑、聚焦时凭据轮换后复制新值、清空后的实际空显示，以及 Tab 跳过互斥隐藏输入。空选择不产生复制请求；这不清除系统剪贴板原有内容，也不证明 macOS 原生按键或系统剪贴板行为。

准备阶段复用既有依赖的 Rust 类型检查、严格 Clippy 和格式检查成功；8 MiB 受限链接触发文件体量门槛后停止，没有可运行测试二进制，行为测试尚未执行。源码审查与后续实际运行结果分别记录。

## 最小 Linux 正常窗口诊断

主审已批准首次窗口诊断的实现和远端执行，具体脚本/配置须审阅后运行。复用默认分支已注册的 `ci.yml`，增加默认关闭的 `linux_window_diagnostic` 手动选项及精确 `expected_sha`；诊断只使用现有 engine 单 Ubuntu 环境，跳过 workspace Clippy/test、gRPC 和 wasm，普通 PR/push/无诊断的手动检查保持原路径。诊断使用独立 concurrency，不取消普通 CI；启动前核 checkout HEAD。

远端 dev debug=0、单 job 构建同 HEAD 正常 App，Xvfb/xdotool 操作一个隔离 portable 实例，使用自有微型 PNG 和真实 X11 键鼠形成未授权 Settings 页的中文/英文、浅/深及窄/宽截图。不得设置 preview 环境变量、注入 UI/授权属性或运行发布流程。job 上限 90 分钟，窗口段外层 TERM 290 秒、10 秒后 KILL，总上限 300 秒；脚本工作 280 秒，精确回收进程。只上传不超过 15 MiB 的截图、操作记录与精简日志；本机不下载 App 或扩建大产物。

首次范围只提供无工程、未授权正常候选窗口的可见区域供主审观察；微型 PNG 仅准备未导入，不证明有工程或有效/到期/撤销租约，下方未入画的控件仍未观察。页面导航失败或窗口启动失败立即停止后续状态采集。真实 grant/移动/undo/保存闭环须在实际图观察后再推进；Linux 证据不替代 macOS 原生焦点、复制或保存验收。
