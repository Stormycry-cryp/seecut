# SeeCut 图像编辑视觉补丁

固定上游提交：`ff4738810134d7f7e68b49f2e4f231b686282ff3`。
补丁：`seecut-theme.patch`；路径、来源与 SHA256 见 `seecut-theme.source.json`。
本补丁由实际 Astra 子代理编写；主代理负责串行构建、宿主集成及最终验收。

## 已实现

- 保留 14 个主题及其序列化 ID。默认深色 `proMedium`、默认浅色 `proLight` 使用 SeeCut 当前主题；原始 `pro` 面板降至 `#1b1b1d`，保持已有四档亮度调整的先后关系。
- SeeCut 深色：面板 `#232326`、画布外背景 `#151517`、输入框 `#29292c`、文字 `#f2f2f2`、强调色 `#ff684a`、强调色上文字 `#171719`。
- SeeCut 浅色：面板 `#f8f9fb`、画布外背景 `#eceef2`、输入框白色、文字 `#20242c`、强调色 `#173e78`、强调色上文字白色。
- 主题边线保留 SeeCut 源定义的透明度；圆角分别为 5/7/12。原始 Pro 面板、菜单、工具栏与所有工具布局保留，未整体放大控件以避免压缩画布和隐藏工具。
- 标题栏沿用 SeeCut `BrandMark` 的 S 字、主题色及几何比例；运行时应用图标复制现有 SeeCut logo 原件，不重新绘制品牌图形。
- 桌面启动标题、原生菜单和文档标题显示 SeeCut 图像编辑；保留未保存标记、文件名偏好与原有标题行为。
- 首页、关于窗口、取色窗口的名称通过显示层适配。上游 Help/社区链接保留原命令和目的地，但以来源信息呈现；关于页保留 PhotoCraft 与作者团队的文本归属。
- 保留所有翻译源键；对应产品名与受限品牌标签只改变显示值。资源来源追加到上游 `ATTRIBUTION.md`；未修改许可证或 NOTICE。

## 视觉依据

SeeCut 来源提交 `247334c2f37cde265b2424923fd3386e23dcc0f3`：

- `src/crates/concat/ui/theme/dark.slint`
- `src/crates/concat/ui/theme/light.slint`
- `src/crates/concat/ui/theme/theme.slint`
- `src/crates/concat/ui/brand-mark.slint`
- `src/crates/concat/ui/assets/seecut-logo.png`

同时查看了已有 `seecut-canvas-dark.png` 产品截图，作为空间与色彩层次的参考；这不是本补丁渲染结果。

## 已验证和待验证

已验证：

- 从固定上游读取各被改文件到独立副本后，`git apply --check` 成功。
- `git diff --check` 成功。
- `rustfmt` 解析和格式化主题、品牌和 i18n 入口成功；它不证明代码编译通过。
- `menu_catalog.rs`、`state.rs`、engine `shell_commands.rs` 与固定源逐字节相同。
- 所有修改的 TSV 保持原翻译键、原顺序及行数。

已编写但尚未运行：深浅色正文/次要文字/主按钮对比度、保留 Pro 布局、所有语言品牌标签、标题标识槽位测试；原窗口标题、原生菜单、菜单导航及关于页定位测试的预期同步更新。

未验证：Rust 编译、UI 单元测试、WASM、真实离屏渲染、窄窗及弹窗布局、鼠标键盘/输入法交互、真实保存和导出。构建和截图由主代理串行执行，避免共享 Cargo 锁和抢占用户焦点。

## 后续边界

- 运行时图标完成替换；Windows EXE/安装包图标、macOS bundle 图标、上游打包脚本与 Web 启动壳尚未适配。不得直接将上游打包目标作为 SeeCut 正式包分发。
- 字体仍复用上游 Inter/JetBrains Mono 与原有多语言 fallback；尚未接入 SeeCut 的 Helvetica Neue 字体来源。字号与控件密度保留原完整编辑器，需在实际渲染后判断文字度量。
- 除默认 SeeCut 深浅色与最暗 Pro 面板外，其余可选主题继续保留上游配色，原功能不删减。
- 此补丁仅证明可重放的视觉源码改动，不证明完整编辑器已接入 SeeCut 产品入口或同窗嵌入，更不证明原有所有编辑能力完成验收。
