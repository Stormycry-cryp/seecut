# 原生常用路径实窗验收与参考图移除修复

本轮为开发 QA，部分通过；不能发布，也不能替代完整 CI 或真实模型验收。PR #21：<https://github.com/Stormycry-cryp/seecut/pull/21>。

## 版本及环境

初始实窗应用源码 `272bcf1f4b2e822b0d7ed94cf8b67b6e27289c95`，打包后二进制 SHA256 `d8592c81c6742d0a56e9fa8dce74b421d962bbbb4b9421109b96edf1b65e992e`。私有运行时 `7a05ee22bc6889e3ac16e9e6b8a414ae823be53c`；冻结应用/zip 保持原样，实际操作在独立 Run.app。构建 dev/arm64 成功（4m18s），最低系统版本 15.0.0，adhoc 签名，未公证、未安装。封存记录 [candidate-manifest.json](candidate-manifest.json) 的 launched=false 是启动前状态，最终运行结论以本文及 validation.json 为准。

使用本机已有屏幕捕获、Accessibility、事件发送权限；只操作精确 Bundle ID/可执行路径所属前台窗口。原始 PNG 没有裁剪、修图或拼接，版本/hash 见 [screenshots.json](screenshots.json)。主窗 1400×866，拖动边缘缩至 1000×760；AX resize 失败不计为产品故障。登录、原生文件选择器及无关旧资产画面未纳入公开截图。

合成服务为 `integrations/toonflow/tests/prompt-ui-fixture.py`，仅监听 `127.0.0.1:18745`，数据在 `.toonflow/frontend-qa-272bcf1/service`。真实本地 HTTP、SQLite、worker 与原生 UI，供应商为合成对象；没有付费调用，也不验证线上质量。输入 PSD/画布来自仓库或旧合成测试的独立副本；没有覆盖用户原件。LUT 副本已含旧 “SeeCut UI QA” 文字，本轮新建文字仅 “QA 272bcf1”。

## 272bcf1 实窗通过项

| 路径 | 实际操作和证据 | 结论 |
| --- | --- | --- |
| LUT 数值同步 | 滑块 .78→.22，数字跟随（15）；数值输入 .65（21），撤销 .22（22），重做 .65（23）；保存离开再打开（38） | 画面、滑块、数字及 manifest 一致，F2 本轮通过 |
| 原生文字 | 新增文字，编辑为 QA 272bcf1，保存（30），离开再打开 | 文本和独立可编辑图层保留 |
| 导出入库/去重 | 未修改画布连续两次导出，库仅一个对应内容条目；仅选择本轮 legacy-v4 移入回收站并确认；重新导出 | 旧 ID bf1f… 为 trashed=true，文件存在；新 ID fdfb… 为活动条目，同内容 SHA a6b6…；见 library-after-*.json，F3 该路径通过 |
| PSD 分层 | 从文件选择器导入 seecut-layered.psd（41）；保留外组/内组、中文名、位图蒙版与隐藏状态；将隐藏图层显示、透明度50→75；另存.comp、导出入库、离开重开（45） | 保存 v7，嵌套结构及 opacity=.75 保留；不是 Photoshop 全兼容声明 |
| 截断 PSD | 通过同一文件选择器打开100字节截断副本（43） | 提示“PSD 段已截断”，当前已保存工程及图层仍在，没有静默扁平化 |
| 矢量蒙版 | 选中路径蒙版（47）；拖动锚点与手柄（48）、撤销（49）、重做；解除链接并保存（50），离开重开（51） | 几何和 linked=false 持久化；本轮没有重新创建所有形状或覆盖完整蒙版矩阵 |
| Jev 推荐 | 添加合成参考图并输入提示词（59） | 真实 UI 显示给定6条候选中的3条；合成协议记录 model=jev-1.14-free、candidateCount=6、imageCount=1 |
| GLM 改写及撤销 | 点击推荐（60）；点撤销（61） | model=glm-5.3-flash、imageCount=0；原英文草稿恢复，随后可再次采用。未验证含引用 token 的全部富文本矩阵 |
| 暗/亮与窄窗 | 初始亮色（04）、暗色画布（54）；1000×760 生成页（70）及 Agent（71） | 生成按钮、模型/强度入口与 Agent 发送区可见；初始费用0。Agent 会话/工具调用链未完成 |

