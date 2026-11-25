# Photo 父窗口人类发送：定点接线设计

结论：可以复用现有 send、Task create/update、账户与租约、原输入 ID 和异步结果回收模式，但目前父级没有 Photo 人类输入准备态，也没有向单 Host worker 发起读取的入口。先解决点击时的画面来源，再接线。现有同次 capture→validate 只覆盖原生采用之后的变化；第一次 capture 之前没有 view/selection 见证，不能把稍后取得的相机和选区写成点击当时的画面。

本文只读当前公开源码和两份现有定点报告，没有代码修改、Cargo、Git、GUI 或模型交图测试。精确文件 SHA256 见 SOURCE_SHA256.json。旧报告中的 Photo 无 snapshot/validate/backing 结论已被当前 host typed API 取代；父窗口尚无这些调用入口的结论仍成立。以下路径相对 Seecut-photocraft-workspace。

## 1. 当前真实回调与状态

| 入口 | 实际流程与可复用部分 | Photo 缺口 |
|---|---|---|
| 普通发送 | agent_ui.rs:3666 on_send → Binding::send:2089 → refresh、can_send、ScopeSnapshot/Lease 一致检查 → capture_document_observation:2064 → 分配 input-UUID → RequestSnapshot:2236 → persist(true):2305 → Controller::send_document_content | 当前 capture 同步；ID 在观察成功后才生成，没有异步 Photo pending send。 |
| 创建 Task | 同一 send 中 task_mode=有 write_token 且 supports_tasks；分配 task-request-UUID，task_id_for 得到 Task ID，TaskIntent 保存 initial_observation_binding；create_task:agent_controller.rs:1158 检查 binding/模型/状态后 enqueue task.create | Task 模式不是另一条可绕过观察的入口；原 client_request_id 必须提前冻结，异步完成时不得重建任务。 |
| 补充 Task | agent_ui.rs:3615 on_task_update → task_update:2435；在 refresh 前取走旧 TaskGrant、withdraw_task_approvals、revoke_task_permits，然后抓原观察、生成 update-UUID/expected_revision；先持久化，再 retry_task_update | 当前仅文字，明确拒绝附件；controller:873 的 task.update wire 没有 content/image 字段。不能把新截图塞入通用附件并声称支持 Task 补充图片。 |
| Task 补充重试 | 查 record.task_updates 中同 Task 未 confirmed 项，复用原 client_request_id/message/observation；只有明确零变更拒绝可更新 CAS revision，最多 2 次 | 新采样不得覆盖原 update 的观察；网络不确定时不能重抓画面后重发。 |
| 原控制门槛 | grant_scope:1318 检查账户 epoch、runtime_account_epoch、read/write lease、DocumentIdentity、IPC generation；TaskGrant::allows 继续核对 current/admitted Task 和控制 epoch | Photo 观察和图片不创建 grant；异步采样不能继承完成时新获得的权限。 |
| 未启动异步工作 | State:286 已有 generation、epoch、account_epoch、runtime_account_epoch；WorkerResult:253 带 session_id/epoch/account_epoch；refresh:1110 按当前/parked 会话路由，Prepared:1167 再比 scope 与完整 tokens | 可复用结果回到 UI 后复查的模式，但 Prepared 是连接配置，不应伪装成图片准备。需要独立 typed pending/result 分支。 |
| 停止与失效 | on_stop:3689 冻结当前 turn；task_action:2380 在 pause/stop 前撤 grant/permit；disconnect:2623 增 epoch；sync_lease:1480 断连/清准备/增 epoch；换会话:817 撤未启动准备 | 当前 stop 依赖已有 turn，采样时还没有 turn，所以只复用原 Stop 回调而不新增 pending 取消分支会漏取消。 |
| 隐藏面板 | close:2618 只 set_open(false)，注释明确保留会话、草稿、活跃请求和批准 | 不能把隐藏自动改成撤销所有授权。新增 pending 是继续还是取消需沿产品既有隐藏语义；最小方案保留 pending，但返回时仍核对原 session/lease。 |

普通 send 的错误回滚会恢复 previous Record 和消息；成功才清草稿、显示运行状态。对 Photo 应将发送前准备阶段留在原草稿/未提交状态，直到观察完成才进入现有 RequestSnapshot→persist→IPC 段。不要过早写 pending=running 或借 Task 的 unknown 表示尚未派发的只读采样失败。

