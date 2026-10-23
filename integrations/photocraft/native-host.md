# SeeCut 内部原生宿主

`src/crates/seecut-photo-host` 管理固定来源的完整 PhotoCraft desktop。它不复刻前端，不启动 headless CLI Session，不创建 Agent 会话或收费账本，也不注册面向模型的工具。既有 SeeCut MCP / Task 的权限、目录范围、预算和任务身份仍由原上层负责；调用内部 transport 前必须通过这些门槛。

## 已核实的上游契约

固定提交 `ff4738810134d7f7e68b49f2e4f231b686282ff3` 没有 `apps/photocraft/src/control_wire.rs`。实际依据为：

- `apps/photocraft/src/main.rs`：`--control 0` 启动独立编辑器；`--control-token-file` 从文件读取凭据；`PHOTOCRAFT_CONFIG_DIR` 覆盖默认配置与恢复目录。启动时不传文档路径。
- `apps/photocraft/src/control_server.rs`：只绑定 `127.0.0.1`，标准错误就绪行是 `photocraft: control server listening on 127.0.0.1:<port>`。宿主仅从自己持有的 ChildStderr 读取，不扫描日志文件、不猜端口、不接管其他窗口。
- `crates/automation/src/security.rs`：每次 TCP 连接的首帧是 `{"id":1,"method":"auth","params":{"token":"…"}}`，成功回复包含同一 ID、`ok:true` 和 `result:{authenticated:true}`。请求上限含换行为 1 MiB。
- `crates/automation/src/{bridge,budgets}.rs`：JSON Lines 回复必须按 ID 配对，回复上限含换行为 8 MiB。上游普通桥在失败后可重试只读；本宿主不自动重试任何请求。
- `crates/ui-egui/src/control.rs`：`ui.inspect` 返回当前窗口、Session、文档和对话框原始状态；`ui.screenshot` 的 `focus` 默认 true，宿主强制 false。截图可能因窗口遮挡而超时，不能通过抢焦点来解决。
- `crates/automation/src/workspace.rs`：读写目录是启动时可信配置；缺失的目录权限会拒绝对应自动化文件访问。用户在完整编辑器内手动操作的文件对话框仍属上游原有 UI。

`app.quit` 在当前源码直接设置 `allow_close=true`，会绕过未保存拦截，因此普通内部调用禁止此原语。正常关闭发送 `ui.menu.invoke`，参数为 `{"command":"file.exit","params":{}}`。`menus::invoke` 先经过 `discard_ui::intercept`；有未保存文档时列出原生 discard 对话框，宿主不代答 Save、Don't Save 或 Cancel。`ui.focus` 也禁止调用。

## 最小 API

所有方法是同步阻塞方法。SeeCut 入口应使用一个持有唯一 `NativeHost` 的专用后台工作线程，主界面只消费状态。

```rust
let mut host = NativeHost::new();
let options = LaunchOptions::new(provenance_path, state_dir);
let info = host.start(options)?;
let raw_state = host.inspect(info.id)?;
let outcome = host.close(info.id, Duration::from_millis(500))?;
let current = host.current()?;
```

- `LaunchOptions` 包含 `provenance_path`、`state_dir`、可选 `read_root` / `write_root`、`startup_timeout` 和 `request_timeout`。目录必须由可信应用配置提供。状态目录须为绝对路径，私有权限为 0700；默认恢复数据位于其 `photocraft-profile` 子目录，不能作为临时缓存清理。
- `InstanceInfo` 包含新生成的 `Uuid`、自有 Child PID、构建 attempt ID、`ready` 和 `InstanceStatus`。`current(&mut self)` 实际 `try_wait` 并回收退出状态；退出后仍返回 `Some(status=Exited)` 便于展示，随后可 `start` 新实例。请求必须携带该实例 ID；新实例不复用旧连接或旧身份。
- `start` 仅支持 Unix。先校验完整 build provenance 的固定源提交、补丁、源树、Cargo.lock、文件数和完整 desktop / CLI heif 特性，再逐个核验两个二进制字节数和 SHA-256。启动前再次核验 desktop。provenance 是可信本机构建收据，不能当作远程发布签名或抵抗有权改写收据与二进制的攻击者。
- 只解析自己 ChildStderr 中有界就绪行。单行上限 8 KiB，就绪前总量上限 1 MiB；后续标准错误以有界块持续丢弃，不转发日志。仅接受精确 `127.0.0.1` 非零端口；认证和实际 `ui.inspect` 验证通过才进入 Ready。
- token 从系统随机源生成 32 字节，存于实例私有 0700 目录下的 0600 新文件；argv 仅传文件路径，移除继承的控制 token / 自动化目录环境变量。token 不出现在返回类型、调试输出或原始日志中。返回 JSON 的同值字符串会被替换。文件在退出或宿主释放时移除；活编辑器不会因为 Drop 被终止。
- `call_internal_after_gate(id, method, params)` 只是内部发送原语，名称不代表已完成权限审核。上层必须先完成既有权限/预算核验，不可把方法名和任意文件路径直接交给模型。它默认按写请求处理，不自动重发；`inspect` 是已知只读方法。
- 发出写操作后发生超时、断连、回复 ID 错误、无效回复或超限，返回 `HostError::Unknown`。上游协议没有执行前拒绝标记或事务回滚保证，因此写调用的所有 `ok:false` 同样返回 Unknown，包括产生部分副作用后的普通错误。此状态要求后续重新读取实时状态或由用户确认，不能重发原操作。普通调用失败只废弃连接，不杀编辑器。
- `close` 观察真实 Child 退出才返回 `CloseOutcome::Exited`；等待到期仍存活则返回 `NeedsUser { inspection }`。dirty 对话框由完整编辑器保留，宿主不自动丢弃。已关闭的进程、旧身份或尚未就绪的实例不会继续发送普通调用。
- 启动在 spawn 后失败时，`current` 保留 PID 和 StartupFailed 状态，禁止再次启动同一宿主。恢复目录可能已加载用户文档，因此失败不自动杀进程。调用者可让用户手动关闭，或在得到覆盖该实例和未保存内容损失的明确确认后调用 `terminate_confirmed`。它仅终止仍存活的自有 Child 的独立进程组，不接受外部 PID。

## 验证边界

当前已写合成小进程与真实 loopback socket 测试，覆盖来源/补丁/二进制拒绝、token 权限、错误认证/ID、请求和回复超限、绝对超时、断连、写结果 Unknown 且零重发、就绪来源、关闭回执、dirty 保留、Drop 不杀进程和新实例使旧身份失效。测试目录统一在主工作区忽略路径 `.toonflow/photocraft-native-host`。

主代理使用 SeeCut 原 Rust 1.93.0 工具链，实际执行 `cargo test --manifest-path src/Cargo.toml -p seecut-photo-host -- --test-threads=1`：9 项通过、0 失败、0 忽略；随后含测试的 `cargo clippy -p seecut-photo-host --all-targets -- -D warnings` 通过。测试使用合成小进程及真实 socket，只验证宿主合同，不证明真实 PhotoCraft GUI、文件保真、输入法、Agent 同 Session、原子截图或正式分发已验收。

上游 inspect 的文档索引不能当作 SeeCut 持久文档身份；本 crate 保留原始值供后续适配，不虚构稳定 ID、原子快照、幂等键或权限系统。