## 发现并修复的交互问题

参考图“移除”按钮条件为 `preview.hovered`，但按钮与预览是兄弟组件。鼠标移到按钮时预览失去 hover，按钮消失；实窗点击可落到预览而无法移除。源码修复 `35a7600` 将移除入口常驻，保留原 action，不改素材删除语义。该操作只移除当前参考，不删除源文件。

修复增量构建通过（4m16s），adhoc 签名校验通过。二进制 SHA256 `db855a6b85bb0ee4aaebd67d01d92d58972e46807974292ce93144666eef855d`，详见 [fix-candidate-manifest.json](fix-candidate-manifest.json)。独立运行副本启动成功，但登录后于18:03:40 UTC 遇到 Documents TCC 提示（Bundle `cloud.stormycry.seecut.devqa.35a7600`）；采样显示主线程 `cloud::publish → std::fs::read → open` 阻塞。故**修复后的移除点击尚未实窗通过**，也未完成本轮纯文本生成自动入库验证。停止依赖权限的操作，没有改权限或绕过校验。旧272截图不能冒充修复后验证。

## 阻塞、限制与隔离观察

- **Flow 启动失败，完整 Agent→COTO→WebView→Flow→生成→资产链未通过。** 初始 app 17:26:45 UTC 启动时，macOS TCC 对该 Bundle 的 Bun 子进程发出 `kTCCServiceSystemPolicyDocumentsFolder` 非 preflight 的授权提示；随后原生服务30秒超时（07）。限定采样中 Bun 阻塞 openat，未出现 ready 文件。证据见 flow-tcc-scoped.txt。没有批准、更改或绕过系统权限。恢复该路径需用户为确切 QA 应用授予 Documents 文件夹访问，或另行批准适当打包位置方案；不需要擅自授予全磁盘权限。新修复包只测原生功能，使用空运行目录覆盖，避免重新启动已阻塞的 Flow。
- **带参考图的合成生成失败**：本地 fixture 的 public_base_url 为 http loopback，产品要求提供给上游的参考 URL 为公开 HTTPS，故两次提交均在调用上游之前以 REFERENCE_URL_UNAVAILABLE 终止。没有为通过测试关闭校验或上传素材，不能把此路径标为通过。
- **隔离不足观察**：独立 portable 资产库首次读取时，默认输出目录仍是 `~/Movies/SeeCut`，自动收录其中两份旧生成文件。原件未修改；没有选中、导出或删除这两项。发现后通过账号页将本 QA 输出目录改为 `.toonflow/frontend-qa-272bcf1/generated`。含旧缩略图的截图不发布，后续原生复测包不复制旧库。这是 QA 初始化缺口，不宣称全程没有读取旧文件。
- 原生文字输入测试使用已有 ABC 输入源，不能替代中文 IME 组合输入验收。早期 Cmd+A 事件只带 flag 未发送 flagsChanged，导致选择失败；修复输入辅助后完成目标，这些尝试不算产品错误。
- 父协调提供 CI 证据：run39284294989/job118165747183 于17:39:06 UTC shutdown143；no-run1274秒，未到5400秒期限；rustc RSS约14.6GiB、MemAvailable约31MiB、SwapFree152KiB，说明严重内存压力，**没有内核 OOM kill 证据，不能断言根因**。严格workspace Clippy和wasm通过，完整测试未通过。此线程没有重跑/取消/轮询远程CI。

## 收尾

18:06:30 UTC 官方10080分钟七天窗口剩余50%，重置2026-10-25 01:02:41 UTC；按最新协调停止线30%记录。两份封存应用及旧11c7d8候选保持原样。原始QA正常退出；被权限阻塞的修复运行副本与本轮合成服务使用限定PID的SIGTERM结束，三个自有PID均不存在；恢复原中文拼音输入源。没有唤醒旧线程、没有新增写入者、没有部署或付费调用。完整结论见 [validation.json](validation.json)。
