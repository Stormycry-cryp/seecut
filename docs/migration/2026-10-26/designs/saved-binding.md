# Photo 已保存文档与原 Agent Scope 的最小接入方案

推荐按 Root 已确认的边界实现：应用通过自己的唯一 Photo worker 做一次闭合 owned-metadata 准备，只为建立受管工作区 Doc 身份及原 Store 历史关联。它不取得 Agent 读取/编辑授权、不采 PNG、不调用模型。UI 安装的缓存只能说明一次成功观察到的关联；每次真正 Agent 读取/执行仍须原授权检查和新的后台 preflight。

当前代码不能只给 active_ids 加一个 photo 分支：还有 App 优先选择、权限按钮二次分流、原 MCP 全局拒绝三处真实门槛。下面列出最小边界；这些接口均为设计，尚未实现或运行。

## 1. 当前证据与真实冲突

| 真实位置 | 已有行为 | 最小处理 |
| --- | --- | --- |
| editor_mcp.rs:81 DocumentIds；:1150 ids | Canvas/Clip 按内存 generation 分配独立随机 project/session；generation 变化清旧 grant | Photo 增单一私有 binding 槽，保留同一随机身份规则，native u64 不投影 JS |
| editor_mcp.rs:1172 active_ids；:1252 sync_workspace；:1290 current_scope | has_instance 即 None，清写作用域；只认 canvas/clip/flow | 仅在显式 Photo target 下查私有已安装关联；没有则 None，阻止退回旧 Canvas；Parent target 仍按真实父页面解析；增加 active_photo 和原撤权路径 |
| editor_mcp.rs:736 assistant_snapshot | supported-kind 仅三种；project_key 从旧 Studio Canvas/Clip path 产生 | Photo key 只能来自 worker 的真实 backing.history_key，不能落入旧 Studio path 分支 |
| agent_ui.rs:135 ScopeSnapshot::choose；editor_mcp_app_read.rs:146 current_page | choose 无条件 App 优先；page6/gallery 被视为 Projects | Photo 入口就在 project-gallery.slint:47，父页通常不离开 gallery。只加 Photo document 仍会选 App。增加显式 target marker：Photo target 选 Photo 或 None；Parent target 维持原 App/Doc 选择，managed 不决定目标 |
| agent_ui.rs:2640 permission | 按钮回调再次读取 assistant_app_snapshot，只要 AppIdentity 存在就调用 App grant | 必须按同次重新取得的实际 ScopeSnapshot/Photo 选择分流；不能只改 choose 后仍签发 App token |
| agent_ui.rs:618 sync_scope；:1022 snapshot；:1482 sync_lease | account+HistoryScope 驱动 Store；Doc lease 变化撤 Task/approval 并断开旧连接 | 复用，安装前后刷新，不创建第二历史 Store、账户身份或会话账本 |
| editor_mcp.rs:1621 handle、:2439 dispatch_write、:2480 dispatch | managed child 被全局 unsupportedWorkspace 拒绝 | 将生命周期围栏与工作目标分开：Parent target 仍可用本页工具；Photo target 只允许闭合 Photo adapter，不能落入旧 Canvas 分支；metadata 不接 MCP |

现有 Canvas `mcp_state`/`mcp_binding_epoch`/`mcp_project_path` 只是内存事实（panes/canvas.rs:2795/2812/2816），assistant_snapshot:747 已明确渲染 tick 不核验文件所有权。Photo 缓存可具有相同用途，但不能因此声称是当前 child 的实时状态。

## 2. 应用内部准备与 Agent 授权必须分开

真实启动 `NativeHost::start` 在 host/lib.rs:306 无需 Agent lease 就已 authenticate + ui.inspect 自己刚启动的 child。这证明应用管理自有编辑器已有独立生命周期入口。Root 本次授权允许沿此性质读取 state/backing，仅为应用创建文档身份。

Astra read-worker 候选 REPORT 和 read_worker.rs:30/40 明确要求先过原 read gate，BackingToken::observation_after_gate:162 也要求同一 ReadContext；host/photo.rs:191/207 的两个 *_after_gate 文档明确要求 read lease/account/Task。因此不能给 bootstrap 构造假 ReadContext，不能说用户尚未授予的 Agent read gate 已通过。

