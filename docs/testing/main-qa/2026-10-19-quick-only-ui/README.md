# 统一快速模式：同版 macOS 实窗验收

PR #16：<https://github.com/Stormycry-cryp/seecut/pull/16>。本轮结论为**部分通过，不能据此发布或宣称完整验收通过**。

应用功能源码 `3cf81760848d8502134e64994a7409e34aa210ce`，基线 `c7196c4ad0678bb5793b83bdc71f90708d01533a`（PR #15）；私有运行时 `7a05ee22bc6889e3ac16e9e6b8a414ae823be53c` 未改变。本轮只验收这一候选。实窗结束后修复了 locale 检查脚本并刷新英文清单，未重新构建应用；因此以下 UI 证据严格对应 `3cf8176`，不声称来自证据提交后的新二进制。

## 交付与验证边界

删除首次快速/专业选择、生成页模式切换、设置工作模式，以及生成表单双布局分支。模型选择、生成强度、参考素材和正常生成参数保留。旧 `creator_mode` 值不再驱动布局，发生常规偏好保存时兼容写 0；无保存事件的退出不会自动改写旧值。本轮未修改 LUT、推荐或资产库实现。

独立便携候选 `.toonflow/SeeCut-Quick-Only-QA.app`，Bundle ID `cloud.stormycry.seecut.quick-only-qa`；原生构建通过（7m56s），签名/Agent manifest 校验通过。Web/Server/Nodes 复用冻结 R3 包。原始二进制 SHA256 `9193d18865c2871a2667893581dc7d34010517357eb5bb48c2d0d93192666e15`，签名后 `353989cb5824498c45603680cfe6c3c82999b0b40d292262c542940c5f8ec18d`。

[candidate-manifest.json](candidate-manifest.json) 是启动前封存记录，其中 launched/actualWindowVerified 为 false；本轮完成状态另见 [validation.json](validation.json)，不能将前者作为最终运行状态。未安装、未部署、未调用真实模型，未运行全量测试。

2026-10-19 03:47:22 UTC，官方只读 `CGPreflightScreenCaptureAccess`、`AXIsProcessTrusted` 均为 true。没有请求或修改权限。通过 CGEvent/AX 操作指定 QA Bundle/进程/前台窗口，使用系统 `screencapture -l` 取得原始 PNG。主窗口逻辑尺寸 1400×866，窄屏 1000×760；实际像素及时间见 [screenshots.json](screenshots.json)。截图未改绘、未拼接。输入辅助源码为 [ui.swift](ui.swift) 和 [act.py](act.py)；[actions-redacted.jsonl](actions-redacted.jsonl) 保留动作、窗口、时间，文本输入参数省略。登录画面、原始账户数据、令牌和带菜单栏的早期 AX 输出未发布。

服务为 `integrations/toonflow/tests/prompt-ui-fixture.py`，新隔离目录 `.toonflow/macos-ui-20261019-service`，只绑定 `127.0.0.1:18743`，使用合成账户、合成 Jev/GLM/图像供应商及真实本地 HTTP/SQLite 链路。测试不代表线上模型质量或真实付费接口成功。旧工程与旧混合 WIP 未清理。03:47 旧 LUT QA 应用正常退出；04:28 本轮 QA 应用正常退出、仅本轮服务停止，已启用的原中文拼音输入源恢复成功。

## 实际动作与结果