## 2. 单 Host worker 与可信 backing 的现状

photo_workspace.rs:21 的 Command 只有 Open/Close。bind:82 建一个 worker、一个命令 channel、一个生命周期 updates channel；100 ms timer 仅 publish Snapshot。worker:190 独占 NativeHost，500 ms recv_timeout 兼顾 child 生命周期。没有 Photo 状态缓存、capture 请求、取消 token、只读 job 槽或面向 Agent 的 worker handle。has_instance/instance_id 只有生命周期 fence 和 child UUID，不含文档身份。

当前可调用的闭合 host API 已具备：

- photo.rs:195 photo_state_after_gate：UI 回调元数据；不是像素/当前几何见证。
- photo.rs:212 photo_backing_after_gate：围绕真实文件核对进行两次 state 读取，要求同 host/doc/generation/revision/backing；绝对 .pcraft、真实 regular file、canonical path 与 held File/dev/inode/size/mtime/ctime 一致。拒绝 read-only、saved_revision=0、无完整路径。PhotoBackingBinding:158 持有 File；history_key:186 仅供本地历史存储，不发给模型、不当权限。
- snapshot.rs:321 photo_snapshot_after_gate：预期 PhotoIdentity + request ID → 历史 PhotoSnapshot；无自动重试。
- snapshot.rs:352 validate_photo_snapshot_after_gate：引用同一个 PhotoSnapshot 对象的 snapshotId，等待新的原生 postInputFrame；返回对应 identity/viewStateId/selectionStateId、帧与年龄。不能改传另一张图；5 秒原生见证期限不会因验证续期。

最小 worker 扩展应保持这一 NativeHost 唯一所有者，新增有界 typed 读命令和独立结果 channel，不在 UI 线程做 TCP、PNG 编码或文件 metadata。一个人类准备操作最多一项在途；避免把公开 Sender<Command> 变成任意控制方法入口。worker 命令只携带冻结的 owned-host/document 预期及本地请求 token，不授予权限；父 UI 在排队前和最终返回时使用原权限判据。若排队期间撤权，worker 应在执行前通过现有 UI 门槛回查，或使用直接由该权限持有者撤销的本地取消标志；这个标志只取消本次读取，不能成为新权限账本。

backing 核对可能阻塞网络文件系统，源码明确不承诺硬 I/O deadline。使用现有单 worker 必须承认这种延迟：UI 保持响应并使结果不可发布，但不能声称 cancel 已中止系统调用，也不能另建一个 NativeHost 并行绕过生命周期所有权。

父 BridgeUi::active_ids（editor_mcp.rs:1172）目前只要 has_instance 就返回 None 并清旧写作用域；assistant_document_observation:698 调同步 Context，DocumentObservationBinding::valid（agent_observation.rs:114）只接受 canvas/clip/flow，三个 revision 是 JS 安全整数。前置工作是给可信 Photo 身份增加独立文档分支和原 R/M/E 作用域注册，使 read lease 明确绑定该 Photo 文档。不能拿旧 Canvas lease、标题或 child UUID 冒充完整文档，也不能绕过这条前置链直接调用 *_after_gate。

Photo native u64 必须保留原值；公开观察用明确无损 Photo schema/opaque 绑定，不把 native 数值强塞现有 JS number。saved backing 应在允许会话发送前准备并发布为可信本地 scope/history key；点击后再次读取只可复核原 binding，不能把换到的另一个文件当原输入工程。PhotoBackingBinding 本身不可 Clone，原对象应保留在 worker 有界槽中，父 UI 只持不含路径的绑定 token 和已验证身份摘要。新 backing 的文件戳必须与原对象/身份相符；历史 key 相同不足以证明文件未被替换。

## 3. 点击时冻结哪些内容

在现有 send/task_update 回调通过 can_send/原租约检查后，先创建本次原 ID，再发异步工作。建议的 PendingPhotoInput 是内存中一次性载体，属于原 State；不是新会话、Task、权限或持久账本。

