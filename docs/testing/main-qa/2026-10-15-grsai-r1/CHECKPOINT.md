# GrsAI R1 验收记录

最终代码候选：`00e4af1b073d0895913ba95275d90b554ecc1e5e`；原生代码来自 `b826374`，其后仅更新网页顶部控件；基线：PR8 merge `6c9da07cc27a744783f8f38bafac62956229a881`。PR 目标为 `codex/seecut-editor-mcp-m2-write`，不是旧默认分支。协调计划第 42 节仍为交付依据。

独立工作树 `Seecut-grsai-r1`，分支 `codex/seecut-grsai-r1`。未清理、重置、迁移旧 C03/Agent WIP。实现和凭据交接见 [grsai-integration.md](../../../grsai-integration.md)。

## 定向验证

- `server-final-tests.log`：58 项服务端回归通过（GrsAI、采购计费、任务恢复、强度与目录）。
- `alias-route-regression.log`：2 项后续回归通过；显式 Image2 fast 别名及历史原请求不换供应商，无 ID 的未知提交不重发。
- `http-tests.log`：17 项 HTTP 账本/恢复测试通过；早期沙箱回环绑定失败后，在允许回环的环境完成。
- `migration-retest.log`：带旧数据的约束迁移及重复初始化通过。
- `flow-contract-tests.log`：Bun 2 项/10 断言通过；规范模型去重、显式策略、父账本去重。
- `native-cloud-tests.log`：原生 cloud 模块 72 项通过。最初过滤器未命中测试的日志不作为通过证据。
- `flow-parameter-tests.log`：直接编译实际 Rust 参数模块的 6 项回归通过，覆盖 Nano 独立比例、Minimax 480p→1080p→480p/768p 时长集合及非法组合拒绝。
- `image-typecheck.log`、`video-typecheck.log`：两个受影响 Vue 节点类型检查通过。
- `seecut-server-typecheck.log`：SeeCutHost 依赖图类型检查通过。上游完整服务检查仍有既有缺失 Electrobun devkit 类型，不声称完整上游检查通过。
- `toonflow-build.log`：完整网页和服务入口构建通过。
- `native-build.log`：首次默认原生构建通过；最终增量构建和候选 UI 验收结果见后续补充。

最新三项审查修正：Nano 比例选择器、原生 Minimax 分辨率变化刷新时长、显式旧 Image2 别名路由兼容。

## 实际 UI 验收边界

使用独立便携应用、独立工程/素材目录和仅绑定 `127.0.0.1:18741` 的合成服务。测试账号和价格只用于本机验收；图片为单像素 fixture。该流程可验证真实客户端控件、保存恢复、任务账本与积分显示，不能证明真实供应商质量、采购金额或视频结果。运行时“已同步到云端”仅指本机 fixture。

实际捕获积分指 SeeCut 账本 committed capture，不是 GrsAI 已核实账单；报价/冻结不作为实际积分。真实调用、实际采购扣费/退款和生产部署均未执行。

## 配置与额度

已只读核对远端 seecut.service、账户 seecut、目录 `/opt/seecut/server`、root:root/600 环境文件 `/etc/seecut/server.env`，未读取文件内容。未读取附件、密钥或私有 Skill profile；未配置或发送凭据。用户后续需在该服务器受信任渠道配置 SECUT_GRSAI_API_KEY、官方 BASE_URL、verified 采购价格、结果主机白名单；部署/重启和真实调用预算另行确认。

2026-10-15 10:24:50 UTC 官方七天窗口 10080 分钟，已用 26%，剩余 74%，重置 2026-10-22 06:13:18 UTC。停止线为剩余不高于 50% 或读取失败。

## 实际窗口发现与追加修复

`8d28c8b` 原生增量构建通过（14m08s），独立包签名通过。实际窗口确认模型列表不重复、独立高强度策略、Nano 4K 与 9:16 同时保留、初始 0。Minimax 三次界面截图验证 480p/15 秒 → 1080p/10 秒 → 480p/15 秒。