| 范围 | 实际动作 / 观察 | 结果与证据 |
| --- | --- | --- |
| 旧专业偏好 | 旧 creator_mode=1 启动、设置页、退出后再次注入 1 重开 | 无模式选择弹窗；设置无工作模式；快速布局。[03](screenshots/03-legacy-professional-normalized.png)、[71](screenshots/71-settings-quick-only.png)、[75](screenshots/75-legacy-preferences-reopened.png) |
| 模型和强度 | 双参考图、模型 nano-banana-2.1、1K/自动；暗色窄窗展开强度菜单 | 模型和参数可见，“快速/高强度”仍是独立生成强度，不是已删除的工作模式。[14](screenshots/14-references-and-parameters.png)、[73](screenshots/73-generation-dark-narrow.png)、[74](screenshots/74-effort-retained.png) |
| 原生推荐 | 英文原提示词+两个合成参考图，展示推荐、点击“柔光棚拍”、撤销 | Chinese rewrite 生效，撤销恢复原文；参考图顺序、模型和参数保留。[15](screenshots/15-recommend-applied.png)、[16](screenshots/16-recommend-restored.png) |
| 推荐调用边界 | 前后读取合成服务计数；记录模型协议 | Jev 请求含 6 个给定候选，UI 展示 3 个；两处点击各触发一次 glm-5.3-flash。原生推荐前后 generation_tasks / billings / ledger 均为 0。见两个 recommend-counts JSON 与 [prompt-evidence.jsonl](prompt-evidence.jsonl)。不是全流程最终 ledger 为 0 |
| Flow 原红框 | 新建 UI Synthetic Flow R2 并打开画布 | 顶部“画布1 / 画布 / 文档”行不存在。[43](screenshots/43-flow-recommend-ready.png) |
| Flow 推荐 | 手动新增图片节点，输入提示词、移动节点到可视区，点击推荐 | 提示词改写，1K/16:9/快速/nano-banana 保留。[43](screenshots/43-flow-recommend-ready.png)、[45](screenshots/45-flow-rewrite-ready.png)。撤销缺失见 F1 |
| Flow 图片生成 | 04:16:01 点击一次生成；等待约 31 秒 | 1 个 succeeded task / 1 个 output / billing quantity=1；实际为合成黑色像素，不将提示词中的 white square 当作视觉断言。[48](screenshots/48-flow-waited.png)、[flow-generation-db.json](flow-generation-db.json) |
| 默认入库 | 生成成功后直接打开个人资产库，没有点击“添加到资产库” | 1 个 Flow 图片自动入库；文件 SHA 与登记 SHA 一致。[49](screenshots/49-library-auto-result.png)、[asset-registration.json](asset-registration.json) |
| Flow 工程重开 | 返回工程列表，再打开同一工程 | 文字节点及生成结果仍在，没有重新生成。[52](screenshots/52-flow-reopened.png)。此为会话内重开，不代表跨应用重启 |
| 原生画布 LUT | 文件选择器打开合成工程，选 Color Lookup 副本，拖动强度滑块 | 画面变色；保存实际 amount≈0.78（原 0.25）；旁边数值仍 0.25，未通过同步显示，见 F2。[58](screenshots/58-native-fixture-opened.png)、[60](screenshots/60-native-lut-slider.png) |
| 原生文字 | T 新增文字 SeeCut UI QA、确认、加粗、左对齐 | 保存 style 为 Left / 700，画面可见。[65](screenshots/65-native-text-styled.png)、[native-saved-state.json](native-saved-state.json) |
| 原生保存和重开 | 点保存，返回画布列表，再打开 | 修改后的颜色和新增文字仍在。[70](screenshots/70-native-reopened.png)、[edited-canvas.comp](edited-canvas.comp) |
| 原生导出 | 导出菜单→导出至资产库，重复同一动作一次 | 第一次入库成功；第二次出现重复条目，见 F3。[67](screenshots/67-native-exported.png)、[68](screenshots/68-native-repeat-export-duplicates.png)。实际导出 [native-export.png](native-export.png) |
| 跨应用重启 | 正常退出，重开同一候选 | 模型选择、强度和导出目录文件值保留；界面登出且草稿为空，不宣称登录/草稿/参考图跨重启通过。旧值 1 被忽略，但未触发偏好保存故文件仍是 1。[reopen-preferences.json](reopen-preferences.json) |

## 复现问题及未完成路径

