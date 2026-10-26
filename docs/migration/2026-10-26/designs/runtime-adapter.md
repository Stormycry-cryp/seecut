# Photo 接入既有 runtime：最小兼容与发图边界

结论：保留原 COTO Session、MediaSessionStore、OperationJournal、managed provider/费用账本和 TaskCoordinator，可以新增独立 Photo adapter。仅放开 kind 枚举、把 Photo 当 Canvas，或只在工具回包时验证一次图片都不完整。下一轮 JS 可先实现闭合解析、普通人类文档工具及原操作恢复；PhotoContext 响应合同、原 snapshot 的内部 parent 复核入口须先定稿。Task 图片补充与 Photo checkpoint 是独立依赖，不能随普通读取宣称已接通。首次送图前与已受理的历史图片分开：五秒 witness 时限不永久施加给全部会话历史。

本次只读定点调查，未修改 public/private、未运行 Git/Cargo/GUI、未调用模型或服务。私有源 HEAD f5cf3b7a5436c4ea264094fa517f22d7a13483c2 由 Root 指定，未用 Git 独立复核；本轮实际读取文件 SHA 见 SOURCE_SHA256.json。旧历史报告中 snapshot 尚不存在的判断已过时，本报告按当前 public typed state/snapshot/validate/edit/receipt API 核对。

## 现有真实入口

私有路径相对 `Seecut-grsai-r1/.toonflow/agent-source`；public 路径相对当前 workspace。

| 位置/符号 | 当前门槛及 Photo 所需最小改动 |
|---|---|
| src/workspace-tools.mjs:13 WORKSPACE_TOOL_NAMES、:19 modelToolsForImages | 当前只有 app/canvas/clip/flow。增加由可信 observe.kind=photo 选出的集合，首批 capabilities/context + photo_state/snapshot/edit/operation；不沿用 common 中 project/library/Canvas 编辑。无图片模型过滤 photo_snapshot。这里只投影已存在工具，缺少工具不能伪造发现成功。 |
| src/editor.mjs:19 DOCUMENT_TOOL_POLICY、:31 parseContext | 新增 Photo policy 与独立 parsePhotoContext 分支。当前三项 numeric revision/contextRevision/selectionRevision 和 kind 白名单拒绝 Photo；不能把原生 u64 转 Number 填入旧轴。 |
| src/editor.mjs:168 sessionTools / invoke / guard、:302 contributor/prepareApproval 等聚合 | 复用 signal、permissionChanging、tool allowlist、checkTaskDispatch、authorizeModel；Photo 走新 adapter，不交 canvas.wrap。扩展 preparation/pending/verifiedOperations 聚合和 contributor 文案。元数据读取只说明结构，不能说模型看见图或当前可见画布已知。 |
| src/clip-tools.mjs:4 validateClipWrite、src/canvas-tools.mjs:132 prepareEdit/:247 journal.allocate | 可复用生命周期约束，不能复制 Clip 的 numeric revision/selectionEpoch 或 Canvas readback。新增 photo-tools.mjs 做 closed action/expected、精确准备、批准后重验、原 journal 分配与回执核对。 |
| src/doc-observation.mjs:11 checkedObservationBinding/:24 observationFromCurrent/:40 checkInitialObservation | 当前只接受 canvas/clip/flow 和固定 numeric 轴。新增独立 photo observation schema，旧记录原样兼容，不制造数值版本。initial-only 规则必须保留。 |
| src/view-context.mjs:30 parseView/:82 sameObservation | 当前只认 Canvas/Clip 和 slintLogicalPixelsTopLeftYDown。Photo state 未提供原生 fullview，首片 view=null/unavailable；真实 snapshot 的 egui points、document pixels、transform 及历史 adoption 单独解析，不能假装 Slint viewport。Photo sameObservation 比较父存储的原 state/选择事实。 |
| src/runtime.mjs:108 sessionTools 建立/:168 工具投影/:172 policy | 保留原 perSession provider、账户/模型选择、writeToken、ask/exact-call。photo_edit 加入 prepareApproval/approvalArguments；photo_operation 按原 operation 查询路径审批与所有权检查。 |
| src/media-store.mjs:152 projectEvent | approval.required 当前只对 canvas_edit/clip_edit 投影精确参数；增加 photo_edit，否则 UI 审批看到的可能不是准备时冻结的原 expected。 |
| src/editor.mjs:58 OperationJournal/:97 linkVerification、src/runtime.mjs:124 recoveryQueries/:430 reconcileEditor | 原 journal 和恢复闭合集合没有 photo_edit/photo_operation。扩展工具名、receipt 空间预留和原查询关联，完整保留原 owner/session/operationId/typed 参数。Unknown 只查原 photo_operation；没有 cache 不能换 ID、重分配 sequence 或重发 edit。 |
| src/runtime-tasks.mjs:263 guardObservation/:331 assertExecution | Task 的 source/epoch/当前 grant 继续权威；initial observation 验证一次，不等同持续 view/pixel 新鲜度。Photo 不得借 observationVerified=true 跳过发图前检查。 |

