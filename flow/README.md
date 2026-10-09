# SeeCut Flow 独立审查候选

原生 Slint 节点画布 + Rust 图模型、编辑命令、持久化及确定性执行器。此候选只运行合成工程；**没有连接真实生成、账户积分或真实资产库**。库入口和生成入口的宿主合同已实现，正式接线由主线串行完成。完整 76 项 Toonflow 范围保留，正式验收仍为 0/76。

- 工作树：`/Users/chenyunzhe/Documents/Codex/2026-10-09/task-2/Seecut-flow-p2`
- 分支：`codex/seecut-flow-p2-isolated`
- 父基线：`1e8009ea9ea7e90c2ceb8720037711f02970b923`
- 唯一改动：新增 `flow/**`；共享 App、Cargo、Theme、账户、正式计划均未修改。
- 接口和文件归属见 [OWNERSHIP.md](OWNERSHIP.md)，验证与缺口见 [HANDOFF.md](HANDOFF.md)。

## 本机运行

```sh
cargo +1.93 test --offline --manifest-path flow/core/Cargo.toml
cargo +1.93 build --offline --manifest-path flow/core/Cargo.toml
# 仅传一个尚不存在的合成工程目录；不会覆盖已有文档。
flow/core/target/debug/seecut-flow-core demo /tmp/seecut-flow-new-fixture
```

原生交互探针使用本机已有 QA 环境，无需安装依赖：

```sh
/Users/chenyunzhe/Documents/Codex_Project/SeeCut/docs/testing/main-qa/2026-10-09-assistant-native/.venv/bin/python \
  flow/qa/native.py /tmp/seecut-flow-new-native-fixture --dark --width 1280 --height 800
```

探针仅以 stdin/stdout 调用 Rust，不监听网络。使用临时合成目录，勿把真实用户工程传给它。图片为 `qa/synthetic.svg` fixture；视频输出是结果清单，不是可播放视频。

在交付节点点「运行到这里」，会分别暂停在图片和视频的采用节点。选择当前批次候选后，再运行到交付。未知提交用「核实结果」恢复，不能反复提交；发生确定失败后才可明确重试。点端口先选输出再选输入；未选择输出时点已有输入断开最后一条连线。生成输入区显示连接的提示词；编辑需选对应提示词节点。右下圆钮打开宿主生成设置回调，探针只报告待主线接线。参考区加号打开明确标注的合成资产库；本地上传按钮不读取真实文件。

提交前需复核官方 `rateLimitsByLimitId.codex` 的 10080 分钟周窗口。剩余严格低于 40% 或不可读取时，保存断点并暂停新实现和测试。
