# M2 阶段 1：工程所有权基础

阶段 1 提供 `concat_host::ownership` 与可独立编译的 `concat-host/tests/ownership.rs`；阶段 2 已增加宿主创建、Session、模板与保存接口。本文的已执行证据仍是 std-only OS 锁定点测试，生产调用者接入、完整宿主编译及原生运行另记。MCP capabilities 仍只读，不能把 helper 测试称为产品已防双写。

## 契约

- `ResourceIdentity::for_project` 解析已有工程目录；`for_canvas` 解析已有 `.comp` 目录包的最终目标，或不存在目标的规范化父目录及最终文件名。依据实际 `load_comp/write_canvas_snapshot`，普通 `.comp` 文件没有本轮 legacy 加载证据，明确拒绝。constructor 会小创建 sidecar，不创建文档、不持 writer 锁；查询 notOpen 工程不应调用它。
- Clip 锁为工程目录的 `.seecut-writer.lock`；Canvas 锁为父目录的 `.seecut-canvas-locks/<完整文档文件名>`。锁与原子替换的 manifest/`.comp` 分离，正常不截断、不删除锁文件。
- identity 依据 root 与稳定 sidecar 的 dev/inode 等价，不以 lowercase 或原始字符串判断。打开时使用核对过的平台 O_NOFOLLOW/O_NONBLOCK，检查目录、实际 FD 与最终 entry 的身份；软链/异常类型/多硬链接 sidecar 拒绝。`.comp` 目录的 nlink 自然包含子目录，不按 regular-file 的 nlink=1 限制；其整包替换不改变父目录+稳定 sidecar 身份。身份变化 fail closed。
- `WriterGuard::acquire` 每次独立打开一个 FD、只做一次非阻塞 OS `try_lock`。同 target 的第二次 acquire 返回 Conflict；同一个逻辑 owner 的保存 worker 用 Arc clone 延续唯一 File。clone 不能用于建立另一份 Session。阶段 2 调用者在当前 Session 重开、同身份 SaveAs 时复用现有 owner。
- 错误区分 Conflict、InvalidTarget、UnsupportedAlias、IdentityAmbiguous、UnsupportedLock、Io。锁不支持或 I/O 错误不作为成功，不 silently fallback 无锁写。

## 已执行的直接风险验证

本机直接使用已有 Rust 1.93.1 编译 std-only helper/test，未进行 Cargo/App 大构建。helper 的 rustc 与 clippy-driver `-D warnings -D missing-docs` 通过。当前目录包契约及只读匹配增量为 16 passed / 0 failed / 1 ignored；ignored 项仅供真实子进程自调用。早期 14 项中的 canvas regular-file fixture 已按实际目录包修正，不能把旧目标类型当当前验收。

根代理已独立复跑相同 rustc/clippy gate、默认并行测试 runner、整体 rustfmt check 与 diff check；证据保存在 `/private/tmp/sc-m2-stage2-mjbnrul2/manifest.json`、`test-result.txt`，包括 helper/test 源文件 SHA256。该证据绑定当前已审目录包 helper 与测试；修改这两个文件后需更新相应定点证据。

覆盖 Clip/Canvas 同进程独立 FD、重复 identity、两进程争用和独立工程；相对路径/符号链接；现存及新目标的大小写和 Unicode NFC/NFD；静态异常 sidecar/硬链接；Clip manifest 替换及 Canvas 整目录包替换、替换的缺失目标区间仍拒第二 writer；保存 worker clone 最后释放；正常退出以及保持 guard 时直接 `process::exit(0)` 跳过 Rust Drop 后锁释放、sidecar inode 保留；首次创建竞争一胜一拒；NAME_MAX 文件名、权限、已有锁文件内容不截断、stale identity 拒绝；只读匹配不创建 sidecar，stale unrelated owner 不抢先掩盖真正匹配的目标。

Canvas fixture 是小目录包：有效 JSON 的 `manifest.json`、`images/` 与有效 1x1 RGBA `preview.png`。helper 只验证 OS 目录目标及锁生命周期，不验证完整文档 schema；这些证据不是实际 loader/save 的运行验收。

测试 fixture 使用运行时 `temp_dir`、PID 与原子序号、0700 新目录，遇已有目录换序号，清理仅限本测试创建的目录。代码没有编译期临时路径。互不相关测试用一个测试 Mutex 隔离 subprocess spawn，单项内部的两个真实进程仍通过 READY/go 握手并发。

## 现场文件系统结果与解释

macOS 当前测试卷大小写及 Unicode NFC/NFD 别名均指同 entry。Rust 的已有 Project/Canvas 别名 canonical target 字符串与 identity 都相等；未创建 Canvas 的别名 target 字符串可不同，sidecar identity 仍相同。此前 Python realpath 的字符串保留写法与 Rust 不同，不能据 Python 结果报现有 Rust API 大小写缺陷。

