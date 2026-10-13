# MCP 画布图片任务

本扩展复用既有 Rust MCP stdio → 本地 IPC → 画布提交路径。旧 `move_selected_image` 契约见 [M2 协议](SEECUT-MCP-M2-WRITE-PROTOCOL.md)。Flow 仍不作为 Agent 活跃工作区。

| 工具 | 参数与结果 |
| --- | --- |
| `library_images` | 原工程/文档身份，可选 query、assetId、offset、limit；最多返回 50 个图片身份、不可变 version 及可用状态，无文件路径 |
| `insert_library_image` | 原 scope、revision、selectionRevision、assetId、version、operationId、clientSequence；回执含实际新层及库版本 |
| `transform_canvas_layer` | 同一写身份及当前 objectId、x、y、scale；x/y 沿原画布变换字段，以画布中心为原点的像素偏移，scale 是两轴缩放绝对值；保留镜像、旋转及蒙版关系 |
| `save_canvas_project` | 同一写身份；只保存已有可信保存位置，结果含 persistedRevision 和实际 manifestSha256 |
| `export_canvas_png` | 同一写身份；冻结正式导出快照，后台合成/编码/默认登记全局库，结果含 PNG 摘要、尺寸、sourceRevision、实际 asset/version |
| `canvas_operation` | 原 scope、operationId、clientSequence；查询原回执，不重复业务写入 |
| `retry_canvas_registration` | 原查询四字段；只恢复该原导出的 PNG 登记，沿原 SHA、provenance、操作及序号，不合成或改写 PNG |

version 为 `{sha256, registration_id, previous_id?}`；未知字段拒绝。新写工具与旧 Move 共用原 operation registry 和文档最高 sequence。模型侧隐藏新写工具的 operationId/clientSequence，由调用者的持久 journal 管理。

新工具必须由可信 App 审批明确的工具名及冻结语义参数。旧 R/M/E 不能自动扩展；写入和登记恢复还需当前 E 租约与 writer 所有权，最终提交再次检查文档、版本、选择和权限。新增权限仅覆盖批准的那次调用，五分钟失效，撤销和上下文切换使其失效。

插入与调整走原画布 history，每次实际修改可撤销一次。新增层提交后同步实际选择版本；只有匹配该新层的原生回执与精确读回可接续，用户手动切换选择会停止本轮修改。

保存与导出完成回执属于原 scope/client/operation/sequence；文档切换不会把迟到结果写入新画布。pending/partial/UNKNOWN 禁止开启第二次业务写入，只能查询原操作；partial 可经可信“恢复入库”操作重试原登记。查询只检查既有登记，不在读权限下产生登记副作用。未命名工程返回 awaitingSelection，路径由可信保存流程决定。

成功由各工具实际证据确定：新增层和 transform 精确读回、保存落盘版本与摘要、PNG 与入库版本同一摘要。恢复可确认原提交事实，但未读取当前新文档时标记 currentReadback 待核对，不能宣称整个任务已完成。

原请求待核对期间租约到期导致运行时退出时，可显式恢复原会话连接。仅接受同一 App/工程/文档、session、client 的 restore；原写身份与 UNKNOWN 保留，恢复响应必须保全每项原操作。恢复连接不发送旧 input，也不重新执行导出，只有原回执核实成功才能解锁。

首次写入退出前没有最终结果、宿主尚无恢复列表时，仍可按上述原身份连接并恢复持久日志。若原核实已落盘而回应丢失，restore 仅请求宿主仍保留的完整原查询身份；随后只读重查同一 operation/sequence。已确认回执必须与原回执一致，不能再次导出或登记；空恢复列表不能解除 UNKNOWN。