最小新增是同一 `photo_workspace::Command` 的独立 `PrepareOwnedMetadata` 目的。它不使用 ReadContext、不返回 BackingToken/CaptureToken、不接受任意 method/path、不允许 capture/validate/execute。原 Agent read_worker 所有 after_gate 语义保持。host 可将真实 state typed 解析和 verify_backing 抽成私有共用函数，新增仅返回 typed backing 的 `photo_owned_workspace_binding(id)` 应用管理入口；两个 Agent *_after_gate 继续各自保留原契约，不把它们重新解释成无需授权的入口。

建议内部接口如下，仅描述签名：

```rust
// 私有 Rust 值，不 Debug/Serialize；没有 token、模型参数或路径输入。
struct OwnedMetadataOrigin {
    request_id: Uuid,
    owned_instance: Uuid,
    managed_epoch: u64,
    ui_epoch: u64,
    target_epoch: u64,
    account_epoch: Option<u64>,
    trigger: MetadataTrigger, // ManagedReady 或 ExplicitAgentEntry
}
fn PhotoWorkerClient::prepare_owned_metadata(
    origin: OwnedMetadataOrigin, deadline: Instant,
) -> Result<PendingOwnedMetadata, MetadataError>;
fn PendingOwnedMetadata::try_result(&mut self)
    -> Option<Result<OwnedMetadata, MetadataError>>;
// OwnedMetadata 只持 origin + native PhotoIdentity + 私有 history_key，
// 不持可转换为 Agent 授权的能力，不含 PNG/raw JSON/path getter 给模型。
fn BridgeUi::install_photo_metadata(
    origin: &OwnedMetadataOrigin, metadata: OwnedMetadata,
    current_ui_source: &CurrentOwnedPhotoSource,
) -> Result<(), MetadataError>;
```

方法名可由 Root/Astra 微调，性质不能改变。origin 是本次待安装结果的来源关联，不是新权限、持久账本或执行会话。最多一个 pending metadata job，共用原容量 8 的串行队列及唯一 NativeHost；普通 UI tick 仅消费结果，不自动反复发请求。

worker 先核对当前 owned child Ready 且 UUID/managed epoch 相同，再读真实 state；无文档直接返回 Unavailable。已有文档时做真实 backing 双读（下节），要求最初 state 的完整 identity 与成功 binding identity 相等；若变化则整次准备失败，不拼接不同文档/版本、不自动重试。UI 名称首版可固定为 Photo，无需多读一次标题或根据路径猜标题。保留 File 的验证与释放都在 worker；UI 只取受信 history_key 和 typed identity。准备 deadline 建议原事件起至多 5 秒，排队/返回/安装均检查。文件系统可能超过 deadline，超时仅丢弃结果，不承诺底层 I/O 取消。

合法触发及复核位置：

1. **受管 Ready 事件**：现 photo_workspace.rs:215 worker 的 host.start 成功形成 RUNNING，:178 UI timer 顺序发布。UI 收到新 managed epoch 的 Ready 后，只有用户原先选定的 Photo target 仍匹配该实例才向 `Binding` 发内部事件，由 Binding 捕获真实 AccountAccess::epoch()、target epoch 和自身 UI/绑定 epoch，仅排队一次。Ready 本身不选择目标，不得覆盖用户已导航到的父页面。这不是将 Slint 可显示字符串当所有权凭据，必须核对 worker 当前 bind/client 与 typed UUID。
2. **Agent 面板显式入口**：app.slint:193 toggle-assistant 打开分支目前只改 Assistant.open；可追加固定 native metadata-requested callback，在 agent_ui.rs:3523 附近绑定到 `Binding::prepare_owned_photo_metadata`。只在真正打开且当前 target 为 Photo、受管 child Ready 时发起；打开输入框保留既有工作目标，不由父窗口页面或焦点另猜目标。未保存后用户保存，再次显式进入/请求可重新准备，禁止刷新 tick 无限重试。
3. **晚结果 UI 安装**：`Binding::refresh` 在 :1022 snapshot 前先 drain 一个 pending；先比较原 account epoch、UI epoch、target epoch、request_id、managed child UUID/epoch、当前 managed phase 和客户端 bind 连接，再在同次短 Studio/Bridge borrow 调 install。来源变化、退出/Closing/NeedUser/失联、deadline 到期或请求被替换，一律丢弃，不建 Store、不签 token、不覆盖继任结果。metadata 可在未登录时准备，但 account epoch 必须仍匹配；原登录约束仍决定 Store/授权是否可用。
4. lifecycle 进入 Open/Close/Exited/ManualRecovery 时先撤销原 pending 和安装槽；账户更换时同样撤来源并清 pending，不能把旧账户排队结果安装到新账户。