首轮未隔离的默认并行 suite 曾在最后 guard drop 后立即重取时出现一次 Conflict。单项/串行通过，后用可控 fork→exec 诊断证明确有以下 OS 生命周期：子进程已 fork 但未 exec 时，继承的 FD 使父进程最后 guard drop 后仍 Conflict；子进程完成 exec/exit 后可再取锁。这个机制可以产生首轮现象，但未实时追踪首轮进程，不能百分之百溯源。当前测试隔离后全部通过；生产没有 sleep/retry，也不抢占短暂 Conflict。

唯一 File 的关闭受所有继承 OS FD 已关闭的边界约束，CLOEXEC 在 exec 时关闭。原生调用者应保留旧工程并明确回报 Conflict。

## 平台与范围

运行证据只覆盖 macOS 当前卷。Apple macOS/iPhoneOS SDK 核对 NOFOLLOW=0x100、NONBLOCK=0x4；[Linux x86 通用 UAPI](https://github.com/torvalds/linux/blob/master/include/uapi/asm-generic/fcntl.h) 为 NOFOLLOW=1<<17、NONBLOCK=1<<11，[Linux arm64 UAPI](https://github.com/torvalds/linux/blob/master/arch/arm64/include/uapi/asm/fcntl.h) 覆盖 NOFOLLOW=1<<15。平台分支按 OS/arch 选择，未审平台明确返回不支持；Linux/iOS/Android 尚无本轮运行证据。

这是遵循同一契约的 writer 协调。旧 binary 不遵守新锁，同用户主动替换目录/sidecar 也不受 advisory 锁保护；未扩展成敌对同用户的文件系统隔离。宿主仅 macOS / Linux x86_64、aarch64 原生 Session 持 Some(owner)，Linux runtime 待 CI；Windows、移动与未审计架构以明确 cfg 返回 None(owner) 保留原 native 编辑行为，不宣称新防双写保证，也不开放新增 MCP 写能力。受支持 desktop 上缺失 owner、所有权 I/O、不支持 OS 锁或 stale identity 均明确失败，不静默退化为无锁写。

## 阶段 2 宿主契约

- `WriterGuard::matches_project/matches_canvas/validate` 只读取身份，不创建 sidecar、不独立 acquire。匹配先排除 unrelated candidate，再验证相关 owner；未知/缺失 unrelated target 返回 false，同原路径 root/sidecar 被替换明确 IdentityAmbiguous。已有 canvas 包及尚未创建包的 alias 仍按稳定 sidecar 判断。
- `projects::OwnedProjectError` 分 `Ownership(OwnershipError)` 与 `Project(String)`。`Session::open_owned/open_info_owned/save_owned` 保留 typed 错误，旧 `open/open_info/save` 映射 String 兼容。取得 owner 后才读文档；存在但读失败/JSON 损坏的 manifest 不再被当空工程准备覆盖。
- `projects::create_owned` 返回不可 Clone、字段封闭的 `CreatedProject`。先创建根目录、取得 owner，再重查 current/legacy manifest 的所有 entry 类型；首写 create_new+sync 不截断已有 entry。`CreatedProject::info` 只借用，`save` 持原 creator owner，`into_session(self)` 一次消费并 move 原 owner，不经 drop/reacquire。`into_info` 明确完成 artifact 后释放 creator；旧 create/instantiate wrappers 采用此语义，不用于交互交接。
- `templates::instantiate_owned` 在同 creator owner 下完成模板 assets/fill/final save，再把 CreatedProject 交 caller adoption。模板 bundle 的 `save` 也取目的 bundle owner、持锁重查 template.json、create_new 写入，不能依旧观察覆盖首份 bundle。
- `Session::writer_guard()` 仅供 save worker 捕获已有 Arc owner。无 public 从 cloned guard 建另一份 Session 的入口。`projects::save_owned(path,document,owner.as_ref())` 与 `SaveLane::write_owned(ticket,path,document,owner.as_ref())` 使用该 owner，不重新夺锁。worker 保留 clone 直到写完或失效。原 SaveLane 排序、writer Mutex、临时文件 sync 和 rename 规则保留，rename 前再次核 ownership。

宿主新增 late creator（current/legacy 均重查不覆写）、Creator→Session→worker 持续 owner、第二 Session 冲突/最后释放再重开、stale save 不改 manifest、坏 JSON 失败释放 owner等测试，并调整 SaveLane 及导出测试的 live writer 重开假设。含 serde/Session/媒体的 host 测试未在本机执行，留给远端 CI；std-only helper 的 16 项通过不能替代它们。

调用者接入与实际跨 native/API/Canvas 保存、关闭、另存、重开需要分别验收。写工具等待全部生产 writer 接入与相应证据完成后再开放。
