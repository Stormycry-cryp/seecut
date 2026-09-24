# Contributing to Seecut

Use the [Seecut repository](https://github.com/Stormycry-cryp/seecut) for
issues and pull requests. For a bug, include the platform, build or commit,
steps to reproduce, and the smallest sample that shows the problem. Remove
private data and credentials before attaching logs or project files.

Check [Seecut Releases](https://github.com/Stormycry-cryp/seecut/releases)
for a published package. If none is available, clone this repository and run
the current source locally. The Rust workspace is under `src/`; its editor
crate and executable are still named `concat`.

## Working on the code

Install the Rust version specified in [`src/Cargo.toml`](src/Cargo.toml),
FFmpeg development libraries, cmake and a C++ compiler. To run the editor:

```sh
cd src
cargo run -p concat --no-default-features --features wgpu
```

For macOS preview packaging, see [`scripts/make-app.sh`](scripts/make-app.sh).
That script packages the built editor and can install it into Applications,
so review its options before running it.

| Path | Contents |
| --- | --- |
| `src/crates/` | Rust engine, host, services and Slint editor |
| `src/crates/concat/locales/` | Interface translations |
| `test/` | Media and analysis fixtures |

Keep each pull request focused on one change. Run formatting and the checks
that exercise the affected code; report the commands and results in the pull
request. If you change interface text, update and check the locale inventory
with `python3 scripts/locales.py` and `python3 scripts/locales.py --check`.
Translation instructions are in [`TRANSLATING.md`](TRANSLATING.md). If you
change a downloadable model, update [`models/manifest.toml`](models/manifest.toml)
and run `python3 scripts/models.py --check`.

## Licensing and upstream attribution

The source is distributed under [`LICENSE`](LICENSE), with the terms in
[`LICENSE-EXCEPTIONS.md`](LICENSE-EXCEPTIONS.md). Preserve existing licence
headers and the attributions in
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md). Identify any third-party
material added by a contribution and its licence.

[`CLA.md`](CLA.md) and [`TRADEMARK.md`](TRADEMARK.md) are retained from the
upstream Concat project. They name the upstream repository and its maintainers;
they do not describe Seecut's contribution process or ownership of the Seecut
name. Do not replace their historical attribution when editing this fork.
