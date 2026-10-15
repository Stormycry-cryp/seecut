# SeeCut MCP M2：移动已选图像层协议

此文记录阶段 3 的 IPC 与 stdio MCP 薄边界。可信授权、画布所有权、排队握手、去重和实际提交由运行中的 App 决定；协议代码及测试不代表原生交互或视觉验收。

## 身份与权限

适配器仍用 `--instance UUID --client CLIENT_ID` 指定唯一 App 实例和客户端。客户端 ID 为 1..=64 个 UTF-8 字节。`SEECUT_MCP_CLIENT_TOKEN` 必须存在，继续只用于 `capabilities`、`context`、`project`、`preview` 四个读工具。可选的 `SEECUT_MCP_WRITE_TOKEN` 为独立写凭据，只用于 `move_selected_image`；缺少它时，该工具直接返回 `notAuthorized`，不发 IPC。配置的 token 各为 36 字节。

读取授权 R / R+M 不会提升为编辑权限 E；两个环境变量不互相替代。写凭据必须来自 App 可信 UI 中针对当前工程、客户端及动作“移动已选图像层”的明确授权，租约为 5 分钟，可撤销；续期须再次经过可信 UI。调用不能授予、续期或恢复权限，App 重启、文档会话重绑或离开画布工作区后的旧授权失效；离开工作区只撤销租约，同文档会话的去重记录及最高序号保留。

MCP 的静态工具列表可列出具名动作。工具存在、适配器中有 token、operationId 合法均不证明当前有编辑权限；以 App `capabilities` 回应和每次调用的授权检查为准。适配器没有 grant / renew 工具。

## MCP 参数

`move_selected_image` 接收以下平铺参数，全部必填，额外字段拒绝：

| 参数 | 类型 | 含义 |
| --- | --- | --- |
| `projectId` | string | 显式预期工程身份，不得为空 |
| `documentSessionId` | string | 显式预期当前打开会话，1..=64 个 UTF-8 字节 |
| `revision` | u64 | 新操作的预期已提交文档版本 |
| `operationId` | string | 由认证客户端、会话及 clientSequence 精确派生 |
| `clientSequence` | u64 | 本客户端在本会话内递增的非零序号 |
| `selectionRevision` | u64 | 新操作的预期选择版本 |
| `objectId` | string | 稳定 image layer ID，规范形式 `layer:<非零 u64>` |
| `deltaX` | f64 | 文档像素 X 位移，有限且绝对值不超过 32768 |
| `deltaY` | f64 | 文档像素 Y 位移，有限且绝对值不超过 32768 |

例如，客户端 `abc`、会话 `12345678-1234-1234-1234-123456789abc` 的第 17 个操作：

```json
{
  "projectId": "opaque-project",
  "documentSessionId": "12345678-1234-1234-1234-123456789abc",
  "revision": 7,
  "operationId": "scm2:1:3:abc:36:12345678-1234-1234-1234-123456789abc:17",
  "clientSequence": 17,
  "selectionRevision": 11,
  "objectId": "layer:42",
  "deltaX": 4.0,
  "deltaY": -3.0
}
```

## IPC 请求

既有有界长度前缀 JSON 帧及 `Response { status, data? }` 格式保持。请求 `method` 为 `moveSelectedImage`。预期工程、会话和 revision 复用顶层字段，六个移动参数位于 `moveParameters`：

```json
{
  "instanceId": "12345678-1234-1234-1234-123456789abc",
  "clientId": "abc",
  "clientToken": "<independent-write-credential>",
  "method": "moveSelectedImage",
  "projectId": "opaque-project",
  "documentSessionId": "12345678-1234-1234-1234-123456789abc",
  "revision": 7,
  "moveParameters": {
    "operationId": "scm2:1:3:abc:36:12345678-1234-1234-1234-123456789abc:17",
    "clientSequence": 17,
    "selectionRevision": 11,
    "objectId": "layer:42",
    "deltaX": 4.0,
    "deltaY": -3.0
  }
}
```

