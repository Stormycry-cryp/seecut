# Agent 可读确认与安全收尾（42.19）

基线 `10599172db41ffc4b7688a63665fcbb842914506`（PR #17），私有运行时固定 `7a05ee22bc6889e3ac16e9e6b8a414ae823be53c`，未修改私有代码或旧混合 WIP。本包只改善确认展示和收尾提示，不更改授权协议。功能源码 **`b4c4cbce2cd38ecb8b3fefa8344ceb02eac072f4`**。定向测试共 **171 项通过**：原生 100、运行时安全 59、模拟编辑链 2、本地 managed HTTP 3、生成任务 HTTP 恢复 7。原生测试构建 7m01s 通过；最终测试结果见 `verification.json`。

## 实现与安全边界

- 从真实解析的 Flow 调用映射新增、连接、删除、切换画布、修改文本/提示词/参数、生成图片/视频。现有对象没有可信显示名时显示稳定 ID；新增对象仅使用请求中已有名称。未知动作、额外字段或不完整参数不能启用确认。
- 默认显示动作与对象摘要，显式“确认”“取消”。完整参数默认折叠，展开后限高滚动；切换完整审批请求即收起详情。长文本保留在详情中，摘要仅显示字符数。Flow 详情不发布账户绑定和宿主配置，敏感键值在显示副本中隐藏。
- 展示副本不修改完整请求。原有冻结完整 Approval → refresh 比对全部字段 → 精确 permit → approve IPC 失败撤销 permit 的路径保留；UNKNOWN 禁止确认，仍可停止。
- 五分钟编辑租约到期、撤销或续期 token 改变继续触发断开。文案要求显式续期/授权，再重连并确认新操作。单轮上限 120 秒另计；停止后需新输入，不恢复旧 pendingInputs，不自动重发或自动批准。
- 本包不声称区分人工停止与单轮超时的实际原因；底层只有通用 Stopped 结果时，显示事实和时限提示，不推断原因。UNKNOWN 仍只核查原操作。

## 验证分层

1. **原生 Rust / Slint**：最终测试构建和定向用例结果见 `verification.json`。构建期间未改动功能源码，七个文件摘要记录于 `frozen-source.json`。这是测试二进制构建，不是新应用安装包，也不是 UI 验收。
2. **实际 COTO 运行时、模拟 MCP 编辑端**：既有 59 项安全测试通过；新增 2 项脚本化链通过。新增链由实际 COTO 发出新增文本 → 设置文本 → 新增图片节点 → 设置模型 → 连接节点 → 提交生成 → 查询的工具调用；六次写入逐项等待明确确认，取消路径零写、零提交。编辑图、生成回执和 `SIMULATED-ASSET` 全由测试传输层模拟，不能作为实际生成、入库或原生 WebView 证据。
3. **实际本地 HTTP / SQLite、合成供应商**：managed-provider 3 项通过，覆盖两步推理和结算、UNKNOWN 原操作核实不重复推理、服务端取消后迟到确认零旧写入；另有 Python HTTP 任务恢复 7 项通过，覆盖原请求身份、账户隔离、授权与计费不重复。使用临时数据库、loopback 随机端口及假供应商，没有真实付费调用。managed 测试编辑端仍为模拟，并未与原生 Flow 串成完整链。
4. **私有 fixture 不可用**：新增公开测试显式跳过 2 项，不冒充通过；在本机固定私有 fixture 可用时 2 项均实际通过。

`locales.py --check` 与 diff 空白检查结果见记录。词表检查仍可报告原有翻译缺项，不代表补齐翻译。

## 复现命令

以下在公共仓库根目录执行；私有源码使用上述固定提交的本地 worktree，不下载或提交私有代码。

```sh
.toonflow/r3-agent-bundle/bin/node --test --test-concurrency=1 .toonflow/agent-source/test/approval-ipc.test.mjs .toonflow/agent-source/test/runtime.test.mjs .toonflow/agent-source/test/lifecycle.test.mjs .toonflow/agent-source/test/flow-runtime.test.mjs .toonflow/agent-source/test/embedded-flow-tools.test.mjs
.toonflow/r3-agent-bundle/bin/node --test integrations/toonflow/tests/agent-confirmation-chain.test.mjs
SEECUT_TEST_SERVER_SOURCE="$PWD/services/seecut-server/src" .toonflow/r3-agent-bundle/bin/node --test --test-concurrency=1 --test-name-pattern='million v2 loopback两步|UNKNOWN阻止新输入|等待审批时服务端取消' .toonflow/agent-source/test/managed-provider.test.mjs
PYTHONPATH=services/seecut-server/src:services/seecut-server/tests python3 -m unittest test_generation_request_recovery -v
python3 scripts/locales.py --check
```

原生构建在 `src/` 内执行：

```sh
CARGO_TARGET_DIR="$PWD/../.toonflow/native-target" cargo test -p concat --lib --offline --no-run
```

定向用例过滤器及结果见 `verification.json`；直接运行该次构建生成的测试二进制，无需启动应用。

## 尚未验收

完整“Agent → 原生 WebView → Flow 编辑 → 真实生成 → 默认资产入库 → 收尾”实链未验证。实际确认/取消点击、详情展开、长参数滚动、窄窗口可见性、IME 与续期后界面恢复均未验证。不得以测试或旧 PR 截图代替这些结论。

遵守用户共享桌面禁令：没有启动/激活 QA 应用、注入鼠键、移动窗口或切换输入源。没有更新生产服务、部署、安装新应用、读取持久凭据或真实模型调用。PR #17 的旧应用和截图不能作为本包构包身份或 UI 证据。

07:18:26 UTC 官方七天窗口 10080 分钟，剩余 74%，重置 2026-10-25 01:02:41 UTC。同期线程快照中本任务为唯一可见活跃 SeeCut 写入者，旧嵌入与审核线程 idle；最终提交前读数另见 `verification.json`。
