# Clip 原位置保存验证

状态：本地接线、默认配置编译及相关无GUI检查通过，开发模式条件分支的补充编译检查通过。本文不代表实际App或正式分发包验收。

## 本批实际证据

| 范围 | 已验证结果 | 证据边界 |
| --- | --- | --- |
| host实际保存、文件读取、Session与旧保存回归 | 同一实际host测试程序58项通过：新保存/读取30、提交仲裁9、Session5、旧保存/owner/并发14，无交集 | 原真实Session、owner、SaveLane、临时工程及文件；没有打开GUI。程序共90项，本批未运行全部。一个空ownership过滤未计入 |
| 公开MCP协议及客户端 | library39、CLI16、integration18，共73项；严格Clippy和正常CLI构建通过 | 实际Rust协议与客户端；不证明原生GUI接线 |
| 私有运行时 | 冻结源码完整641项，保存相关72项通过；实际集成源码逐字节匹配 | 实际COTO和本地服务，模型/native端点为合成fixture；主代理集成后另复跑3个原查询失败场景 |
| 正常CLI保存联合检查 | 9组，95个Unix请求，无wire错误 | 实际正常CLI、live运行时与COTO；原生端点为合成fixture，文件解析不冒充生产engine或fsync |
| 正常CLI兼容检查 | 15组AppControl与原工具范围检查通过 | 使用同一新CLI和live运行时；模型/native业务端点仍为合成fixture |
| 原生/Slint默认配置 | 最终同一程序8组不重叠检查433项通过、0失败、1项辅助入口忽略；严格Clippy和工作区格式通过 | 实际760项中选择434项。9项保存回归使用真实Session/文件和原Permit/Receipts；来源、完成队列与UI接线未作为真实App事件验收 |
| 开发模式条件分支 | assistant-local-dev的cargo check通过 | 仅补充编译检查，覆盖UI guard跨worker的条件分支；未运行该feature测试或App |

host程序SHA256为 `6b246cd393da3606b60bdd9e4dff06ddce86f5670b6dd41bf3db5904ec0bfbb3`。最终原生测试程序为 `7531ba40ad36e918077fcd1dffa1d2d37d37792d4e4624c5c1d50265d0df25e4`，196份输入的冻结摘要为 `e48c48f12f36fee97ca407c6bf143505dd66c7a3f6c6091de7d5e6ba00837285`。正常MCP CLI为 `e9a3d0611b8f45985288a76260fb569f7622f1bb1a0081eaf40b32c65aec5af9`。

主代理核对host实际程序的90项列表、58项选择无交集、各日志及三份源码摘要；另独立核对最终原生实际760项列表、434项选择无交集、196份源码、程序及日志摘要，以及正常CLI与运行时/COTO摘要。忽略项为exit_helper_process辅助入口，由process_exit_reaps_child_and_grandchild测试启动，不是遗漏的业务测试。源码与冻结记录放在本机`.toonflow/clip-save-native-draft`、`clip-save-host-draft`、`clip-save-protocol-draft`与`clip-save-cli-gate`，这些本机材料不作为覆盖清单的仓库文件引用。

## 已修复的实际问题

- 旧工程迁移后的编码可能超过默认JSON递归限制。现以落盘读回相同的字节解析器在提交前检查：原depth123材料被提前拒绝、旧文件保持可读；depth120正常保存。独立helper复核与实际host回归分别保留。
- 保存受理后的查询若返回无saveProof的resourceLimit或leaseExpired，原运行时可能误清UNKNOWN。现保留原操作待确认，覆盖有身份/无data、原COTO查询、重启恢复及禁止新保存；明确受理前拒绝和可信not_sent仍按原规则处理。
- serde的内部标签unit变体会忽略多余字段。Save改用空结构体变体，合法JSON保持`{kind:"save"}`；实际schema封闭，并经直接stdio调用确认多余字段及非法读取参数在Unix派发前被拒绝。
- 旧Save审批后用户先编辑，原实现会将新Project摘要拼入旧revision的未提交回执，使运行时无法核清。现从可信审批后一次捕获的原Permit取来源；实际Session与原generation/摘要不一致时，不分配host票据，typed拒绝仍保留原H0。真实AddTrack、原文件不变、原receipt重复查询和缺来源拒绝回归通过，独立静态复核与live逐字节一致另核。
- 原完成投递遇到Studio/Bridge借用冲突时可能丢结果或panic。现忙时保留队列，释放后只写原Receipts；原来源已切换则不执行清dirty回调。回归使用真实两层RefCell冲突、原Host结果、原WriteRegistry范围变化及Receipts，不代表执行了真实GUI重入。

联合检查首轮前8组通过，第9组的QA解析器误把RMCP明确的反序列化拒绝文字当作JSON。修正QA解析器并保留原失败材料后，完整9组通过；未修改业务逻辑绕过失败。

覆盖表新增保存映射和1条源变更前回调，保持34项全局注册、29项Doc工具、170条业务路线。99个精确暂存文件的独立源码副本通过清单检查，包含全部已跟踪Slint文件，不含本机忽略证据。首个QA副本遗漏清单JSON，在真正检查前失败；补齐输入后通过，没有修改业务源码或检查器。

## 尚未取得的验收

真实App的原生授权、页面/选择/弹层、系统选择框及长任务联合操作，真实模型、作品质量、完整CI及正式分发分别待验。未启动前台App、操作鼠标/键盘/焦点或调用付费模型。源变更前的UI接线静态审查与gate/Bridge组件测试不等于实际GUI事件验收。

原生历史回执仍受24小时/1024条和重启丢失边界约束；丢失后当前文件相同不能消除原UNKNOWN。保存只写工程manifest，不复制媒体或导出成片。Android继续暂缓。
