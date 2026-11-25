# Photo 父级写派发：定点只读设计

结论：复用现有 ClipSave/ClipExport 的受理与异步回执模式。原 UI 回调内完成 WriteRequest.claim、exact permit claim、WriteRegistry.begin、原 Receipts reserve/attach、缓存 pending 和 WriteRequest.complete；同一个 photo_workspace worker 执行 TCP；完成后只更新原 Receipts 附件。不能把 WriteRegistry.finish 移到另一个 UI 回调，也不能让旧 WriteRequest 的 Running 状态充当跨线程写许可。

本报告仅依据当前公开源码与既有 parent bridge 报告，未读私有 runtime、未运行 Cargo/Git/GUI、未改任何生产或候选生产代码。下列新增类型/方法是建议，不是现存实现。当前父 MCP 仍对 managed Photo 返回 unsupportedWorkspace。读取 SHA 见 SOURCE_SHA256.json。

## 1. 可直接复用的实际异步路径

| 实际文件与符号 | 已有行为 | Photo 对应最小落点 |
|---|---|---|
| editor_mcp_write_request.rs:1、:81 claim、:93 complete、:122 wait | 要求 action/dedup/complete 同 UI 回调；Queued 超时原子取消，Running 超时只能 Unknown | 原封保留，用于 UI 受理。受理动作是冻结原 job、原账本与入队，不是 TCP 完成 |
| editor_mcp_write_registry.rs:294 cached、:336 begin、:391 finish | authorize 先于 cache；保存完整 P 并 PartialEq；begin 即消耗 sequence，InProgress 不允许再执行；finish 不重授权且只允许原回调 | 扩 EditFingerprint::Photo；仍在受理回调 finish(pending)，不对同 record 第二次 finish |
| editor_mcp_clip_execute.rs:92 execute_clip，特别 :123-190 | cached → exact permit.claim → writes.begin → receipts.reserve；Save/Export 分支排后台，再同时 finish 两个 pending 结果 | 新 execute_photo_admission 仿此分支，保持原调用顺序与错误处理 |
| editor_mcp_clip_save.rs:392 queue_save、:93 drain_completions | UI memory-only final check、原 deadline/account/source；后台工作，完成队列先获真实 UI 借用再 drain；旧结果存原回执，只在源仍匹配时更新当前 UI | Photo worker 的 completion 复用同一原则；不要另建结果数据库 |
| editor_mcp_clip_export.rs:158 Attachment、:265 drain_completions、:303 queue_export；canvas_policy.rs:1420 attach_clip_export、:1480 complete_clip_export | 原 Receipts 上挂冻结来源和动态进度；completed 修改原条目，query 可观察原 pending 后的进展 | 加 PhotoAttachment 与 attach/photo_original/complete_photo，仍是同一个 Receipts.entries |
| canvas_policy.rs:497 Permit、:590 claim、:609 valid；:635 OperationIdentity、:663 Receipts | 精确 Intent、300 秒批准；Receipt 族当前 clip:bool | Intent 加 Photo 闭合分支，原 bool 改为小 enum Canvas/Clip/Photo，不能把 Photo 标成 Clip 或 Canvas |
| photo_workspace.rs:21 Command、:83 bind、:190 worker | channel 仅 Open/Close；唯一 NativeHost 在 serial worker，Sender 藏在 UI closures | 暴露同一 worker 的内部 typed client；增加 Read/PrepareEdit/QueryReceipt，不创建第二 NativeHost |

这里揭示一个容易误用的差异：writes 缓存的是不可变受理结果，Receipts 才是可更新的原操作观察。重复 edit 命中缓存可继续返回原 pending；随后 photo_operation 查询同一个附件的新状态，不能借重复 edit 再启动 worker。现有 write registry 的 24h/1024/highest_sequence 规则继续生效，清理后 Unknown，不能因结果已清理而重新执行。

## 2. 冻结内容与精确 ID 映射

