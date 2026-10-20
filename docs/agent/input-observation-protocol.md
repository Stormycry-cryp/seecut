# 文档初始观察协议 v1

普通文档发问会保存点击时的工程、选择和视图。此引用只说明问题来源，不授予编辑、Task 或计费权限。原生接线范围为已有的人类 follow_up；运行时另支持 interrupt，但本批没有新增原生入口。

## 发送与绑定

Doc initialize 请求 `documentObservationProtocolVersion:1`，运行时确认 `document_observation_binding:true / document_observation_protocol_version:1` 后才启用。旧运行时缺能力时，普通发送明确不可用；连接与原操作核对保留，不能静默改为未绑定发送。App 范围禁止这个字段，继续独立的 AppRead v1。

原生从当前读取许可对应的真实 `BridgeUi::handle(Context)` 捕获数据，在附件整理前创建原 inputId 与 `DocumentObservationBinding`，保存进原 RequestSnapshot，再发送。不能从助手展示摘要补造字段。

绑定为闭合对象：

```text
appInstanceId, clientId, kind, projectId, documentSessionId,
revision, contextRevision, selectionRevision,
selectedObjectIds, viewStateId
```

kind 为 canvas/clip/flow，修订号为非负安全整数。选择保留真实完整顺序，不重复或截断。viewStateId 字段必须存在，值为已有 view-v1 摘要或明确 null；它不证明文件内容、可见范围或焦点。Flow 必有 `{canvasId,accountScope,sessionId}`，sessionId 等于 documentSessionId，viewStateId 为 null；画布和剪辑禁止 Flow 字段。

## 受理、历史与恢复

运行时在发送队列前复制输入，在模型准备与附件持久化前核对原观察，并在附件处理后再读取一次真实 Context。首次上下文准备根据该轮真实 input.applied 的 inputId 找回唯一原 input.accepted，核对后才进入原有选择和操作回执规则。

同一 inputId 增加、删除或改变绑定均拒绝。重复 ACK 仅返回原回执，不重新读取、推理或执行。原操作 UNKNOWN 继续沿原 operationId 查询和独立回读，不因旧观察过期重做写入。

历史 Store 检查类型、inputId 与 Doc/App/Task 互斥结构；历史的 project_key 只是原工程历史域，不能反推出 instance/client/documentSession。完整新发送范围由真实捕获、控制器与运行时分别核对。旧记录缺字段继续可读，不填入当前身份，也不读取 RequestSnapshot 自动重发。

## 当前边界

本协议约束普通输入的初始阶段，后续仍沿原操作和独立回读推进。页面操作档通常走 task.create；Task create/update 的人类观察来源、自动续轮和冲突处理使用[独立Task协议](task-input-observation-protocol.md)，不能用普通 follow_up 的验证代替。bound steer、普通输入多来源及模型完成时的视图推进仍未声明完成。

逻辑、协议与真实 COTO 测试不代替实际 App、授权按钮、模型观看或作品质量验收。证据见[本轮验收](../testing/main-qa/2026-10-20-full-agent/README.md)。
