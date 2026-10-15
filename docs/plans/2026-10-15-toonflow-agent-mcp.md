# Flow 的 Agent tools 与 MCP 接入安排

分支 `codex/seecut-toonflow-embedded`。Flow 采用 Toonflow 的真实节点/文档工作区；原生剪辑与图片画布保留。项目、自动保存、云快照和生成业务已通过本地受控验收；以下 Agent/MCP 工具是明确的后续接口方案，不表示已经可由 Agent 操作新 Flow。

## 当前实际边界

| 部分 | 已有接口与状态 |
| --- | --- |
| Flow 项目 | 独立持久 UUID → 工作目录，项目可有多个画布和文档。剪辑、图片画布、Flow 没有统一父项目。 |
| 私有宿主桥 | Bun → Rust loopback JSON-lines 白名单 RPC；原生持有账户凭据，业务调用携带冻结 expectedScopeKey，任何读写前检查。该桥不是公共 MCP。 |
| 生成 | catalog/prepare/find/submit/lookup/task/register；先保存 requestId、原请求与报价，再唯一提交；未知结果查原任务。GET 恢复不收费，显式继续提交仍复用原 attempt。 |
| 云端 | 独立账户的项目快照、固定版本下载、SHA 和 CAS；生成记录随快照保存，导入仅恢复查询资格，不赋予重新提交资格。正式服务端部署尚未执行。 |
| SeeCut Agent | 原生 JSONL 子进程 initialize/session.create/restore/send/approve/reconcile；目前新 Flow 的专用工具尚未注册。 |
| Toonflow Agent | 源码能力保留，嵌入模式不挂载第二套 Agent 界面和独立供应商设置。 |
| 既有编辑器 MCP | 原生图片画布的读取、变换、素材插入、保存与导出仍沿原协议。旧原生 Flow 界面、控制器和 Agent 节点引用入口已删除；flow_read/flow_write 已从工具注册中移除，旧协议请求返回 unsupportedWorkspace。新 Toonflow 工具尚未接通。 |

现有 editor_mcp 的 projectId 是文档代际/会话标识，不可当永久项目主键。Flow 工具必须使用新的持久项目 UUID、实际 canvasId、文档版本及会话身份。

## 工具从同一套业务接口注册

建议按三层接入，Agent tools 与 MCP 只做协议包装，不复制生成、保存或资产业务：

1. 只读：flow_list_projects、flow_get_workspace、flow_read_canvas、flow_list_node_types、flow_get_task。返回实际项目、节点、连线、版本、忙碌/保存状态和原任务回执。
2. 编辑：flow_add_node、flow_update_node、flow_connect_nodes、flow_delete_nodes、flow_switch_canvas。复用 Toonflow 已有 canvas tools 和 flushSave，不直接改 JSON 冒充 UI 操作；成功须收到实际保存回执。
3. 生成与交付：flow_prepare_generation、flow_submit_generation、flow_get_generation、flow_import_asset、flow_export_asset。生成沿当前受管链路与报价，执行授权绑定原 requestId/quote/scope；查询或网络重试不能自动产生新付费请求。素材继续使用 SeeCut assetId、不可变 version/SHA，工作区只接收校验后相对文件。

每个写工具至少接收 projectId、canvasId、expectedRevision、operationId。服务端固定账户与权限，参数不能自选 owner 或绕过账户检查。操作回执记录真实 nodeId、revision、保存结果和 task/asset 身份。超时与 UNKNOWN 查询原 operationId，禁止换新身份自动重发。

MCP 对外暴露同名受控工具；通用 appOperation、任意供应商源码、插件安装、shell、支付等能力不随 Flow 默认开放。独立 Toonflow 的 MCP runtime 初始化和源码存在不代表新 Flow 的外部控制已经验收。

## UI 与协议接线顺序

先让原生 Agent 对当前 Flow 工作区获得只读 context，再开放节点编辑，最后接生成和素材交付。工作区显隐、切项目、返回列表和关闭窗口必须更新实际绑定；异步结果只写回发起任务的原项目。原生子 WebView 与 Slint 的层叠、焦点和快捷键仍由宿主管理。

验证应覆盖原生 Agent 中看见真实节点 → 执行受控编辑 → UI 变化 → 保存重开，以及同一工具经 MCP 获得一致结果。模拟调用、工具 schema、构建或 MCP 服务启动均不能替代这些实际证据。

## 代码位置

- 上游版本与差异：integrations/toonflow/upstream.json、integration.patch、SOURCE.md；生成源码在忽略目录 external/toonflow。
- 本地服务：apps/server/src/seecutHost.ts、seecutProjects.ts、seecutGeneration.ts、seecutNative.ts（位于生成的上游依赖中）。
- Rust 桥：src/crates/concat/src/toonflow_bridge.rs、toonflow_service.rs、toonflow_webview.rs。
- 原生复用：cloud/flow_generation.rs、personal_library.rs、editor_mcp.rs；公共 MCP 包 src/crates/concat-editor-mcp。
