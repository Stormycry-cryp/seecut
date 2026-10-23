# PhotoCraft 来源与重建边界

- 仓库：https://github.com/storytold/photocraft.git
- 固定提交：`ff4738810134d7f7e68b49f2e4f231b686282ff3`
- Workspace 版本：`0.14.0`；最低 Rust `1.95`，本接入固定构建工具链 `1.95.0`。
- 代码许可：MIT OR Apache-2.0。完整上游保留 `LICENSE-MIT`、`LICENSE-APACHE`、`NOTICE`、`ATTRIBUTION.md` 及各资产许可文件。
- 上游 ArtCraft 名称、字标和 `docs/brand/` 图形有独立品牌条款，不在代码开源许可内；SeeCut 主题补丁替换产品界面品牌。分发前仍需核对全部实际打包资产。

`upstream.json` 是可重建来源与补丁顺序的合同。完整上游源码位于忽略目录 `external/photocraft`，支持独立 Git checkout 或现有 Git worktree。它保留 desktop、CLI、engine、格式、自动化、文档、测试和其他上游内容，不把 headless MCP 当作当前 GUI Session 接入。

当前受控补丁为 `patches/seecut-theme.patch`，SHA-256：`c8706c00aa0dd2cdf4eee31f89d578db9b882f4b288a8388513a5910b1aa96c2`。补丁包含二进制 PNG，必须保留完整 Git binary patch。来源脚本核对严格 origin、40 位 HEAD、版本、最低 Rust、补丁哈希与路径；按清单顺序在临时目录重放，然后逐文件核对完整内容和可执行位。已存在的工作区只读核验，拒绝未登记修改、暂存、未知未跟踪或忽略文件、软链接和特殊文件。不会 reset、自动重新应用补丁或删除文件。工具链、缓存和 target 位于主工作区 `.toonflow/`，不放在依赖源码内。

## 字体与可选格式

官方桌面和 CLI 构建都启用 `heif`，本接入构建脚本同样显式启用 `photocraft/heif` 和 `photocraft-cli/heif`。HEIF/HEIC 是只读能力；若依赖或工具链失败，应保留失败证据，不静默关闭特性。AVIF 是 codecs 的另一个可选写入特性，上游常规 desktop/CLI 没有把它列为默认特性；不能把不启用 AVIF 误写为本接入删除上游常规功能。

Inter、JetBrains Mono 和许可随固定源码保留。官方发行另含 `craft-fonts` 的日文字体，它是可选构建输入，未指定时回退到系统 CJK 字体。脚本不自动下载字体；启用时要求独立干净 Git 来源、显式 40 位提交及 `CRAFT_FONTS_REQUIRED=1`，记录提交和字体 manifest 哈希。嵌入字体不等于已验证与官方发行完全一致，正式包还需带齐对应 OFL 文本并检查字体显示。

## 已执行的验证层级

2026-10-23 完成固定上游静态提取、20 项脚本防护回归，以及当前主题补丁的真实完整源码校验。回归包含真实合成子进程及孙进程的短超时清理、非零退出原错误保留、独占证据目录和 preflight 失败记录；不执行 Cargo。固定上游包含 2,009 个普通文件；重放主题补丁后为 2,011 个。当前 patched tree SHA-256 为 `398c2fca006b44f3c1a241c123d3fad882f472ae9f72a7c1c27ef28b6b39cdfa`；`Cargo.lock` SHA-256 为 `36a1de83fea16781dc7186d33d8064cb5dfa878a2a0847228737640b1e64e2d0`。

本基础交付没有执行 Cargo、启动 GUI、安装应用或发布。完整桌面/CLI 编译、实际命令转储、GUI 操作、文件保真、同 Session Agent 和正式分发分别需要后续证据；后续执行结果以相应构建 provenance 与验收记录为准。