首次本机合成生成在服务端成功，客户端却报 `invalid service task`：Flow 核心持久化状态白名单未包含服务端既有 `provider_accepted`、`processing`、`validating` 状态，导致节点安全停在待核对（0），未自动重发。`b826374` 补齐白名单，新增真实存储保存/重新打开/原任务完成回归；`async-execution-tests.log` 的 20 项执行记录测试通过。该问题已在新候选实际恢复原任务并重新打开工程后关闭。

工具曾短暂断连，原 fixture 进程结束。恢复使用同一数据库与原测试账号，不重置余额；新增 `--resume` 开关仅用于已存在的合成 fixture。生产服务未受影响。

## 最终结果

`b826374` 原生构建通过（12m26s），最终网页构建通过，候选包 `/tmp/SeeCut-GrsAI-R1-clean-header.app` 在复制合成运行数据前完成签名校验。`artifact-manifest.json` 记录源提交、原生二进制、打包二进制、Toonflow 补丁、节点和服务入口摘要。

实际 UI 查询原任务成功、素材入库，节点从 0 恢复为“已完成 2”；返回项目列表后重新打开仍保留高强度、4K、9:16 和 2。数据库前后记录完全一致，仍为同一任务及同一计费父记录；详见 synthetic-ledger-before/after-recovery.json。测试单像素图片显示黑色，不代表模型生成质量。

便携 QA 数据复制曾保留旧候选素材绝对路径，校验正确拒绝越界。仅在关闭本次 QA 副本后，逐文件核对内容摘要并重定位复制数据中的两处路径，原候选不变；这不是产品校验放宽或真实个人库迁移。证据见 synthetic-fixture-path-relocation.json / candidate-clean-header.json。

截图 01–02：模型去重/零积分；04–06：Minimax 时长回切；09–10：原任务完成与重新打开；11：深色窄窗口；12–13：最终浅色/深色界面。03、07、08 保留发现问题时的过程截图，不作为成功证据。

用户随后要求去掉红框中的“画布1”和“画布/文档”切换。00e4af1 仅在 SeeCut 嵌入界面隐藏这两个入口；画布初始化、项目保存和节点保留。最终实际浅色/深色截图确认移除，原任务仍已完成 2。

2026-10-15 11:05:26 UTC 官方七天窗口 10080 分钟，已用 30%，剩余 70%，重置 2026-10-22 06:13:18 UTC（北京时间 14:13:18）。

Draft PR #9：https://github.com/Stormycry-cryp/seecut/pull/9 。尚未合并、部署或配置任何真实凭据；真实付费图片/视频、采购扣费/退款及生产验收仍待用户安全配置与预算确认。CI 的服务端契约、wasm 已通过，其余平台情况见 ci-code-snapshot.json；不宣称全绿。

Android CI 日志确认 runner 收到 shutdown signal，退出 143（见 ci-android-runner-shutdown.log），未确认代码编译失败；不在本轮取消/重启任何 CI runner。最终提交会触发新的 PR 检查，须看最新 SHA 状态。

## 用户追加：生成结果默认入库

核对现有成功链路：`seecutGeneration.ts::refresh` 在完成时无条件调用 `registerResults`，先取得原生 `generation.register` 的资产 ID 和不可变版本，再把结果回填画布。图片、视频共用该链路。原生 `FlowClient::register` 使用账户/任务/输出身份和摘要去重；重新查询原任务不会生成第二份资产。无须启用设置，也无须点击通用的“添加到资产库”。本次没有增加第二条自动保存通路。

在最终候选直接打开个人资产库，显示 1 项，内容为先前自动登记的合成结果；没有执行手动添加操作。实际窗口证据为 `screenshots/14-generated-result-auto-library.jpg`。该行为已满足最新默认入库要求；真实供应商图片/视频仍未调用。

本轮收尾断点：源代码 `00e4af1b073d0895913ba95275d90b554ecc1e5e`，候选 `/tmp/SeeCut-GrsAI-R1-clean-header.app`；新截图已纳入 artifact-manifest。下一阶段 R2 为新 Flow Agent/MCP 实际操作及展开 Agent 保留画布背景，本轮未开始其实现，保留同一写入者和工作树。真实凭据等待不阻塞独立产品功能。

2026-10-15 11:11:18 UTC 新鲜官方额度：七天窗口 10080 分钟，已用 31%，剩余 69%，重置 2026-10-22 06:13:18 UTC，仍高于 50% 停止线。
