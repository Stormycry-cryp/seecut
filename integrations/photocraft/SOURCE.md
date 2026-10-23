# PhotoCraft 来源与重建边界

- 仓库：https://github.com/storytold/photocraft.git
- 固定提交：`ff4738810134d7f7e68b49f2e4f231b686282ff3`
- Workspace 版本：`0.14.0`；最低 Rust `1.95`，本接入固定构建工具链 `1.95.0`。
- 代码许可：MIT OR Apache-2.0。完整上游保留 `LICENSE-MIT`、`LICENSE-APACHE`、`NOTICE`、`ATTRIBUTION.md` 及各资产许可文件。
- 上游 ArtCraft 名称、字标和 `docs/brand/` 图形有独立品牌条款，不在代码开源许可内；SeeCut 主题补丁替换产品界面品牌。分发前仍需核对全部实际打包资产。

`upstream.json` 是可重建来源与补丁顺序的合同。完整上游源码位于忽略目录 `external/photocraft`，支持独立 Git checkout 或现有 Git worktree。它保留 desktop、CLI、engine、格式、自动化、文档、测试和其他上游内容，不把 headless MCP 当作当前 GUI Session 接入。

当前依次重放 `seecut-theme`、`seecut-theme-polish`、`seecut-agent-control`、`seecut-profile-guard`、`seecut-adopted-snapshot`、`seecut-backing-binding`、`seecut-managed-close` 和 `seecut-capture-validation` 八项补丁，各自完整路径和 SHA-256 以 `upstream.json` 为准。主题补丁包含二进制 PNG，必须保留完整 Git binary patch。后六项分别连接实时 Session 私有控制、子编辑器自身持有的恢复目录锁、实际绘制采用帧读取、保存关联换代、保留未保存提示的封闭关闭及原快照的帧末复核。来源脚本核对严格 origin、40 位 HEAD、版本、最低 Rust、补丁哈希与路径；按清单顺序在临时目录重放，然后逐文件核对完整内容和可执行位。已存在的工作区只读核验，拒绝未登记修改、暂存、未知未跟踪或忽略文件、软链接和特殊文件。不会 reset、自动重新应用补丁或删除文件。工具链、缓存和 target 位于主工作区 `.toonflow/`，不放在依赖源码内。

## 字体与可选格式

官方桌面和 CLI 构建都启用 `heif`，本接入构建脚本同样显式启用 `photocraft/heif` 和 `photocraft-cli/heif`。HEIF/HEIC 是只读能力；若依赖或工具链失败，应保留失败证据，不静默关闭特性。AVIF 是 codecs 的另一个可选写入特性，上游常规 desktop/CLI 没有把它列为默认特性；不能把不启用 AVIF 误写为本接入删除上游常规功能。

Inter、JetBrains Mono 和许可随固定源码保留。官方发行另含 `craft-fonts` 的日文字体，它是可选构建输入，未指定时回退到系统 CJK 字体。脚本不自动下载字体；启用时要求独立干净 Git 来源、显式 40 位提交及 `CRAFT_FONTS_REQUIRED=1`，记录提交和字体 manifest 哈希。嵌入字体不等于已验证与官方发行完全一致，正式包还需带齐对应 OFL 文本并检查字体显示。

## 已执行的验证层级

2026-10-23 完成固定上游静态提取、20 项脚本防护回归，以及当前主题补丁的真实完整源码校验。回归包含真实合成子进程及孙进程的短超时清理、非零退出原错误保留、独占证据目录和 preflight 失败记录；不执行 Cargo。固定上游包含 2,009 个普通文件；首轮只重放主题补丁后为 2,011 个。首轮 patched tree SHA-256 为 `398c2fca006b44f3c1a241c123d3fad882f472ae9f72a7c1c27ef28b6b39cdfa`；`Cargo.lock` SHA-256 为 `36a1de83fea16781dc7186d33d8064cb5dfa878a2a0847228737640b1e64e2d0`。

随后主代理使用任务内 Rust 1.95.0 完整编译 desktop 与普通 CLI，启用两者官方 HEIF 特性，单任务构建通过。实际执行本次 CLI 的 `commands --json`，得到 935 个编译命令，静态候选无缺失；命令注册不等同逐项功能验收。desktop 为 94,366,288 字节，SHA-256 `84ba4746922c2b45f9c2d5e74583696b873d593bea252acca3c7c0ce55c2788c`；CLI 为 62,460,288 字节，SHA-256 `377c06e481a94e0148390159a577e24ad9ffbb006f20185da6653c18b0cf502a`。完整本机凭证位于忽略目录 `.toonflow/photocraft-build-01/`，不随源码提交。

未启动前台 GUI、安装应用或发布。首轮离屏例程实际构建并检查了 8 张图片；后续发现只设置 OS 外观未切换编辑器主题，旧 light 标签不能作为浅色验收。当前四补丁版本已重建离屏例程，通过真实 `ui.set` 显式设置 `proMedium` / `proLight`，重新渲染并逐张检查 10 张图片：深浅色的常规/窄窗首页、关于对话框、800×500 多图层文档及其窄窗。中文与布局正常，社区链接保持次要层级，没有抢占本机焦点。GUI 操作、文件保真、SeeCut Agent 上层接线和正式分发分别需要后续证据。源码树或特性改变后须重新构建，不能沿用上述二进制的成功记录。