## 先固定最小 PhotoContext 与观察合同

当前 public `concat-editor-mcp/src/photo.rs` 只定义请求：四种 typed opaque handle、PhotoExpected、PhotoParameters、PhotoSnapshotQuery 和两种 action；没有 PhotoContext 响应或 parent token 验证请求。`main.rs` 目前只有 Request 的两个 None 兼容字段，没有 Photo #[tool] 注册。下面是待 Root 审定的最小响应方向，不是当前已存在字段：

- Doc 外层继续 appInstanceId/projectId/documentSessionId/client grant；kind=photo 的条目额外携带 canonical documentId/stateId、selectedObjectIds、明确 bounded/truncated/busy/dirty 事实。JS 使用单独 photo 分支；不要求或伪造旧 numeric revision/contextRevision/selectionRevision。
- PhotoStateId 只引用 parent 持有的精确原读取映射，含完整 typed native `(child UUID, DocId, generation, revision)`、active/selected layer、截断和已证选择事实。native u64 永不进入模型 Number。不同 child、重新打开或 SaveAs 绑定变化不能靠标题或原路径找回旧 handle。
- 当前 `seecut-photo-host/src/photo.rs:95-97` 只有 has_pixel_selection；child `seecut_agent.rs:177-179` 明确 selectionBounds=null、measurement=notSampled。不能从这个布尔值、相同 revision 或相同 bounds 推断完整原 mask 未变化。首片可显示结构未知，但带像素选区的人类观察/视觉编辑链须拒绝或等待实际 snapshot coverage/selectionState 映射；最小可验收用例限定无 pixel selection、selected list 未截断。不要自动清选区。
- plain PhotoState 明确 mask/view unknown，不进入完整新 Photo HumanObservation。人类输入的 Photo 来源应绑定当时实际 ready Snapshot 的原 token、selectionStateId、viewStateId、scope/document/state/原选层与实际 adoption 证明；不能只给旧 schema 加 kind=photo，也不能用稍后重新 capture 替换初始输入。纯 metadata 问题可以说明结构事实，但不声称有完整 pixelMask/view 人类来源或模型已收到图。
- 新 photo observation 必须在 checkedObservationBinding/observationFromCurrent/sameObservation 的独立分支共同定义，再由 task-observation 的现有校验复用；不能仅给 legacy kind 数组加 photo。旧 numeric 记录和旧无观察记录不升级成今天的 Photo 观察。

## 普通 state → snapshot → edit → operation 的最小 JS 切片

新增 `src/photo-tools.mjs`，只解析 parent 已协商的闭合响应及 canonical handle，不向 child 转发任意 params/path。编辑 action 仅 layerSetOpacity/undo；model schema 隐藏 operationId/clientSequence，由原 journal 分配。准备时冻结原 PhotoExpected 和 semantic action；批准后重读并要求对应原状态/选择仍有效。Undo 使用独立批准和原操作。

photo_snapshot 先做原 read/media/账户/模型图片能力检查、原人类/Task source 检查、parent capture 与原 expected 对应检查，再验证同一个 token；只接受原 PNG 和 declared sRGB/selection/view/adoption 证据。工具新捕获的 token 必须关联原 applied input/Task round，但不能把工具观察改写成用户的新输入。返回后继续做权限和 source 检查。元数据、PNG/ICC/pixelHash、historical uiPaintSubmitted、postInputFrame、物理 present 是不同证据层级。

