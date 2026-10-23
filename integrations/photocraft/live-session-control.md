# 实时 Session 私有控制合同

这些方法运行在完整编辑器的 UI 控制回调中，操作该窗口持有的真实 `Session`。它们尚未接入对模型公开的 SeeCut MCP 或 Task，也不授予权限。内部宿主调用前，仍须完成 SeeCut 的账号、会话、文档、精确调用、目录、预算和取消检查。

## 身份与读取

宿主为每次启动生成新的 `hostInstanceId`，通过可信环境传给自己的子编辑器。未绑定或非法绑定时，全部私有方法返回 `hostBindingUnavailable`。请求不能提供绑定值来启用端点。

`seecut.state {}` 返回有界文档和图层元数据、选区是否存在、历史状态、繁忙原因与回执缓存状态。返回明确标记 `atomicPixels:false`；视口几何来自上一轮布局，不能和另一次截图拼成原子观察。选区边界返回 null 与 `selectionBoundsMeasurement:notSampled`，不会在 UI 线程扫描选区像素，也不将未采样当成空选区。最多列出 256 个图层及 256 个已选图层，名称最多 256 个字符，并报告截断。

文档身份包括 host、原生 DocId、`documentGeneration` 及 revision。进入 Session、另存为到不同原始路径或解除文件关联时，generation 会改变；同路径普通保存保留 generation。关闭重开保留 DocId 时也会换代。generation 耗尽时保留已经发生的保存事实并置 0，私有方法明确拒绝使用，不回绕。原生 u64 身份在 Rust 内核对；未来经过 JavaScript/MCP 时必须转换为无损的字符串或不透明标识，不能用 JavaScript Number 传递。

`document.backing` 只报告真实 Session 的四项事实：`nativePath`、`pathStatus`、`sourceReadOnly`、`savedRevision`。路径最多 4096 字节，空串、NUL 或超限时返回 null 与明确状态，不截断、不猜标题。`recorded` 不代表文件已核验，`savedRevision` 不代表独立保存代次；UI 回调不访问文件系统。父级后台的封闭读取和文件核验见 [native-host.md](native-host.md)。

## 封闭写操作

`seecut.execute` 严格拒绝未知字段，必填字段如下：

```json
{
  "operationId": "task-operation-1",
  "expected": {
    "hostInstanceId": "8e88a522-e405-40cd-9ef0-56b530ec0c21",
    "docId": 1,
    "documentGeneration": 1,
    "revision": 1
  },
  "action": { "kind": "layerSetOpacity", "layerId": 2, "opacity": 0.4 }
}
```

首批只开放普通图层透明度与 `{"kind":"undo"}`。透明度限定有限数值 0..1；背景图层的隐式转换被拒绝。撤销需要上层单独授予的精确调用许可。两者调用真实 `Session.execute`，保留原有 automation policy 和临时 Session 授权，不通过合成点击执行，也不隐式提交用户的临时编辑。

文档、generation 或 revision 不符，正在拖动、输入、预览、打开对话框、处理后台任务，或开启脚本事件时拒绝执行。参数先校验，合法操作才保留回执位置。已执行、已拒绝及引擎失败均记录 before/after 状态；外层 `ok:true` 表示已得到业务回执，调用者必须继续检查 `result.status`，不能据此报告修改成功。

## 回执与未知结果

`operationId` 限定 1..128 个 ASCII 字母、数字或 `-_.:`。相同 ID 与相同完整类型化参数只返回原回执，即使后来手工编辑、撤销或切换文档；参数不同则拒绝。缓存最多 1024 条，不淘汰旧 ID，达到容量后拒绝新操作。

`seecut.receipt {"operationId":"task-operation-1"}` 只查询原记录。未命中或仍在执行返回 unknown；全部结果都标记 `safeToResend:false`。缓存仅在当前编辑器内存中，重启不能恢复。超时、断线、错误回复或引擎失败都不构成自动重发授权；上层必须查询原操作并重新读取文档，需要新修改时使用新的独立许可。

## 当前验证

主代理实际运行固定源码和受控补丁的 Rust 1.95 测试：15 项私有控制测试、1 项 generation 耗尽测试、2 项主题测试通过。控制测试使用真实 Session、文档、历史和原控制入口，覆盖重复回执、独立撤销、旧版本、关闭重开、切换文档、繁忙拒绝、容量、授权及参数边界。曾发现 Serde unit variant 忽略撤销的额外字段，改为空 struct variant 后原失败用例通过。

五补丁版本另已通过 38 项 UI 回归和 2 项真实控制套接字期限回归，包含上述控制测试及实际绘制分支的采用帧读取。读取合同见 [adopted-snapshot.md](adopted-snapshot.md)。这些结果不证明原生窗口、SeeCut 上层权限接线、模型调用或完整功能已验收；交付前身份核验和真实编辑器进程联测另行完成。
