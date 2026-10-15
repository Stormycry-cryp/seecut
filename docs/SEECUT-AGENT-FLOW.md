# Agent 与本地组件

Flow 使用 Toonflow 工作区。旧原生 Flow 界面、控制器、Agent 节点引用入口和 flow_read/flow_write 工具已移除；原生桥接收到旧工具请求会返回 unsupportedWorkspace。新 Flow 的 Agent/MCP 尚未接通，接入安排见 [Flow Agent/MCP 方案](plans/2026-10-15-toonflow-agent-mcp.md)。

原生画布和剪辑继续通过现有 context、project、preview 等接口提供上下文。画布写操作仍要求当前编辑权限及对冻结参数的批准，运行时分配操作标识。IPC 响应丢失时查询原操作回执，不能分配新标识重做。

服务端只允许当前编辑器工具作为新调用。历史会话中的旧 Flow 工具调用与结果仅作为上下文保留，不重新赋予执行能力。素材附件保留资产库的固定版本，发送时重新校验；历史 Flow 节点附件提示重新选择资产库素材。编辑历史消息产生新输入，原请求和计费标识保持不变。

## Local macOS bundle

The normal desktop entry locates `Contents/Resources/agent/components.json`. The complete local component input contains fixed Node and MCP executables, the matching private runtime, its production dependencies, build identity and third-party notices. No credentials belong in this directory.

`SEECUT_AGENT_BUNDLE=/absolute/sealed/bundle scripts/make-app.sh` is an optional local packaging input. `scripts/agent-bundle-manifest.py seal` creates its bounded file manifest; `verify` rejects missing, changed, extra or linked files. Packaging copies the verified input, rewrites native dependencies, signs inner executables, regenerates the manifest from their final bytes, then signs and verifies the outer app.

The private runtime and assembled local application are not public repository or CI artifacts. Omitting the optional input retains public-only packaging. This local prerequisite does not claim cross-platform release signing, notarization, update delivery, or verification of a live paid provider.
