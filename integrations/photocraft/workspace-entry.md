# SeeCut 开发入口与当前边界

图像项目列表保留旧画布及 `.comp` 工程入口，新增完整图像编辑器卡片。新工作区首先使用 SeeCut 管理的独立编辑进程和窗口，同一 Slint 窗口内嵌尚未验收。

开发运行需通过可信本地配置 `SEECUT_PHOTO_BUILD_MANIFEST` 指向本批成功的完整构建 provenance。未配置时显示组件不可用；正式包的 Helpers 查找和签名尚未接入。宿主实际校验二进制，不因配置文件存在就标记就绪。

入口由一个串行后台 worker 持有唯一 `NativeHost`，Slint UI 通过计时器消费状态，不在 UI 线程等待 IPC。启动、关闭、待保存、连接失败及进程退出分别显示真实状态；启动失败且进程仍存活时不再次启动。正常关闭保留上游保存提示，不代答丢弃、不调用会绕过保存的 `app.quit`，不因宿主释放强杀文档。

启动动作先在当前 Slint 回调中设置保守保护，防止排队中的 Agent 请求沿用旧画布身份。编辑器处于启动中、存活、待保存或状态不确定时，当前 MCP 返回 `unsupportedWorkspace`，并撤销旧文档写绑定。这个临时保护不表示 PhotoCraft 已接入 Agent，也不声称子窗口处于前台；后续同 Session 接线必须替换为真实 Photo 文档身份与权限。

已覆盖 SeeCut 源码中六处显式 `quit_event_loop` 对应的关闭路径。若编辑器仍受管理，父窗口保留并提示先完成保存关闭，随后用户再次退出 SeeCut。macOS Cmd+Q 的 Cocoa 原生终止通路还在调查，不能据此声称所有系统退出路径均已保护。

本批通过实际 Slint 编译，新增 callback 已更新来源清单；完整 SeeCut Rust 类型检查、真实窗口生命周期、输入法、焦点与键盘行为仍待验。宿主测试及严格 Clippy 结果见 [native-host.md](native-host.md)。前端及原生入口由实际 Astra medium 实施，后台宿主由实际 Sol high 实施，主代理串行审查集成。
