# SeeCut Flow 的 Toonflow 依赖

- 上游：https://github.com/HBAI-Ltd/Toonflow-app
- 固定版本：v2.0.6
- 固定提交：c6756b9c23e87aff1ecafad6ebf8dfbe015a74a2
- Bun：1.3.14
- MIT 许可完整保留于同目录 LICENSE；上游传递依赖保留各自许可。

## 组织方式

本目录保存 upstream.json、integration.patch、LICENSE 和来源说明。上游完整 Web/Server/节点/工具/扩展/文档源码由固定提交加补丁生成到忽略目录 external/toonflow，不把上游整仓纳入 SeeCut 的提交。构建仍保留完整工作区与正式节点，不以假节点或重写画布替代。

```sh
python3 scripts/prepare-toonflow.py
cd external/toonflow
/path/to/bun install --frozen-lockfile
cd ../..
SEECUT_BUN=/path/to/bun python3 scripts/build-toonflow.py
python3 scripts/package-toonflow-experiment.py --binary /absolute/path/to/concat --output /absolute/path/to/SeeCutFlow.app
```

prepare 对已有源码只校验受管标记及补丁文件摘要，发现本地修改即停止，不覆盖。build 不隐式安装依赖或下载 Bun；宿主启动 Bun 时使用 --no-install，阻止运行时隐式下载依赖。独立实验包不替换已安装应用，使用独立数据边界；不代表正式分发版本。

## 适配边界

1. 嵌入模式先显示 SeeCut 风格 Flow 项目列表，点击项目才进入真实多画布/文档工作区。返回先保存，失败保留当前内容。独立 Toonflow 首页、引导、全局设置、供应商充值、设备互联、商务社群和第二套 Agent 入口不挂载，普通模式保留上游功能。
2. 集中 SeeCut 明暗色彩：浅色强调 #173e78，深色强调 #ff684a，映射 Element Plus、TDesign、VueFlow 和 Dockview。原生发送主题与关闭保存事件；关闭保存期间临时冻结页面交互，先保存设置、最后保存工作区，失败或超时恢复编辑。
3. Wry 子 WebView 由同一 Slint/winit 窗口管理。本地 Bun 在后台异步启动，Flow 等待期间可切换原生画布；服务监听随机回环端口，用一次启动 Cookie 保护页面/API。Rust 账户业务桥使用另一条私有 TCP JSON-lines 通道，令牌不进入浏览器，冻结 expectedScopeKey 防跨账户读写。
4. 项目按持久 UUID 管理；本地自动保存、文件版本冲突和恢复副本；云 ZIP 快照带 SHA、版本检查及幂等回执。快照保留项目内容和可恢复生成记录，不保存账户令牌或上游全局设置。
5. 正式生成函数转接 SeeCut 原生目录、报价、原任务提交/查询及素材登记，结果仍写入真实工作区相对文件。未知结果只核对原任务；显式继续提交复用原身份，GET 恢复不收费。独立价格展示尚未接通，不能把空估价当作零积分。
6. i18n 字典按当前语言异步加载。列表先读本机项目，云端后台刷新；空闲预取工作区路由，不提前挂载工作区或启动 Agent。

## 验证边界

完整 Web/Server、8 个正式节点及宿主构建通过；Rust bridge 的 17 项单测、后端 39 项测试通过。完整 Host 在隔离空缓存下实际 2178 ms 就绪，缓存文件数保持 0。实际 Web 操作已确认文档编辑后返回列表、重开仍保留中文内容；原生本地 QA 账户的项目 a 已有云快照 revision 3。

构建、静态类型检查、受控服务、原生界面和正式云端分别记录，不能相互代替。原生 GUI 已完成本地假供应商的图片、视频生成及原视频任务恢复，两项供应商各调用一次，输出 SHA 与登记版本一致；付费调用为 0；新增 Flow 存储尚未部署生产，真实付费模型与 Windows 运行未验收，独立报价展示尚未接通。Agent/MCP 的新 Flow 工具仍按 docs/plans/2026-10-15-toonflow-agent-mcp.md 后续接入。

生成查询只在完整记录内容变化时落盘并触发同步；100 次相同查询受控验证只有首次写入与通知。明确 BUSY 使用原请求短时重试，其他未知结果仍只查询原任务。