建议 PhotoAttachment 挂在现有原 Receipt 上，至少保留：原 Scope/client/OperationKey/parent scm2 ID；完整原父 typed PhotoParameters；批准时原 native PhotoIdentity 与 resolved PhotoEditAction；原 saved backing 作用域/opaque layer 解析结果；原 Permit、账户/会话与可选 Task 来源；固定 childOperationId；唯一 job phase/revoker；实际回执、readback 与状态。token 只在原私有内存中，不进 Debug/Serialize/模型结果。

**完整参数相等核验位置**：

1. 新受理先用当前原读写授权核对 scope/client，按父 key 查现有 PhotoAttachment；若是已受理操作，使用其原 expected/resolved action，绝不拿当前 native revision 重建旧 fingerprint。
2. 原父 typed parameters、operationId/sequence 与附件逐项相等；再以原 native 绑定组成 EditFingerprint::Photo 调 writes.cached/begin 的原 PartialEq。所有不相等都 parameterMismatch；任何入队之前完成比较。
3. 新操作只从批准冻结的原 native 绑定解析目标；旧 opaque layer handle 不能解析到新文档。附件只能 reserve/attach 一次，不允许更新时替换原 payload 或 child ID。
4. worker UI 握手与 completion/query 更新再同时匹配 Scope/client/sequence/父 operationId/固定 childId/原 typed payload；来源不匹配不更新另一个结果。

**固定 child ID 字节方案**：先用 concat-editor-mcp/src/lib.rs:292 operation_id(client,session,sequence) 验证原 scm2 完全相同；其原字节可为 Unicode、最长 192，不能直接转发或截断。只在 Begin::New 接受原操作时计算一次：

```text
LP(s) = u32 little-endian UTF-8 byte length || exact UTF-8 bytes; 不归一化 Unicode
B = ASCII("seecut-photo-child-operation-v1") || byte(0x00)
    || LP(parent Scope.instance)
    || LP(parent Scope.project)
    || LP(parent Scope.session)
    || LP(authenticated client)
    || u64_le(original client_sequence)
    || LP(original validated scm2 operationId)
    || original child UUID raw 16 bytes
    || u64_le(original native DocId)
    || u64_le(original native documentGeneration)
childOperationId = "photo-op-v1-" || lowercase_hex(SHA256(B))  // 精确 76 ASCII bytes
```

不把可变 action/revision/当前状态塞进 ID 重算入口。完整 payload 相等由原账本/附件保存与比较，不能拿 hash 代替；这样同一父操作改参数也不会暗中获得新 child ID。上次粗略报告提出的 payload fingerprint 可用于诊断校验，但不作为派生第二 child ID 的途径。恢复只读已存 childOperationId，不以当前 child 或参数重新计算。若丢失原附件/映射，只能 Unknown，不猜 ID、不中途生成替代操作。

**u64 无损**：native host/DocId/generation/revision/layerId 全部留 Rust typed 值；模型层只接收有文档作用域的 opaque handles 或协议明确的规范十进制字符串，拒绝任意 JS Number 转换。现有 parent clientSequence/revision 安全整数与 native u64 是两套值，不能用 frame 或截断 native revision 填旧字段。parent 安全观察代次如采用计数，必须独立 checked 分配并保存精确 native 映射，耗尽拒绝，不能回绕。Task/recovery 的现有安全整数校验见 agent_controller.rs:1803 附近。

## 3. 受理、真实派发与取消的精确分界

建议内部接口，全部不可由模型直接构造：

