# SeeCut 新机恢复与后续接入资料

核对日期：2026-10-26。本目录保存可复用的设计和源代码候选，不注册模块、工具或运行时入口。

## 恢复入口

| 范围 | 仓库与分支 | 已核对提交 |
| --- | --- | --- |
| 当前应用代码 | `https://github.com/Stormycry-cryp/seecut.git` / `codex/seecut-photocraft-workspace` | `e80320568f965108e5b4f26c3a2a5e3df211bdee` |
| 私有 Agent | `https://github.com/Stormycry-cryp/seecut-agent.git` / `codex/agent-toonflow-r2` | `f5cf3b7a5436c4ea264094fa517f22d7a13483c2` |
| 独立只读统计 MCP 草案 | 公共仓库 / `codex/seecut-readonly-mcp-recovery-20261026` | 见该分支的 `services/seecut-server/MCP_MIGRATION_STATUS.md` |

本迁移分支基于上述应用提交，只增加本目录中的恢复资料。新机先克隆当前应用和私有 Agent；不要复制旧 `.git` worktree 指针、绝对路径软链接或 `node_modules`。

```sh
git clone --branch codex/seecut-photocraft-workspace https://github.com/Stormycry-cryp/seecut.git
git clone --branch codex/agent-toonflow-r2 https://github.com/Stormycry-cryp/seecut-agent.git
```

私有 Agent 的 `package.json` 固定 `@coto/agent` 到 `Stormycry-cryp/COTO#e768013d640bea50580c0ab3d0a32ef2fab54b22`。须在新机取得对应私有仓库权限并按锁定依赖重建，不能沿用旧机依赖软链接。配套 fixture CI 使用公共服务提交 `247334c2f37cde265b2424923fd3386e23dcc0f3`；完整验证方法以各仓库当前 CI 为准。

PhotoCraft 上游源码不需要整包迁移：已提交的 `integrations/photocraft/upstream.json` 固定上游提交和 8 个补丁。按 `integrations/photocraft/README.md` 重新准备源码与工具链。旧机独立编辑器构建和 target 缓存均可重建，本目录不保存二进制。

## 当前完成边界

- PhotoCraft 完整上游源码准备、独立编辑器、受控宿主与 Parent/Photo 目标基础代码已存在。
- Photo 的父级 MCP/Agent 实际派发、模型看图、任务读回与完整编辑闭环尚未接通；权限入口保持关闭。
- 主应用本地编译与严格 Clippy 的历史证据，以及独立编辑器隔离运行证据，均不能代替完整 macOS 应用交互验收。
- 当前提交的 [完整 CI](https://github.com/Stormycry-cryp/seecut/actions/runs/39930608140) 中，Engine 测试、wasm 通过，`Agent source inventory` 失败；原因是界面回调清单与实际 Slint 声明不一致。macOS 打包被跳过。
- Nix、iOS 和 PhotoCraft 轻量合同 CI 已通过；Android 暂不纳入工作范围。尚无该提交对应的统一 SeeCut macOS 安装包。
- [PR #24](https://github.com/Stormycry-cryp/seecut/pull/24)、[PR #21](https://github.com/Stormycry-cryp/seecut/pull/21)、[私有 Agent PR #3](https://github.com/Stormycry-cryp/seecut-agent/pull/3) 保持 Draft，未合并。

## 保存的候选

`SOURCE_MANIFEST.json` 固定复制来源、文件字节数和 SHA-256。以下源代码未加入 Cargo 模块或任何默认构建；它们保留已有测试，后续集成时仍须重新适配和验证。

| 文件 | 价值与已核对证据 | 使用边界 |
| --- | --- | --- |
| `candidates/editor_mcp_photo_state.rs` | 有界状态映射、opaque 文档/图层句柄；源文件 SHA 与 Root 的 7 项隔离测试及严格 Clippy 凭证一致 | 使用真实宿主/MCP rlib 和提取的 Scope 验证；成功 `Some(PhotoDocument)` 路径仅编译检查，未完成 Bridge/worker 联测 |
| `candidates/editor_mcp_photo_control.rs` | 冻结原写操作身份、取消与派发的 CAS；修复后 SHA 与 Root 的 11 项隔离测试及严格 Clippy 凭证一致 | 未接入完整 registry、UI 审批、原写回执或 native I/O，不代表可直接执行编辑 |
| `candidates/read_worker.rs` | 历史只读 worker 的撤销、截止时间、原 capture token 和 File 生命周期参考 | 已落后于当前 `photo_workspace/owned_metadata.rs` 的唯一 actor/client；不能直接应用旧 `photo_workspace.rs` 或创建第二个 Host。需定点适配，本次不声称对当前入口编译通过 |

`designs/` 是按原字节保留的历史接入设计，其中行号和阶段描述可能早于当前代码。当前接入决定以本索引、实际源码和现有计划为准。尤其：

1. saved-binding 的基本目标选择和 owned metadata 已进入当前代码，不能按旧报告重复实现。
2. 所有 Photo 读取和写入复用唯一宿主 worker；原生 u64 身份留在 Rust，JS 只得到 opaque 句柄。
3. 人类发送须绑定同一次原 capture，在真实外发前检查原来源和授权；过期时不能自动补拍并继续原输入。
4. 五秒有效期约束首次新图外发；已被真实 managed operation 受理的历史图须沿原请求回执区分，不能把历史图永久当作当前选区。
5. 原写操作必须沿原 WriteRegistry、审批、幂等与 receipt 完成；Unknown 只查询原操作，不重复派发。
6. Task update 当前没有图片合同，Photo checkpoint 和父窗口真实模型送图仍需独立实现及验收。

## 旧工作区的处理判断

- 公开仓库全部 28 个本地分支的现有提交，都能从现场核对过的远端分支或其历史恢复；本地分支名相同不代表 HEAD 相同，恢复以上表精确提交为准。
- 旧 `seecut-agent` 的 12 个 dirty 文件全部与已提交 `f950410e534e5bd76407f3bc15453d31700f62b5` 字节相同，该提交已包含在当前私有分支；不重复提交。
- 旧 `Seecut-editor-mcp-m1` 的 58 个变更文件已存在当前应用的祖先历史，包含已淘汰的原生 Flow 实现；不覆盖当前代码。
- 旧的 20 个 Slint 完整文件为与当前方案分化的界面试稿，本次不采纳或提交。它们留在旧机原路径；恢复当前产品无需这些试稿。
- 两份旧验收文档及 4 个合成 QA fixture 放在 `historical/`，仅供追溯。它们的行号、接口、问题状态和验收结论均不能直接沿用。

## 下一步与本机约束

先在新机恢复精确提交和依赖，再核对界面回调清单的真实业务覆盖，修复该项 CI。随后在原 Photo actor 上逐片接入已保存候选，按状态读取、原图观察、受控编辑和实际任务读回依次验收；不要直接应用整个旧工作区。

用户要求：暂不处理 Android；前端由真实 Astra 执行，medium 可用；业务实现按项目约定交给 Sol high。测试不得抢鼠标、键盘、焦点或音频。一次只运行一个重 Cargo 构建，周额度剩余达到 50% 停止线时收束并交接。发布、合并和部署需要相应授权。

登录状态、会话/任务数据库、portable 用户数据、私有素材和凭据没有写入本目录或 Git。需要保留时必须另行确定精确路径和迁移方式，不能因源码已同步就删除旧机数据。新机通过 GitHub 登录、服务端受控环境配置和应用重新登录取得权限；不在交接文件中填写密钥。