| 冻结项 | 来源与结束时要求 |
|---|---|
| session_id、State.generation/epoch、controller.ipc_generation | 取点击时状态；结果只投递到原 pending，不能命中同名新会话或重连进程。 |
| input ID / task-request ID / update ID、原文字、引用、模型/effort/policy | 保留原请求内容。普通 send 沿原 inputId；Task create 沿 client_request_id；update 沿原 update ID 与 CAS precondition。 |
| account_epoch、runtime_account_epoch、原 read lease、需要时 write lease | tokens 仅内存、不得 Debug/持久化；完成时要求同一账户与同一租约，不能用新 token 替换。 |
| Photo owned host、原 PhotoIdentity、已核对 backing token/文件版本 | 原 identity/backing 变化直接停止本次准备。State 元数据只作核对，不能重新选文档。 |
| 原 Task ID/revision/control epoch、task_permit_revision、此前 grant/update 候选 | update 在等待开始前先撤旧 Task 授权，与现有 task_update 相同；返回后 task_id、CAS、epoch、scope 仍对应原人类动作才可沿原 controller 方法提交。 |
| capture 选择 | 必须由下节产品语义决定：使用点击前保留的同次截图，或明确允许点击后第一次稳定采样。不能在结果返回时才决定采用哪次画面。 |

只读工作失败/过期/取消时保留原草稿，释放原 pending，不调用 send/create_task/retry_task_update。IPC 尚未派发可精确报告未提交；一旦进入既有 controller enqueue，就沿原 RequestSnapshot/TaskIntent/reconcile 规则处理，不再通过 pending 机制重发。后台结果迟到不应新增用户消息、清除新草稿或覆盖当前会话状态。

## 4. 首次 capture 前的缺口及 Root 需要决定的取舍

场景：父点击 t0 已固定 doc/revision；child 在 t1 才处理 snapshot。t0→t1 期间用户 pan 或改变多选再回到原值，之后 capture→validate 完全可成功。source8 的见证从 adopt 开始，这个结果不证明 t0 的相机/选区。仅固定 PhotoIdentity、两次 metadata、比较最后相等、父端时间戳都不能补回缺失区间。

### 方案 A：保持严格的点击时画面语义

在原 read lease 有效、可信 Photo backing 已就绪、Agent 面板处于发送准备状态时，经单 worker 预先取得一张 capture，并完成初次 validate。父准备态保留这张图的 token；点击时只选择这个已存在的 token，暂停替换该见证。点击后再次 validate 同一个 snapshot，再通过父门槛进入发送。原生从早于点击的 capture 持续单调跟踪到点击后的验证，可以把点击包含在未变化区间内。

没有有效预先见证时，按钮显示正在准备观察或该次准备返回未提交；可以自动准备下一次可用状态，但不得在同一次点击下偷换一张后采样图作为点击画面。5 秒 TTL、窗口不绘制、worker 排队、文件核对和 runtime 附件处理都可能让见证过期。需要 Root 接受短期预采样成本及准备等待；预采样只在已有读取授权内，不抢焦点、不新增授权确认。若要求点击随时成功，现有 5 秒单见证 API 无法保证。

这条方案可复用现有 source8 capture/validate，不需要新原生截图协议。要遵守单见证限制：不能在原输入准备或模型最终复核期间自动 capture 替换原 witness；后台更新应暂停，旧 witness 失效后本次输入失败，之后的独立准备才能创建新 capture。

### 方案 B：接受发送后采样语义

点击时冻结原 ID、权限、可信文档和文本；允许 child 稍后的第一张稳定画面作为本次请求的明确采样边界，随后只验证该次 capture。不得写成点击时画面，不宣称 t0→capture 的 view/selection 未变。文档/host/backing 仍严格固定，不能自动切换文件。

这是较小的父级实现，但改变原人类观察语义，需要 Root 明确接受，并确保公开/私有协议与界面描述一致。该方案不需额外人类确认；决策是产品语义本身，不能在实现里暗中降级。若用户要求的就是点击那一刻，B 不满足。

本报告不替 Root 选择。两条方案都禁止在旧 input/update ID 下用后来的新 capture 替换已经承诺的观察。单靠新增 begin_capture 请求也不能解决点击到该请求抵达 native 之前的时间差；若想要无 PNG 预见证，需要新的 native 连续观察协议，已超出当前最小接线。