## 3. 三种身份各自沿用原机制

- **持久历史键**：只有 host/photo.rs:496 verify_backing 成功所得 `history_key()`。它严格要求非只读、saved_revision > 0、真实 recorded native path、absolute .pcraft regular file、严格 UTF-8 canonical path；两次 state 的完整 identity/backing 相等，path/descriptor/file stamp 在返回前重复核对。:579 产生 `photo:<canonical path>`，不会依赖标题、inode 或 native doc ID 猜工程。
- **父权限身份**：沿用 `DocumentIdentity {instance_id: BridgeUi.instance_id, project_id: random UUID, document_session_id: random UUID}`。instance 是原 SeeCut bridge 身份，不拿 child UUID 冒充。Photo 私有绑定键保留 owned child UUID/native doc_id/document_generation（均原生 u64）及受信 history_key。首次成功与绑定键变化才分配全新 DocumentIds。
- **native expected**：完整 PhotoIdentity 保留在私有槽/原附件，revision 可刷新但不生成父新身份。真正请求使用新 preflight，不能把安装时旧 revision 当点击瞬间事实。

正常 native revision 改变、same-path 原子保存造成 inode/file stamp 改变，都保留同一 history_key 和父 DocumentIds。Save As、切 tab/new native doc、document_generation 变化、child 换代、路径丢失/只读/未保存/recovery saved_revision==0：撤旧 scope，清 token/permit/pending，只在新成功准备后创建新父 Doc session。若 canonical history_key 变化而 native triple 没变，也不能继续旧 scope，先撤旧关联并以新明确准备建立新身份；它可能是外部别名解析变化，不应猜为同一工程。

`ScopeSnapshot::history_scope` 在 agent_ui.rs:178 已可直接返回 HistoryScope::Document(project_key)，`sync_scope`:687 调现有 Store::open_scope(account, key)。Store 的 valid_key 接受非空无 NUL <=4096 bytes（agent_sessions.rs:543），与 host 返回键边界一致。Store 用账户+历史键的长度分帧摘要确定原历史文件（:255），不需要临时 project_key 或新账本。同一路径重新打开可恢复同一历史文件，但新 Doc session 仍不能恢复旧执行授权，符合 agent_identity.rs:55 的 Doc restore 精确身份要求。

history_key 只在可信 Rust Store/历史引用内部使用，不投影 Slint、日志、模型参数或公开 report；项目显示名首版固定 Photo 即可。当前报告没有检查私有 runtime，不能据此宣称未来模型适配已保证无路径泄漏，适配验收必须检查真实输出。

## 4. UI 安装与原授权的最小改动

`BridgeUi` 增一个 private PhotoBinding 槽和 active_photo；可比较的 binding key 与父 DocumentIds 独立保存。`install_photo_metadata` 仅安装来源核对后的关联，绝不修改 grants 或 trusted_grant。`active_ids` 同步读取它，绝不 TCP/canonicalize/metadata/open 文件，也不从 `Studio.session` 取 Photo path。

`sync_workspace/current_scope/sync_document` 补 Photo 分支。离开/失效先走原 revoke_move 和原 grant 移除，通知未来 Photo pending CAS stop(ScopeChanged)，撤销原 exact-call permits、Task/approval/准备中的输入；再绑定新 scope。writes.bind_scope:130 会清旧 lease 和 histories，不能借再次打开旧 native doc 复用旧 sequence。因此 receipt miss/child 重启永远不得 execute 重发，旧结果查询仍需使用原附件/original ID 的保守 Unknown 规则。

