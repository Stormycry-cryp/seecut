# Flow 原任务查询后的选择误判修复 — 2026-10-19

当前结果：源码修复、定向验证和独立候选打包完成；**实际窗口整链收尾仍待复验，R2 未标记完成**。

## 问题与改动

10 月 15 日实际窗口中，`flow_get_task` 已取得原请求的 completed 结果和资产版本，随后 Agent 却以“用户选择已改变，本轮编辑已停止”结束。宿主 `editor_mcp.rs` 将 Flow 的内容 revision 同时作为 selectionRevision；查询更新节点任务结果并保存会推进内容版本，从而误触发冻结选择校验。

修复让每个画布实例维护独立的选择计数，监听规范排序后的节点和连线选择集合，使用同步 watcher 保留两次宿主观察之间“选走再选回”的变化。通过工作区、WebView 消息及原生 Context 传到 Agent。内容和视角更新仍推进原内容版本，不再冒充选择变化。

原有 Agent selection guard、文档会话、账户、画布和写入 revision 校验均保留；私有运行时代码未改，仅增加契约回归。原生 Context 必须收到新的 selectionRevision 字段，旧页面缺少字段时拒绝建立上下文，不静默假定选择没变；必须使用本次匹配打包的原生和页面。

未修改生成提交、原任务恢复、扣费或资产注册逻辑。

## 固定来源

- R2 公开源码：`5d6429e373d731478494f480afb2a18d02a8a9e3`，接续 `8c82ae7`，Draft PR #10。
- 推荐分支集成源码：`d938ca67ff90efc66f64a5630fdcf9b5451b1569`，接续 `4503f3b`，Draft PR #11。R2 合并时仅解决集成补丁文本的上下文/行号冲突，保留已核对的推荐版完整补丁。
- 私有回归：`7a05ee22bc6889e3ac16e9e6b8a414ae823be53c`，接续 `aaaeae4`，Draft PR #3；只有测试文件变化。
- 旧 C03 混合 WIP、用户原 Flow 工作树和旧执行线程未修改或启动。

## 本轮验证

- 私有运行时：`node --test test/flow-runtime.test.mjs test/embedded-flow-tools.test.mjs`，12 项通过。新增内容更新后正常收尾、实际改选、选走再选回、切图、换账户五种契约场景；保留原未知操作恢复与不重发验证。
- 宿主上下文：`bun test integrations/toonflow/tests/flow-context.test.ts`，1 项 / 6 断言通过。实际发布模块把独立 selectionRevision 传给宿主，任务刷新保持会话，切图更换会话，画布未就绪时清空上下文。
- 原任务恢复与门控：`bun test integrations/toonflow/tests/flow-gate.test.ts integrations/toonflow/tests/flow-task-recovery.test.ts`，14 项 / 67 断言通过。合成场景中只有一次原提交、一次计费，保存重开后的查询保留原请求和资产版本；查询不提交新生成。
- 两个公开工作树的 Toonflow patch/source 校验通过；`git diff --check`、Rust fmt 通过。
- 受影响的嵌入 Web 生产构建通过（3.95 秒）；原生 `cargo +1.93 build -p concat --bin concat --config 'profile.dev.package.concat.debug=0'` 通过（2 分 55 秒）。保留现有大 chunk 警告，不做无关拆分。
- Web 类型检查未通过：现有 `electrobun.config.ts` 引用的 `.hutch/devkit/api/config/ElectrobunConfig` 在本机缺失（TS2307）。没有创建假声明绕过。

这些是本轮定向测试与构建结果，不能替代新的真实窗口验收或真实供应商验收。

## 交付及剩余阻塞

独立候选位于 `.toonflow/SeeCut-Flow-Selection-QA.app`，未安装、未启动。Bundle ID 为 `cloud.stormycry.seecut.flow-selection-qa`，固定本机合成服务地址 `http://127.0.0.1:18743`。代码来源、二进制摘要及整包文件摘要在 `candidate-manifest.json`；签名和 5135 个 Agent 清单文件校验通过。

当前可用工具中没有窗口操作/CUA 工具，不能实际操作 App、获取新截图或完成整链收尾。10 月 15 日的 `/tmp/SeeCut-Prompt-Recommendations-Final.app` 已不存在；旧截图和回执仍在 `../2026-10-15-flow-r2-ui-closeout/`，只代表旧版本。旧合成服务数据目录还在，但不能假定旧临时 App 的项目/会话数据也仍可用。没有为此触发新生成，也没有伪造重开或 UI 证据。

下一步在窗口工具可用后，先检查旧项目副本能否恢复；可恢复时只查询原任务并核对原请求/积分/资产版本和正常轮次结束。若旧项目数据确已丢失，必须明确记录为新的合成链验收，不能声称沿用旧原任务。随后继续 PR #11 推荐界面复核；真实 Jev/GLM/媒体验证仍需用户安全配置服务端凭据。

本轮没有读取、复制、配置或传输密钥，没有真实模型/付费调用，没有部署、生产重启或 PR 合并。官方七天窗口 10080 分钟：2026-10-19 00:11:17 UTC 附近读取已用 5%、剩 95%，重置时间 2026-10-25 01:02:41 UTC；后续开工必须重新读取，停止线仍为剩余 50%。
