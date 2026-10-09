# 2026-10-09 Flow 候选交接

## 结论与边界

P2 的独立合成闭环已可运行：节点/有序连线、类型和循环校验、单节点与祖先串行执行、版本化输入、持久保存重开、未知结果核实、取消/晚到结果、显式重试、输出回读。P3 已有图命令、单节点拖动、复制/排列、会话撤销重做；P4 已有持久 operation receipt 和宿主边界。它不是完整 P2/P3/P4 验收，更不是完整 76 项复刻。正式矩阵维持 0/76。

基线 `1e8009ea9ea7e90c2ceb8720037711f02970b923`，分支 `codex/seecut-flow-p2-isolated`。实际路径和文件归属见 OWNERSHIP。全部新增位于 `flow/`；主工作树及未提交 WIP、共享清单/路由/Theme/账户、私有 seecut-agent 均未写入或复制。没有 push、PR、部署、真实费用或新凭据。

## 差异导览

| 文件 | 内容 |
| --- | --- |
| core/src/graph.rs | 开放 kind/version 图模型，类型/端口基数/循环/有限几何校验，图历史基础 |
| core/src/store.rs | 独立 document/runtime 快照；OS 单写者锁、原子写、文档 CAS、损坏保留、失败后关闭重开；save-as 清空执行权限 |
| core/src/runner.rs | 确定性串行模拟器，输入指纹、持久提交意图、原作用域查询、选择批次、取消/重试/交付回读 |
| core/src/commands.rs | revision + operation 回执；原子新增连线、移动、复制、排列、视口等图操作 |
| core/src/host.rs | 复用 SeeCut 生成、账户和资产库的最小合同；真实模型仅保存/交接，模拟器不能执行 |
| core/src/protocol.rs | QA stdin/stdout 与同一图历史，非生产 MCP/HTTP 服务 |
| ui/workspace.slint / composer.slint | LibTV 媒体节点/细连线/浮动编辑区；SeeCut 双主题和图标；用户提供输入区格式；Agent 安全区 |
| qa/ | 本机 Slint 探针、合成资产库选择器与明确标注 SVG fixture |
| evidence/ | 测试日志、CLI 完整闭环及原生合成会话文件，可核对输入和结果事实 |

## 验证

- `cargo +1.93 test --offline ...`：**37 项通过**（25 reliability + 9 host + 3 commands）。完整输出 `evidence/tests.txt`。
- `cargo +1.93 clippy --offline ... --all-targets -- -D warnings`、fmt check：通过。
- 原生 Slint 编译和 Python QA adapter 语法：通过，`evidence/native-compile.txt`。
- CLI 冻结演示 `evidence/demo.txt`：两次暂停等待显式采用，最终 Video 清单，关闭重开读回同一 id/version；30 虚拟点、1 次模拟交付。工程 `evidence/frozen-demo/`。
- Native CUA 真实操作：分别采用图片与视频候选→交付→关闭重开；未重复执行。后续在生成节点通过合成资产库添加版本化参考；输入变化使旧下游失效。中文多行提示词保存并重开读回。相关事实保存在 `evidence/native-session/`，在仅素材/文本编辑之后仍为7次执行、30虚拟点、1交付。
- UI 实际查看深灰/橙红与白/深蓝两种主题，1280×800 和 1024×720 内容尺寸。缩略图编号/加号/正文/底部参数/圆按钮可见，右侧保留 Agent 安全区。无共享全局字体改动。
- 看过本机 LibTV 04/05/06 原图和 Agent composer 原图；也直接看过用户本轮附图，采用其结构而非内容。之前转交的 Library 文件 `libfile_5e6d5c5295c881918a6166624f5a9072` 没有可用只读 materialize 工具，未绕过权限，未声称该截图被修复。截图检查结果在本会话 CUA 输出中，未伪造本地截图文件。
- CUA 的组合键/粘贴在此 Slint 窗口第一次产生了字母 a/v，随后用原生可访问字段设值并保存验证多行。**未验真人中文 IME 组合态、真实剪贴板快捷键**；不能据原生 TextInput 宣称已验通过。

只读复核发现并修复：成功任务反复 retry 导致重复虚拟扣点、非有限坐标导致文档不可重开、旧文档快照覆盖新图、旧 run 永久 running；新增资产回填绕过撤销历史及目标删除后旧回调读不到回执。均有回归覆盖，最后两项经只读复核确认闭合。

## 主线下一步，须串行集成

1. 审查本地冻结 commit 后再接共享 Cargo/App/controller。新增 Flow 导航；根据实际主线 Theme WIP 调整导入，不覆盖整文件。
2. 适配现有 WriterGuard/SaveLane 的 Flow resource；实现窗口关闭/切换工程的未保存草稿保护。当前草稿/撤销栈仅会话内，进程意外退出草稿会丢失；文档与执行事实持久化已独立。
3. 复用现有 `GenerationPage` 及腾讯云账户/统一积分。接 `GenerationDraft` 到服务器能力目录、既有报价确认和任务提交；按原作用域/幂等键查询、供应商取消、结算、下载、托管注册与交付须各自验证。当前 Runner 是模拟器，不可直接当线上 TaskClient。
4. `assets-requested(node, local)` 接既有个人/团队资产库和导入流程。补实际 Asset 内容版本/hash 与可用状态；未完成上传/托管不能作为已就绪输入。当前探针仅合成资产库已通，本地上传回调只提示待宿主接线。
5. P3 继续框选/多选、多图切换 UI、完整剪贴板、命名/删除/菜单/快捷键、可持久恢复的历史、模板/历史与真实交付；当前拖线是点击输出/输入的方式，未完成拖拽连线手势和完整无障碍/性能验证。
6. P4 继续生产 MCP 的有界读、鉴权与当前上下文校验、preview/confirm、公开工具注册和真实供应商事件。随后按现有 76 项矩阵推进音频/3D/插件等，不能把当前六类节点当完整实现。

## 额度与恢复

实质阶段前已用受支持 `get_usage_limits` 读取 `rateLimitsByLimitId.codex` 周窗口（10080 分钟）。最近一次 11:12:38 UTC 剩余 **57%**。严格低于 40% 或不可读，SeeCut/LiveDay0/本 Flow 线都仅做必要保存收尾并暂停新增实现和测试；不高频轮询、不换号/买额度。

所有成果保存在独立工作树；冻结后继续工作的起点是本候选 commit。先读本文件及 OWNERSHIP，重新查额度，与主线确认实际共享接线顺序；未获共享文件归属前仍只写 `flow/**`。