历史四项补丁重放后为 2,014 个文件，源码树 SHA-256 `d93a309a979058e1a9449b97ee666a32988b79f1cee38631646989397a7f10bd`，Cargo.lock 仍为上述摘要。新增控制合同见 [live-session-control.md](live-session-control.md)，启动与配置目录锁见 [native-host.md](native-host.md)。当前源码已经改变，首轮构建收据只能作为历史证据，原生宿主会拒绝用它启动新版本。随后本机完整重建这四项补丁的 desktop/CLI，官方 HEIF 特性保持启用，耗时 6 分 57 秒；主代理再次核对实际二进制摘要并执行 CLI，仍有 935 个编译命令且无静态候选缺失。四补丁 desktop SHA-256 `4939c2e245a82f3d578a19dfbc63c99a07934129b3ebfb95bce1242055049616`，CLI SHA-256 `f51fff613c4543bde35eae1a2d88e06fe87495e811819e831083acba2c8502dc`，独立凭证位于 `.toonflow/photocraft-build-02/`。四补丁离屏视觉已按上述 10 张显式主题图片复验。这些二进制只匹配四补丁来源。

历史五补丁版本已完成验证，重放后共 2,019 个文件，源码树 SHA-256 `4bcec483a7490740d0a52d27d46542eac212e3c8895c4b9033ec1b42e8c812ac`，Cargo.lock SHA-256 `9ce360951da8005e23e81fd5dd3e7b2dd2b95c7f5056991ca556ed4fa239593e`。原生宿主来源锁同步更新，拒绝旧的四补丁收据。主代理实际运行五补丁 library 回归：引擎 1 项、UI 38 项通过，真实控制套接字的 2 项期限回归通过；所有执行用例 0 失败、0 忽略。快照协议及支持范围见 [adopted-snapshot.md](adopted-snapshot.md)。该五补丁 desktop/CLI 已在本机完整重建，官方 HEIF 保持启用；实际 desktop 为 94,576,448 字节，SHA-256 `a66cf1d8d031f67c172e0241c158c8a4f188aa3063a4c0213998017cebc27693`，CLI SHA-256 仍为 `f51fff613c4543bde35eae1a2d88e06fe87495e811819e831083acba2c8502dc`。主代理独立重验源码与两个实际二进制，并在新私有目录执行 CLI：935 个注册命令、静态候选无缺失，凭证为 `.toonflow/photocraft-build-03/`。隔离 Linux 联测另行记录。

历史七补丁来源共 2,021 个文件，tree `60f3fb47e318d1fc216cf150a83b025b908fb6526e9f9297be1e650bb8711909`，Cargo.lock `f1d8cb5645735284d089b955c368019de783ee7d0c9eeed3ffe0670b7288fafd`。新增保存关联换代和封闭关闭；UI 真实授权回归仅增加已锁定的 automation 测试依赖，没有放宽文件访问策略。主代理在该精确来源实际执行引擎 2 项、UI 47 项、独立打开/解绑 3 项及真实套接字 2 项回归，均通过、0 忽略。包含真实 automation 授权下的退出提示与取消，GPU 采用帧用例显式执行。

该七补丁版本的 desktop/CLI 已在本机单任务完整重建，两者均启用官方 HEIF，耗时 2 分 18 秒。desktop 为 94,542,896 字节、SHA-256 `3fcafdddb820215888cd1259e9ec10df4c3e798ca8de0fcec7a8dda9cb199e86`；CLI 为 62,460,512 字节、SHA-256 `efc2a414c5d761fb0ca15784e5cc0ed04a479543e8bd34fb610c602b19d91dac`。主代理独立重验完整来源和两个实际二进制，再在新私有目录执行 CLI，得到 935 个编译命令、静态候选无缺失；凭证位于 `.toonflow/photocraft-build-04/`。未启动本机前台编辑器；完整 Linux 进程联测另验。

当前八补丁来源共 2,024 个文件，tree `254fbf10b1fb6b00955f038bf36c822562873cae618daafa68018f2c38d2cb3c`，Cargo.lock 保持上述七补丁摘要。引擎 2 项、UI 68 项及真实控制套接字 3 项回归通过，0 失败、0 忽略；包含真实 CPU/GPU 主画布的捕获、下一次实际绘制及帧末确认、平滑滚动拒绝、稳定选择组合和记录失效后不恢复。套接字新增验证请求的跨队列 5 秒期限。

该八补丁 desktop/CLI 已保留官方 HEIF 完整重建，耗时 2 分 5 秒。desktop 为 94,608,480 字节、SHA-256 `44017892f6b46aa00f1772de996f0a4a37379f1bfa059c96c2a29a81e2c319f4`；CLI 与七补丁相同。主代理独立重验完整来源和两个实际二进制、再次执行 935 命令核对，并用真实原生宿主 `VerifiedBuild::from_provenance` 校验本次收据与产物，全部通过，凭证为 `.toonflow/photocraft-build-05/`。旧七补丁收据不能启动当前八补丁宿主。此后精确 SeeCut 提交 `6d8dd80504bc6144c089ea420555d89b74567741` 的[隔离 Linux 完整编辑器联测](https://github.com/Stormycry-cryp/seecut/actions/runs/39918618869)已成功，真实回包通过封闭 snapshot/validate/execute/receipt。主代理下载 artifact `12487578202`，重新解码四张 16×16 PNG，共核对 1024 像素；透明度 0.4 为 alpha 102，撤销图与原图摘要一致。同一原快照从真实 capture 帧 87 验证到 postInputFrame 89、年龄 44ms，非整数几何及 u64MAX 视口均通过。此为 CPU 文档合成与帧末检查，未证明物理 present、父级 MCP/Task 或模型看图。
