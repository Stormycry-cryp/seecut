# Agent 圆角浮层与会话权限（2026-10-20）

本包修复 Flow 中展开面板、收起按钮周围露出的矩形底板，并把 Agent 页面权限改为互斥的“允许读取”／“允许页面操作”。基线 `e0136fb072ab1f9fe9980687d5d178d63fd6d434`，本轮功能提交 `73d8782`；私有 runtime `6f07373` 已在私有仓库提交推送，源码不进入公共仓库。最终构建、测试、候选包摘要见 `verification.json`；功能源码文件摘要见 `frozen-source.json`。

## 行为与实现

- 原 WebView 挖空范围比面板四周多 8px，且为直角矩形；它暴露了下层原生页面。现在 Slint 表面半径、WebView 的 even-odd 圆角遮罩和原生 hitTest 使用同一形状。Flow 中不挖透明阴影，原生页正常阴影、面板与输入框保留。缩放手柄固定 16×16 viewbox，在圆角内部悬停显示。
- 固定 Agent 写权限使用明确的会话生命周期（无 deadline），不是延长五分钟计时。外部手动 MCP 客户端维持原来的有期限授权，设置页不能用固定 Agent 身份创建或续期租约。
- “允许页面操作”同时建立读取权限；切回“允许读取”保留读取 token，立即撤销旧写 token、原生/Flow 精确操作许可及旧确认。新建、切换、结束会话和账户变化不继承授权；同一有效会话重连可以保留。
- `session.permissions` 只走可信宿主 IPC，以 sessionId、单调 revision 和 mode 校验。只替换 MCP 编辑器传输，COTO/provider 会话不重建、不重发输入；更新时中断旧轮次，旧确认不能因重新授权复活。错误应答或传输失败保持禁写。
- 单轮 120 秒、明确高影响操作确认、对象/选择/版本校验、UNKNOWN 原操作核实与去重保留。精确单次操作 permit 的时限是独立执行边界，不是页面授权续租。
- 默认权限菜单有两项权限选项；连接和刷新模型作为分隔线后的辅助动作保留，不增加第三种权限。

最终后台构建通过：应用 6 分 12 秒、测试二进制 8 分 16 秒，MCP stdio 二进制也已构建通过。最终定向测试 121 项原生 + 72 项运行时/合成链路全部通过；另 1 项原生辅助子进程用例按设计跳过。独立源码 harness 与权限注册表复跑不重复计入。

## 验证分层

- 原生 Rust/Slint：后台构建与定向宿主、权限、确认和会话测试（含连续权限更新）；不启动 App。结果见最终验证清单。初次新用例发现进程桥接白名单遗漏 `session.permissions`，请求在派发前被拒绝；现已补齐该确切方法，重新构建后复测。
- 实际 COTO / 模拟 MCP：既有安全回归 59 项，新增会话权限 8 项。覆盖切档保留同一 session/provider、旧确认失效、错误会话/版本拒绝、失败禁写、EOF 不复活、新会话不继承、UNKNOWN 不清除、不重放、可信 IPC 协商与凭据脱敏。
- 模拟编辑确认链 2 项；loopback HTTP / SQLite / 合成 provider 的计费、UNKNOWN 与迟到审批取消 3 项。无真实模型或付费调用。
- 无窗口 Chromium：加载既有打包 Flow 的真实项目列表网页，API 使用只读合成项目。实际执行页面鼠标点击、键盘输入、筛选、取消弹窗，检查深浅主题与 1200/420px 视口，共 6 项通过，无 pageerror。`headless-web/` 保存截图、请求事实与静态资源摘要。它不覆盖画布写入、Agent UI 或原生窗口合成边界。

## 实窗证据的准确范围

`before/`、`before.json` 是旧 272bcf1 运行副本的原始复现。`first-fix-native/` 是 **00:35 首版浮层修复**的实窗结果：深浅色展开、收起没有矩形底板，圆角外点击交还 Flow；确切二进制摘要在该目录的 manifest。该版尚未包含固定手柄 viewbox 和新的会话权限，不能作为最终版本实窗验收。

未收录标注错误的 56/57 截图，也不把曾发生指针定位问题的拖拽操作视为通过。此前原生参考图移除、失败回填、Flow 手工连线与重开持久化的证据单列在相邻的 `2026-10-20-native-flow-resume/`。

用户正在使用电脑，本轮恢复后没有系统鼠标、全局按键、输入源切换、窗口激活或前台抢焦点。最终新权限菜单实际点击、热切档 UI 连通、窄窗和各页面的原生表现、最终 Wry/Slint 圆角穿透与缩放手柄拖拽仍待原生实窗验收。历史 manifest 中 `flowRuntimeCommit` 字段记录的是当时的私有 Agent runtime 提交，不是 Toonflow 网页源码提交；网页资源以单独摘要清单识别。完整 Agent→原生 Flow→生成→资产库链仍未通过，不以浏览器或模拟结果替代。旧核验曾切换输入源，本轮没有操作用户当前输入源。

## 复现

在 `src/` 使用仓库固定工具链及已有目标目录构建：

```sh
CARGO_TARGET_DIR="$PWD/../.toonflow/native-target" cargo test -p concat --lib --offline --no-run
CARGO_TARGET_DIR="$PWD/../.toonflow/native-target" cargo build -p concat --offline
CARGO_TARGET_DIR="$PWD/../.toonflow/native-target" cargo build -p concat-editor-mcp --features stdio --bin seecut-editor-mcp --offline
```

测试二进制的定向过滤器见 `verification.json`。私有 runtime 需上述固定提交和已安装的锁定依赖。在私有仓库执行（`node` 使用已配置的 Node 22+）：

```sh
node --test --test-concurrency=1 test/approval-ipc.test.mjs test/embedded-flow-tools.test.mjs test/flow-runtime.test.mjs test/lifecycle.test.mjs test/runtime.test.mjs test/session-permissions.test.mjs
node --test --test-concurrency=1 --test-name-pattern='million v2 loopback|UNKNOWN阻止新输入|等待审批时服务端取消' test/managed-provider.test.mjs
```

在公共仓库根目录运行 `node --test integrations/toonflow/tests/agent-confirmation-chain.test.mjs`；它默认寻找 `.toonflow/agent-source`，可通过 `SEECUT_TEST_AGENT_SOURCE` 指定准确私有 checkout。

网页脚本为 `integrations/toonflow/tests/headless-project-list.mjs`。设置 `SEECUT_QA_WEB_ROOT` 指向准确候选的 `toonflow/build/web`，`SEECUT_PLAYWRIGHT_MODULE` 指向已安装 playwright-core 的入口，`SEECUT_CHROMIUM_EXECUTABLE` 指向无窗口 Chromium；可选 `SEECUT_QA_OUTPUT` 指定隔离证据目录。运行 Node 脚本自动建立随机 loopback 端口，拦截外部请求并在结束时关闭浏览器和服务。

未运行远程 CI、未部署、未配置持久凭据、未启动最终候选 App。272/35a 封存包及旧混合 WIP 保留。

## 开发运行副本的签名边界

固定程序、MCP 和私有 runtime 成对更新；签名严格校验时暂时移出原有 `portable`，随后原位恢复，不读取其凭据。恢复后的开发副本包含可变且未封存的用户数据，整包严格签名报告该目录的 120 项新增资源，未报告缺失或修改的封存资源；这不作为发布签名通过。确切结果见 `runtime-manifest.json`。封存包与用户数据保持原样，最终 App 不启动。
