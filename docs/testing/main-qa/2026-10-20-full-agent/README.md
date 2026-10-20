# 全功能 Agent 实施与验证记录

当前范围：macOS 桌面端；Android 按用户最新要求暂缓，下面移动端信息仅记录此前已经完成的步骤，不作为本轮验收门槛。

## 第一批：真实视图与覆盖清单

公共视图源码提交 `9a5fc86`，覆盖清单及实施计划提交 `d0f2026`，私有运行时提交 `a3536f0e0b57242f7fc0513bcb204d920929edd5`。在原功能分支推进，没有合并或发布。

- 原生测试二进制后台编译成功。`native_view_context` 新增 5 项通过；同一二进制 `editor_mcp::` 全部 62 项与 `agent_controller::` 全部 63 项通过，共 125 项（5 项已经包含在 62 项中）。前者覆盖坐标映射、未测量/移出视口、选择遮罩及指纹，后者复核原会话、权限、旧操作与恢复行为。
- 原生 context 提供真实画布视图和有限剪辑视图；预览仍为最大边长 256 的整文档缩略图。未得到的时间线终点、预览缩放及渲染帧时间明确不可用。
- 私有运行时本地定向 159 项通过，公共合成 COTO 确认链 2 项通过。随后[私有完整 CI](https://github.com/Stormycry-cryp/seecut-agent/actions/runs/39408032719)的 Node 22.19.0 与 Node 24.x 均成功；Node 22 日志为 189 项、0 失败。该 CI 使用合成供应商及锁定依赖，不能作为实际模型作品验收。
- `python3 scripts/check-agent-coverage.py` 校验源码声明、MCP 对应和 UI 入口快照，实施起点另盘点 161 条业务路由。分母尚未全部确认，不报告全应用完成率。
- Android 临时 swap 与诊断脚本的 10 项命令替身测试通过，本机没有创建 swap。[公共 CI](https://github.com/Stormycry-cryp/seecut/actions/runs/39408659394)与[移动端构建](https://github.com/Stormycry-cryp/seecut/actions/runs/39408659377)由该批推送触发；初始检查中清单任务与 wasm 已通过，主引擎及手机编译仍在运行。结果以确切提交的最终检查为准。

复现原生测试需在 `src/` 使用仓库固定工具链与缓存：

```sh
CARGO_TARGET_DIR="$PWD/../.toonflow/native-target" cargo test -p concat --lib --offline native_view_context
CARGO_TARGET_DIR="$PWD/../.toonflow/native-target" cargo test -p concat --lib --offline editor_mcp::
CARGO_TARGET_DIR="$PWD/../.toonflow/native-target" cargo test -p concat --lib --offline agent_controller::
```

本批未启动前台 App，没有系统鼠标、键盘或焦点操作。原生实际交互、完整 Agent 生成入库、图片/视频/音频质量、全功能逐项覆盖、长任务恢复和正式分发仍分别待验。已有逻辑和合成证据不替代这些验收。

## 第二批：显式画布编辑

新增 `canvas_edit`，采用稳定图层ID，覆盖27类显式图层、选区、栅格蒙版、笔刷/擦除、撤销重做、调整和RGB曲线操作。复用现有App编辑路径和历史、原操作ID、精确许可、写权限与版本检查；扩展 `project.canvasState` 独立回读像素内容摘要及实际工具状态。

本批修复了变换后多点笔触映射、笔刷实际计算量、完整回执大小、Undo/Redo恢复边界和运行时日志容量。编辑提交前再次检查实际 writer 所有权和原授权。笔触、目标像素、参数、子树、完整回执均有提交前预算，超限不先修改文档再返回错误。

`editStateId` 覆盖真实文档像素、完整像素选区、笔刷、绘制模式、图层展开和历史状态。运行时在审批前读取并冻结 `expectedEditStateId`，通过可信审批投影交给原生卡片，原模型参数保持不变。状态变化时拒绝执行，不刷新印记重做。Undo/Redo按真实历史恢复文档版本，不能假设版本号总是增加；原来源版本、内容摘要、操作身份和独立回读继续严格匹配。

当前候选的实际验证：

- 原生 `canvas_edit` 定向13项通过，已包含在 `editor_mcp::` 的75项内；同一最新二进制 `agent_controller::` 63项及画布Agent辅助2项通过，共140项。验证实际像素、历史、蒙版、调整、坐标、回执重试及提交前零执行边界，没有启动前台App。
- MCP协议与stdio合计28项通过；笔刷工作量新测试1项通过；公共模型服务工具合同4项通过。
- 私有运行时最终185项通过，其中画布编辑25项。包含完整COTO审批流程、审批持久投影、真实历史版本序列 `[1,0,1]`、低版本Undo的UNKNOWN原回执恢复和来源摘要防伪。供应商和MCP为本地替身，不能当作真实模型或原生App端到端证据。
- 覆盖清单校验通过：42个原生命令、45个画布辅助接口、26个MCP注册、161条业务路由；这些数量不表示全应用完成率。
- 最新公共宿主 `cargo clippy -p concat --lib --offline -- -D warnings` 通过。协议/stdio严格Clippy也单独核对。

公共模型服务工具合同已在候选源码纳入 `canvas_edit`，工具词汇与MCP注册保持一致；该服务变更尚未部署。完整真实模型与原生App链、成品质量仍未验收。当前仍需逐项确认画布文字、矢量、文件型调整等扩展能力，不能把27类动作当作全部画布功能完成。
