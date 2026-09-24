# Seecut

[简体中文](README.md) · [English](README.en.md)

<img src="src/crates/concat/ui/assets/seecut-logo.png" alt="Seecut logo" width="96">

Seecut is a desktop workspace for making images and videos. Start with prompts and reference media, or import local files and continue in the canvas or an editing project.

## Screenshots

These are captures of the running macOS candidate with public demo media. Generation is shown while signed out; no online generation was submitted.

| Light | Dark |
| --- | --- |
| ![Generation light](docs/screenshots/seecut-generation-light.png) | ![Generation dark](docs/screenshots/seecut-generation-dark.png) |
| ![Assets light](docs/screenshots/seecut-assets-light.png) | ![Assets dark](docs/screenshots/seecut-assets-dark.png) |
| ![Canvas light](docs/screenshots/seecut-canvas-light.png) | ![Canvas dark](docs/screenshots/seecut-canvas-dark.png) |
| ![Editing light](docs/screenshots/seecut-editing-light.png) | ![Editing dark](docs/screenshots/seecut-editing-dark.png) |

## Workflow

1. In Generation, choose Quick or Professional mode, write a prompt, and add references. Online generation needs an account and a service connection.
2. In Assets, organize local media and saved generation results, browse folders, and send media to the canvas or an editing project.
3. In Canvas, edit images and layers. In Editing, add clips to the media area, arrange the timeline, and export.

Local media management, canvas work, and editing can be used offline. Available online models and costs are shown in the app at the time of use.

## Install and build

On macOS, unzip a `Seecut-macos.zip` candidate built by this project and move `Seecut.app` to Applications. This repository does not yet offer a public download package.

Building from source requires Rust 1.93, Xcode Command Line Tools, CMake, a C++ toolchain, and FFmpeg 7+ development libraries:

```sh
cd src
cargo run --profile quick -p concat
```

To package a macOS app, build the release binary from the `src` directory above, then run the script from the repository root:

```sh
cargo build --release -p concat
cd ..
SEECUT_INSTALL=0 ./scripts/make-app.sh
```

The package is written to `outputs/` beside the repository. See the [source guide](src/README.md) for build details.

Current checks focus on macOS Apple Silicon. Seecut candidates for other desktop platforms still need on-device review. See the [implementation QA record](docs/SEECUT-FIGMA-IMPLEMENTATION-QA.md) for checked behavior and limits.

## Feedback and license

Issues and focused PRs are welcome. For a bug report, include steps to reproduce, the OS version, and a redacted screenshot or log when useful.

This repository retains [AGPL-3.0-or-later](LICENSE). See [LICENSE-EXCEPTIONS.md](LICENSE-EXCEPTIONS.md) and [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for additional permissions and third-party notices. Seecut builds on the Rust and Slint desktop code of [Concat](https://github.com/jub0t/Concat), retaining upstream author and contributor notices. Parts of the image canvas were ported from Robbie Tilton's [Compositor](https://github.com/robbietilton/Compositor) (MIT).
