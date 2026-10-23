# 失败卡原因与可恢复输入

默认失败卡显示由错误码映射的短原因，移除卡片下方重复的“生成失败”。保留整卡详情入口，并增加信息图标。标题允许省略，原因留有左右12px边距；详情弹窗显示固定处理建议，完全不回显服务端 message 或未知 code。未推断未知 HTTP 失败一定可以重试，也不展示 URL、凭据或供应商内部消息。

“编辑输入”沿用原 task-refill 动作；保留缺失参考位、打开创建页、关闭详情、清除旧报价后按既有流程重新报价，不提交生成。无可恢复提示词的旧任务隐藏编辑按钮。选择失败任务不再走媒体下载分支或弹出无关的“尚未完成”错误。任务状态机、幂等键和服务端计费代码未改。

## 定向验证

结果与文件校验值见 validation.json。mapping-harness.rs 直接引用生产错误码模块；2项测试覆盖参考/服务/配置错误区分、结果待核实避免重复提交、任意错误码不回显。离线 failure-feedback 场景使用生产 task_item、render_tasks、server_task_snapshot 和 refill_task，检查 JSON 历史重开、安全文案、缺失参考位、编辑返回表单、原任务和 captured_credits 不变、不排队生成、无提示词旧任务拒绝回填。未运行全量测试或计入未执行的 concat 单测。

软件渲染截图仅使用合成输入；1000×760、900×640，两种主题，以及详情和旧任务无编辑入口。它们不是原生实窗截图，不能扩大为 Agent、Flow 或真实模型通过。已有原生按钮修复35a7600的点击复测继续待Documents许可，本次没有启动打包App、改变权限或换路径绕过拒绝。

复现（在src目录使用仓库固定Rust工具链）：

```sh
CARGO_TARGET_DIR="$PWD/../.toonflow/native-target" cargo build -p concat --offline
rustc --edition 2024 --test ../docs/testing/main-qa/2026-10-19-failure-feedback/mapping-harness.rs -o ../.toonflow/failure-feedback-qa/mapping-tests
../.toonflow/failure-feedback-qa/mapping-tests
cd ..
SEECUT_UI_PREVIEW_SCENE=failure-feedback SEECUT_UI_PREVIEW_DIR="$PWD/.toonflow/failure-feedback-qa/reproduce" .toonflow/native-target/debug/concat
```

初次命令从仓库根执行时误用了不完整的默认stable工具链，切到src后使用固定1.93工具链通过；没有安装或修改工具链。初次WIP fixture把仍有提示词的任务当成“不可恢复”，断言失败；修正为真正缺失提示词后复核，不改变产品恢复规则。原始开发日志保留在.toonflow/failure-feedback-qa。

## 版本边界

272bcf1对应已有原生实窗部分通过，证据文档提交28eb593；35a7600对应常驻移除按钮构建通过、实际点击待测。本轮source.json固定源文件hash和父提交，validation.json记录最终软件渲染二进制及PNG hash。旧11c7d8、272、35a封存包未变动。未部署、未配置凭据、未调用付费模型，远程CI由主协调处理。