photo_edit 通过原 exact-call/Task/write gate。收到原回执后核对 operation/owner/expected/action、actual before/after 与独立当前 readback；applied 也可能是 no-op，不自行推断 history +1。failed/rejected 回执不能仅凭字段名证明无副作用。记录可证原事实，不把 RemoteRejected/transport Unknown 转为可重试。超时/断连保留原 journal 和 pending，photo_operation 只查询同一个原 ID；child 重启、内存回执缺失继续 Unknown。不得复用 runtime 的 managed model reconcile 来代替 native 编辑恢复。

## 图片实际外发前：准确位置与缺失入口

工具返回 image block、COTO 持久化该 block、图片实际包含在 provider 请求中是三个步骤。普通 Photo state、contributor 或 observationBinding 只能证明读取/绑定，不能证明模型看过。

已核对的真实链路：

1. `editor.mjs:228-258` 的现有 preview/clip_preview 在工具回包前做能力/授权/前后 observe，但此后仍有 COTO emit、media 持久化、context contributor、预算/quote 等 await。
2. 本机安装的 COTO `dist/core/session.js:640` 把 `{invocationId,message,metadata}` 写入 tool.completed；toolMessage(:512) 保留 message.id/toolCallId/toolName。metadata 不进入该 provider message 的 image part，但仍在原事件历史。
3. `media-store.mjs:39-71` 的 image 分支仅保留既有 ContentPart 像素字段或 mediaRef，丢弃 image-part 自定义属性。应使用原 tool.completed metadata + invocation/turn/message/toolCallId + PNG SHA 关联同 token，不能把凭据或 snapshot 元数据塞进 image part；也不能只以相同 PNG SHA 认同一次 capture。
4. COTO `dist/core/context.js:27` 先运行 contributor，再形成 request；旧历史压缩通过 withoutImageData 摘要，不证明旧图仍有效。应在最终 request 中识别真正仍存在的 Photo image，不凭工具曾执行过就记送图。
5. `managed-provider.mjs:310 stream` 在 await budgetFor/store 后构造 requestData/body；:339 quote POST 和 :355 operation POST 都包含完整 requestData。现有 taskGate 是同步 Task gate，没有原 snapshot 校验。`authorizeTool(:240)` 核实的是此前 managed 推理，不是下一次图片的原生新鲜度。

建议最小扩展是给原 per-session provider 注入**异步、只读的 request guard 回调**，保持同一 provider owner/managed store/模型操作/账本：

- requestData 冻结及 fingerprint 之前，按真实 COTO message 对应的已存事件 metadata 找到原 snapshot token，检查精确 PNG SHA、parent/doc/state/selection/view 绑定及原 invocation/turn/input。新 metadata 来自可信 parent，模型字符串不能声明它。这里的 token 是原 snapshot opaque handle，不是读写凭据；验证不生成新 grant，原生 u64/frame 关系由 Rust 保存和核验，不作为 JS Number 输出。
- 每次向 account service 发 quote 及创建 operation **前**都等待该 guard。对尚未被证实受理的原图检查 witness；对已有原操作受理证明的历史图检查原来源关联与当前 read/账户/会话控制，不重新要求五秒 witness。quote 已含图片，不能等到 create 才做首次检查。await 返回后立即重验原 signal、permissionChanging、账户/Doc read/media grant 和当前 Task/source epoch，再执行既有 transport。
- 对尚未证实受理的首次送图，guard 必须向可信 parent 对同一个原 token 复核；parent 查自己持有的 PhotoSnapshot，复用 validate_photo_snapshot_after_gate 和原 read/account/Task gate。不能发第二次 capture、换 snapshotId、只查询新 state，或把 stateId 相同当成 view/选择证明。五秒过期准确拒绝，不降低时限。
- 当前 public 四方法没有这种只读 token 复核入口，私有 ipc.mjs 也只有 parent→runtime 命令/响应与事件，没有现成 runtime→parent callback。建议额外闭合 readonly helper，由 trusted runtime/parent 通道调用且不模型注册；parent 根据原 token 查持有的 PhotoSnapshot，再真实 validate。需 Root 固定该内部读取通道并协商能力；不能偷偷在 frozen PhotoSnapshotQuery 中塞 mode/params/path，也不能公告第五个模型工具冒充已有能力。该入口未完成时不应把 Photo image 作为可用能力。
- guard 失败必须沿原 managed 状态机保留明确 pre-submit / uncertain 分类；在 operation POST 已可能发出后继续原 managed reconcile，不能为了视觉重试再建模型操作。不得篡改原报价 body/fingerprint、预算或费用定义。未受管 provider 的 HTTP 边界本轮没有证明可插入等价 guard，首片不承诺这类 provider 的 Photo 发图。

