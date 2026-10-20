# 全功能 Agent 实施与验证记录

当前范围：macOS 桌面端；Android 按用户最新要求暂缓，下面移动端信息仅记录此前已经完成的步骤，不作为本轮验收门槛。

## 第一批：真实视图与覆盖清单

公共视图源码提交 `9a5fc86`，覆盖清单及实施计划提交 `d0f2026`，私有运行时提交 `a3536f0e0b57242f7fc0513bcb204d920929edd5`。在原功能分支推进，没有合并或发布。

- 原生测试二进制后台编译成功。`native_view_context` 新增 5 项通过；同一二进制 `editor_mcp::` 全部 62 项与 `agent_controller::` 全部 63 项通过，共 125 项（5 项已经包含在 62 项中）。前者覆盖坐标映射、未测量/移出视口、选择遮罩及指纹，后者复核原会话、权限、旧操作与恢复行为。
- 原生 context 提供真实画布视图和有限剪辑视图；预览仍为最大边长 256 的整文档缩略图。未得到的时间线终点、预览缩放及渲染帧时间明确不可用。
- 私有运行时本地定向 159 项通过，公共合成 COTO 确认链 2 项通过。随后[私有完整 CI](https://github.com/Stormycry-cryp/seecut-agent/actions/runs/39408032719)的 Node 22.19.0 与 Node 24.x 均成功；Node 22 日志为 189 项、0 失败。该 CI 使用合成供应商及锁定依赖，不能作为实际模型作品验收。
- `python3 scripts/check-agent-coverage.py` 校验源码声明、MCP 对应和 UI 入口快照，实施起点另盘点 161 条业务路由。分母尚未全部确认，不报告全应用完成率。
- Android 临时 swap 与诊断脚本的 10 项命令替身测试通过，本机没有创建 swap。[公共 CI](https://github.com/Stormycry-cryp/seecut/actions/runs/39408659394)与[移动端构建](https://github.com/Stormycry-cryp/seecut/actions/runs/39408659377)由该批推送触发；初始检查中清单任务与 wasm 已通过，主引擎及手机编译仍在运行。结果以确切提交的最终检查为准。

复现原生测试需在 `src/` 使用仓库固定工具链与缓存：

```sh
CARGO_TARGET_DIR="$PWD/../.toonflow/native-target" cargo test -p concat --lib --offline native_view_context
CARGO_TARGET_DIR="$PWD/../.toonflow/native-target" cargo test -p concat --lib --offline editor_mcp::
CARGO_TARGET_DIR="$PWD/../.toonflow/native-target" cargo test -p concat --lib --offline agent_controller::
```

本批未启动前台 App，没有系统鼠标、键盘或焦点操作。原生实际交互、完整 Agent 生成入库、图片/视频/音频质量、全功能逐项覆盖、长任务恢复和正式分发仍分别待验。已有逻辑和合成证据不替代这些验收。

## 第二批：显式画布编辑

新增 `canvas_edit`，采用稳定图层ID，覆盖27类显式图层、选区、栅格蒙版、笔刷/擦除、撤销重做、调整和RGB曲线操作。复用现有App编辑路径和历史、原操作ID、精确许可、写权限与版本检查；扩展 `project.canvasState` 独立回读像素内容摘要及实际工具状态。

本批修复了变换后多点笔触映射、笔刷实际计算量、完整回执大小、Undo/Redo恢复边界和运行时日志容量。编辑提交前再次检查实际 writer 所有权和原授权。笔触、目标像素、参数、子树、完整回执均有提交前预算，超限不先修改文档再返回错误。

`editStateId` 覆盖真实文档像素、完整像素选区、笔刷、绘制模式、图层展开和历史状态。运行时在审批前读取并冻结 `expectedEditStateId`，通过可信审批投影交给原生卡片，原模型参数保持不变。状态变化时拒绝执行，不刷新印记重做。Undo/Redo按真实历史恢复文档版本，不能假设版本号总是增加；原来源版本、内容摘要、操作身份和独立回读继续严格匹配。

当前候选的实际验证：

- 原生 `canvas_edit` 定向13项通过，已包含在 `editor_mcp::` 的75项内；同一最新二进制 `agent_controller::` 63项及画布Agent辅助2项通过，共140项。验证实际像素、历史、蒙版、调整、坐标、回执重试及提交前零执行边界，没有启动前台App。
- MCP协议与stdio合计28项通过；笔刷工作量新测试1项通过；公共模型服务工具合同4项通过。
- 私有运行时最终185项通过，其中画布编辑25项。包含完整COTO审批流程、审批持久投影、真实历史版本序列 `[1,0,1]`、低版本Undo的UNKNOWN原回执恢复和来源摘要防伪。供应商和MCP为本地替身，不能当作真实模型或原生App端到端证据。
- 覆盖清单校验通过：42个原生命令、45个画布辅助接口、26个MCP注册、161条业务路由；这些数量不表示全应用完成率。
- 最新公共宿主 `cargo clippy -p concat --lib --offline -- -D warnings` 通过。协议/stdio严格Clippy也单独核对。

公共模型服务工具合同已在候选源码纳入 `canvas_edit`，工具词汇与MCP注册保持一致；该服务变更尚未部署。完整真实模型与原生App链、成品质量仍未验收。当前仍需逐项确认画布文字、矢量、文件型调整等扩展能力，不能把27类动作当作全部画布功能完成。

## 第三批：显式剪辑编辑

新增 `clip_edit`、`clip_operation`，覆盖30类时间线、轨道、片段、基础文字、关键帧、音量/淡入淡出、音视频分离及撤销重做操作。素材放入仅支持工程媒体区已有对象，定格仅支持图片；文件导入、视频帧提取、完整效果和工程保存/视频导出继续待接入。

所有操作复用当前 Studio 的 Session、原生命令和历史，以稳定对象ID定位。审批前冻结完整状态，审批与提交均检查当前工程、文档会话、时间线、选择、轨道锁、writer与精确许可。剪辑 Undo/Redo 的实际版本按原生行为递增，与画布历史恢复旧版本分别处理。

`project.clipState` 提供文档及编辑状态摘要、指定对象的实际属性与缺失ID。写回执保留原参数、原版本、创建/删除对象和实际回读；私有运行时再次独立读取。源工程、临时预演、历史目标限制为16 MiB；音频分离的复制与计算量、完整56 KiB回执均在原生提交前检查。结果未知只查原 `clip_operation`，不重新分配身份。

当前实际验证：

- 原生剪辑21项已包含在 `editor_mcp::` 的98项内，同一产品源码的 `agent_controller::` 63项、Session历史5项、画布Agent绘制/导出辅助2项均通过，共168项；不启动前台App。CI借用修复后的单项重跑通过，不重复计数。
- 公共模型服务工具合同4项、覆盖清单和diff检查通过。目前为42个原生命令、45个画布辅助接口、28个MCP注册、161条业务路由，尚无完整业务分母。
- 私有运行时在隔离的冻结源码中通过231项剪辑及关联回归，其中剪辑45项。覆盖完整COTO审批、30类动作、原回执恢复、错误目标和删除集合、分离音频归属，以及旧工程的长调速曲线、关键帧超调、长名称等合法回读；新写入参数上限继续保留。
- 原生产品库严格Clippy通过。[上一批公共CI](https://github.com/Stormycry-cryp/seecut/actions/runs/39413521458)在测试代码的一处 `cloned_ref_to_slice_refs` 报错，退出码101，耗时2551秒；已修正借用写法。本次日志不能证明OOM，新的全仓库CI结果另行记录。

最终MCP协议/stdio34项、协议all-targets严格Clippy通过，正常stdio CLI已重新构建。二进制SHA256为 `b3ad7a7d5136a759d99eb48cbf6533216c50951a2943814c79b7ee026774bb60`；主代理用标准initialize/tools/list实测28项注册，仅列目录，没有连接真实App端点。

本批仍是原生逻辑、协议与本地替身证据，完整真实模型→实际App→作品链、原生视觉交互与成片质量未完成验收。Android按当前范围暂缓；本功能候选的PR不再自动执行Android构建，其他分支及显式/夜间/发布调用保留原规则。

配套私有 `dfbde73` 的[Node 22.19.0和24.x检查](https://github.com/Stormycry-cryp/seecut-agent/actions/runs/39421052890)均已成功；公共 `3397fa4` 的[服务检查](https://github.com/Stormycry-cryp/seecut/actions/runs/39421002493)成功；该次引擎检查已被后续推送取消，以公共 `87d273a` 的[引擎检查](https://github.com/Stormycry-cryp/seecut/actions/runs/39422100835)最终结果为准。移动端任务已确认Android为skipped，不能把它当作Android编译通过。

## 第四批：Flow原生成身份

`flow_execute_node` 在开始时先持久保留一个生成UUID，经可信函数上下文传到图片、视频及局部重绘节点，不允许模型参数指定此UUID。节点保存真实本地record后才回执 `version:1 / stage:recorded`；该阶段不表示上游生成已完成。丢回包、重开和后续轮询继续查询同一个UUID，可从历史中读取，不认领用户后来手动发起的生成。

补丁SHA为 `c50e213b81bc5a09e6b44d7929fadeeca68cde71a5550e208cfd2fd7df0fc7cf`。从固定上游本地archive重新应用补丁，62个源码文件逐一SHA匹配且 `prepare-toonflow.py --verify-only` 通过；没有把构建产物或依赖纳入补丁。

- Flow gate、磁盘回执、真实nodeAi/工具registry合成链24项、独立context1项、完整COTO允许/拒绝2项通过。主代理另复跑生成绑定8项通过，已包含在24项内。
- 在隔离目录真实构建全部8个节点、Web、server/index.js和SeeCut server/seecutHost.js入口成功。相关节点、scaffold和仅Web源码的Node vue-tsc检查通过；原Web/server完整类型检查仍因既有Hutch SDK类型文件缺失而未通过，没有伪造stub或安装桌面SDK。
- 一次构建目录错误误清理了原ignored节点产物。停止错误进程后，仅从完整隔离构建补回5个缺失文件，保留3个已匹配文件；最终原节点目录精确8个普通文件，SHA全部与隔离构建一致。源码、依赖和已交付App没有因此误改。

以上均未调用付费模型或启动前台App，不能代替真实生成作品、费用及实际原生交互验收。

## 第六批工具目录体积

本轮正常构建CLI的SHA256为 `b8a6c70f0bbcfc296f47279475281cf449b0f619369ad58449cad5a1cb55316f`。通过真实stdio initialize/tools/list读取29项，紧凑JSON为64281字节；使用现有COTO客户端及私有sessionTools实际包装，模型工具定义为61328字节。测量仅启动MCP客户端，使用合成实例和凭据，没有连接实际App或发起计费请求。

| 模型可见范围 | 工具数 | 工具定义JSON字节 |
| --- | ---: | ---: |
| 全部原生工具 | 29 | 61328 |
| 画布 | 14 | 26962 |
| 剪辑 | 6 | 22746 |
| Flow | 17 | 17240 |

这些数值仅统计工具定义，不含系统指令、工程上下文、历史、图片或任务内部工具；不能当作真实token用量或费用。首次可信workspace选择保留原工具对象、参数和执行函数，不凭模型参数切换范围。


## 第五批：持久创作任务

私有 `6685304` 将任务状态保存在原会话存储下，沿用COTO writer、输入历史、业务操作journal和managed预算，不建立第二套费用账本。支持明确积分上限、串行续轮、幂等用户补充、快速暂停/停止、原输入恢复、原结果查询及有界退避。检查点关联本步骤真实操作，完成前重新读取属性、保存/PNG凭据或Flow原UUID资产结果，聊天最终文字不直接完成任务。

本地完整337项通过，0失败/取消/跳过，106.3秒；日志 `/tmp/seecut-runtime-tasks-release-test.log`。包含真实COTO配合本地managed服务、续轮账务和控制竞态。额外源码复核覆盖原生成身份与stage分离、发送前私有WeakSet标记、原子任务状态和旧epoch约束，未发现新增高影响问题。以上仍为逻辑和合成服务证据。

本次6685304的[私有CI](https://github.com/Stormycry-cryp/seecut-agent/actions/runs/39426452780)已确认Node 22.19.0与24.x均成功；CI固定公共 `87d273a8e4c93f3cb8d020da54b567d316cd9bb5` 服务合同。新服务尚未部署；本轮已接原生Task控制、任务范围内逐次自动许可和UI，相关逻辑通过下述联合测试，真实App链仍未验收。任务绑定当前文档会话，尚未实现跨工作区自动交接；重开后暂停，不承诺App关闭后本地继续执行。


## 第六批：个人库三类素材与剪辑导入

新增 `library_assets` 查询固定版本的图片、视频和音频，仅返回元信息，`available` 不代表内容通过SHA校验。`clip_edit.importLibrary` 按原操作异步校验和probe，最终重验原范围、账户、助手会话、完整编辑状态、writer及精确许可后调用原生AddMedia。创建和去重都返回真实mediaId；原导入证明与当前工程独立回读共同核对固定引用，不宣称媒体质量。

原PNG交付的内部核读仅接受同client、scope、原账户和助手会话的成功导出回执。来源在首次接受时冻结；pending、partial和UNKNOWN不授权；登记重试不填补或覆盖旧来源，旧无来源记录也不放行。泛查询继续要求精确许可。

- 正常MCP协议/stdio完整42项、all-targets严格Clippy及正常CLI build通过。macOS临时socket长路径仅在测试fixture修复。
- 实际原生媒体探测已核对16×16图片、64×64/4fps/1秒视频和22050Hz双声道/0.25秒音频；不是模拟decoder输出。
- 私有完整运行363个条目中362通过，另1个billing模块因隔离服务目录缺catalog未加载；补齐原catalog后billing模块4项通过，合计覆盖366项用例。此为组合覆盖证据，不写成单次全绿。真实CLI schema的nullable limit兼容另以28项相关回归验证；14文件冻结摘要为 `68128a8dafbcec461c3a762ce5435c68d9599d842cb26340a9d92a8c39d22a5a`。
- 本地managed合成链覆盖library_assets→importLibrary→final，沿用原COTO审批与三次原推理账务；重复请求只查询原操作。配套公共服务合同及关联63项通过，服务没有部署。
- 原生候选字段样例与私有parser交叉核对3项通过，涵盖元信息查询、创建、去重；样例使用合成身份，不能作为真实App回执。

最新同一原生测试二进制SHA256为 `688f0dcff84e558c4de66fbe1cd0661262c949c95f7399a91be26ad51271f50b`（2026-10-20 14:30:11 UTC）：agent_154通过、1个测试辅助入口默认忽略；editor_mcp119通过；bounded_digest1通过。三组过滤集合无交集，共274通过、1忽略；旧阶段249不叠加。包含原生媒体探测、原AddMedia及去重/撤销重做、OwnedOutput来源、任务控制和legacy move撤权逻辑。最新concat --lib --tests严格Clippy通过。没有启动前台App或抢占鼠标、键盘、焦点。单素材8GiB、单个在途probe；现有FFmpeg probe没有硬取消能力，挂起时占住该槽位，迟到完成仍须重验授权。当前未覆盖云/团队素材、本机任意路径、批量送入或自动切换目标工程。

第六批私有77829dc的[Node22.19.0与24.x CI](https://github.com/Stormycry-cryp/seecut-agent/actions/runs/39441374469)均通过；公共d01f9a8的[服务CI](https://github.com/Stormycry-cryp/seecut/actions/runs/39441490848)通过。[引擎CI](https://github.com/Stormycry-cryp/seecut/actions/runs/39441492909)在cargo fmt发现lib.rs模块排序，后续编译和测试未执行，本批修正。更早87d273a的引擎在编译测试时runner失联，官方注解仅确认通信丢失，没有失败编译步骤或可取回日志，不能据此认定OOM。

## 第七批：App页面与原问题恢复

源码新增独立App只读范围及app_context/page_items/page_preview，原capabilities按当前App/文档范围分别回应。八类普通页面从实际UI缓存投影，不读取整个库或磁盘；发问前固定页面三字段并按原inputId保存，后续工具及模型完成结果核对原输入。媒体使用实际缓存Image、当前预览或列表缩略图；敏感和未接入弹窗共用原生模态校验。详情见[App读取合同](../../../agent/app-read-protocol.md)。

恢复保留原COTO session、历史和managed账本，只接受同账户/client的明确重新授权；旧inputId不改绑或自动重发。宿主结束旧问题仅撤回原pending输入，并为原只读调用记载明确无结果终态；原费用未核实则继续阻塞。近期历史摘要有64项上限，当前未决列表完整。该动作不证明业务完成。

- 私有live用Node24.15.0、当前公共服务源码单次完整417/417通过，0失败/取消/跳过，108.375秒。涵盖真实COTO、合成MCP、stdio/IPC与loopback managed；10个实现和测试文件组合SHA为 `81499500267d79f926224bcce1622b63d655159edea68f96b21e30f9be6510d8`。移植仅修两处测试服务路径，没有以旧隔离服务冒充当前公共合同。
- 前端由实际gpt-6-astra/high子代理接手实现和检查；本批轻量harness122通过、1个辅助入口忽略。原生独立轻量64项通过，其中包括真实RGBA→PNG、摘要和换图后拒绝。上述集合存在重叠，不能相加为联合原生总数。
- 公共服务关联64项通过；Linux引擎新增临时swap与cgroup祖先OOM计数及最终资源采样，10项swap命令替身测试和7项supervisor测试通过。本机未创建swap，新配置尚未在runner实测，不能声称修复了失联原因。Android源未改，本轮继续暂缓。
- 最终原生/Slint联合编译通过；同一最终原生binary的6个无交集过滤组共301通过、1个辅助入口默认忽略：agent_163、editor_mcp122、AppPage12、实际PNG2、Cloud账户1、bounded_digest1。binary SHA为 `ce45ca7a1c06ba81321da2ab3c8bd842d7213a64addff4007ea8379869fb50db`，23个Rust/Slint源码的冻结组合SHA为 `11576ad6b81fde84c4f5fb296bd322093785b915bb80b2710a8f145153fa868d`。严格Clippy要求的三处最小修正为tests-only Store::open、移除冗余局部变量、将form_text移至tests模块前；已全部包含在最终二进制与复测中。初始binary的301项不重复计数，也未运行未经审阅的全部628项。
- MCP协议/stdio47项通过，正常CLI SHA为 `e8441c01a70d05d6b7cf69ee0198a02cdb41759311883f01917a242c555f303f`。真实stdio目录32工具、紧凑JSON67973字节。真实COTO客户端和私有包装器保留Doc29工具/61328字节，画布14/26962、剪辑6/22746、Flow17/17240；App4/3225，绑定字段对模型隐藏。此为定义JSON体积，不含上下文/图片，不是token或实际费用。
- fresh CLI、真实COTO与合成Unix端点完成四个App方法共17次IPC交叉验证，确认标准成功envelope、PNG image字节摘要、普通nameTruncated字段及过期/未授权拒绝。合法schema使用oneOf/const，首次校验脚本误以为enum，修正严格解析后32目录核对通过，没有为夹具改生产协议。该合成端点不是运行中的App。测量日志保留222行既有AJV未知uint/uint64/double格式警告；实际数值范围继续由原生及私有层检查，没有修改COTO。
- 四个App模型工具说明修正后，App/工作区定向61项通过；参数、执行和权限逻辑未改变。417项为该说明修正前的完整运行，61项为其后的受影响回归，不相加为478项。
- 宿主 `cargo clippy -p concat --lib --tests --offline -j 1 -- -D warnings` 通过；MCP all-targets/stdio严格Clippy和完整工作区格式检查通过。本批没有前台App、实际授权按钮、真实模型观看或作品质量证据，不算全功能完成。
