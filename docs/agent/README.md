# SeeCut 全功能 Agent 覆盖记录

目标：用户交代任务后，Agent 能理解当前内容，操作各模块并交付真实作品。总体范围、顺序和验收见[实施计划](../plans/2026-10-20-seecut-full-agent-plan.md)。

[feature-coverage.json](feature-coverage.json) 逐项记录原生剪辑命令、画布辅助接口、MCP 注册以及 UI 回调声明。`businessRoutes` 单独展开通用 action/changed 路由中的业务功能。源码清单仍在补充业务归组，不能用清单条数作为全应用功能分母或完成率。鼠标按下、拖动和组件重复回调不各算一个用户功能。

每项分开记录：

- `mcpTools`：实际注册的工具对应。空列表表示尚未建立对应关系；内部 helper 不自动算 Agent 可调用。
- `agentStatus`：目前是否仅发现源码、注册工具，或已建立操作链。
- `acceptance`：`pending` 待验、`source` 源码证据、`logic` 逻辑验证、`app` 实际应用验证、`task` 完整任务验收。
- `evidence`：与对应验收级别相符的仓库内证据。工具注册、合成供应商和测试回执不能代替真实模型、原生交互或成品质量。

执行 `python3 scripts/check-agent-coverage.py` 检查新命令、MCP 工具、UI 声明、过期条目及能力公告是否遗漏。CI 执行同一检查。源码扩展后使用 `--update-inventory` 刷新声明，再补业务归组、工具对应和证据；刷新不会提升任何验收状态。

新增工具必须接到活跃 App 的业务状态、版本与权限检查，适用时共用手动操作的撤销历史和原操作回执。UI 的通用 `changed` 通知可能依赖 UI 已写入的属性，直接转发回调不能代替业务适配。
