<div align="center">

<img src="docs/screenshots/seecut-logo.png" alt="SeeCut logo" width="112">

# SeeCut

#### An all-in-one app for audio and video creation

AI generation, image design, audio tools, and video editing in one app.

[![Rust](https://img.shields.io/badge/Rust-Native_Engine-111827?style=for-the-badge&logo=rust&logoColor=white)](https://github.com/Stormycry-cryp/seecut/tree/codex/seecut-unified-mac-candidate/src)
[![Slint](https://img.shields.io/badge/Slint-Desktop_UI-2563EB?style=for-the-badge)](https://slint.dev)
[![License](https://img.shields.io/badge/License-AGPL--3.0--or--later-3B82F6?style=for-the-badge)](LICENSE)

![All in One](https://img.shields.io/badge/All_in_One-Audio_·_Video_·_Image-8B5CF6?style=flat-square)
![AI](https://img.shields.io/badge/AI-Generation_·_Flow_·_Agent-D97706?style=flat-square)
![Status](https://img.shields.io/badge/Status-In_Development-10B981?style=flat-square)

[简体中文](README.md) · [English](README.en.md) · [Development branch](https://github.com/Stormycry-cryp/seecut/tree/codex/seecut-unified-mac-candidate) · [Report an issue](https://github.com/Stormycry-cryp/seecut/issues)

</div>

SeeCut brings image, audio, and video creation into one desktop app. Start with a prompt, a reference image, or your own media. Generate what you need, edit images and layers, then combine footage, voiceover, music, and captions into a finished video.

The goal is to keep media useful throughout the process, from generation and organization to editing and export. Make a cover, revise a product image, or create a complete short video, starting from whichever workspace suits the task.

---

## What you can do

| Part of the process | What it offers |
| --- | --- |
| **AI generation** | Generate images and videos from prompts and references, with quick mode or more detailed controls. |
| **Image design** | Import images and supported layered PSDs, then edit layers, text, adjustment layers, and masks on the native canvas. |
| **Audio and video editing** | Arrange footage, voiceover, and music on a multitrack timeline, with splitting, trimming, transitions, speed, and volume controls. |
| **Captions and voiceover** | Add words and sound with speech-to-text captions and local text-to-speech tools. |
| **Asset management** | Organize local media and generation results in folders, then use them in the canvas, Flow, or an editing project. |
| **Flow and Agent** | Organize creative steps as nodes. Let the Agent read workspace state and use connected editing and task tools within permissions granted in the app. These capabilities are still being developed. |

Local media management, the image canvas, and audio and video editing work offline. Online AI generation requires an account and a service connection; available models and costs are shown in the app. Local caption and voice tools require their model files.

## A look inside

The macOS candidate has light and dark themes across Generation, Assets, Canvas, and Editing. These screenshots use public demo media; Generation is shown while signed out.

| Light | Dark |
| --- | --- |
| ![AI generation in light mode](docs/screenshots/seecut-generation-light.png) | ![AI generation in dark mode](docs/screenshots/seecut-generation-dark.png) |
| ![Asset library in light mode](docs/screenshots/seecut-assets-light.png) | ![Asset library in dark mode](docs/screenshots/seecut-assets-dark.png) |
| ![Image canvas in light mode](docs/screenshots/seecut-canvas-light.png) | ![Image canvas in dark mode](docs/screenshots/seecut-canvas-dark.png) |
| ![Audio and video editing in light mode](docs/screenshots/seecut-editing-light.png) | ![Audio and video editing in dark mode](docs/screenshots/seecut-editing-dark.png) |

## One creative workflow

For example, to make a short product video:

1. **Prepare media.** Import product images, footage, voiceover, or music, or generate the images and videos you need.
2. **Edit the visuals.** Work with layers, text, color, and masks on the image canvas to prepare covers and other visual assets.
3. **Organize and reuse.** Keep media in the asset library and send it to the workspace you need. Use Flow nodes to organize generation steps.
4. **Make the video.** Arrange the timeline, add audio and captions, adjust pacing, transitions, and volume, then export.

You can also open the image canvas for a single design or import existing footage and start editing.

---

## Project status

SeeCut is in development, with current builds and validation focused on **macOS Apple Silicon**. SeeCut builds for other platforms still need on-device validation. This repository does not yet offer a public download package.

Current SeeCut source lives on [`codex/seecut-unified-mac-candidate`](https://github.com/Stormycry-cryp/seecut/tree/codex/seecut-unified-mac-candidate). The repository's default branch currently retains the initial Concat baseline; use the development branch to build or try SeeCut.

See the development branch's [implementation QA record](https://github.com/Stormycry-cryp/seecut/blob/codex/seecut-unified-mac-candidate/docs/SEECUT-FIGMA-IMPLEMENTATION-QA.md) for checked behavior and known limits.

## Build from source

The native macOS app requires Rust 1.93, Xcode Command Line Tools, CMake, a C++ toolchain, and FFmpeg 7+ development libraries.

```sh
git clone --branch codex/seecut-unified-mac-candidate --single-branch https://github.com/Stormycry-cryp/seecut.git
cd seecut/src
cargo run --profile quick -p concat
```

Flow's web workspace and local service require Bun and prepared Toonflow resources. Full Agent functionality also needs its companion runtime. See the [source guide](https://github.com/Stormycry-cryp/seecut/blob/codex/seecut-unified-mac-candidate/src/README.md), [Flow integration guide](https://github.com/Stormycry-cryp/seecut/blob/codex/seecut-unified-mac-candidate/integrations/toonflow/SOURCE.md), and [Agent packaging guide](https://github.com/Stormycry-cryp/seecut/blob/codex/seecut-unified-mac-candidate/docs/SEECUT-AGENT-FLOW.md#local-macos-bundle) for native builds and full app packaging.

## Feedback and contributions

[Issues](https://github.com/Stormycry-cryp/seecut/issues) and focused PRs are welcome. Share creative needs, feedback, or reproducible problems. For a bug report, include steps to reproduce, the OS version, and a redacted screenshot or log when useful.

## Acknowledgments and license

SeeCut builds on [Concat](https://github.com/jub0t/Concat)'s Rust and Slint desktop application, retaining upstream author and contributor notices. Parts of the image canvas come from Robbie Tilton's [Compositor](https://github.com/robbietilton/Compositor), and the Flow workspace integrates [Toonflow](https://github.com/HBAI-Ltd/Toonflow-app). Thank you to these projects for the foundation.

This repository retains [AGPL-3.0-or-later](LICENSE). See [LICENSE-EXCEPTIONS.md](LICENSE-EXCEPTIONS.md) for additional permissions and [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for third-party notices. The relevant Compositor and Toonflow components retain their MIT licenses.