历史 Photo image 会在下次 request 再出现。原图一旦有原 managed 操作的精确受理证明，就可作为明确历史来源继续参考：不因五秒过期、Agent 自己已批准编辑、后续当前 view 改变而阻塞全部历史或偷偷重新 capture。历史图仍受原账号/会话/read 控制，但不授予当前编辑许可，不代表现在文档内容。没有受理证明的图片不能仅凭存在于 COTO 历史升级成已送图；它在再次首次外发时仍须原 witness 有效，否则失败/只保留准确历史文字，不能换新图冒充。失败前请求投影的调整只能发生在 fingerprint/quote 之前；冻结后不能悄悄修改 body。

### 写回原记录的真实边界

| 阶段 | 可记录的事实 | 写入现有何处、禁止的推断 |
|---|---|---|
| metadata state / ready snapshot / tool.completed | state-only 未见图；或已取得原 token/PNG SHA、原 selection/view、原 invocation/turn/input/source 关联。 | 在原 tool.completed metadata 保存来源与首次未送图标志；COTO image content 仍走原 MediaSessionStore。工具完成不等于 provider 受理。 |
| 原 requestData 已冻结但尚未 POST | 原 PNG 确实列入该 requestFingerprint，且首发 witness 验证在该边界通过。 | 将 bounded photo 输入引用和验证证据附在同一个 managed operation 的元数据，跟随 prepared/quoted/creating 持久事务。不能新增媒体/结果/收费账本。 |
| quote 已返回 | account service 给原 body 的 quote_id/request_digest。 | 仍是原 managed op 的 quoted 事实；不是模型受理，不能据此标模型看过。quote refresh 仅保留原 body/ID 的既有规则。 |
| operation POST 获真实且通过 checkOperation 的原 snapshot | 原 client_request_id/local_session_id/turn_id/step_id/budget_id/request_digest 与已冻结 op 完整一致。 | 在 recordManagedSnapshot 原 store.mutate 内为该 op 的原 Photo 输入补充 managedRequestAccepted 证明，引用原 operation_id、request_digest、版本/state。这是 account service 已受理该原请求；queued/dispatching 不证明上游模型已理解图片。 |
| POST 超时/断连或回包未验证 | 提交可能发生，接收状态未知。 | 原 op stage=unknown/原 managed reconcile；不另建 op 或重发图，不将 quote 或本地 tool metadata 升级为 accepted。仅 GET 原 operation/by-request 得到同 request_digest 的真实 receipt 后，才按上一行补证明；不能因当前 witness 已过期而改写那次历史首发事实。 |
| 原成功 managed result 并 doneDelivered | 原请求链成功完成、原结果已交 COTO。 | 继续原 store 的 settled/doneDelivered 事实，不新开推理；这是成功请求/交付边界，不等于模型视觉结论正确。 |

`checkOperation` 位于 managed-provider.mjs:50，确实核对上表身份和 request_digest；recordManagedSnapshot(:77) 在同一 ManagedOperationStore.mutate 队列合并原操作和预算。后续只增加有界输入来源证明字段及严格校验，不动余额/结算定义。原 COTO tool.completed 是不可改写的历史事件，首次记录不能事后伪装成当时已受理；读取展示可将原 metadata 与同一个 managed op 的后续证明 join 成派生投影。forwardManaged(:189) 当前只是状态投影，也不能单凭它广播过就冒充持久送图成功。恢复时必须从原记录核对完整关联，缺字段的旧记录保持 unknown/not proven。


## 历史 HumanSource 与当前执行上下文必须分开