```rust
photo_workspace::client_on_ui() -> Option<PhotoWorkerClient> // 原 bind 建立的 Sender
PhotoWorkerClient::try_prepare_edit(FrozenPhotoJob) -> Result<(), QueueRefusal>
PhotoWorkerClient::try_query_receipt(OriginalPhotoQuery) -> Result<(), QueueRefusal>
// 原 worker 事件送回 UI；只含 typed facts，绝不携带 NativeHost/TCP 连接
WorkerEvent::ReadyToDispatch { original: OriginalPhotoRef, preflight: PhotoPreflight,
                               decision: SyncSender<UiDispatchDecision> }
WorkerEvent::Completion { original: OriginalPhotoRef, result: PhotoWorkerResult }
editor_mcp_photo::admit_on_ui(...) -> Response // 原 WriteRequest callback 内
editor_mcp_photo::authorize_dispatch_on_ui(...) -> UiDispatchDecision
editor_mcp_photo::complete_on_ui(...)          // 只更新原 Receipts，不再次 writes.finish
```

通道应有明确小容量（首版一个未结束 Photo edit，控制队列如 8）；UI 使用 try_send，不在 UI send/recv 阻塞。序列 worker 对 ReadyToDispatch 的回复等待有界，超时或 UI 不可用时争取取消未派发 job，不无限占住 Close。Open/Close 保持原唯一 owner；关闭前置撤销尚未开始的写。注意 verify_backing 的 canonicalize/metadata 明确不承诺网络文件系统硬截止时间；同一 worker 若卡在文件 I/O，UI 可继续响应，但不能承诺 Close 必定及时执行，更不能再建一个 NativeHost 绕过所有权。

最小阶段按以下顺序，**派发阶段保存在同一 PhotoAttachment 的小型 execution control，不是新权限/结果账本**：

| 阶段 | 实际动作与检查 | 取消与结果 |
|---|---|---|
| 原 UI callback 尚未 claim | 原 WriteRequest Queued；不可访问 writes 或消费 permit | 原 wait 取消胜出，晚 callback claim=false，busy/appUnavailable 可证明没有受理或 child write |
| UI 受理 callback A | claim；原批准来源/Scope/读写 gate；cached；permit.claim；begin；reserve+attach；注册 job 的撤销句柄；入队；writes.finish(pending)、Receipts.finish(pending)、WriteRequest.complete(pending)，全部同 callback | 入队失败存同操作 notDispatched；begin 已消费的序号不退回，也不换 child ID 重试 |
| worker Prepare | 在同一个 host 上读取真实 state/backing，核对原 child/doc/gen/revision/已保存 scope；不调用 execute；发 ReadyToDispatch | job 仍未派发，可由单调取消胜出证明 childExecuteCalled=false；不能把被取消的 read 当成写 |
| UI callback B 派发前 | 短借用 UI，原 Scope/账户/IPC/会话、原读写 lease/token、原 permit intent/有效期与撤销状态、原 Task 来源/turn/input/control epoch 全复核；原 backing summary 仍匹配；返回一次性内部 Allow/拒绝 | 不重新 grant/renew、不 begin 第二次；不查文件、不 TCP。busy/借用失败拒绝或有界等待，不能跳过 |
| worker 最后派发点 | 取得 Allow 后，用同一小型原子/互斥状态将 Ready→DispatchStarted；与 Cancel/Revoked 争用；再检查原 deadline；紧接真实 photo_execute_after_gate(原 child,原 childId,原 expected,原 action) 一次 | 只有取消先胜出的 Pending/Ready→NotDispatched 才能宣称未派发。Started 后取消不能改写为未执行或自动 Undo |
| worker 返回/UI completion C | 原身份字段全匹配才更新原 Receipt；存真实 applied/rejected/failed 或 Unknown；原结果不因随后撤权而被伪造成取消 | 当前文档、授权与原来源不同则不应用其当前 UI/Task 状态；原真实结果仍归原操作 |

这个 job control 只仲裁本次已经批准动作的排队/开始，不签发新权限。需新增的最小同步原语是单个 job 的 Pending/Ready/DispatchStarted/NotDispatched 状态和 checked 身份；不能直接把 AtomicBool 的一次 load 当成取消证明。可参照 concat-host/src/export_publication.rs:108-246 的 CAS/原 lease 前后检查，但它的 PublishClaim 是导出语义，不能作为 Photo 已执行证明直接复用。receipt/result 仍只存在原 Receipts；生命周期最多保存有界活跃 revoker 弱引用路由，不能另存第二套 payload/结果。