`Request.move_parameters` 是有 serde default 的 `Option<MoveParameters>`，旧读请求可省略。`Request` 与嵌套 `MoveParameters` 使用 camelCase 并拒绝未知字段。四个读方法禁止非空 `moveParameters`；移动请求禁止非空 `contextRevision`、`offset`、`limit`、`maxEdge`。无关可选字段省略或 `null` 均表示未提供。移动请求缺失身份、版本或嵌套参数返回 `invalidInput`。

适配器及 IPC listener 在转交 App 前调用 `validate_request_shape`，检查写请求形状、规范身份和位移范围。它保留既有读方法的 App 身份/范围检查。App 仍须基于已经认证的客户端重新核验 operationId，并在同一 UI 提交点检查权限、实例、会话、所有权、选择及版本。

## 规范身份

`operation_id(client, session, sequence)` 输出：

```text
scm2:1:<client UTF-8字节长度>:<原始client>:<session UTF-8字节长度>:<原始session>:<规范十进制sequence>
```

两段身份均为非空、各最多 64 字节；sequence 为 1..=18446744073709551615。最终 ID 最多 192 字节。长度按 UTF-8 字节计数，保留原始字符串，不做 Unicode normalization。身份内允许冒号；长度避免分隔符碰撞。序号无前导零、加号或其他别名。App 重新生成整个字符串并作精确相等比较，不信任调用方提供的长度或版本。

`parse_layer_id` 只接受小写 `layer:` 加非零 u64 的规范十进制表示。`layer:0`、`layer:01`、`layer:+1`、空白、非 ASCII 数字和溢出都拒绝。`validate_move_parameters` 同时检查 operationId、objectId、非有限值和每轴位移界限；JSON 不能表达 NaN / Infinity，Rust 调用也在序列化之前拒绝这些值。

## 提交、重试与结果

App 将 XY 作为一次历史动作提交，成功回包状态为 `ok`，包含实际提交的坐标、`changed: true`、提交后的 `revision` 和 `selectionRevision`。实际 f32 坐标无变化时返回 `unchanged`（仍包含实际坐标及 `changed: false`），不创建 history、anchor、dirty 或 revision。现有 transform、linked mask anchor、结果范围及画布可编辑条件按阶段 3 计划检查，薄适配器不修改文档。

同一 operationId 的重试必须保留原始的全部请求参数和预期版本。App 先检查当前授权、实例、会话，再识别缓存操作；已成功请求的重试不能因为旧 revision / selectionRevision 被新鲜度检查误拒。相同 ID 异参数拒绝。App 保留本会话 highestSequence、最近最多 1024 个结果及最长 24 小时缓存；旧序号结果已淘汰时返回 `outcomeUnknown`，不得重新执行。撤权或租约到期后不能通过缓存绕过授权。

| 边界 | 适配器结果 | 操作含义 |
| --- | --- | --- |
| 缺少独立写 token | `notAuthorized` | 未发 IPC |
| 本地移动参数或形状非法 | `invalidInput` | 未发 IPC |
| 编码失败、连接失败、设置超时失败、首字节发送前失败 | `appUnavailable` | 请求未发送 |
| 写请求发送任意字节后发生发送错误 | `outcomeUnknown` | 适配器无法证明 App 未开始 |
| 完整写请求发出后超时、断连或收到不可解析回应 | `outcomeUnknown` | 保留原 operationId 及参数重试 |
| 收到完整可解析 App 回应 | 原样返回 | App 的状态为准，包括 `busy`、`unchanged` 等 |

适配器不会自动重试、递增 sequence 或取消文档动作。四个读方法仍保留原有传输语义：超时为 `busy`，断连或发送失败为 `appUnavailable`，其他回应解码失败为 `ioFailure`。IPC I/O timeout 为 10 秒；App 的 UI 等待和 queued / running / completed / cancelled 握手采用独立的 750ms 语义。只有 App 能证明 queued 取消获胜时才回 `busy`；已经 running 后的超时或掉线为 `outcomeUnknown`。

## 验证范围

本边界的测试覆盖 UTF-8 字节长度与原串保留、分隔符碰撞、零值/溢出/规范身份、未知和无关 serde 字段、非有限值/位移边界、独立凭据，以及发送前/部分发送/发送后传输状态。App 的可信授权、去重、单次提交、单次 undo、保存重开和真实窗口样式需要各自的当前证据。
