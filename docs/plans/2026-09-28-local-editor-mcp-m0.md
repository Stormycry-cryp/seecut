# SeeCut 本地编辑器 MCP：M0 契约与活跃状态

状态：M0 设计候选，尚未实现 MCP 连接或编辑工具。基线为 `codex/seecut-canvas-refinement` 的 `e2839eb127b79dcf186cf884d86943065003b464`，包含画布 Backspace/Delete 和焦点修复。后续实现按 M1–M5 分阶段交付，不能将本文当作已运行的服务。

## 1. 当前归属和复用边界

| 位置 | 已有能力 | M0 判断 |
| --- | --- | --- |
| `concat/src/lib.rs`、`host.rs`、`studio.rs` | 原生窗口的 `Shell` 持有 `RefCell<Studio>`；Slint 回调和后台结果在事件循环修改它并重新发布。剪辑的 `Studio.session` 是窗口正在编辑的 `concat_host::Session`。 | 本地 MCP 的活跃状态读写必须送进这个窗口的事件循环。不能在协议线程借用 `Studio`。 |
| `concat/src/panes/canvas.rs` | `CanvasPane` 持有画布文档、稳定的 `LayerId`、绘画目标、选择、历史、自动保存及变换中断状态；`CanvasMsg` 已被 UI 回调复用。 | 画布与剪辑是两份不同类型的活跃文档；桥接须分别识别、串行提交并发布。行号只供 UI 展示，MCP 写入使用图层 ID。已有 `agent_*` 辅助函数缺少统一错误返回和变换边界，不直接作为远程写入口。 |
| `concat-api`、`concat-server` | `Api` 按工程路径另持有 `Session`；`Hub` 将它串行放在独立线程。现有 JSON-RPC/gRPC 接口含通用 `edit.apply` 和文件路径。 | 保留供原有无界面场景使用；本地编辑器 MCP 不通过它打开第二份可写 Session，也不原样暴露通用命令／路径。复用纯业务 `concat_project::Command` 和宿主服务，不复用会话归属。 |
| `Concat-main/services/seecut-server` | 生产数据快照的只读 MCP，位于另一功能分支。 | 数据源、权限和部署对象均不同；不合并到本地编辑器服务。 |

`concat/src/host.rs` 还记录现有远程服务的独立会话。当前 `Session::open_info` 没有跨会话写锁，因此现状不能证明两份状态不会同时写一个工程。M1 只读桥接可先接入；M2 开放写入前，要在窗口和 `concat-api` 的打开路径共同取得按规范化工程身份区分的 OS 独占写锁，持有到关闭，冲突时拒绝第二个写会话。已打开的旧会话也要被识别或关闭后重新取得锁；不能只给新 MCP 客户端加锁便宣称排除了旧服务双写。未保存画布只由当前 App 持有，绑定路径后也进入相同所有权规则。

旧剪辑工程「SeeCut 一期验收」曾使 `StartPane::update` → `projects::open` 在主线程的系统 `__open` 阻塞。具体文件原因未查明。工程打开纳入后台读取、超时/取消反馈和完成时会话代次核对；被 OS 阻塞的文件操作不保证能物理取消，超时后保持原工程与 UI 可响应。M1 的只读上下文不依赖先完成该工程打开工具。

## 2. 外部方案检查

