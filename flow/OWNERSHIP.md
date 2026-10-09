# Flow 独立候选：文件所有权与共享接线

- 公共基线：`1e8009ea9ea7e90c2ceb8720037711f02970b923`。
- 分支：`codex/seecut-flow-p2-isolated`。
- 工作树：`/Users/chenyunzhe/Documents/Codex/2026-10-09/task-2/Seecut-flow-p2`。
- 当前唯一产品写入范围：新增 `flow/**`。未修改主工作树、共享 App/路由、共享 Cargo、Theme、账户/积分、正式计划。未复制私有 Agent 或主线未提交修改。未推送、部署或提交 PR。
- 项目根及祖先检查未发现适用的 `AGENTS.md` 或项目 `.agents/skills`。已读 SeeCut 根四份指定计划/设计文档；已实际查看 `04-libtv-selected-node.png`、`05-libtv-media-node-graph.png` 像素。

## 最小宿主接口

`flow/core` 是独立 Rust crate，当前确定性模拟器无网络依赖。公开域入口：

- `graph::{Document, Graph, Node, Edge}`：项目/文档/图 ID、版本/单调 revision、有序类型端口、校验。
- `commands::{Request, Command, Receipt}`：包含文档 ID、图 ID、expected_revision、operation ID。同 ID 同请求重放只回旧回执；冲突拒绝。宿主先完成当前文档及权限核验，此 API 自身不是 MCP 鉴权。
- `store::Store`：单写者 OS 文件锁、独立 `document.json`/`runtime.json` 原子快照，文档级 CAS；保存失败后关闭重开，不继续写。现为本机小实验，不替代现有 WriterGuard 的产品接线。
- `runner::Runner`：单节点运行、仅祖先串行运行到目标、按批次显式采用、原作用域核实、取消与确定失败后的显式重试。输入快照和结果事实保留在 runtime。相同成功输入重复运行/重试复用，不新建付费意图。
- `protocol::Session`：仅 QA stdin/stdout 接口，不监听端口；撤销/重做历史只在本次会话保存，尚待宿主持久历史方案。
- `flow/ui/workspace.slint::FlowWorkspace`：输入节点/连线/候选/状态模型；输出选择、单次拖动提交、端口连接、运行/核实/停止、提示词提交、视口回调。使用基线 SeeCut `Theme`、`IconButton`、`AppButton`；`FlowComposer` 为单层输入面，使用原生 TextInput。`launcher-clearance` 默认 104px，宿主传实际 Agent 安全区；不自行创建第二个 Agent。

- `host::{AssetPickerRequest, LibraryAsset, GenerationDraft}`：冻结文档/图/节点/账户作用域、revision、operation；素材回填支持替换素材节点或原子新增有序参考节点+连线。保存版本化引用，无路径/token。重复回调只回历史 receipt；跨账户、过期、不可用或类型不符拒绝。
- `FlowComposer`：顶部编号缩略图和加号，正文，底部宿主参数与上传图标，右下主操作。遵循用户提供的格式，保留 SeeCut 双主题。图标有可访问名称和 tooltip；候选/参数值保留必要文字。生成提示词显示已连接输入，不另造独立 AI 服务。

## 请主线串行接入的文件与职责

1. 在主线 Cargo/App/Rust controller 中添加此 crate 和 `FlowWorkspace` 的引用；Flow 导航紧随生成。独立分支不改这些文件。
2. 宿主添加 Flow ResourceIdentity / SaveLane 适配及关闭/切换/退出未保存保护；将 `save-label`/草稿状态接入宿主。不要把 Flow 塞进 `concat.json`、剪辑 Timeline 或像素 ImageDocument。
3. `GenerationPage` 当前依赖全局 SeeCut 状态，内部 `GenerationForm` 不是独立公开组件。主线负责把 `shared-generation-requested(node)` 映射为既有生成页/会话，填入 `GenerationDraft` 并保留已有草稿；不得复制它的账户/模型/报价/提交逻辑。提供现有腾讯云账户作用域、模型能力/报价、task client、按原幂等键只读核实、取消/结算状态。当前 simulator 的虚拟点数只作 fixture，不是新增钱包/队列/账户服务。前台不放密钥/提供商/服务地址。
4. 将 `assets-requested(node, local)` 接入现有资产库选择或本地导入；导入完成且托管可用后才调用 `accept_asset`，不把本地路径当成已上传资产。提供 `AssetRef { id, content_version, media_kind }` 的真实版本/hash 与托管确认；现 Asset 只有 ID，不假设已有 content_version。真实任务、下载、登记和交付回执须分别实现并验证后再开放真实运行。
5. Flow MCP 权限、当前上下文、有界读取和工具注册由主线衔接。候选命令层只是基础，未实现完整 D08–D11；3D/音频/插件等仍按 76 项矩阵进入 P3/P4。

## 不可据此关闭的验收

这一候选仅为 P2 核心及独立原生合成宿主，另推进 P3 图编辑/复制/排列/撤销与 P4 幂等命令回执接口基础。没有真实图像/视频生成文件；输出是明确标记 simulated 的版本化结果清单，图像缩略图是公开合成 SVG fixture。没有真实积分/账户/资产库/剪辑或画布交付。没有完整 App 集成，也没有完整 P2/P3/P4 或 76 项迁移验收。正式矩阵仍 0/76。