## 5. 异步返回与最终交图门槛

建议复用 WorkerResult→Binding::refresh 的短 UI 回调模式，新增独立 PhotoPrepared 分支。需要同时匹配 session_id、generation、pending ID、原账户/IPC/租约、Photo 绑定、Task epoch，以及原 cancelled 标志。当前 Prepared 只核对连接配置，不能直接承担这些约束。schedule:1451 的 hot 判据和 publish 的 busy/can_send 必须包含 pending photo input，否则等待中可重复发送，或只按 1 秒低频轮询耗尽 5 秒 TTL。

取消挂接到原 Stop、Task pause/stop/update、disconnect、sync_lease、账户切换、换会话和 child 退出/未知状态。普通 Stop 还没有 current_turn_id 时，也应取消这个原 pending；不能靠现有 frozen_turn.is_some() 分支。Task update 的旧 grant 已撤销，不应在采样失败后自动复原旧批准。隐藏面板按现有 close 语义不自动取消，避免无要求改变产品行为。

普通 send 与 Task create：观察成功后才构造最终 RequestSnapshot/TaskIntent，仍先持久化再调用现有 controller 方法。持久化失败不派发。prepare ID 可以写入历史作为未提交来源，但不能借 observation_binding=None 绕过后续 schema，也不能在恢复时自动重采样并发送。最小实现可只保留内存 pending 和原草稿，崩溃后明确未提交。

Task update：目前 wire 只支持 message + observationBinding，不支持图片 content。若要求补充时模型必须收到同一次截图，要扩展已协商的 Photo 观察/媒体引用协议，让 runtime 在原 update ID 下索取保留的同次 snapshot，并在交图前复核；或明确扩展 task.update 的绑定媒体字段。两者都需要对应 runtime/服务合同支持，不能仅改父 UI 声称完成。等待期间过期则保留原 update 的来源并停止交图，不在原 update ID 下采另一张。

editor_mcp.rs:2589 的 Preview 已有后台编码→on_ui 最终身份/权限/view 校验→返回 image 的发布模式；on_ui:2111 通过 event loop 回 UI，750 ms 等待。可复用时机，不能在 UI 内调用阻塞 Host API，也不能把 Canvas 的 view_state_id 来源用来验证 Photo。由于 on_ui 超时后闭包仍可能稍后执行，读取门槛回查可以用它，但真正提交 send/Task 的回调必须同时具有一次性 pending/CAS 认领，不能把一次超时当成尚未执行的证明。

普通附件路径 agent_references::content:60 读取资产缩略图并产生 image 数据，不带 Photo 原 witness；Controller::send_frozen_content:1381 只验证观察身份、图片种类/长度与能力，不会再次调用 native validate。把 Photo PNG 加进这个列表，会丢掉实际模型消费前的原见证复核。必须保留独立 Photo 媒体引用和精确 input/capture 关联，或在 runtime 最终构造模型输入前增加对应可信 parent 回查；沿用原模型图片能力、预算和媒体大小限制。普通 send 最多 2 张 image、总 content 最多 8 项，新增当前画面会与用户已选图片竞争额度，需明确拒绝超限或分配规则，不能静默丢用户附件。

## 6. 四层证据不能混用

| 层级 | 可以陈述的事实 | 不应升级为 |
|---|---|---|
| PhotoState / backing | 可信 owned host 元数据、所记录文件对象及版本；支持本地历史 scope | 点击时 view/selection、原子像素、编辑许可 |
| 历史 PhotoSnapshot | 同次真实 UI paint 采用的冻结文档、几何、选区和 PNG | 返回时仍有效、点击前未变、GPU present |
| postInputFrame validation | 对原 snapshot，新的真实 paint 和该帧输入结束后仍满足内部见证条件 | 永久当前、未来不变、原输入写权限 |
| 模型最终收到 image | 原 input/update 绑定、权限/Task/模型能力/预算/媒体门槛及最终可信复核之后，实际 provider 请求确实携带这张 PNG | 仅凭生成文件、HTTP 成功、入队或测试回执就宣称模型已看见 |

