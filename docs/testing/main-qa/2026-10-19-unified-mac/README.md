# 统一 Mac 开发候选（42.22）

**最新交付：[Clippy 修正后的 11c7d8e 候选](final/README.md)。以下保留 ad395f8 旧包历史，不代表最终修正状态。**

2026-10-19 从固定公共源码 `ad395f82e0ed72ad00dd6e89431a5c80d39f0bf8` 与私有 runtime `7a05ee22bc6889e3ac16e9e6b8a414ae823be53c` 构建。主应用、MCP、Flow Web/Server/Host/节点均重新编译；未复用旧编译宿主或网页。后续本目录提交只保存证据，不改变候选功能源码。

## 本机交付

- App：`/Users/chenyunzhe/Documents/Codex_Project/SeeCut/outputs/SeeCut-Candidate-20261019-ad395f8.app`
- ZIP：`/Users/chenyunzhe/Documents/Codex_Project/SeeCut/outputs/SeeCut-Candidate-20261019-ad395f8.zip`
- arm64，版本 0.2.2，Bundle ID `cloud.stormycry.seecut.candidate.ad395f8`。
- **最低 macOS 15.0**：主程序最低 11.0，但实际随包 19 个动态库要求 15.0，已据全部 Mach-O 调整 Info.plist，不能宣称支持 macOS 11。
- **dev / unoptimized + debuginfo**，开发 ad-hoc 签名，无 Developer ID、未公证；不是正式发布包。
- 空 `Contents/MacOS/portable/` 已保留。API 固定 `http://127.0.0.1:18745`，未启动该服务，在线功能不能直接视为可用。

封装主二进制 SHA256：`06d03072d812d1ef749525ca8292e4fa243ed31978427d5d51c347b7fe6e3a86`。ZIP SHA256：`1ca60ef9fb3f8e29c1b6ac238b995377c700989f4e20ecdba01206e102d7724a`，241212216 字节。完整来源与封存时间见 [candidate-manifest.json](candidate-manifest.json)。整包 6,384 文件逐字节与 ZIP 核对一致；完整文件摘要保存在本机 `.toonflow/unified-mac-20261019/files.json`，摘要根在 manifest 中。

## 本轮实际验证

- 原生主程序和 MCP 离线构建 8m54s 成功；Flow 全部资源按当前 integration.patch 与 Bun 1.3.14 重建成功。874 个固定公共构建输入、15 个私有源文件 SHA 与打包输入一致。
- 24 个 Mach-O 均为 arm64；所有实际加载依赖解析到包内或系统路径。AnyDoc 插件保留的构建机绝对 dylib 自身标识改为 `@rpath/anydoc.darwin-arm64.node`，随后重新签名；Flow 其余文件与新构建逐字节一致。
- 外层 deep/strict 及每个 Mach-O 的签名校验通过；Agent 5,135 文件 manifest 通过。Node 24.15.0、13 个运行时模块语法和 IPC/runtime 导出及依赖解析通过。没有初始化 provider、启动 IPC 或运行 App。
- App 附带公共许可证、例外说明、第三方通知、Synonym 字体许可，以及既有 Agent/Flow 来源许可。
- 本轮只做上述编译和装配检查，复用下表既有定向测试，不将旧截图当成新包实窗验收。没有重复全仓、模型或 R3 测试。

本地封装脚本首次 seal 使用相对路径被现有校验器拒绝，打包随后因未封存清单拒绝；改用要求的绝对路径 seal 后重新打包成功。没有绕过清单校验。初次 Flow 构建未找到默认 Bun，随后显式指定已有校验过的 Bun 1.3.14。两者均未改变功能源码。

## 功能与证据分层

