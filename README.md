<div align="center">

<img src="docs/screenshots/seecut-logo.png" alt="SeeCut Logo" width="112">

# SeeCut

#### 一站式音视频创作应用

说出你的目标，让 Agent 操作 SeeCut，完成从素材到成片的创作。

[![Rust](https://img.shields.io/badge/Rust-Native_Engine-111827?style=for-the-badge&logo=rust&logoColor=white)](https://github.com/Stormycry-cryp/seecut/tree/codex/seecut-unified-mac-candidate/src)
[![Slint](https://img.shields.io/badge/Slint-Desktop_UI-2563EB?style=for-the-badge)](https://slint.dev)
[![License](https://img.shields.io/badge/License-AGPL--3.0--or--later-3B82F6?style=for-the-badge)](LICENSE)

![All in One](https://img.shields.io/badge/All_in_One-Audio_·_Video_·_Image-8B5CF6?style=flat-square)
![AI](https://img.shields.io/badge/AI-Generation_·_Flow_·_Agent-D97706?style=flat-square)
![Status](https://img.shields.io/badge/Status-In_Development-10B981?style=flat-square)

[简体中文](README.md) · [English](README.en.md) · [开发分支](https://github.com/Stormycry-cryp/seecut/tree/codex/seecut-unified-mac-candidate) · [反馈问题](https://github.com/Stormycry-cryp/seecut/issues)

</div>

SeeCut 把图像、音频和视频创作放进同一个桌面应用，核心目标是让 Agent 贯穿全部功能：用户说明想做什么，Agent 理解当前页面和已有素材，自主安排步骤，直接操作 SeeCut，完成后交付作品和处理结果。

这个目标覆盖生成素材、修改图片、配音、字幕、剪辑和导出，让 Agent 在不同工作区之间持续推进同一项任务。用户可以随时查看进展、补充要求，或直接在界面里接着编辑。

---

## Agent 的目标体验

**你交代目标，Agent 自己操作 SeeCut，做完后把结果交给你。**

- **理解当前页面**：结合实际画面和结构信息，知道用户所在的工作区、当前项目、选中对象、已有素材和任务状态。
- **跨全部功能执行**：根据目标调用生成、资产管理、图片画布、音频工具、视频剪辑、Flow 和导出能力，自主推进常规步骤；缺少关键输入或需要额外授权时再向用户确认。
- **持续完成长任务**：跟进异步生成和处理任务，保留执行进度，处理中途补充与可恢复故障，检查实际产出，最后返回作品、保存位置和完成情况。

为支持这一体验，SeeCut 的架构目标是把应用内全部业务接口工具化，通过 MCP 统一提供给 Agent。Agent 与用户手动操作使用同一套业务能力、权限和项目状态，产出的内容可以继续在应用里编辑。

## 你可以用它做什么

| 创作环节 | 可以做的事 |
| --- | --- |
| **AI 生成** | 从提示词和参考素材生成图片、视频，使用快速模式或专业参数继续调整。 |
| **图片设计** | 导入图片和支持的分层 PSD，在原生画布中编辑图层、文字、调整层和蒙版，制作封面与视觉素材。 |
| **音视频剪辑** | 在多轨时间线上组织视频、配音和音乐，完成分割、裁切、转场、调速与音量调整。 |
| **字幕与配音** | 使用语音转字幕、本地文字转语音等工具，为视频补充文字与声音。 |
| **资产管理** | 整理本地素材和生成结果，按文件夹查找，并送到画布、Flow 或剪辑工程继续使用。 |
| **Flow 工作流** | 用节点组织提示词、素材与生成步骤，复用多步创作流程。 |
| **Agent 创作** | 逐步将全部功能接入自主执行，让 Agent 理解当前页面，跨工作区持续完成用户交代的任务。 |

本地素材管理、图片画布和音视频剪辑可离线使用。在线 AI 生成需要登录并连接服务，可用模型和费用以应用内显示为准；本地字幕和配音工具需要相应模型文件。

## 看看界面

以下展示 macOS 候选版本的生成、资产库、画布和剪辑界面，支持浅色与深色主题。截图使用公开演示素材；生成页为未登录状态。

| 浅色 | 深色 |
| --- | --- |
| ![AI 生成浅色界面](docs/screenshots/seecut-generation-light.png) | ![AI 生成深色界面](docs/screenshots/seecut-generation-dark.png) |
| ![资产库浅色界面](docs/screenshots/seecut-assets-light.png) | ![资产库深色界面](docs/screenshots/seecut-assets-dark.png) |
| ![图片画布浅色界面](docs/screenshots/seecut-canvas-light.png) | ![图片画布深色界面](docs/screenshots/seecut-canvas-dark.png) |
| ![音视频剪辑浅色界面](docs/screenshots/seecut-editing-light.png) | ![音视频剪辑深色界面](docs/screenshots/seecut-editing-dark.png) |

## 从一句话到完整作品

比如，你给出几张商品图，然后说：

> 用这些商品图做一条 15 秒竖屏短视频，配上中文旁白、字幕和音乐，再做一张封面。

我们希望 Agent 能在 SeeCut 内完成整项任务：

1. 读取当前项目和商品图，安排制作步骤与素材用途。
2. 按需生成补充镜头，在图片画布里处理画面并制作封面。
3. 制作旁白和字幕，选择音乐，在时间线上组织镜头与声音。
4. 检查实际画面、声音和字幕，调整节奏、转场与音量，保存可编辑工程并导出。
5. 告诉用户完成了什么，交付成片、封面及其保存位置；有未完成项时明确说明。

同一套工作区也支持手动创作。你可以单独做一张图、处理一段音频，或从已有片段开始剪辑。

---

## 当前进展

SeeCut 正在开发中，目前主要在 **macOS Apple Silicon** 上构建和验证。其他平台的 SeeCut 版本仍需实机验收，当前仓库尚未提供公开下载包。

Agent 已接入部分工作区读取、编辑和任务工具；全部业务接口的工具覆盖，以及跨全部功能的长程任务验收，仍在推进中。上面的自主创作流程描述产品目标。

最新 SeeCut 源码位于 [`codex/seecut-unified-mac-candidate`](https://github.com/Stormycry-cryp/seecut/tree/codex/seecut-unified-mac-candidate)。仓库默认分支目前保留 Concat 初始基线，体验和构建 SeeCut 请使用开发分支。

功能验证与已知限制见开发分支的[实现核对记录](https://github.com/Stormycry-cryp/seecut/blob/codex/seecut-unified-mac-candidate/docs/SEECUT-FIGMA-IMPLEMENTATION-QA.md)。

## 从源码开始

macOS 原生应用需要 Rust 1.93、Xcode Command Line Tools、CMake、C++ 工具链和 FFmpeg 7+ 开发库。

```sh
git clone --branch codex/seecut-unified-mac-candidate --single-branch https://github.com/Stormycry-cryp/seecut.git
cd seecut/src
cargo run --profile quick -p concat
```

Flow 的 Web 工作区和本地服务需要另外准备 Bun 与 Toonflow 资源；完整 Agent 功能也需要配套运行时。原生应用构建与完整功能打包的准备方式分别见[源码说明](https://github.com/Stormycry-cryp/seecut/blob/codex/seecut-unified-mac-candidate/src/README.md)、[Flow 集成说明](https://github.com/Stormycry-cryp/seecut/blob/codex/seecut-unified-mac-candidate/integrations/toonflow/SOURCE.md)和 [Agent 组件打包说明](https://github.com/Stormycry-cryp/seecut/blob/codex/seecut-unified-mac-candidate/docs/SEECUT-AGENT-FLOW.md#local-macos-bundle)。

## 反馈与参与

欢迎提交 [Issue](https://github.com/Stormycry-cryp/seecut/issues) 或 PR，分享创作需求、使用体验和具体问题。报告问题时，请附上复现步骤、系统版本，以及可脱敏的截图或日志。

## 致谢与许可

SeeCut 基于 [Concat](https://github.com/jub0t/Concat) 的 Rust 与 Slint 桌面应用开发，保留上游作者及贡献者声明。部分图片画布实现移植自 Robbie Tilton 的 [Compositor](https://github.com/robbietilton/Compositor)；Flow 工作区集成自 [Toonflow](https://github.com/HBAI-Ltd/Toonflow-app)。感谢这些项目提供的基础。

本仓库代码沿用 [AGPL-3.0-or-later](LICENSE)。附加授权见 [LICENSE-EXCEPTIONS.md](LICENSE-EXCEPTIONS.md)，第三方声明见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)；Compositor 和 Toonflow 的相关部分保留各自 MIT 许可。
