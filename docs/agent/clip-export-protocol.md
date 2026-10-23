# Clip 视频导出协议 v1

状态：工具、原回执、原生接线和持久Task已按本合同实现，适用检查见[导出QA](../testing/main-qa/2026-10-21-clip-export/README.md)。源码和逻辑检查不等于真实App、模型创作或作品验收。macOS优先，Android暂缓。

## 工具与固定来源

复用clip_edit增加结构化动作，所有字段必填并拒绝额外字段：

```json
{"kind":"exportVideo","name":"成片名称","codec":"h264","shortSide":1080,"fps":30,"quality":"balanced","tenBit":false}
```

name为1..256 UTF-8字节，去除首尾空白后须与原值相同，不允许路径分隔、NUL或控制字符；仅为显示名称，不作为文件系统目标。codec闭合为h264/hevc/av1；shortSide为720/1080/1440/2160；fps为24/30/60；quality为high/balanced/small，复用当前原生CRF16/20/26，preset固定veryfast；tenBit必须布尔。尺寸复用面板按原时间轴宽高比例和偶数边长计算，仍受单边8192及总像素16777216限制。模型不能提交文件路径、工程JSON、滤镜或证明字段。

外层沿原projectId/documentSessionId/timelineId/revision/selectionRevision、可信expectedEditStateId、operationId/clientSequence。native审批捕获原generation、Project摘要、真实活动时间轴和输出选项；旧raw到达时若来源已改变，保留原审批来源返回明确拒绝，不填入完成时的新工程。

受理时使用当前Host同一Exporter.begin，捕获原Project、工程目录、Catalogue、可跨线程Titles、账户/Agent会话、原library root及原lease。应用分配opaque outputId和registrationId，输出仅落入受管clip-exports目录，不能用模型name拼接路径。原生不增加编辑revision、不标记已保存、不修改撤销历史。

worker从冻结请求通过与renderer共用的render_facts得到准确帧数、结束时间和是否计划音频；不能另写一套round/transition/sound算法，也不能从成片probe回填期待。输出参数规范化在Project的私有派生副本内进行，原工程摘要与原来源保持；输入清单可记录派生副本摘要，不能冒称二者相等。

原生capabilities分别公告clipExportProtocolVersion:1与clipExportOutputProtocolVersion:1；缺少任一能力不允许准备/发送exportVideo，不回退为任意路径或手工脚本。

## 同一原任务的生命周期

受理/回复期限只约束受理，worker不沿用单次MCP的15秒或原审批permit的短有效期。已受理任务绑定原账户/Agent会话、文档范围、写授权及可选原有限lease，最终发布按[发布合同](clip-export-publication.md)仲裁。普通同工程编辑和选择不撤销冻结导出；切工程/关闭、切账号/会话、降为读取、明确撤权或Task暂停/停止在发起事件时撤销原Revoker，不能等待Studio借用。

只使用原Job.cancel_handle，不迟到调用全局exporter.cancel。发布先取得资格时，后续停止不得把已经发布或登记的事实改成未发生；原完成结果始终保存到原操作，忙时保留队列，不写入新文档或新账号的当前交付。

## 原导出回执

沿用原envelope及以下固定身份和来源，tool始终为clip_edit：

```text
appInstanceId, clientId, projectId, documentSessionId,
operationId, clientSequence, tool,
edit, generation, sourceRevision, sourceSelectionRevision,
sourceTimelineId, sourceEditStateId, sourceDocumentStateId,
outputId, registrationId, exportProof
```

edit为原规范化exportVideo动作；原source字段与保存协议同规则，sourceDocumentStateId仍为clip-v1加SHA。所有版本/计数须为非负安全整数，clientSequence非零；opaque outputId/registrationId为原生UUID字符串，不新增业务操作历史。

exportProof为version=1的封闭联合：

| state | envelope status | 必要字段 |
| --- | --- | --- |
| pending | pending | version/state、phase、framesDone、framesTotal |
| notPublished | exportNotPublished | version/state、publishClaimed布尔、publishAttempted:false、reason |
| registrationPending | partial | version/state、inputManifestSha256、expected、summary、durability:confirmed |
| verified | ok | version/state、inputManifestSha256、expected、summary、durability:confirmed、asset |
| outcomeUnknown | outcomeUnknown | version/state、publishClaimed:true、publication、stage |

pending.phase为preparing/rendering/verifying/publishing/registering；准备期总帧数可为0，已知时framesDone不得超过framesTotal，进度不作为完成证明。notPublished仅在能够证明原任务未尝试最终发布时构造；reason为stopped/revoked/accountChanged/sessionChanged/documentChanged/leaseExpired/sourceChanged/unsupportedInput/resourceLimit/engineFailure/outputInvalid/ioFailure。CAS后到期但hook拒绝时publishClaimed:true仍可notPublished，不回滚CAS事实。

outcomeUnknown.publication为unconfirmed或confirmed，分别表示实际发布尚未确认或已确认发生；stage为publish/directorySync/outputReadback/inputPostcheck/registration/completion。confirmed不证明文件当前存在或仍同内容。通用查询失败、原回执丢失或重启不能填造这些细节；没有合法exportProof时继续保留原UNKNOWN，不换ID重导出。

expected来自原冻结请求共用的渲染计划，字段为width/height/rateNum/rateDen/frames/codec/tenBit/audioExpected。summary来自实际完整核对，字段为byteLength/sha256/codec/bitDepth/width/height/rateNum/rateDen/videoFrames/firstVideoPositionSeconds/lastVideoPositionSeconds/videoSeconds/audioStreams。每条音轨含index/codec/sourceSampleRate/sourceChannels/sampleFrames/seconds/peak/rms，最多8条。bitDepth仅为8或10并匹配期待，所有实际源帧一致；summary不使用容器duration代替完整解码。字节、视频和音频上限及CFR/AAC误差沿[成片核对合同](clip-export-output-verification.md)。音频时长由实际标准化48000Hz双声道采样帧数计算，有限peak/RMS不表示已试听。

