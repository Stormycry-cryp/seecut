# R4 / C03 可编辑画布文字

本页保留首轮交付记录。最新对齐与像素字号修复见 [repair/README.md](repair/README.md)，初版候选和证据未覆盖。

从干净 R3 `d546262ad6fc28016b8e47f411ba250f916c6b69` 整合。旧候选位于顶层验收目录 `2026-10-12-agent-canvas-mcp-loop/public-candidate`；477 个文件与 `pane-6` / `native-2` 记录完全匹配，原目录未改写。仅提取文字相关差异，旧原生 Flow、Agent、服务和旧锁文件未带入。

## 行为

- 原生画布文字按钮创建；双击重编辑中英文多行文字。编辑框跟随对象位置；完成或取消共用现有图层事务，切换页面、工具或图层进入既有未提交确认。
- 字体、字号、颜色、粗斜体、对齐、描边、阴影、换行宽度、行距和字距。内容输入一次撤销；独立样式修改独立记录。文字框和颜色框持有编辑快捷键。
- 复用 concat-text、ImageLayer、LayerTransform、CanvasSnapshot 和 PixelStore；文字与向量源互斥，预览像素是派生缓存。复制使用独立像素与文字源；变更清除旧命中范围缓存。
- 缓存系统字体和字形覆盖，保留请求字体名称；中文换行与跨字体回退，缺失字符显式报错。没有新增渲染引擎、字体上传或逐键 PNG 编码。
- 为画布启用 serde_json 的精确浮点往返，修复旋转值重载后因 1 ULP 偏差导致的不可变导出元数据误判；保留同一导出回执重复登记测试。
- 含文字项目写 v3，继续读 v1/v2；重开从可编辑源重建缓存。沿用正式 PNG 导出、资产登记去重与现有画布/剪辑素材入口。
- 保留 R3 的嵌入式 Flow、云冲突处理和画布灵感推荐；Jev 从自有提示词选择、GLM 5.3 Flash 按需改写不在本包重复实现。

## 证据与限制

源码提交 `009bfa134545a15b205587eaa131b3bb531edf73`。21 项针对性测试通过：字体 9、文字数据/GPU 3、原生文字 4、形状回归 3、旧项目 1、混合导出入库/复用 1。

精确源提交、命令、结果和产物 SHA 见本目录 validation/source-comparison/candidate-manifest。测试中的文本来自合成提交字符串，不等于真实中文输入法预编辑；导出 PNG 是渲染产物，不是应用截图。

当前环境没有可用的实际窗口/CUA工具。真实 IME、快捷键焦点、窄窗口布局、点击导入剪辑和跨进程 App 冷启动仍未验收；不据单元测试或 PNG 伪称通过。未启动 Astra 审核，未部署或请求任何付费模型。

## 来源与许可证

文字候选沿用仓库 AGPL-3.0-or-later；新模块保留 SPDX。concat-text 使用既有 fontdb/rustybuzz/ttf-parser/tiny-skia；新增引用均为工作区已有库及 serde，无新第三方库版本。字体由本机系统发现，不复制、提交、上传或重新分发系统/用户字体。成品沿用现有 Agent/Toonflow 许可和来源清单。


## 复核入口

- `mixed-editable.comp/` 是图片、C01 形状和中英文文字组成的可编辑 v3 项目；`mixed-text.png` 是对应导出像素。
- `export-receipt.json` 保存合成测试的唯一资产身份和 SHA；原始隔离库位于 `.toonflow/c03-review-fixture-v2/portable/personal-library/`。
- `fixture-before-fix.log` 是发现浮点元数据偏差的失败记录，修复后的 `fixture.log` 通过；没有删掉失败历史。
- 原生测试在复制的测试可执行文件旁创建独立 `portable/` 后运行，不改用户的资产库或最近项目。`native-results.json` 记录精确过滤器。混合产物测试需 `SEECUT_C03_FIXTURE_DIR` 指向该隔离可执行文件目录，并用 `--exact --ignored` 显式执行。
- 字体测试首次原始输出留在任务工具记录；`source-comparison.json` 记载命令与结果，没有为补日志重复运行。

候选：`.toonflow/SeeCut-Canvas-Text-QA.app`，独立 portable 数据目录，测试 API 为 `http://127.0.0.1:18743`；原生构建、深度签名和 Agent 清单校验通过。未安装、启动或连接生产。Web/Server/六节点产物逐文件确认与 R3 候选相同，未重复构建。