`assistant_snapshot` 把 Photo 列为 document family，history_key 只取私有已安装槽。`ScopeSnapshot::choose` 和 `Binding::permission` 同时按下节显式 marker 选择：Photo target 有有效关联走原 Doc snapshot，没有则无 Doc/App fallback；Parent target 继续按本页原规则选 App/Doc。managed 只负责子进程生命周期和关闭保护，不能作为工作目标。

原 `assistant_registry_snapshot/assistant_registry_permission` 不改成新的授权器：它们仍从原 scope、R/M grant、write registry、受信点击产生 token。Binding::publish:1962 仍要求登录或原 local-dev 条件；connect_managed:3032 仍要求真实 read_token/account/components；grant_scope:1318 仍要求账户 epoch、同一原 lease 和 Task。metadata 成功不连接 Agent、不设置 read/write token，不自动发送内容。

`assistant_permission` 的 supported-kind、`grant_ready` 的 Photo readiness 需新增分支，但不能将 metadata 存在等同于可执行写。首片仅接 Doc/history 可保持 write_ready=false；闭合 Photo 写 adapter 真正接入后才启用原 GrantWrite 流程。真正 Agent R/M/E 路由应复用现有 request instance/project/client token 校验及原 media gate；每次调用前重新核对原账户/lease/Task/来源，后台新读 backing/identity，回 UI/模型前再核对。已有 after_gate read worker 正是后续执行这些已获授权请求的入口，bootstrap summary 不可替代其 context。

## 5. 显式工作目标与导航：最小 marker

Root 已明确 managed child 只证明应用仍管理该子进程，不决定用户的 Agent 工作目标。建议在 photo_workspace 的 UI 私有 Rust 状态中加一个 marker，与生命周期 state 分开：

```rust
enum AgentTarget {
    Parent,
    Photo { owned_instance: Option<Uuid>, managed_epoch: u64 },
}
struct TargetState { target: AgentTarget, target_epoch: u64 }
```

`target_epoch` checked 增长，耗尽拒绝继续选择/安装；不是新权限或文档 session。Photo 在启动前可为 None，由同一次 managed epoch 的成功 Ready 补齐 UUID；补齐不能跨 target epoch。binding 槽还记录其安装 target epoch，工作目标变化后旧 pending/缓存不能作为当前 scope 使用。可保留旧历史关联供内部比较，但返回 Photo 必须显式准备并重新核验，不恢复旧写许可。

**选择 Photo 的唯一用户事件**：PhotoWorkspaceEntry 的 `open-editor` 受信点击（photo-workspace.slint:42，Rust photo_workspace.rs:123）先选 Photo 并撤前一目标写许可，再进入原 Open 流程。已存活时当前按钮 disabled（:41），需要最小追加 `return-to-photo` native callback/按钮，选择同一受管实例的 Photo target，不重新 start，也不调用 focus/raise。文案显示来源为图像编辑器；Agent 当前来源可直接复用 `Binding::publish` 的 `Assistant.current_project`（agent_ui.rs:1633）显示 Photo，准备中/不可用显式显示状态。只展示静态来源/状态，不展示 native identity/path。

**进入输入框保留目标**：app.slint:193 的 toggle-assistant 只负责展开，不能把 Parent page6/gallery 自动当成 App，也不能把任何 managed child 自动当成 Photo。按 marker 选择 ScopeSnapshot；Photo target 元数据尚未 ready 时 scope=None、token=None、不能回退旧 Canvas；Parent target 则沿原 `choose(document, app)` 优先级。显式点击进入 Photo 后再打开 Agent，仍是同一 Photo 来源。

**真实导航立即撤 Photo 写许可**：已有 `SeeCut.change-page/change-canvas-gallery-open` 在 state.slint:70/74 提供同步 pre-change hook，现只通知 Clip；changed page/gallery 在 :158/163 调 app-page-navigation-changed。导航实际入口为 cloud.rs:3396 navigate → :3404 navigate_unchecked → :2671 load_page → change-page，包含 navigate-resume（:8917）。Settings 在 panes/settings.rs:173/181 Open/Close 先调用 cloud::app_page_navigation_changed，Rust入口 lib.rs:1906。最小接线是在这些真实父导航的 pre-change 位置调用 `photo_workspace::select_parent_target()` 并先 stop Photo 未派发 CAS/撤 Photo 写 permit；随后按新页面解析 Scope。不得只轮询最终页码，否则 A→B→A 会恢复旧许可。

