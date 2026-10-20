# Clip 原位置保存协议 v1

状态：主代理已确定下一批接口设计，后端与配套运行时仅准备隔离候选。本文不是已实现或实际App验收声明。实施顺序见[保存计划](../plans/2026-10-21-agent-clip-save.md)。

## 工具与范围

复用 `clip_edit`，增加无参动作 `edit:{kind:"save"}`；原外层projectId、documentSessionId、timelineId、revision、selectionRevision及可信注入的expectedEditStateId保持。保存整个当前Clip工程原位置，不只保存指定时间轴，不增加编辑revision或撤销项。原operationId/clientSequence、精确许可和单一OperationJournal保持；未知只查原 `clip_operation`。

原生在Clip capability独立公告 `clipSaveProtocolVersion:1` 和 `clipPersistedReadProtocolVersion:1`。缺少任一能力时禁止准备保存，不回退为手动点击、关闭工程或任意路径写入。既有工具数量不变，旧动作和默认project读协议保持。

`project` 增加可选且唯一允许值 `readSource:"persistedClip"`，与objectId/objectIds/offset/limit互斥。该模式仅读取当前真实Clip Session的原位置manifest；模型不提供路径、manifest名称、期望hash或JSON。不提供readSource时保持原内存对象读取。

## 原回执

保存沿用原响应envelope和原身份：

```text
appInstanceId, clientId, projectId, documentSessionId,
operationId, clientSequence, tool:"clip_edit"
```

受理后的保存回执还必须保留原冻结来源：

```text
edit:{kind:"save"}, generation,
sourceRevision, sourceSelectionRevision, sourceTimelineId,
sourceEditStateId, sourceDocumentStateId, saveProof
```

这些字段来自原请求与原生实际快照，不用完成时的新工程填充。sourceDocumentStateId沿既有 `clip-v1-<64位小写SHA256>` 算法；各版本和计数必须是非负安全整数。

saveProof是version=1的封闭联合：

| state | envelope status | 必要字段 |
| --- | --- | --- |
| pending | pending | version/state；仅表示原操作受理，不能推断尚未提交 |
| notCommitted | saveNotCommitted | version/state、commitClaimed布尔值、renameAttempted:false、reason |
| verified | ok | version/state、saveTicket、persistedRevision、manifestName、byteLength、snapshotSha256、manifestSha256、persistedDocumentStateId、documentVersion、durability:"confirmed" |
| outcomeUnknown | outcomeUnknown | version/state、commitClaimed:true、renameAttempted/renameSucceeded布尔值、stage |

saveTicket是原SaveLane票据的规范十进制u64字符串，只有内部归属意义，不新增业务操作身份。manifestName只允许concat.json或wolfcut.json；byteLength为1..16777216；SHA256为64位小写十六进制；成功时两个文件摘要必须相同，persistedRevision等于sourceRevision，persistedDocumentStateId等于sourceDocumentStateId。documentVersion是实际文件的可读工程版本。

notCommitted.reason闭合为 `cancelled/expired/superseded/sourceChanged/ownershipChanged/targetChanged/resourceLimit/ioFailure`；只有原生能证明未尝试rename才能返回。CAS已发生但时钟后验明确到期时，commitClaimed:true仍可返回expired，不能伪称未获得提交权。

outcomeUnknown.stage闭合为 `afterClaim/rename/directorySync/readback/engineParse`；renameSucceeded:true必须同时renameAttempted:true。读回不符、目录同步失败和确认丢失不得以普通ioFailure当作未写入。未包含合法保存证明的ioFailure不能终结原保存操作。

verified只能由真实rename、文件/目录同步、新开原manifest、有界字节与hash核对，以及实际engine解析与原冻结Project等价后构造。CAS、dirty=false、内存相同、模型文字都不能构造成功。保存不复制媒体、不生成成片。

## 独立落盘读取

`project(readSource:"persistedClip")` 返回封闭对象：

```text
clipPersistenceProtocolVersion:1, source:"freshManifest",
appInstanceId, clientId, projectId, documentSessionId, generation,
observedRevision, manifestName, byteLength, manifestSha256,
documentStateId, documentVersion
```

observedRevision只描述冻结时内存观察，不是从文件推断的持久revision。documentStateId通过实际文件engine解析所得Project按既有clip-v1摘要算法计算，必须有界，不能调用可能panic的未预检摘要路径。

UI冻结真实Session、原owner/lane、账户和读取来源，worker在同lane下新开文件读取。返回前再次核对原读许可、账户、会话及文档generation；失效时不把旧路径内容返回到新范围。读回不授予写权，不推进编辑revision。

运行时完成当前保存验证前，须独立调用该读取模式，精确匹配原generation、实际文件名称/长度/摘要/解析摘要及当前观察。若后来保存或编辑已改变当前内容，原历史保存事实保持，当前交付核实失败。恢复的allowDetached只能核原不可变证明，当前读回仍为pending。

## Task与恢复

新增typed交付 `kind:"clip-save",workspace:"clip"`，其余沿现有id/description/stepId；通过本步骤真实轮次的原clip_edit Save、native-operation evidenceRef及原完整回执摘要关联。Task完成前独立读取实际manifest并将核对摘要写入现有deliverableProof，不新增保存历史账本。

原生24小时/1024条内存回执与重启丢失边界如未改变必须原样保留。没有原回执时，当前文件内容相符不能消除原UNKNOWN。恢复不换ID重做、不清未知费用，也不借当前读许可恢复写权限。

本批不扩大32步、单工具并发、15秒MCP请求、16MiB状态、56KiB回执等既有限制。超时不证明worker已停止；继续查询原操作。新增实现必须与本合同逐项验证，未实现字段不得伪造填充。