`task-observation.mjs:7 humanSources/:16 humanSourceProjection/:29 roundSource` 按原 create/update clientRequestId、planRevision、原 observationBinding 保存来源；旧记录不得用当前观察补造。`doc-observation.mjs:40 checkInitialObservation` 只认当前 turn 的唯一 input.applied→input.accepted，不取最新 pending/steer 代替。`runtime-tasks.mjs:242 acceptedRound/:253 observationAdmission/:263 guardObservation` 要求原可信调度对象、唯一原输入、roundSource 与 epoch；初始核对完成后 observationVerified 是历史事实。

发图前同时复查两件事：原工具图片属于哪一个原 invocation/turn/applied input，其历史受理证明是否真实；**现在**是否仍是允许执行的 session/account/Doc grant、Task controlEpoch/activeDispatch/step 与 sourceRound。当前 Task update 的 controlEpoch 变更和 fences 仍必须阻止旧轮后续 quote/create，即使历史 HumanSource 本身合法。不得让新的 followup 观察覆盖旧轮原 source，也不能要求旧历史照片永远代表当前选择。初始 Photo HumanObservation 只在正确原输入首次进入时核对；已批准的 Agent 编辑后，当前执行 readback 可沿真实 receipt/原适配规则推进，不能永远要求当前 stateId 等于初始 HumanSource。新的工具 snapshot 自有 token 对照其当次 current expected，原 source 关联保持不变。

## Task update 的图片是独立工作

当前 `runtime-tasks.mjs:143 command(update)` 白名单只有 sessionId/taskId/clientRequestId/expectedRevision/message/futureSteps/futureDeliverables/observationBinding；`task-coordinator.mjs:146 update` 只接 message 和可选 observationBinding，followup(:170) 存 text。`task-store.mjs:114` 校验 followups.text。没有 image/content 字段。

Task create 可用既有 content/MediaSessionStore 保存附件；`runtime-tasks.mjs:289` 只在 ordinal=1 hydrate definition.content，后续 update 的 message/historical observation 不会自动产生图片。不能把 observationBinding 或原创建附件说成用户这次补充图已送给模型。

若下一阶段支持 update image，需要独立合同及实现：扩展同一 update 的内容/原 snapshot 证据、幂等 requestDigest、原媒体引用/大小/模型能力验证、followup 持久化、humanSourceProjection 与轮次附件选择；沿原 controlEpoch/cancel/continuation 规则送入新轮。不能重建 Task 定义、模型选择/总积分/grant，也不能把图片放进 text。旧无图 update 必须兼容，旧记录不补造附件。

Photo Task checkpoint 也尚未兼容：runtime-tasks.mjs:23 deliverableSchema、task-store 的 workspace 枚举无 photo；:423 touchesProperty、:483 当前独立 readback 仅 Canvas/Clip。最小复用原 document-property / opacity + 原 native-operation 证据，新增 photo workspace 分支与真实 photo_state 层值复读，不新增 Task 调度器或第二交付账本。不能凭 photo_edit 工具成功或普通 state 读取直接完成 Task。

## 下一轮建议交付与验证

先定 PhotoContext/Photo observation 响应，以及非模型原-token验证通道。然后 JS 业务实现限定 photo-tools.mjs + editor/workspace/view/doc-observation/runtime/media-store 的必要接线；managed-provider 仅增加原 provider 请求 gate，不重写支付/报价/结算；Task 初始来源适配可复用原结构，update 图片与 Photo checkpoint 另列验收。

最小测试应覆盖：canonical opaque/Number 拒绝、metadata-only 无送图宣称、原状态/选层变化拒绝、同像素不同 token 不混用、首次过期/撤权/child restart 零自动 capture、工具回包与 quote/create 之间 Task update/取消阻止外发、quote 不升级受理、create Unknown 仅原查询、原 receipt 证明后历史图不被五秒永久阻断、Agent 批准编辑后仍可继续历史参考、旧来源不冒充当前 readback、原 request 指纹不变、provider/模型账本复用、独立 Undo 批准、旧 Canvas/Clip/Flow/Task 记录兼容。Task update 当前传 image 必须拒绝；以后支持时另测原附件来源/幂等/媒体限额/续轮。

本报告没有运行这些测试。后续 fixture/单元测试、真实 parent MCP 进程、实际 managed provider image 请求、原 Task/批准/receipt/undo/checkpoint 是不同验收层级；没有实际交图的读取不属于模型视觉验收。