**批准卡清理与撤权须区分**：canvas_policy.rs:518 forget 与 :522 revoke 本就不同；ClipExport 已接受 job 不因卡片清理停止。Photo 也应把已消费的精确批准来源留在原附件，普通 forget 只清卡片，不重授予，也不隐式取消。callback B 检查原 intent/期限与 job revoker，加当前原授权，而不是在卡片正常删除后仅凭 Permits.valid 找不到 entry 就误判新权限。显式 revoke/clear、账户/会话/Task/source 变化必须先通知同一 job revoker；不能把 forget 当成这些事件的唯一处理。若 Root 选择让 Photo 的 forget 即撤销待派发工作，也必须明确规定并测试，不能无意继承 ClipSave 与 ClipExport 不同的清理语义。

现有触发点可复用：editor_mcp.rs:258 assistant_registry_permission、:1119 revoke_tool_permit、:1172 active_ids/:1252 sync_workspace；agent_ui.rs:1318 grant_scope/:1343 sync_task_grant/:3379 revoke_task_permits；agent_controller.rs 多个现有 clip_export_fence::revoke_session 前置。所有新分支应在同样的真实事件之前通知 Photo 句柄，不依赖某次 UI 刷新才发现。TaskGrant::allows（agent_task_grant.rs:146）只认原 scope/current+admitted turn/input/control epoch；异步等待期间若该原 turn 已不再允许，待派发编辑必须拒绝，不能转挂后来 Task。

限制必须说清：UI grant、worker TCP 和 child callback 不是一个跨进程事务。父撤权若在 DispatchStarted 之后发生，可能已有写；只能保留真实结果/Unknown。此分界需要产品接受，不能承诺撤权在已发送请求上回滚。child 最终 expected/busy 检查仍由真实 seecut_agent.rs:311 preflight 与 :393 execute 在同一 UI callback 执行，不能以父缓存替代。

## 4. saved backing、恢复与最终读回

保存门槛沿 Root 既定约束：只允许 photo_backing_after_gate（photo.rs:212、:496 verify_backing）双读确认的已保存 regular .pcraft；savedRevision=0、read-only、无路径、非 UTF-8/超限或验证失败均不建立写 scope/history。project_key 取真实 binding.history_key，仅父后台读取，不把规范路径发模型。工程同路径正常保存可能替换 inode，保持 project_key；不能将 fileIdentity/inode 或 savedRevision 当作新 project UUID/save generation。

DocState.set_backing_path（external engine/lib.rs:313）在真正改路径/丢路径时改变 generation；SaveAs/Detach/重开/child 重启使旧 document session 失效；父端发现变化时立即沿原 bind_scope(None/新 scope)、grant/permit/Task 撤销。对应 job 未 Started 时撤销，Started 后只保留原结果；返回旧路径或切回同标题不得恢复旧 grant。worker 最新核验只证明该时点的关联和文件对象，不是连续 filesystem 锁或历史字节证明。当前 photo_workspace 的 500ms 循环只轮询 child 生命周期，没有文档/generation 推送订阅；不能宣称父 UI 已即时知道每次原生 SaveAs。最小安全切片在每次准备/派发前做已授权真实读，并让 child generation 检查拒绝期间竞态；空闲 UI 的即时同步若另有产品要求，需要独立设计真实通知或授权轮询，不凭旧 UI 缓存延续写身份。

