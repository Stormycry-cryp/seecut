# App 普通页面导航协议 v1

状态：协议、日志核心、后台队列、原生宿主与配套运行时已集成，本地定向验证通过。实际App导航、账号授权交互和付费模型链仍待验收。

## 范围与权限

仅支持 `generation / personalLibrary / teamLibrary / templates / account / credits / settings / projects` 八种普通页面。打开工程、生成、上传、付费、Doc 编辑及跨工作区 Task 不属于本协议。

AppRead v1 保持原行为。App 范围另协商 `appControlProtocolVersion:1`，省略时使用旧协议，显式 null 或未知版本拒绝。控制凭据是独立 UUIDv4，不复用 Doc 写令牌，不改变读取和页面操作两档的产品权限。模型不能通过参数或页面内容授予权限。

| 范围 | 原生 capability 工具数 | 规则 |
| --- | --- | --- |
| Doc | 29 | 继续按实际工作区过滤，不包含 App 导航工具 |
| AppRead，未协商控制 | 4 | `capabilities / app_context / page_items / page_preview`，`readOnly:true` |
| AppControl v1 | 6 | 另含 `app_navigate / app_operation`，`appControlSupported:true`，独立版本为1；`readOnly` 为 `controlGranted` 的反值 |

stdio 原始目录保持旧32项；只有协商控制的 App 进程增加到34项。上述四/六项约束属于原生 capability 和私有 App 工具范围。模型不支持图片时再去掉 `page_preview`。

`controlGranted/readOnly` 投影当前具名client和账户的原生grant状态；读取capabilities不携带控制token，也不授予执行权。实际导航始终另核本次请求的独立控制token、原意图和单次permit。原操作查询继续只需当前读取许可。

## 模型参数与原操作身份

模型只提供 `app_navigate {target}` 或 `app_operation {operationId}`。可信运行时填入其余字段：

```text
app_navigate {
  appSessionId, operationId, sessionId, clientSequence,
  before: {pageId, pageEpoch, pageStateId}, target
}
app_operation {operationId, sessionId, clientSequence}
```

`sessionId` 是原 COTO 会话，`appSessionId` 是实际原生 App 会话。client、原 COTO session 各最多64 UTF-8字节；序号是非零安全整数。原ID格式为：

```text
sca1:1:<client字节数>:<client>:<原session字节数>:<原session>:<序号>
```

运行时从原会话实际持久化的 `app_navigate tool.started` 顺序派生序号，包括失败或未派发项；每个原会话最多128项。不由模型、自报结果或重试生成新ID。

原生请求独立携带 `app_control_token`；导航仍使用当前 App 读凭据。查询不带控制token、旧AppSession或当前页面参数，只凭当前实际同账户/client读许可核对原ID。

## 可信宿主握手

运行时转发真实 `tool.started` 的副本时，在 `data` 中增加：

```text
appControlIntent = {version:1,inputId,operationId,clientSequence,before,target}
appViewBinding = inputId 对应原 input.accepted 的完整七字段
```

七字段为 `appInstanceId/clientId/accountScope/appSessionId/pageId/pageEpoch/pageStateId`。原COTO事件、原输入和旧操作不被改写。同turn后来的合法steer按实际accepted/applied事件核对；新导航使用其开始前实际贡献的输入。普通文字与最终result仍保留该turn最初问题的绑定。

原生宿主核对自己保存的人类输入、真实applied/started、当前账户、控制许可、来源和已确认的自有导航链，将同一原始意图交给唯一后台journal保存为queued。收到真实Prepared后，UI再次核对原turn、来源和许可，将记录接受到Registry并登记精确内存permit，才发送允许的可信IPC：

```text
app.control.authorize {sessionId,inputId,turnId,invocationId,operationId,allowed}
```

六字段闭合，`allowed` 为布尔值，不带内层version。queued仅保存原意图，不授予执行权；允许ACK前禁止进入running或派发导航。ACK也不表示业务已派发或完成。错误身份、冲突决定、迟到ACK及撤销后的ACK都不能产生执行权。

## 原生执行与完成

