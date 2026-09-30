# M2 阶段 1：工程所有权基础

阶段 1 提供 `concat_host::ownership` 与可独立编译的 `concat-host/tests/ownership.rs`。尚未接入生产 Session、Studio、API 或 Canvas，MCP capabilities 仍只读，不能把 helper 测试称为产品已防双写。

## 契约

- `ResourceIdentity::for_project` 解析已有工程目录；`for_canvas` 解析已有 `.comp` 的最终目标，或不存在目标的规范化父目录及最终文件名。constructor 会小创建 sidecar，不创建文档、不持 writer 锁；查询 notOpen 工程不应调用它。
- Clip 锁为工程目录的 `.seecut-writer.lock`；Canvas 锁为父目录的 `.seecut-canvas-locks/<完整文档文件名>`。锁与原子替换的 manifest/`.comp` 分离，正常不截断、不删除锁文件。
- identity 依据 root 与稳定 sidecar 的 dev/inode 等价，不以 lowercase 或原始字符串判断。打开时使用核对过的平台 O_NOFOLLOW/O_NONBLOCK，检查目录、实际 FD 与最终 entry 的身份；软链/异常类型/多硬链接 sidecar 及硬链接文档拒绝，身份变化 fail closed。
- `WriterGuard::acquire` 每次独立打开一个 FD、只做一次非阻塞 OS `try_lock`。同 target 的第二次 acquire 返回 Conflict；同一个逻辑 owner 的保存 worker 用 Arc clone 延续唯一 File。clone 不能用于建立另一份 Session。当前 Session 重开、同身份 SaveAs 在下一阶段复用现有 owner。
- 错误区分 Conflict、InvalidTarget、UnsupportedAlias、IdentityAmbiguous、UnsupportedLock、Io。锁不支持或 I/O 错误不作为成功，不 silently fallback 无锁写。

## 已执行的直接风险验证

本机直接使用已有 Rust 1.93.1 编译 std-only helper/test，未进行 Cargo/App 大构建。helper 的 rustc 与 clippy-driver `-D warnings -D missing-docs` 通过。定点测试 14 passed / 0 failed / 1 ignored；ignored 项仅供真实子进程自调用。

覆盖 Clip/Canvas 同进程独立 FD、重复 identity、两进程争用和独立工程；相对路径/符号链接；现存及新目标的大小写和 Unicode NFC/NFD；静态异常 sidecar/硬链接；manifest/文档原子替换仍持锁；保存 worker clone 最后释放；正常退出以及保持 guard 时直接 `process::exit(0)` 跳过 Rust Drop 后锁释放、sidecar inode 保留；首次创建竞争一胜一拒；NAME_MAX 文件名、权限、已有锁文件内容不截断、stale identity 拒绝。

测试 fixture 使用运行时 `temp_dir`、PID 与原子序号、0700 新目录，遇已有目录换序号，清理仅限本测试创建的目录。代码没有编译期临时路径。互不相关测试用一个测试 Mutex 隔离 subprocess spawn，单项内部的两个真实进程仍通过 READY/go 握手并发。

## 现场文件系统结果与解释

macOS 当前测试卷大小写及 Unicode NFC/NFD 别名均指同 entry。Rust 的已有 Project/Canvas 别名 canonical target 字符串与 identity 都相等；未创建 Canvas 的别名 target 字符串可不同，sidecar identity 仍相同。此前 Python realpath 的字符串保留写法与 Rust 不同，不能据 Python 结果报现有 Rust API 大小写缺陷。

首轮未隔离的默认并行 suite 曾在最后 guard drop 后立即重取时出现一次 Conflict。单项/串行通过，后用可控 fork→exec 诊断证明确有以下 OS 生命周期：子进程已 fork 但未 exec 时，继承的 FD 使父进程最后 guard drop 后仍 Conflict；子进程完成 exec/exit 后可再取锁。这个机制可以产生首轮现象，但未实时追踪首轮进程，不能百分之百溯源。当前测试隔离后全部通过；生产没有 sleep/retry，也不抢占短暂 Conflict。

唯一 File 的关闭受所有继承 OS FD 已关闭的边界约束，CLOEXEC 在 exec 时关闭。原生调用者应保留旧工程并明确回报 Conflict。

## 平台与范围

运行证据只覆盖 macOS 当前卷。Apple macOS/iPhoneOS SDK 核对 NOFOLLOW=0x100、NONBLOCK=0x4；[Linux x86 通用 UAPI](https://github.com/torvalds/linux/blob/master/include/uapi/asm-generic/fcntl.h) 为 NOFOLLOW=1<<17、NONBLOCK=1<<11，[Linux arm64 UAPI](https://github.com/torvalds/linux/blob/master/arch/arm64/include/uapi/asm/fcntl.h) 覆盖 NOFOLLOW=1<<15。平台分支按 OS/arch 选择，未审平台明确返回不支持；Linux/iOS/Android 尚无本轮运行证据。

这是遵循同一契约的 writer 协调。旧 binary 不遵守新锁，同用户主动替换目录/sidecar 也不受 advisory 锁保护；未扩展成敌对同用户的文件系统隔离。phase 2 在已接受的平台接入全部 writer，其他平台保留现有 native 编辑，新增 MCP 写能力不开放并标明没有新防双写保证。

## 下一阶段

先增加面向已持 owner 的纯只读身份匹配，API 查询不创建 sidecar；然后将创建首写、模板填充与 Session/adopt 的连续 owner 交接、Session、Studio SaveLane 和 Canvas 同步/异步/agent 保存串行接入。初写在持锁后重查 current/legacy manifest，防止晚取得锁的创建者依据旧检查覆盖首份工程。写工具等待所有生产 writer 接入验收后再开放。