Unknown 处理只走 `photo_receipt_after_gate(原 owned child UUID, 原 childOperationId)`，先要求原客户端/账户/读取 gate 与原 Receipt 来源匹配。查询不是新写，不消费另一个 write sequence，不需要重新批准一个 opacity 动作。恢复入口在 agent_controller.rs:1741 reconcile_editor / :1757 update_editor_recovery 中增加 Photo 闭合族；保留其原 scm2/key/参数，不把 Unknown 转成新的 execute。

若 child 已重启/消失，原 UUID 不再 owned，或 cache miss 返回独立 UnknownReceipt（内存缓存不恢复），仍 Unknown；不在新 child 查询后重发，不以当前 opacity 看起来相等当作原操作成功。receipt lookup 的 Known 没有 action 参数，父适配必须再按附件的原 expected/action 检查 applied 的 target ID/Undo 空 target；rejected/failed 保留实际 before/after，不推断无副作用。

真实 readback 也必须经过同一个 worker：完成先把实际回执存原附件；UI 再核对当前读取 gate、account/Task/source，才排一个 ReadState/必要 backing 读；返回 UI 再检查同源。若期间撤销读取，不继续读或把新事实交给模型。只允许原 applied 回执 + 当前同 native identity/目标的实际值读回支撑核对；不要求 opacity/Undo 总是 revision/history +1，不把 f64 原请求与 f32 源值做未经定义的精确浮点等同。Undo 的 Task 目标必须明确，不把 applied 的 undo 本身宣称所有交付完成。

公共 agent_tasks.rs 只有 bounded TaskSummary/请求投影，agent_controller.rs:292 识别 task_checkpoint；实际 checkpoint 业务验证器不在本次允许读取的公开链内。故本次能给出的是原回执/真实读回的闭合输入及 public controller 扩展点，不能宣称原 Task 已能验证 Photo property。后续 parent/runtime 接线须分别增加 Photo deliverable 验证，仍沿原 Task checkpoint/reconcile，不新造 Task/财务账本。

## 5. 最小实现切片与验收

1. **父 typed 与原账本**：concat-editor-mcp 的 PhotoParameters/OperationQuery 和 shape；editor_mcp.rs 的 EditFingerprint::Photo、Photo scope/dispatch 分支；canvas_policy 的 Photo Intent、OperationFamily、同 Receipts 的原附件。先只 state + opacity/独立 undo + 原操作查询，不放通 generic command/path。
2. **唯一 worker**：photo_workspace 增 typed client、同 worker 的 PrepareEdit/QueryReceipt/ReadState 事件与 bounded try_send；UI event drain 连接 admit/authorize/completion；不开第二 NativeHost，不在 UI 做 TCP/fs。
3. **异步分界**：原 WriteRequest/WriteRegistry 回调规则不改；新增只属于原附件的 dispatch phase/revoker，把真实 lifecycle/task/account/scope 前置连接；原 payload/childId 不可替换。
4. **返回与恢复**：原 Receipts 更新/原操作读恢复，UI gated readback，public controller Photo ToolAction/recovery family；checkpoint 实际验证为后续独立验收依赖。

定点验收必须覆盖：UI queued 取消零 registry/permit/child；受理后 queue-full/Close/Task pause/账户换代/SaveAs 拒绝且同 sequence 消耗不复用；Ready/Cancel CAS 两个合法次序、Started 之后任何错误保持真实回执或 Unknown；相同原 scm2+payload 只一个 child execute、参数冲突绝不计算新 child ID；Unicode/冒号/最大序号映射固定且 76 ASCII、native >2^53/u64MAX 无损；scope revoke 后迟到 completion 不污染新文档且真实旧结果不被伪造取消；模态借用失败保留 completion 不丢失；普通卡片 forget 与显式 revoke 区分；worker 唯一 owner/有界队列/无 UI TCP；child restart/cache miss/父附件淘汰均只 Unknown 零重发；receipt 后读回改变/读权限撤销不能完成 checkpoint。

本次只有源代码可行性与最小改动设计，没有实现、编译、MCP/Agent/Task 运行或模型交付验收声明。