- **F1：Flow 推荐改写后撤销不可见。** 在图片节点输入原文→等待三个推荐→点击“柔光棚拍”。文字已改写，但“等待更新”一直可见，撤销入口缺失（45）。未定位根因、未修复，不能借用原生推荐撤销通过来覆盖 Flow。
- **F2：LUT 强度显示滞后。** 打开合成工程→选 Color Lookup 副本→将滑块从 0.25 移到约 0.78。画面和保存值变化，右侧数字仍 0.25（60）。直接数值输入的尝试未生效，不计为通过。
- **F3：相同画布重复导出产生两个素材条目。** 不修改画布连续两次“导出至资产库”，库共 3 项：两份画布加 1 份 Flow 图片（68）。两次导出均 46378 字节，SHA256 均为 `e0d32737b2c2bf4ed24c7247b121e29df4ddec0c27c19485c22ae9b2b730824b`，底层文件可复用，但两个 registration_id / item id 不同；内容去重验收未通过。
- **F4：Flow 节点靠下时推荐区域挤出窗口。** 图片节点在默认新增位置出现推荐后，输入/生成区超出视口；适应视图增至 116% 仍不能解决（[40](screenshots/40-flow-fit.png)）。本轮通过手动上移节点完成后续操作，不宣称自动布局通过。
- 登录完成关闭浮层后曾出现底层模板预览（原始 09），疑似点击穿透；未重复确认，列观察项，不能直接认定根因。
- **R2 Agent 全闭环未通过。** Agent 已创建并设置文字节点（[29](screenshots/29-flow-text-configured.png)）；新增图片动作出现真实确认（[30](screenshots/30-flow-add-image-confirm.png)），因等待确认超时停止，之后继续时 5 分钟授权过期（[33](screenshots/33-flow-continuation-result.png)）。没有绕过确认/权限保护，没有完成 Agent 创建图片→连接→生成全链。后来的手动图片生成单独记账，不能冒充 R2 Agent 成功。
- **R3 冲突 UI 矩阵未执行**，不宣称三类冲突/取消/重试/断网路径通过。推荐竞态、失败保留、视频、账户/服务重新绑定也未逐项执行；完整矩阵仍见 `../2026-10-19-prompt-fixes/ACCEPTANCE.md` 与 `../2026-10-19-cloud-conflicts/ACCEPTANCE.md`。
- 旧文字双击编辑未成功进入编辑框；新增文字路径独立通过。LUT 文件导入/None/0/撤销重做/混合模式和蒙版矩阵未做本轮 UI 验收。中文 Unicode 显示正常，但本轮临时使用已启用 ABC 输入源，**没有中文 IME 组合输入通过证据**。

## CI 定向修复

CI run `39196734261` 的 `scripts/locales.py --check` 失败。当前候选与只读提取的基线 c7196c4 均报告完全相同的 16 个 gone key，见 [locales-before.log](locales-before.log) 与 [locales-baseline.log](locales-baseline.log)。不是快速模式清理删除了这些文案。

原因一：检查器在第一个顶层 `#[cfg(test)]` 截断 Rust 文件，但 canvas.rs 第 43 行只是外部 `mod c02_tests;`，大量正式 UI 文案在后面。原因二：Color Lookup 来自 `ADJUSTMENT_KINDS` 表再通过 `tf(key)` 翻译，旧直接调用正则未收录。修复仅让检查器先跳过外部测试模块声明，并读取该名称表，保留内联测试段排除；刷新英文清单，新增三个既有动态名称，不删除任何翻译。

修复提交 `94033b4`。定向验证 `python3 scripts/test_locales.py`：1 test passed；`python3 scripts/locales.py --check`：exit 0。后者仍提示各非英文 locale 缺少 Curves、Gradient map、Hue & Saturation 三个名称（按现有策略使用英文回退，非失败）；不声称翻译齐全。日志见 [locales-regression.log](locales-regression.log)、[locales-after.log](locales-after.log)。不因检查器修复重复全套构建/UI，CI 新运行结果另核对；旧 run 的 workspace/macOS build skipped 不能写成通过。

## 收尾

本轮未启动其他写入者。04:31:30 UTC 官方七天窗口 10080 分钟 used=19%，剩余 **81%**，重置 **2026-10-25 01:02:41 UTC**；高于用户 50% 停止线。候选仅本机留存，未安装/发布/合并 PR。审查仍需确认上述失败项和未验路径；冻结候选没有为掩盖问题而重建。

旧 head 的 Android arm64 检查失败、iOS 检查仍运行；本轮未调查移动端，不将这些检查标为通过。最终本机清理和写入者快照见 [final-checkpoint.json](final-checkpoint.json)。
