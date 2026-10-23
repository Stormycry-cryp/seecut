# Clip 原位置保存接入计划

状态：保存后端、公开MCP、原生入口与私有Task运行时已集成本地功能分支。host58项、MCP73项、运行时641项，以及同一原生/Slint程序433项定向检查通过，辅助入口1项忽略；严格Clippy和格式通过。开发模式条件分支补充编译检查通过，真实App仍待验。

目标：Agent保存当前已打开的Clip工程，并核实原位置实际manifest。沿原Session、writer、SaveLane和操作回执完成，不接通用另存或视频导出。

## 提交与撤权

UI冻结原Session的实际快照、路径、writer、lane/ticket及完整许可来源，交给不可复制的执行对象。模型不提供工程JSON或路径。UI只做内存身份与许可检查，文件系统检查、写入与清理均在worker。

每个原操作有一个瞬态、单次使用的sealed gate。撤权、token轮换、账户/会话、文档、真实编辑或新保存票据，须在对应源状态改变前使尚未提交的gate失效。worker在最后owner、固定manifest及最新ticket核验后，以CAS争取提交权；新票据与CAS共用短内存仲裁，不持该锁做文件I/O。

- UI先使gate失效：本操作不能rename原manifest，返回明确未提交。
- worker先获得提交权：原操作已开始提交，之后可能完成rename；撤权立即阻止尚未开始和未来操作，不能将这条已开始操作改报取消。
- CAS只证明开始提交。成功还需要实际rename、目录sync、同lane锁内重新打开原manifest、有界hash与JSON/工程解析核实，且与冻结快照一致。
- rename尝试后的错误、确认丢失或读回不一致保留原操作待核实，不换ID重做。文件当前内容相同不能补造本次操作成功。

不让worker持writer锁等待UI，不让UI等待文件I/O。原有手动和自动保存行为保留；已有关闭时的同步保存不计为本批新异步能力。

## 实施顺序

1. Sol high在隔离目录实现host guarded-save入口与真实fixture，覆盖原位置、legacy manifest、同owner、新票据与CAS竞争、截止时间及真实读回。纯gate可独立rustc验证；实际host测试由唯一Cargo执行者串行运行。
2. 主代理复核后端候选与既有Clip原操作/查询合同，定稿最小保存回执及独立落盘读取协议。保留原operationId、clientSequence与COTO，复用现有业务事实，不新增历史账本。
3. Astra high接原生UI：原人类来源和许可冻结、源变更前失效、异步原回执、旧结果归属及dirty规则。主代理不编写前端。
4. Sol high接私有运行时与typed Task交付；以原回执和独立实际manifest核对完成，不能以dirty=false或模型文字声明完成。
5. 冻结源码，串行运行相关host、原生/Slint、运行时及正常CLI检查，成对更新已有功能分支和Draft PR。

## 验收与限制

真实临时工程与Session应覆盖CAS前取消且原manifest不变、CAS后撤权仍只回报原实际结果、同lane新保存不被旧保存覆盖、原writer连续持有、旧回调不清新revision的dirty、原manifest实际字节/摘要/解析一致。文件和目录同步返回成功只证明对应系统调用，不代表真实断电试验。

当前Clip原生回执保留24小时/1024条且重启丢失；本批若不改变该事实，必须明确重启后原操作仍可能UNKNOWN，不能凭当前文件猜原保存结果。保存不复制媒体，不代表视频导出或媒体源内容冻结。

实际App、付费模型、成片质量与正式分发分别验收；Android继续暂缓。此计划没有授予合并或发布权限。