verified和registrationPending均须先核输入副本、临时成片完整解码与摘要，实际文件/父目录同步，以及独立最终路径同内容核对。登记前对真实prepared副本做原VerifiedOutput的内容核对，发生在pending journal/commit前。verified还须沿原registrationId取得固定assetId/version，独立核相同版本的managed文件。asset使用既有{assetId,version}形状，version.sha256等于summary.sha256，version.registration_id等于operation:加registrationId；previous_id沿既有固定版本规则。登记已经发生但未获核验成功可以保持registrationPending，不声称从未入库，不删除或覆盖真实结果。

## 查询、取消和登记恢复

clip_operation保持只查询原操作，不隐式取消、登记或重新渲染。

新增clip_export_control写工具，参数只含projectId/documentSessionId/operationId/clientSequence/action；action闭合为cancel/retryRegistration，目标必须是本client本会话的原exportVideo。它复用原导出身份，不分配新的导出操作或clientSequence；控制调用有自己的COTO invocationId。cancel和登记恢复都须适用的真实写权限与精确控制许可，不能借只读查询发起写入。原生Task暂停/停止通过原Revoker直接取消，不等待模型再发工具。

控制回执为单独封闭外层：clipExportControlProtocolVersion:1、action、operationId、clientSequence、result、original。original是同一原导出的真实Response，保持tool:clip_edit和原来源；取消控制不能改写已成功原回执。result为cancelAccepted/alreadyPublishing/alreadyTerminal/registrationAttempted。retryRegistration仅允许已经核实成片的registrationPending，沿原登记身份和原固定内容恢复；未知发布或缺失原证明不能借它重新渲染。外层控制成功本身不代表原导出完成，运行时仍核original和独立输出。

新增clip_export_output只读工具，参数为projectId/documentSessionId/operationId/clientSequence。原生必须先找到同一原成功或已核实成片回执及原账户/会话归属，模型不提供路径、assetId、version或期望hash。只有verified可给出当前交付成功读取；registrationPending只返回尚待登记的明确状态，不能完成Task。

后台原操作查询和独立成片核对可以发生在两次推理轮次之间。仅存在真实原导出附件、当前read/media权限且原scope/client/账户/会话一致时，原clip_operation及clip_export_output沿受管原结果读取，不要求再启动模型轮次取得named permit。其它clip_operation保留既有审批；该例外不开放宽泛素材检索或任何写入，control始终要求当前写权和本次精确许可。

成功读取的封闭数据为clipExportOutputProtocolVersion:1、source:freshExport、appInstanceId/clientId/projectId/documentSessionId/operationId/clientSequence、outputId/registrationId、inputManifestSha256、asset、summary、checkedAtUnixMs。实际新开原输出与精确asset/version对应的managed文件，字节/摘要一致可沿用原完整解码事实。不同版本、回收站、文件丢失/变化、来源或许可失效均不能返回成功。只读核对不恢复写权或原发布历史，普通同工程revision变化不冒充原素材变化。

## 运行时与Task

导出沿现有OperationJournal和原调用证据保存，pending/partial/unknown只查询原操作；受理后resourceLimit、leaseExpired或查询ioFailure不是原任务未发布证明。所有回复先核原完整身份、来源、原输出身份、封闭schema及字段交叉约束，不接受模型自行填写成功证明。

首次合法回执确定的outputId/registrationId不能被后续回复替换。通用无证明查询失败保留原合法内容事实；registrationPending只能沿同一输入清单、期待、成片摘要和登记身份推进到verified。notPublished及verified是原操作终态，后续取消或查询失败不能把它们改写；当前文件核对失败单独保留当前读取未确认。已经确认的publishClaimed或历史publication不能倒退，已知总帧数也不能由完成回执换成另一份成片。

Task增加kind:clip-video、workspace:clip，仍沿既有id/description/stepId；通过当前步骤真实轮次的原exportVideo及native-operation evidenceRef关联。完成前独立调用clip_export_output，精确匹配原outputId/registrationId、固定asset/version、summary摘要与inputManifestSha256，再保存当前deliverableProof。历史allowDetached只保留原历史事实，当前读回仍pending，不能完成新视频交付。

同一步可以制作多份成片，也可以在已证明未发布的可恢复失败后继续尝试。新的尝试仍须当前来源、权限、预算和独立原操作，保留旧终态。原任务尚未明确、尚待登记或只恢复了历史成功却未重新读取成片时，先核实原任务；不能靠新导出替代核实。已经取得当前固定成片读取成功后，才可继续同一步的下一份合法导出；多份独立视频交付分别关联各自原操作。

已确认原生受理的pending导出由现有Task coordinator自动查询，轮询元数据保存在同一原journal的related invocation，重启不清零。先结束原轮次和推理预算，等待期间只真实查询原clip_operation；按1/2/4/8/15秒退避、最多240次，仍受原Task总时限、暂停/停止代次、账户、文档、当前权限、费用和容量限制。初始ioFailure/超时且没有合法受理证明时继续UNKNOWN，不能自动视为正在运行。原成片完成并独立读取成功后才自动续轮，不新增导出或等待期间的推理费用。

Task仍单文档，原生回执仍24小时/1024条内存记录及重启可能丢失；本批不增加第二套业务账本、不放宽单轮120秒/32步或工具/回执预算。实际App、模型创作、画面/字幕/声音与正式分发分别验收。
