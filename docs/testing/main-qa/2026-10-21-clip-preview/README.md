# 当前剪辑监看帧 QA

状态：候选实现已完成适用的无窗口验证，尚未通过真实 App 验收。本文分别记录协议/runtime、GPU 和原生证据，不表示真实窗口、模型或创作链通过。Android 暂缓。

## 已确认

| 范围 | 最终证据 | 限制 |
| --- | --- | --- |
| MCP 协议、stdio、集成 | 55 + 21 + 18 = 94 项通过，0 失败或忽略；严格 all-targets Clippy、格式、正常 CLI 构建通过 | native 返回为合成 fixture |
| 私有 runtime | Node 24 全量 818/818，0 失败、跳过或取消；其中新增预览 71 项，包含 PNG 修复 13 项 | 85 个实际 runtime 输入、21 个 Rust 输入分别冻结，不重复相加测试 |
| 正常 CLI 联合 | 实际正常二进制、runtime、COTO 与 media-store 5 组通过；262548 字节高熵 256px PNG、坏摘要、native 错误带字节、交付前换帧、非 ASCII chunk 均核对；journal 为 0 | native/model 为合成端点，不是实际 App 或模型观看 |
| 真实离屏 GPU | Apple M5 / IntegratedGpu / Metal，2 项通过、0 失败/忽略；实际 sample/copy/map、全帧缩图、RGBA/alpha/方向/padding、原 compositor shader 和三槽覆盖、取消/期限/复用通过；严格 Clippy 和格式通过 | 晚通知使用实际 GPU 配合注入观察时序，不证明驱动卡死；不代表 Slint/OS 呈现 |
| 原生与 Host | 最终原生程序 240 项通过；Host 139 项复用同源码、同 GPU 配置、同程序摘要的有效证据，共 379 个不同测试，0 失败/忽略；含测试 all-targets Clippy 与 workspace fmt 通过 | 429 项输入冻结；244 项 Host 及依赖输入未变，不将两轮说成单次运行；没有启动 App/音频 |
| 服务词汇 | 当前 37 个工具词汇，相关 36 项合同测试通过；请求未提供 clip_preview 时供应商不能自行引入 | 818 全量使用此前 36 词服务源码；本次新词另经合同检查，尚未部署 |
| 覆盖检查器 | 实际 Doc 32 项字面量列表与 4 种解析正反例通过 | 全局 37 / Doc 32 / 旧 AppRead 4 / AppControl 6；已知业务路由 170，功能分母仍不完整 |

正常 MCP CLI SHA256：45c8080d7b7412791f1f1507ca6866ae37c65fedbe2edc8e75fa8320fc57009c。
原生测试程序 SHA256：5d6eafee3a71ae0431f6f6e82f7813c9029edb12bac400c98a34e026b3bbcd34。
Host 测试程序 SHA256：9ca542e945b3cb5c0cf952f8a2c016a78976e0dd6d1c73e2db06c961ffdb0475。
GPU 测试程序 SHA256：ea1b5b6a2d6f35dd551394ba88972036d5730ec5dd6195d35e49f30e181ba0ce。
运行时完整日志 SHA256：81eb6a438603af48525d098e24a498e2750487ebf2e2fa2ffb5529694c846464。

主代理已独立核对对应源码、baseline/candidate、补丁、程序及日志摘要，串行采纳；原生 8 文件从原始补丁加三次局部修复在隔离目录重放后，与最终源码逐字节一致。原始记录保存在本机隔离验证目录；本文的源码与文档引用均为已跟踪文件，不以本机忽略文件作为覆盖检查依赖。

## 修复及原失败保留

PNG chunk 类型原用 Node ascii 读取会清除字节高位，部分无效名称因而误通过。修复为保留原字节的 latin1 后进行原 ASCII 范围校验。IHDR/IDAT/IEND 各字节高位和实际 COTO 拒绝回归，在旧实现 13 项均失败、修复后通过；正常 CLI 也验证拒绝。旧 805 全量只属于修复前候选，不与最终 818 相加。

Rust 首跑的旧工具总数断言仍为 36，加入新工具后修正为 37，其他 schema/凭据规则保持。GPU 首跑因测试插件 ID 不符合既有命名规范退出 101，修复仅改测试 ID；最终原断言全部通过。原生首次编译遇到大型 json 宏递归上限，已用局部工具字面量和独立能力字段赋值修复，未提高全局递归上限。严格 Clippy 随后发现多余解引用及较大的消息枚举变体，已局部修正并在最终程序上重跑原生 240 项。Host 原日志中的 codec stderr 拆行曾令统计器漏算，统计器主动拒绝成功；修复解析后从未修改的日志逐名核实 139 项，不放宽测试。

## 仍待验证

真实 Slint/Skia、窗口、原生 GPU 接线、模型观看、运行时性能和完整创作任务另验；未启动前台 App 或操作鼠标、键盘、焦点。

运行中替换 LUT 或效果 Catalogue 后，本批保守拒绝预览读取，需重启 Host；活动抠像或画笔任务期间也拒读。能力只证明已采用图像及原生输入版本，不检查外部程序原地替换的媒体/字体磁盘新鲜度。普通页面可见卡片和焦点测量仍在隔离验证，未算作完成。

源入口见[预览协议](../../../agent/clip-preview-protocol.md)、[实施计划](../../../plans/2026-10-21-agent-clip-preview.md)、公共 clip_preview.rs / clip_preview_stdio_tests.rs、原生 editor_mcp_clip_preview.rs / panes/monitor.rs 及 renderer gpu/preview.rs。
