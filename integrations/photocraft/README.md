# PhotoCraft 完整工作区依赖

先看 [SOURCE.md](SOURCE.md) 的来源、许可和验证边界；开发入口见 [workspace-entry.md](workspace-entry.md)，内部宿主见 [native-host.md](native-host.md)，实时文档控制见 [live-session-control.md](live-session-control.md)，采用帧读取见 [adopted-snapshot.md](adopted-snapshot.md)。主计划为 `docs/plans/2026-10-23-photocraft-workspace.md`。需要 Python 3.12+、Git，以及已显式安装的 Rust 1.95.0；脚本不自动安装工具链、不修改全局默认 Rust、不启动应用。

## 恢复与校验

```sh
python3 scripts/prepare-photocraft.py
python3 scripts/prepare-photocraft.py --verify-only
python3 scripts/photocraft-baseline.py --check
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts -p test_photocraft_integration.py -v
```

目标不存在时，prepare 显式从锁定仓库取得完整源码、检出提交并按序应用受控补丁。目标已存在时只读核验；主题修改与补丁不一致时停止，不覆盖本地修改。`--verify-only` 不下载。基线从固定 Git 对象提取，独立于正在编辑的主题文件；它列出工具、菜单、Shell 命令、面板、UI 控制方法、engine 注册模块、静态命令候选和 codec 能力表，并保留相关源码哈希。菜单目录包含禁用占位项，静态命令候选不等同编译后的完整注册表。

## 构建完整 desktop 和普通 CLI

以下命令须在主代理持有本任务 Cargo 构建锁时执行。两者在同一次 Cargo 构建中启用官方 `heif` 特性；缺失依赖会失败，不减少功能开关。

```sh
python3 scripts/build-photocraft.py \
  --toolchain-bin "$PWD/.toonflow/rustup/toolchains/1.95.0-aarch64-apple-darwin/bin" \
  --rustup-home "$PWD/.toonflow/rustup" \
  --cargo-home "$PWD/.toonflow/photocraft-cargo" \
  --target-dir "$PWD/.toonflow/photocraft-target" \
  --evidence-dir "$PWD/.toonflow/photocraft-build" \
  --jobs 1 --timeout 3600
```

默认 `--frozen` 禁止网络下载。第一次需要 Cargo 获取锁文件内的依赖时，显式加 `--allow-network`，保持 `--locked`；这只允许依赖下载，不安装 Rust 或字体。`--profile release` 生成 release 构建；`--target` 可指定已安装目标。构建输出、缓存与证据目录只允许位于主工作区 `.toonflow/` 子目录。

默认 `--jobs 1`，一次只运行一个重构建。每次构建必须指定尚不存在的 `--evidence-dir`；示例路径已存在时，下一次改用新路径，例如 `.toonflow/photocraft-build-attempt-2`。脚本独占创建目录并先写入 attemptId 和 building 状态，preflight 或构建失败都会写 failed；已有目录直接拒绝，不能复用其中的旧 built 凭证。缓存和 target 可以继续复用。

macOS/Linux 的实际构建运行于独立 session/process group；超时、中断或异常会终止本次整组并等待，仍有活动子进程时不会报告成功。父进程返回成功但遗留活动子进程，也会终止整组并报告失败。Windows 在启动任何构建前明确拒绝：该受控脚本尚未实现 Windows 进程树清理；完整上游 Windows 源码和功能仍然保留，后续另行实现及验收。

可选嵌入字体使用 `--craft-fonts /absolute/path/to/craft-fonts --craft-fonts-commit <40位提交>`；不指定时 provenance 明确记录系统字体回退。分发时需另行拷贝对应字体许可。构建脚本记录 `cargo-messages.jsonl`、`cargo-stderr.log`、`provenance.json`，从实际 compiler-artifact 事件取得两个二进制路径、字节数和 SHA-256。失败会留下 `status=failed`，不会沿用先前成功状态。成功只证明构建；不会执行二进制或声称 GUI 已验收。

## 后续运行时证据

主代理核对成功 provenance 后，可显式执行其中的 `photocraft-cli` 二进制，将 `commands --json` 输出保存到 `.toonflow/`。再用：

```sh
python3 scripts/photocraft-baseline.py --check \
  --runtime-commands .toonflow/photocraft-build/commands.json
```

此操作只比较已有转储并报告新增或缺失候选，不运行 CLI，也不改写静态基线的 `runtimeEvidence`。转储生成过程与二进制 provenance 必须共同保存。普通 CLI/MCP 使用 headless Session；当前 GUI 的 Agent 接入须走实时编辑器，另行验证文档与视图身份、权限、幂等、撤销恢复和真实交互。
