# Agent 读取当前剪辑监看帧

状态：实现及适用无窗口验证完成，真实 App 验收待做。承接[全功能计划](2026-10-20-seecut-full-agent-plan.md)，视频导出阶段已随公共 cbc41c5b、私有 cad2bb15 推送。此批增加真实内容读取，不改变剪辑或播放状态。

## 目标与范围

Agent 根据当前 Context 读取原生 Monitor 已采用的最新画面，识别当前文档、时间线、真实采样时间和版本。首批只读暂停、空闲且已就绪的帧，最长边 256 像素。画面来自现有 CPU/GPU 合成结果，不重新读取媒体或重算时间线；播放中快照、用户发问时的帧附件、界面选框与屏幕视口另批接入。

## 实施步骤

1. MCP 增加只读 clip_preview、协议版本 1、封闭 frame 查询与已采用帧元信息。原 Canvas preview 保留；成功只输出一张 image/png，text 和 structuredContent 不重复包含 base64。
2. Runtime 将工具单独按 Doc read 处理；调用前后核对读取范围、模型视觉能力、文档版本、选择、view 与 frame。核验实际 PNG 字节、SHA256、RGBA8 格式、尺寸、CRC 与有界解压，复用现有 media-store，不分配业务 operation 或写权限。
3. GPU 沿当前 Device/Queue 在 UI 回调内立即提交全帧缩略采样，异步读取专用 staging；原三槽纹理后续覆盖不得改变本次结果。每个 reader 只允许一个尚未物理回收的请求；取消或超时后仍等队列完成再复用。
4. 原生 Monitor 请求与结果携带原文档代次、时间线、输入和实际量化时间。新文档或输入变化后不能采用旧结果；播放显示仍可采用同来源较早时间帧，但 Context 在播放中拒读。暂停后只有最新同来源结果可读。
5. 原生捕获与交付前复核账号、会话、实例、文档、版本、选择、view、frame 及 read/media 授权。GPU 非阻塞 poll，PNG 编码放到 worker；读取不自动暂停、跳转或重新合成。
6. 补服务词汇、覆盖清单与正常 CLI 联合验证；完成对应检查后成对提交功能分支并更新 Draft PR。前端和原生 UI 由实际 Astra/high 子代理实施，Root 审查、串行集成和验收。

## 验证门槛

- 协议与运行时：封闭字段、缺 capability、错来源/帧/版本、错误 PNG 或摘要、撤权及交付前变化均拒绝。保留 Canvas、保存、导出和 Task 旧回归；正常 CLI、实际 COTO 和 media-store 联合运行，明确 native/model 为合成端点。
- GPU：实际 adapter、shader、map 和像素结果，验证方向、RGBA、全帧缩图、非整行 padding、ring 覆盖，以及单在途、取消、超时和实际完成后复用。没有 adapter、CPU 替代或跳过都不能记为通过。
- 原生：原文档/代次/时间线/输入/真实时间绑定，迟到结果、暂停稳定、playing/busy/pending 拒绝，交付前撤权不泄露字节；适用严格 Clippy 与格式检查。
- 实际 Slint/Skia 呈现、窗口、模型观看及完整创作链另验。离屏 GPU 和逻辑测试不能证明操作系统已显示该帧。不操作前台鼠标、键盘或焦点。

## 当前证据

- MCP 最终 lib55/stdio21/integration18 共94项通过，严格Clippy、格式、正常二进制构建通过。正常CLI、实际runtime/COTO/media-store五组通过，native/model为合成端点。
- 修复PNG原始chunk名称高位校验后，Node24全量818/818通过；新增预览71项已包含其中。85个runtime实际输入、21个Rust输入分别冻结并由Root核对，private7文件已采纳。新服务clip_preview词汇另通过36项合同检查，未部署。
- GPU真实取得Apple M5/Metal，2项通过、0失败或忽略；实际shader/GPU map、整帧缩略、方向、RGBA/alpha、padding、三槽覆盖及取消/期限/复用通过，严格all-targets Clippy和格式通过。
- 原生最终8文件由Root审查、核对429项冻结输入并独立重放补丁后采纳。最终原生程序240项通过；Host139项复用同源码、同GPU配置及同程序摘要证据，共379个不同测试、0失败/忽略；含tests的all-targets严格Clippy与workspace fmt通过。宏递归上限及两项Clippy问题已局部修复，未提高全局递归限制。真实App仍未验，不能据此认定工具整体已验收。
- 覆盖检查当前全局37、Doc32、旧AppRead4、AppControl6项；已知业务路由170条，整体功能分母仍不完整。验证详情见[QA记录](../testing/main-qa/2026-10-21-clip-preview/README.md)。

普通 App 页目前的 filteredList 继续与真实可见卡片、通用焦点区分；本批不将其标为完成。Android 暂缓，周额度剩余 30% 停止新增工作并保留精确恢复记录。

运行期替换 LUT 或效果 Catalogue 后，现有 Monitor 计划与 ReaderPool 可能保留旧缓存，不能只更新来源戳。本批在原 Host 的 Catalogue 发生替换时明确返回 unsupported，重启 Host 后恢复；显示与既有缓存实现保留，缓存失效另批修复。活动抠像或画笔任务未结束时，该预览也拒读。
