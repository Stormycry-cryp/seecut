# Task 人类输入观察协议 v1

页面操作档的发问由 task.create 创建持久任务，任务补充走 task.update。二者分别保存用户发送时的完整观察，并与各自原人类请求ID关联。普通文档 follow_up 的协议见[文档初始观察](input-observation-protocol.md)。

## 原生输入与历史

Doc 初始化单独请求 taskObservationProtocolVersion:1，并核对 task_observation_binding:true 及 task_observation_protocol_version:1；普通Doc观察能力和durable_tasks不能代替此确认。App范围不协商Task。

创建任务在异步附件整理前捕获真实Context，绑定嵌入原TaskIntent.initial_observation_binding。新补充的绑定放在原TaskUpdateIntent.observation_binding。Task历史的顶层普通Doc observation_binding保持缺失，不能同时声明两种输入。

发送前要求完整绑定与当前实例、client、工程、文档及kind一致。历史类型只核结构，不能替历史证明当前权限。旧记录缺字段保持可读，不填入今天的观察；旧无绑定pending不得自动补造来源后重发，恢复只核对原Task。

## 运行时与自动轮次

wire字段为task.create.initialObservationBinding及task.update.observationBinding，复用完整DocumentObservationBinding。创建进入原幂等摘要、任务定义和TaskStore；补充进入该次原clientRequestId的摘要及followup，不覆盖创建来源。ACK丢失或CAS重试保持原ID与原绑定，不能重新采集选区。

自动轮次只从原任务记录取来源，在原COTO输入中保存内部引用。公开send不允许声明此内部字段。首次上下文准备沿实际input.applied找回唯一原input.accepted，再关联原定义及补充；真实Context匹配并将核对结果持久保存到原TaskStore后才允许模型准备。

已核对来源不会成为后续所有轮次的固定工程版本。每轮普通输入仍包含原人类ID及完整观察，保留用户所指的原选中对象；它们不授予系统或编辑权限。32KiB完整输入容量超限时停止，不截断对象ID或丢弃早先要求。

尚未核对的多份绑定完全一致可共同核对；若完整观察不同，以blocked和human_observation_conflict暂停并保留所有来源，不自行选择某条继续。观察核对、操作完成、费用结算与作品交付分别记录。

## 权限与验收边界

原TaskGrant、预算、控制代次、暂停/撤权和UNKNOWN规则保留。绑定不能跨账户、工程或工作区迁移任务；原结果未知只查原业务身份，不创建第二次写入或计费请求。

联合门禁记录见[本轮验收](../testing/main-qa/2026-10-20-full-agent/README.md)。逻辑、真实COTO与本地模拟服务不代替原生App、模型观看、实际作品质量或跨模块完整流程验收。