当前 PNG 是全图透明 sRGB 合成缩略图，最大边 256，附当次视图/选区上下文；没有视口裁切和面板/选区蚂蚁线。这一点须在接线验收沿用，不能将其描述为用户整个编辑窗口截图。

## 7. 最小修改范围及不重叠所有权

| 所有者 | 文件 | 必要范围 |
|---|---|---|
| 父 UI 协调 | agent_ui.rs | 原 send/update 两阶段、一次性 pending、原 ID/lease/account/task 冻结、取消和结果回收、hot/busy；不另造 Task。 |
| 单 worker | photo_workspace.rs | 暴露受限 worker handle；有界读取请求/结果、原 backing/snapshot 持有槽、取消/过期、close 生命周期串行；不改原生权限。 |
| Photo bridge | 新 editor_mcp_photo.rs + editor_mcp.rs 的必要路由 | 可信 Photo DocumentIdentity/backing scope、原读权限核对、opaque capture 引用、同次最终 validate 和不交图拒绝。共享 editor_mcp.rs 由 Root 串行整合。 |
| 观察与持久化协议 | agent_observation.rs、agent_sessions.rs、agent_tasks.rs、agent_controller.rs | 明确 Photo 无损观察、保留来源/媒体引用、能力协商；沿原 send/create/update 方法，未知版本明确 unsupported。任务普通工具授权仍由原 grant 路径负责。 |
| 跨进程媒体协议 | concat-editor-mcp 的 lib.rs/main.rs及独立 Photo 模块；私有 runtime 对应观察/媒体最后交付分支 | 原输入可关联同次 snapshot 与验证结果，最终交图前回查。具体 wire 须与 Root 当前合同工作对齐，本报告未重新调查私有 runtime。 |
| Root 集成 | main.rs 模块声明、共享 manifest/合同 pin、测试装配 | 串行处理重叠文件及实际版本协商；不由上述实现者自行放宽。 |

现有 seecut-photo-host 的 state/backing/snapshot/validate API 足以承载方案 A/B 的核心读取，不需要因为父 UI 等待而另建 Host。若 Root 选择更长 TTL、无 PNG 预见证或跨 child 持久见证，则是额外 native 范围，不属于本次最小方案。

## 8. 真正接线后的验收

1. 原 on_send 点击只产生一个 inputId；采样返回前零 send/task.create。连点、迟到结果、切会话、重连均不创建第二次输入，也不清新草稿。
2. 原 Task create 保存同一个 task-request ID；原 update 在采样前撤旧 grant，CAS/ID/观察保留；未知网络结果只查询原请求，不采新图重放。
3. 点击前无 witness、点击后改相机/选区再回原位：A 必须未提交；B 只在明确采用发送后采样语义时可继续，输出不能称点击画面。加入不同文档再切回案例，始终不可替换绑定。
4. 每个阶段撤 read/write lease、换账户、IPC generation、Task pause/stop/update、关闭或重启 child：结果不进历史媒体和模型输入。普通 Stop 在尚无 turn 时也能取消。
5. 同 .pcraft 路径替换 inode/内容、Save As/generation 变化、无保存路径、readonly：不能以 basename/history_key 相同通过。实际文件读取阻塞不阻塞 UI；取消后迟到结果被丢弃。
6. 保留同次 snapshotId/PNG/selectionStateId/viewStateId，通过原单 worker capture→validate；idle repaint、focus loss 可通过；pan/选择/缩放/跨显示器/末帧变化及 TTL 过期拒绝，无焦点获取。
7. 原始用户附件占满 2 图时，按已选产品规则明确处理；不静默丢图。Task update 必须实际证明原协议接收其绑定媒体，不能只检查父 Record。
8. 真实 parent UI→原 Agent→原 Task/MCP→native CPU/GPU→runtime→provider 输入验收。对原 snapshotId 可追踪最终 image bytes/hash与原 input ID；父已验证但 runtime 附件准备期间发生变化时，模型没有收到该 image。再独立验收 opacity/undo，不将读取证明当编辑授权。

交付停在设计：Root 需要决定严格点击时画面采用 A，还是明确接受 B 的发送后采样；以及 Task update 的同次媒体协议/两图额度规则。其余身份冻结、取消、权限与最终复核均是接线必要条件，不需要新增人类确认来替代已有授权。
