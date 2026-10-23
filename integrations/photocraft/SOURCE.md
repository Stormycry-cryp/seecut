# PhotoCraft 来源与重建边界

- 仓库：https://github.com/storytold/photocraft.git
- 固定提交：`ff4738810134d7f7e68b49f2e4f231b686282ff3`
- Workspace 版本：`0.14.0`；最低 Rust `1.95`，本接入固定构建工具链 `1.95.0`。
- 代码许可：MIT OR Apache-2.0。完整上游保留 `LICENSE-MIT`、`LICENSE-APACHE`、`NOTICE`、`ATTRIBUTION.md` 及各资产许可文件。
- 上游 ArtCraft 名称、字标和 `docs/brand/` 图形有独立品牌条款，不在代码开源许可内；SeeCut 主题补丁替换产品界面品牌。分发前仍需核对全部实际打包资产。

`upstream.json` 是可重建来源与补丁顺序的合同。完整上游源码位于忽略目录 `external/photocraft`，支持独立 Git checkout 或现有 Git worktree。它保留 desktop、CLI、engine、格式、自动化、文档、测试和其他上游内容，不把 headless MCP 当作当前 GUI Session 接入。

当前依次重放 `seecut-theme`、`seecut-theme-polish`、`seecut-agent-control` 和 `seecut-profile-guard` 四项补丁，各自完整路径和 SHA-256 以 `upstream.json` 为准。主题补丁包含二进制 PNG，必须保留完整 Git binary patch。后两项分别连接实时 Session 私有控制与子编辑器自身持有的恢复目录锁。来源脚本核对严格 origin、40 位 HEAD、版本、最低 Rust、补丁哈希与路径；按清单顺序在临时目录重放，然后逐文件核对完整内容和可执行位。已存在的工作区只读核验，拒绝未登记修改、暂存、未知未跟踪或忽略文件、软链接和特殊文件。不会 reset、自动重新应用补丁或删除文件。工具链、缓存和 target 位于主工作区 `.toonflow/`，不放在依赖源码内。

## 字体与可选格式

官方桌面和 CLI 构建都启用 `heif`，本接入构建脚本同样显式启用 `photocraft/heif` 和 `photocraft-cli/heif`。HEIF/HEIC 是只读能力；若依赖或工具链失败，应保留失败证据，不静默关闭特性。AVIF 是 codecs 的另一个可选写入特性，上游常规 desktop/CLI 没有把它列为默认特性；不能把不启用 AVIF 误写为本接入删除上游常规功能。

Inter、JetBrains Mono 和许可随固定源码保留。官方发行另含 `craft-fonts` 的日文字体，它是可选构建输入，未指定时回退到系统 CJK 字体。脚本不自动下载字体；启用时要求独立干净 Git 来源、显式 40 位提交及 `CRAFT_FONTS_REQUIRED=1`，记录提交和字体 manifest 哈希。嵌入字体不等于已验证与官方发行完全一致，正式包还需带齐对应 OFL 文本并检查字体显示。

## 已执行的验证层级

2026-10-23 完成固定上游静态提取、20 项脚本防护回归，以及当前主题补丁的真实完整源码校验。回归包含真实合成子进程及孙进程的短超时清理、非零退出原错误保留、独占证据目录和 preflight 失败记录；不执行 Cargo。固定上游包含 2,009 个普通文件；首轮只重放主题补丁后为 2,011 个。首轮 patched tree SHA-256 为 `398c2fca006b44f3c1a241c123d3fad882f472ae9f72a7c1c27ef28b6b39cdfa`；`Cargo.lock` SHA-256 为 `36a1de83fea16781dc7186d33d8064cb5dfa878a2a0847228737640b1e64e2d0`。

随后主代理使用任务内 Rust 1.95.0 完整编译 desktop 与普通 CLI，启用两者官方 HEIF 特性，单任务构建通过。实际执行本次 CLI 的 `commands --json`，得到 935 个编译命令，静态候选无缺失；命令注册不等同逐项功能验收。desktop 为 94,366,288 字节，SHA-256 `84ba4746922c2b45f9c2d5e74583696b873d593bea252acca3c7c0ce55c2788c`；CLI 为 62,460,288 字节，SHA-256 `377c06e481a94e0148390159a577e24ad9ffbb006f20185da6653c18b0cf502a`。完整本机凭证位于忽略目录 `.toonflow/photocraft-build-01/`，不随源码提交。

未启动前台 GUI、安装应用或发布。首轮离屏例程实际构建并检查了 8 张图片；后续发现只设置 OS 外观未切换编辑器主题，旧 light 标签不能作为浅色验收。当前四补丁版本已重建离屏例程，通过真实 `ui.set` 显式设置 `proMedium` / `proLight`，重新渲染并逐张检查 10 张图片：深浅色的常规/窄窗首页、关于对话框、800×500 多图层文档及其窄窗。中文与布局正常，社区链接保持次要层级，没有抢占本机焦点。GUI 操作、文件保真、SeeCut Agent 上层接线和正式分发分别需要后续证据。源码树或特性改变后须重新构建，不能沿用上述二进制的成功记录。

当前四项补丁重放后为 2,014 个文件，源码树 SHA-256 `d93a309a979058e1a9449b97ee666a32988b79f1cee38631646989397a7f10bd`，Cargo.lock 仍为上述摘要。新增控制合同见 [live-session-control.md](live-session-control.md)，启动与配置目录锁见 [native-host.md](native-host.md)。当前源码已经改变，首轮构建收据只能作为历史证据，原生宿主会拒绝用它启动新版本。随后本机完整重建这四项补丁的 desktop/CLI，官方 HEIF 特性保持启用，耗时 6 分 57 秒；主代理再次核对实际二进制摘要并执行 CLI，仍有 935 个编译命令且无静态候选缺失。当前 desktop SHA-256 `4939c2e245a82f3d578a19dfbc63c99a07934129b3ebfb95bce1242055049616`，CLI SHA-256 `f51fff613c4543bde35eae1a2d88e06fe87495e811819e831083acba2c8502dc`，独立凭证位于 `.toonflow/photocraft-build-02/`。最终四补丁离屏视觉已按上述 10 张显式主题图片复验；后续快照候选暂未纳入来源。
