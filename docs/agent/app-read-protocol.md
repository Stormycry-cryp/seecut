# App 页面读取协议 v1

适用范围为 macOS 候选中的生成、个人素材、团队素材、模板、账户、积分、设置和项目列表。打开的画布、剪辑或 Flow 文档继续使用原文档协议。当前仅接入页面读取，后续页面操作与跨工作区任务交接独立验收。

## 身份和读取许可

App 范围使用真实 `instanceId / clientId / accountScope / appSessionId`，不填充替代的 `projectId / documentSessionId`。`accountScope` 是规范服务根地址、NUL 和可信登录用户 ID 的 SHA256；账户代次改变会更换 AppSession。普通导航保留 AppSession 和聊天历史。

Agent 初始化明确提供 `scopeKind:app`、`appReadProtocolVersion:1`、账户与 AppSession，完全省略文档身份、写 token 和 Task 协议。必须协商成功 `app_read_scope:true / app_read_protocol_version:1`。原生与私有运行时分别核对当前账户和原会话归属，不能用模型参数授权。

stdio 客户端使用原 `SEECUT_MCP_CLIENT_TOKEN` 和三个可信启动变量：`SEECUT_MCP_SCOPE_KIND=app`、`SEECUT_MCP_ACCOUNT_SCOPE`、`SEECUT_MCP_APP_SESSION_ID`。缺字段、混合文档身份或附带写 token 均拒绝。模型不能修改客户端启动环境。

用户在现有读取控件中明确允许页面及已有媒体预览。许可在当前用户会话内有效，没有五分钟期限；撤权、账户变化及重新授权会使旧 token 失效。App 许可与文档读写许可分别管理，不产生 Task 或编辑权限。

## 工具和数据

| 工具 | 参数 | 结果 |
| --- | --- | --- |
| `capabilities` | 无业务参数 | 当前 App 范围、实际读取许可、四个只读工具和资源上限 |
| `app_context` | `appSessionId` | 页面、筛选/表单/选择、列表数量与当前预览身份 |
| `page_items` | AppSession、页面三字段、`offset / limit` | 当前筛选结果的连续分页，默认 0/20，最多 50 项 |
| `page_preview` | 同一页面、原 `itemId / previewVersion` | 已载入 UI 图像或缩略图的 PNG，最大边长 256 |

三个新工具的成功结果统一为 `{status:ok,data:{scopeKind:app,appInstanceId,clientId,accountScope,appSessionId,page,...}}`。页面为 `{pageId,pageEpoch,pageStateId}`；导航代次及图像缓存版本为非零安全整数。`offset / limit` 可以省略，显式 `null` 拒绝。

`pageStateId` 固定用户视图，包括筛选、选择、表单、参考顺序、条目身份和当前预览。后台进度、下载状态、余额和费用不改变它；普通数据另有 `contentStateId`。实际导航事件递增代次，包括 A→B→A。列表来自真实 UI 缓存；`filteredList` 不表示屏幕可见卡片，视口和焦点未测量时明确为未知。

项目路径只保留在宿主映射中，对外使用不透明条目 ID。表单、名称、素材文字及图片作为普通工具结果，不进入可信系统指令。快照最多 2048 个条目和 512 KiB，单次结构结果最多 48 KiB；超限明确拒绝。长名称以 `nameTruncated` 标明展示截断，原完整名称仍影响视图指纹。

媒体仅从原 UI Image 句柄取像素，不打开任意文件或远程 URL，不捕获桌面或视频播放帧。源像素最多 4194304，PNG 最多 1 MiB；编码后重新核对原账户、许可、页面、目标与图像版本。MCP 将 PNG 放入 image 内容，结构结果移除 base64 并保留成功 envelope。`pngSha256` 对应实际返回的 PNG，`sourcePixelSha256` 对应原缓存 RGBA；二者均不证明素材文件版本或作品质量。模型不支持图片时移除 `page_preview`。

设置与已审核的只读媒体预览可以和助手共存。登录、支付和其余未接入的弹窗阻止页面读取；完整快照和图片编码后的最终核对采用同一模态状态。

## 发问和恢复

原生点击发送时，先按原 `inputId` 保存页面三字段，再异步处理附件。运行时补齐可信实例、客户端、账户和 AppSession，保存完整七字段。工具前后、上下文贡献及模型完成结果均核对原输入页面；过期结果不认领为新页面结果。迟到模型迭代器仍完整收尾并核对原费用，不重新提交付费请求。

恢复沿原 COTO `sessionId`、历史和 managed 账本。`recoverExisting` 仅检测已有记录，不为 App 隐式重新授权。新实例/AppSession 需要人类明确恢复、当前读取许可及 `rebind:true`；必须同账户和客户端，不建立累计授权 sidecar。恢复不自动执行旧输入；旧 inputId 不能跨范围重发，即使页面三字段相同。

宿主动作 `app.abandonReads({sessionId})` 对应结束旧问题，只适用于无活动轮次、原费用已核实关闭的 App 会话。它沿原 inputId 撤回待处理输入，沿原 invocationId 记录 `stale_read_abandoned / resultAvailable:false`；不表示读取成功或业务交付。当前轮次先使用原停止动作，未知费用先沿原操作核对。动作不发起模型或 MCP 调用，不自动续轮；ACK 丢失后查询原 status。

App 状态保留完整当前 pending/unresolved ID。已结束输入、弃读和已观察工具仅展示最近 64 条，并提供总数及截断标记；文档状态投影保持原合同。

## 验收

协议、逻辑测试、真实 COTO 与本地合成服务分别记录。工具注册或 PNG 摘要不能代替真实 App、模型观看、播放、操作或成品质量验收。当前证据与未确认项见[本轮验收记录](../testing/main-qa/2026-10-20-full-agent/README.md)。Android 暂缓。
