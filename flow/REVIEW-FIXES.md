# Flow 冻结候选集中修复

基线：`f2fbd0d61f78d81c5892b93d205af4f0894e2a34`，分支 `codex/seecut-flow-p2-isolated`。本增量只处理主会话指定的运行快照一致性和 revision 溢出；原 `HANDOFF.md` 及历史证据未改写。

## 运行快照

- 校验原尝试的节点参数、标识、作用域、输入指纹及模拟报价。
- task 必须对应原尝试并匹配 scope/fingerprint；从冻结输入重新计算纯模拟输出，拒绝与 task 不一致的内容。此计算只生成内存值，不运行任务、不扣点、不写文件。
- Intent/Failed 无 task；Unknown/Succeeded 必须有 task；CancelRequested 可有或无 task；Cancelled 按接纳与否保留对应输出。成功和已接纳取消的输出必须等于 task 输出。
- delivery 集合必须精确等于成功交付的 Media 输出集合；虚拟总额必须等于全部已接纳 task 的报价之和（含待核实和取消），累加检查溢出。
- 不要求历史 graph/node 仍存在，也不要求单节点尝试的 run_id 存在于 runs。
- 重开在自动恢复 running 状态之前校验；损坏 JSON 保留原字节并明确失败。这是内部一致性校验，不是对恶意整体重写的签名验证。

## Revision

`next_revision` 使用 `checked_add`。文档保存、命令、连接、采用结果、History edit/undo/redo 和 Session undo/redo 的所有加一均使用该边界。允许 MAX-1 到 MAX、MAX 读取和已有回执重放；继续变更明确失败。

`History::undo/redo` 的接口从 `bool` 改为 `Result<bool>`，用于区分无历史与版本耗尽；先检查版本再弹栈。唯一现有调用测试已适配。

## 验证

- 相关测试：`cargo +1.93 test --offline --manifest-path flow/core/Cargo.toml --test integrity --test revisions`，7/7 通过。
- 完整核心：`cargo +1.93 test --offline --manifest-path flow/core/Cargo.toml`，44/44 通过。
- `cargo +1.93 clippy --offline --manifest-path flow/core/Cargo.toml --all-targets -- -D warnings`，通过。
- `integrity.rs`：19 种可解析 JSON 损坏均拒绝且文件字节不变；6 种取消/重开边界通过；删除原节点或图后历史可读取。
- `revisions.rs`：文档/图 MAX 重开、旧回执、直接连接、历史栈、Session 撤销/重做、采用结果失败保护。
- 只读复核未发现阻断项；其孤立 task 测试建议已采纳，随后重新执行上述三项验证。
- 运行输出在 `evidence/review-fixes-{targeted,tests,clippy}.txt`，源码 SHA-256 在 `evidence/review-fixes-sha256.json`。

## 运行约束与未验范围

本轮额度依次读取周剩余 51%、50%、50%；当前规则为严格低于 30% 或不可读即安全暂停。未购买额度、换号或调用付费 API。

未修改 UI、共享清单、主线工作树和生产服务，未推送。真实生成、主线集成、最小窗口尺寸、真实 IME/快捷键、展开 Agent 避让、关闭草稿保护仍为集成门槛；P3/P4 与 76 项完整范围不变，正式通过仍为 0/76。