单一后台线程依次执行原journal的prepare、running落盘、终态保存及确认，UI不等待journal文件I/O。原始raw请求只能消费ACK前已登记的同一意图，不能再次prepare或分配新业务身份。收到精确匹配的raw请求后，才以真实文件/父目录同步获得running证明；最终UI回调内重验账户、读/控制许可、精确permit、租约和完整before，随后立即调用原具名导航。

原生单独分配NavigationRequestId，在同一次受控导航的实际采用出口捕获真实目标页；只有该事实可调用complete。普通回调返回、pageEpoch变化、后来手动到达相同目标、模型文字及页面JSON相等都不构成采用证明。Projects进入实际画布项目列表；Settings沿原设置开关。Settings覆盖Clip/Canvas/Flow工作区时，本批不授予页面操作许可，不自动关闭设置或跨文档。

完成事实经后台保存、真实屏障及有效owner的ACK后，才能返回confirmed。撤权、停止、新输入、账户/会话/窗口变化先同步撤销执行，再异步保存原事实。已接受的running写入仍按顺序保存，以便后续取消修订连续；迟到proof不能恢复执行。

prepare期间失效且尚未被Registry接受的唯一Prepared，由原journal的cancel_prepared消费并保存原操作的cancelled终态；已接受记录沿Registry原取消流程保存。这样，允许ACK后、raw到达前停止，也能查询原操作的取消结果。迟到prepare只能清理原意图，不能补发允许ACK；真实writer失效、文件错误或未确认保存仍保持UNKNOWN。

## 回执与恢复

成功仍为 `Response {status:ok,data:AppNavigationReceipt}`，只投影原ID、原origin的instance/client/AppSession、before、target、recordedStatus、status、durability、completion、humanGate与独立currentObservation。内部authority、账户对象、路径和凭据不进入回执。

- `completed + confirmed` 必须有原导航实际目标binding，运行时还独立读取完整新页并精确匹配后，才能推进该turn读取基线。
- recorded completed但终态保存未确认，仍返回effective outcomeUnknown；running、断线和模糊结果只查询原ID。
- currentObservation独立描述当前真实页面，允许与原完成页不同，不能代替原完成事实。
- 普通磁盘query不产生持久确认。恢复的confirm_query必须对同一原记录重新完成真实文件和父目录同步；它不创建Registry、Prepared或执行许可。running恢复后仍为UNKNOWN。
- 可信 `app.reconcileOperation {sessionId,invocationId}` 只在原turn停止后查询原调用，确认终态后核对原COTO项，不派发模型或导航，不清除未知费用。`app.abandonReads` 不得弃置未决控制写入。

整个派发前预检覆盖授权、ACK和原生读取的取消及总5秒期限。尚未调用原始导航时，可返回独立wrapper结果 `not_dispatched/navigationSent:false/resultAvailable:false`；这不属于native业务完成回执。原受管费用仍独立收尾。进入原始导航之后的模糊状态保持UNKNOWN；进程崩溃且缺少原生终态时，notFound不能被当作未派发证明。

## 当前证据

冻结运行时569项、公共服务合同36项及独立4场景/14项运行时/2项本地受管账务复核通过。主代理核对111文件逐字节采纳，并在live重跑4场景通过。纯核心31项、恢复6项、后台线程13项及准备中取消12项在同一独立harness共62项通过，另有10项不可伪造凭据的编译拒绝验证。

MCP正常all-targets stdio测试68项、严格Clippy和正常CLI构建通过。主代理用正常CLI与live运行时完成15组真实COTO联合检查，包含宿主ACK、工具裁剪、撤权、丢失ACK和原操作恢复；原始32项工具对象保持不变。

最终原生/Slint测试编译、宿主含tests严格Clippy与工作区格式通过。同一最终二进制六组无交集测试415项通过、既有辅助1项忽略，742项中选择416项；七项新适配层用例使用实际Generation与真实journal worker，导航采纳使用明确的测试fixture。主代理核对实际二进制、32文件源码指纹、各组日志及实际测试目录一致。

这些测试使用真实COTO、文件屏障、线程或本地服务配合合成模型/native端点；实际App采纳、模型看图和作品质量仍分别待验。最新阶段及完整CI结果见[验证记录](../testing/main-qa/2026-10-20-full-agent/README.md)。