现 `cloud::app_page_navigation_changed` (:35) 只推进 App `Navigation` 的 checked page_epoch（editor_mcp_app_page.rs:289），是现成的兜底失效事件，但它目前为 changed 通知且不带来源，不能单独充当原子 pre-change 撤权。保持原 App epoch 不变；给 Photo 新增的同步前置通知只做 marker epoch/原撤权句柄更新，不 TCP/fs、不借用繁重 Studio。Settings pre-change 也调用同一通知。已在 Parent 时导航仍推进来源 epoch以废弃旧 pending；仅标题/列表过滤变化不改变 target。

导航到素材库/设置/剪辑/旧 Canvas 后 marker=Parent，现 `active_ids/handle/dispatch/dispatch_write` 的全局 has_instance 拒绝应改为当前 Photo target 的定点路由边界；实际父页面继续使用其既有 App/Doc 上下文、原 account/read/write gate。child 的 has_instance/defer_close/保存保护完全保留，不因此关闭。关闭 Settings 后仍 Parent，返回 Photo 要显式 return-to-photo，不能因页面值碰巧回到原 gallery 复活旧 Photo 权限。

若 navigation 因原 Canvas dirty prompt 尚未提交，应在实际导航获准后的同步 pre-change 才改变 target；不能在 navigate 最初尝试时先切目标后又被用户 Cancel。相关导航事件之后，晚 Ready/metadata 必须匹配 target epoch 才能安装为当前目标；不能把用户从素材库抢回 Photo。metadata 可成功时不采 PNG，切回 Parent 后禁止继续原 Photo send/write；已 DispatchStarted 的结果仍留原附件且只能 query 原 child ID，不能记成未执行。

## 6. 阻止错误绑定的验收

建议只在实际消费此设计的 UI/worker 改动中补这些测试，本报告未产测试代码：

1. Ready 或显式面板事件无 Agent token 也可排队一次 metadata；无 state/backing 则无 Doc/history，且 capture/validate/execute 调用计数为 0。UI tick 只 drain，不追加后台请求。
2. 独立 child、退出/重启、旧 managed epoch、旧 request、target epoch 改变、account epoch 改变、UI binding 被替换、超时/取消/晚结果，均不能 install/新建 Store/签 grant；继任槽不被旧失败清除。
3. 真实保存 .pcraft 的双读成功，ScopeSnapshot 为 Photo Doc；显式选 Photo 后父停留 Projects/gallery 时也不能错误选择 App；明确导航其他父页后则必须选择其真实 App/Doc，child 仍 managed。permission 回调必须按目标走对应原 GrantRead。尚未用户授权时 read/write token 均 None。
4. 同路径保存、更换物理 inode、正常 revision 改变保留父 DocumentIds/history key；SaveAs/切 tab/generation/child 变化撤旧 token/permit/Task/pending，分配新随机 session，旧 expected/旧批准拒绝。路径 missing、nonUTF8、只读、recovery、非 regular file 均不产生历史关联。
5. 安装缓存后 child 自主 SaveAs/编辑且尚未再观察：缓存不声称 current；下一真实 Agent gated preflight 必须发现差异并拒绝旧 scope，不能凭缓存交模型或执行。仅终值换回也不能恢复已撤销的旧 scope/permit。
6. Photo→素材库/Settings→返回父页的真实导航即时撤写许可；打开/关闭 Agent 不改目标；用户取消父导航不误切目标；Photo pending 不回退旧 Canvas；Parent 页面不被 managed child 阻断；A→B→A 不复活旧批准。
7. 静态/调用线程断言 active_ids/assistant_snapshot 无 TCP/fs；Host 双读只在唯一 serial worker；无第二 NativeHost；所有日志/Slint/模型协议不含 canonical path/history_key/file IDs 原值。

已完成只读源码核对和 SHA 固定，未修改源码、运行 Cargo/Git/GUI 或读取私有 runtime。此为可实施设计，尚无 adapter、授权或端到端完成证明。