2026-09-28 查官方 [Rust SDK](https://github.com/modelcontextprotocol/rust-sdk)、[发布记录](https://github.com/modelcontextprotocol/rust-sdk/releases/tag/rmcp-v3.4.0)及标签 `rmcp-v3.4.0`（commit `fd7811fdaa9fefa1c8034534b4d7a31c97204f89`）：`rmcp` 3.4.0，Apache-2.0，声明 MSRV Rust 1.88；仓库开发工具链为 1.96。SeeCut 的 `src/rust-toolchain.toml` 固定 1.93，版本声明相容，但实际依赖构建和目标客户端握手仍属 M1 验证。SDK 已提供 stdio 服务端、工具 schema 和协议处理，应复用；本地 App 会话桥接与权限不由 SDK 代做。固定已发布版本，不跟随 `main`。

[VS Code 官方自定义编辑器文档](https://github.com/microsoft/vscode-docs/blob/main/api/extension-guides/custom-editors.md)展示了“编辑内核变更后同步多个视图、撤销归属到文档”的成熟模式，可借鉴单一文档所有权；它的扩展 API 与 SeeCut 的 Slint 线程模型不同，不引入 VS Code 代码。

## 3. 活跃 App 桥接

```text
外部 Agent / COTO
  → rmcp 3.4 stdio 进程（每客户端一个）
  → 当前 OS 用户的本地 IPC（实例 ID、连接凭据、消息上限）
  → 正在运行的 SeeCut App
  → UI 事件循环中的 Shell / Studio
  → CanvasMsg 或共享的剪辑 Command / Studio 方法
  → UI 发布、历史、保存状态；结构化结果返回 IPC
```

1. App 启动时生成随机 `appInstanceId`；连接配置固定该实例和 App 提供的连接令牌。没有匹配实例时返回 `appUnavailable`，不在 stdio 进程创建工程或选择“最近窗口”。本地 IPC 首选用户专属 Unix domain socket（目录只供当前用户进入、socket `0600`）；Windows 适配后续确定。Unix 用户隔离之外，仍检查连接令牌和 App 内批准的客户端、工程及权限。多实例各有自己的端点。
2. IPC 接收线程只解码、限流和排队。通过 `slint::invoke_from_event_loop` 在窗口线程取得 `Shell`，同一临界区读取会话/版本/授权并执行短命令，然后走现有 `publish` 路径；用一次性响应通道回传。后台重计算和 IO 在工作线程执行，提交结果时再次核对实例、工程会话和 revision。UI 线程不得同步等待 IPC、网络或文件打开。
3. UI 与 Agent 的写入共用同一文档所有者。剪辑优先复用 `Studio::apply`、`Studio::handle` 和 `concat_project::Command`；画布优先复用 `CanvasMsg` 经 `Studio::handle` 的变换边界、历史和保存路径。桥接在版本校验后把稳定图层 ID 映射为当前 UI 消息需要的行号。现有入口多为 `()` 并用 toast 表示失败，M2 应增加一个薄的类型化命令/结果适配器，使权限拒绝、失效目标和成功后的 revision 可返回客户端；不得绕过 UI 的校验直接改 `document`。
4. App 用户始终可以编辑；每个工程同一时间最多一个 Agent 写入租约，短期到期且可在 UI 撤销。租约只协调 Agent，不能阻止用户操作；用户每次改动使旧 `expectedRevision` 冲突。连接授权来自可信 App 界面，客户端传入的批准标志无效。
5. 活跃手势、自由变换确认、文件选择和覆盖层有明确 `busy`/`needsUserAction` 状态。画布 Backspace/Delete 的 UI 语义是有像素选区时清像素、否则删当前图层；MCP 则必须提交具体图层 ID 或明确的选区版本，不能凭上一条“当前选择”推断目标。待处理变换沿 UI 三选流程，不替 Agent 自动选择“应用”或“放弃”。

## 4. 标识、版本和操作 schema

以下是内部契约形状；MCP 工具将只开放有限的具名动作，而不是把 `action` 作为万能脚本参数暴露：

```json
{
  "context": {
    "appInstanceId": "runtime-uuid",
    "activeWorkspace": "canvas",
    "documents": [{
      "kind": "canvas",
      "projectId": "opaque-host-id",
      "documentSessionId": "open-epoch-uuid",
      "revision": 42,
      "contextRevision": 12,
      "selectionRevision": 7,
      "selectedObjectIds": ["layer:19"],
      "dirty": true,
      "busy": null
    }]
  },
  "write": {
    "operationId": "client-session-17",
    "clientSequence": 17,
    "appInstanceId": "runtime-uuid",
    "projectId": "opaque-host-id",
    "documentSessionId": "open-epoch-uuid",
    "expectedRevision": 42,
    "objectIds": ["layer:19"],
    "arguments": {}
  },
  "result": {
    "status": "applied",
    "revision": 43,
    "affectedObjectIds": ["layer:19"],
    "undoGroupId": "opaque-history-id"
  }
}
```

- `projectId` 是宿主生成的不透明资源标识，覆盖未保存画布；MCP 不用路径作写入目标。`documentSessionId` 每次打开、关闭、另存后重新绑定或进程重启都改变。`appInstanceId` 每次 App 启动改变，阻止旧连接命中同名工程。
- 对外 `revision` 是文档提交版本，每次用户或 Agent 的文档更改以及撤销/重做都严格递增。画布已有 `revision_clock` 防止历史回退复用数字，剪辑 `Studio.revision` 随编辑递增；M1/M2 需核对所有写入口后统一封装，不直接承诺现有字段已覆盖每种修改。`contextRevision` 随选择、焦点、播放头、缩放、手势/变换预览和渲染请求等可见上下文变化递增；`selectionRevision` 仅标识选择目标变化。视图版本不伪装成文档编辑。
- 每张预览帧标注其 `documentSessionId`、`revision`、`contextRevision`、请求 ID、帧尺寸/坐标系和 `stable` 状态。M1 只返回已完成且与请求版本均匹配的稳定帧；播放头已移动、异步渲染未完成或存在未提交笔迹/变换时返回 `busy`/`stalePreview`，不把旧帧说成当前画面。后续如开放临时预览，必须另带手势代次与 `transient` 标记。
- `operationId` 由已认证客户端 ID、`documentSessionId` 和单调递增的 `clientSequence` 唯一确定，服务校验三者一致；不能换个序号重用同一 ID。M2 在每个客户端/文档会话内保留最近 1024 个结果，至多 24 小时；同 ID 同参数且结果仍在窗口内返回原结果，同 ID 不同参数拒绝。服务保留该会话的最高序号，序号已经经过但结果过期/淘汰时返回 `outcomeUnknown`，绝不作为新操作执行；客户端须重新读状态。App 重启会改变实例与文档会话，旧写请求被拒绝。生成、导出等外部副作用由 M4 另建跨重启持久操作记录，按稳定操作 ID 查终态。
- 操作在 UI 线程同一提交点验证客户端授权、租约、实例、文档会话、revision、对象仍存在且可编辑，然后执行。成功返回新版本；无副作用命令返回 `unchanged` 而不虚增版本；失败不部分提交。长任务先回 `jobId`，按冻结的原会话和版本提交，不能在用户切换工程后写入新工程。
- 错误码至少区分 `appUnavailable`、`notAuthorized`、`leaseConflict`、`wrongProject`、`staleRevision`（附最新摘要）、`objectGone`、`busy`、`stalePreview`、`needsUserAction`、`invalidInput`、`ioFailure`、`outcomeUnknown`。超时不自动重放写请求，先以操作 ID 查询。

## 5. UI 动作覆盖与实施顺序

表中“已有”指代码入口，不代表已成为 MCP 工具；“首批”按 M1/M2 划定，其他动作在后续里程碑保持显式缺口。权限 `R` 为读取结构，`M` 为读取媒体，`V` 为控制窗口视图和选择，`E` 为可逆编辑，`X` 为输出，`D` 为资产删除，`P` 为付费生成。读取授权不自动包含视图控制。测试编号对应第 6 节。

| UI 动作 / 既有入口 | MCP 输入 → 输出 | 权限；撤销 | 失败/未覆盖与验证 |
| --- | --- | --- | --- |
| 读取当前工作区、工程、选择、历史可用性；`Studio`、`CanvasPane`、Slint globals | 实例 → 两种文档摘要、稳定 ID、版本、busy | R；无 | 无 App/越权；T1、T2。M1 首批。 |
| 读取工程结构和对象；`Session.project()`、`ImageDocument` | 工程 ID、分页/深度 → 轨道/片段/图层树和参数 | R；无 | 限量、失效对象；T1、T3。M1 首批。 |
| 读取当前画面/缩略图；`CanvasPane` 合成、monitor | 工程 ID、文档及上下文版本、区域/尺寸 → 带帧版本的有上限图片内容 | M；无 | 旧帧、未提交预览、尺寸过大、无 GPU/解码失败；T3。M1 只返回稳定帧。 |
| 画布选层、选区与工具；`CanvasMsg::LayerPick/SelectAll/Deselect/Tool` | 文档/对象 ID、选区参数 → 新视图状态 | V；无 | 不能用可变行号代表 ID；T2、T4。M2 先做选层，画笔手势后续。 |
| 画布移动/翻转/变换；`CanvasMsg::Nudge/Flip/Transform*` | 层 ID、文档像素/角度/目标值 → 位置和 revision | E；可撤销 | 非图像层、待处理变换、单位错误；T4、T5。M2 移动一层首批。 |
| 画布增删、排序、显隐、混合和不透明度；`CanvasMsg::Layer*` | 层 ID、有限参数 → 受影响 ID 和 revision | E；可撤销 | 删除组的范围、蒙版、锁定态、旧 ID；T4、T5。M2 首批只暴露经验证的子集。 |
| 画布像素选区删除、填充、画笔/橡皮、蒙版与调整；`CanvasMsg` | 目标层/像素或蒙版 ID、选区版本、参数/笔迹 → revision | E；可撤销 | 目标与当前 paint mode 不一致、手势中断、费用/性能；T4、T5。M2 后续切片，不能以 Backspace 隐式目标代替明确 ID。 |
| 画布撤销/重做与保存/另存；`CanvasMsg::Undo/Redo/SaveComp*` | 会话、期望版本、授权保存句柄 → revision/保存结果 | E；撤销产生新版本；保存无历史项 | 不跨用户后续编辑，失败不改保存标记；T5、T6。M2 首批。 |
| 剪辑项目打开/新建/关闭；`StartMsg`、`Studio::open_project/close_project` | 项目 ID 或宿主选择句柄 → 新会话/任务状态 | V/E；关闭可能丢弃未保存内容 | 主线程 `__open` 阻塞、未保存保护；T7。M3 前解决异步打开。 |
| 素材导入与素材区删除；`MediaMsg`、`Command::AddMedia/RemoveMedia` | 授权资源 ID、明确素材 ID → 素材 ID/影响片段 | E/D；可撤销的工程记录，源文件不删 | Probe IO、关联片段范围；T8。M3。 |
| 片段插入、移动、修剪、分割、删除；`Studio::apply`、`concat_project::Command` | 素材、轨道/片段 ID、时间/帧 → 片段 ID、revision | E；可撤销 | 锁定轨、源/目标范围、素材区与时间线混淆；T8。M3。 |
| 剪辑选择、播放头、预览与撤销/保存；`Studio` | 稳定 ID、时间或版本 → 视图/画面/保存结果 | V/M/E 按动作；编辑可撤销 | UI 选择与文档版本分离、媒体缺失；T2、T8。M3。 |
| 资产库列表、分类、批量移动、删除；`personal_library.rs`/云端动作 | 授权库/文件夹和冻结的资产 ID 集 → 结果/失败列表 | R/E/D；按后端结果 | 本地源文件和库记录分开，删除确认/批量部分失败；T9。M3。 |
| 生成报价、提交、查询与结果入库；`cloud.rs` | 引用资源、参数、预算授权 → 报价/jobId/结果 ID | P/M；外部副作用不可撤销 | 参数快照、幂等、费用、登录状态；T10。M4。 |
| PNG/视频导出与入库；`CanvasMsg::Export*`、`ExportMsg` | 宿主授权目的地、规格 → jobId/文件或资产 ID | X/M；输出文件不可撤销 | 文件同名、任务取消、真实产物；T10。M4。 |

全局/剪辑快捷键规范中的共用命令定义是 M0 依赖：键位、菜单和 MCP 逐步映射同一业务动作；不把尚未实现的快捷键当成 MCP 能力。旧规范中“无选区且画布焦点不删整层”的条款已被用户新要求和 `e2839eb` 的当前实现覆盖，后续同步规范时以当前行为为准。UI 仍保留输入法、文本字段、顶层弹层和手势的优先级；MCP 没有物理焦点，必须显式传对象与会话。

## 6. 后续验证矩阵与下一切片

| 编号 | 需要证明的行为 |
| --- | --- |
| T1 | App 未启动/多个实例/未授权连接：返回明确状态，不创建第二份可写工程，不误连另一窗口。 |
| T2 | 用户改选择或切页后，旧选择上下文不能让 Agent 删错图层/片段；输入框和模态键盘隔离仍按原生 UI 规则。 |
| T3 | 结构与画面同时匹配文档及上下文版本；播放头改变或未提交手势时旧帧不冒充当前帧；对象删除后 ID 失效，预览大小和资源读取受限。 |
| T4 | UI、快捷键和 Agent 对同一画布层操作后共享历史/画面；旧 revision 或旧会话拒绝，组/蒙版目标准确。 |
| T5 | 自由变换或手势中接到 Agent 写入时返回忙碌/待用户决策；撤销不跨用户后续编辑；保存重开一致。 |
| T6 | 旧自动保存回包、另存失败与重复 operation ID 不误报成功；去重窗口过期返回 `outcomeUnknown` 而不重放，也不覆盖新工程。 |
| T7 | 问题剪辑工程打开时 UI 保持响应；超时/取消报告真实状态，旧读取结果不切换新工程。 |
| T8 | 导入只进素材区；只有明确排片命令写时间线；锁定轨/旧对象拒绝。 |
| T9 | 资产移动/删除遵守冻结 ID、授权范围和部分失败反馈。 |
| T10 | 付费/导出断线后按 operation ID/jobId 查终态，不重复扣费或覆盖文件；验证实际产物。 |

M0 设计审查需确认：活跃窗口是唯一写入目标、独立 `Api` 的双写缺口和锁门槛已明确、两类工程的会话/版本 schema 能识别旧请求、上表每个业务族均有现有入口与待补缺口。以上 T1–T10 是后续实施验证，不标为当前通过。

M1 最小实施切片：在 `concat` 进程建立实例 ID、只读上下文快照与用户隔离 IPC；用 `rmcp` 固定版本做 stdio 发现和只读工具；连接未授权或 App 退出有明确错误。M2 先落实跨会话独占写锁，再增加 App 内类型化写入入口、版本/租约检查以及“移动已选图像层 → UI 实时显示 → 一次撤销 → 保存重开”的闭环。M3/M4 的动作按上表逐批暴露。每个新实现问题先按仓库内代码与 GitHub 官方来源定点核对，再加入依赖；本 M0 文档不触发大型构建。