| 功能 | 代码具备 | 后台证据（沿用固定来源） | 此候选实窗 | 生产 |
| --- | --- | --- | --- | --- |
| 快速模式、移除原顶部红框栏 | 是 | [快速模式记录](../2026-10-19-quick-only-ui/README.md)；本轮同版重建 | 未验；旧图仅历史 | 未更新验证 |
| Jev 从给定提示词池选、GLM-5.3-Flash 按需改写，原生画布/Flow 接入 | 是 | [推荐契约与合成链](../2026-10-15-prompt-recommendations/README.md) | 未验 | 真实 Jev/GLM 未验；不将文本改写等同图像模型 |
| 默认生成结果入库、F1–F4 修复 | 是，含 89e77c2 审核补丁 | [44 项 + 22 项](../2026-10-19-ui-fixes/README.md)；含富文本回填、调整值同步、导出去重、浮层联合避让 | 修复后完整复验未做 | 未更新验证 |
| Agent 可读确认/取消、详情与租约收尾 | 是 | [171 项](../2026-10-19-agent-confirmation/README.md)，模拟编辑与合成 HTTP 边界已注明 | 完整原生 Agent 链未验 | 真实模型及收费未验 |
| C03 可编辑文字 | 是 | [原始验证](../2026-10-19-canvas-text/README.md)、[对齐/字号修复](../2026-10-19-canvas-text/repair/README.md) | 中文 IME、焦点及当前版未验 | 未更新验证 |
| 自然饱和度、黑白、Color Lookup | 是 | [两调整层](../2026-10-19-canvas-adjustments/README.md)、[LUT](../2026-10-19-color-lookup/README.md)，实际 GPU/导出资产链 | 当前版控件未验 | 未更新验证 |
| 可编辑矢量蒙版 | 是 | [100 项](../2026-10-19-vector-masks/README.md)，实际 GPU、隔离资产库 | 未验 | 未更新验证 |
| PSD 有界导入 | 是，支持范围受限 | [107 项及外部 fixture](../2026-10-19-psd-import/README.md)，v7 工程 | 未验 | 未更新验证 |
| P01 分页、筛选、详情、回滚、订单恢复 | 按 42.22 已有实现保留 | 本轮不重做、不新声明后台测试通过 | 当前版未验 | 真实账户/支付未验 |
| R3 云同步冲突 | 是 | [42 项隔离 HTTP/SQLite 与桥接契约](../2026-10-19-cloud-conflicts/README.md) | 原生实窗及真实两设备未验 | 未更新验证 |

## 服务匹配与发布阻塞

配套服务应取公共源码 `ad395f82…` 中的 `services/seecut-server`，该目录最后变更提交 `f3f0e3f3b53b711c1e579d4afc4c8a39da91a820`，Git tree `84c4b936dc558e61bf0f6d85759900c8bcfb4769`。私有 runtime 固定上述 `7a05ee22…`；不需要重复实现 P01。

生产精确提交 `84f1165955cf12b5c6f84557a57d6a2d70bf9459` 仅来自 **01:45–01:52 UTC 历史只读检查**。本轮 **08:51:36 UTC 匿名 GET** 仅确认 health 正常、目录 `2026-09-23.1`、支付 configured=false / live_initiation_enabled=false；接口不提供 SHA，不能推断当前精确生产提交。候选比历史版本增加 GrsAI/采购成本计费、Agent managed 合约、Jev 池内推荐及 GLM 改写等，不能把旧服务当作新客户端的已验配套版本。见 [服务来源](server-compatibility.json) 和 [新鲜公开状态](public-service-status.json)。

剩余必要项：
1. 真正隔离桌面的实窗复验：F1–F4、Agent 确认/取消完整链、画布/PSD/文字 IME，以及 R3 两设备。独立 Bundle 不代表鼠键隔离；当前禁止启动或控制窗口。
2. 配套服务更新、持久配置和供应商/账户/计费/支付真实链须另行授权；无密钥读取、写入或付费调用。本包未部署、未安装、未启动或修改生产。
3. 正式发行需 release 构建、明确支持的 macOS 范围、Developer ID/公证与安装升级验证及授权。当前 ad-hoc 开发包不替代这些步骤。

现有 CI run [39224801711](https://github.com/Stormycry-cryp/seecut/actions/runs/39224801711) 已结束：wasm 成功、macOS candidate job 因触发条件跳过、Engine tests/lints 失败。失败是 `cargo clippy --workspace --all-targets -- -D warnings` 的两处 `collapsible_if`：`concat-canvas/src/document.rs:228` 与 `gpu.rs:1284`，见 [失败摘录](ci-failure-excerpt.log)。这是合并/发布前必须收敛的 CI 阻塞；不是本轮 Mac 编译、依赖或签名失败。本轮限定固定源码候选封装，未为 lint 改写已封存源码或重建候选，也未触发另一轮 CI。

旧 WIP、旧候选与当前用户应用保留。本轮没有共享桌面输入、窗口激活、输入源切换、新系统账户或云迁移；无真实付费调用、凭据配置、合并或发布。

收尾官方七天额度：2026-10-19 09:02:49 UTC，窗口 10080 分钟，剩余 **67%**，重置 2026-10-25 01:02:41 UTC，仍高于 50% 停止线。见 [quota.json](quota.json)。

日志副本只去除行尾空白；CI 摘录另去掉终端颜色标记。原始日志保留在本机 `.toonflow/unified-mac-20261019/`。
