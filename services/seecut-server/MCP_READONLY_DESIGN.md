# SeeCut 生产数据只读 MCP

## 目标与边界

该服务让受控的 ChatGPT 连接查询 SeeCut 生产业务统计和有限的任务运行状态。它独立于业务 API 和生成 worker，通过 SQLite 一致性快照读取数据，不进入生产请求链路。

“所有真实数据”指所有已批准业务域的真实聚合数据，不代表开放所有表和字段。服务不提供任意 SQL、资源读取、写工具或用户枚举。

## 可查询数据

| 工具 | 返回内容 | 限制 |
| --- | --- | --- |
| `get_system_summary` | 用户、团队、有效团队资产、生成任务、订单总数 | 仅聚合 |
| `get_generation_summary` | 按状态、类型、供应商统计任务数与报价积分 | 1-90 天 |
| `get_asset_summary` | 按媒体类型统计有效团队资产数和字节数 | 1-90 天 |
| `get_commerce_summary` | 订单状态、积分账本类型、钱包余额聚合 | 1-90 天，无用户与交易标识 |
| `get_generation_task_status` | 指定任务的类型、模型、状态、错误码、时间与尝试次数 | 必须提供完整任务 ID |

以下字段永不返回：邮箱、密码哈希、session 和邮件 token、用户 ID、团队成员、提示词、请求 JSON、供应商原始响应、供应商任务 ID、错误详情、支付交易号、对象键、文件名、文件哈希、IP、API key 和私钥。

## 数据与性能隔离

`seecut-mcp-snapshot.timer` 每五分钟调用 SQLite backup API 生成一致性副本，再用原子替换发布。备份按页复制并主动让出执行时间。MCP 进程仅以 `mode=ro&immutable=1` 打开快照，启用 `query_only`，没有主库路径和写权限。

默认拒绝超过 15 分钟的旧快照。每个查询限时 1.5 秒，响应不超过 256 KiB，请求不超过 64 KiB，每个 key 默认每分钟 30 次。systemd 另限制 CPU、内存和进程数。生产可根据观测降低频率；不得通过扩大主库权限解决快照失败。

## 鉴权与审计

MCP 使用独立 Bearer token。token 放在仅服务账号可读的文件中，权限必须为 `0600`，不得写入环境模板、Git、Nginx 日志或审计日志。轮换时生成至少 32 个随机字符的新 token，更新 ChatGPT 连接后撤销旧 token。

每次请求写 JSONL 审计记录，包括时间、request id、key id、工具名、参数规范化哈希、状态、错误类别、结果行数和耗时。审计不记录参数原文、token、SQL、邮箱、提示词、对象键或响应内容。日志应接入现有受限日志采集和保留策略。

## 信任边界与威胁

1. 公网到 Nginx：TLS、固定 `/mcp` 路由、请求体限制、限流；Nginx 日志只记 URI，不记查询和 Authorization。
2. Nginx 到 MCP：仅 loopback；MCP 再次验证 Bearer token，使用常量时间比较。
3. MCP 到快照：独立低权限账号，只读文件权限；固定 SQL 和参数绑定。
4. 快照生成器到主库：使用现有 `seecut` 账号的数据库读取能力，只写专用快照目录。

主要滥用场景包括提示注入诱导越权、任意 SQL、通过明细重识别用户、昂贵查询拖慢生产、token 泄漏和审计日志二次泄漏。对应控制是固定工具白名单、聚合优先、敏感字段永久排除、按完整业务 ID 的单条查询、独立快照、资源上限、独立凭据和脱敏审计。

## 部署步骤

1. 创建 `seecut-mcp` 系统组和账号、`/var/lib/seecut/mcp` 与 `/var/log/seecut`，设置最小目录权限。快照任务以 `seecut` 用户读取主库、以 `seecut-mcp` 组创建 `0440` 快照；MCP 账号只能通过组权限读取快照。
2. 用密码学随机源创建 `/etc/seecut/mcp.token`，属主 `seecut-mcp`、权限 `0600`。
3. 创建 `/etc/seecut/mcp.env`，只配置监听地址、快照路径、token 文件路径、审计路径、key id 和资源限制。不要放业务服务密钥。
4. 安装并启用 `seecut-mcp-snapshot.timer`，先确认快照完整性、年龄和权限。
5. 安装 `seecut-readonly-mcp.service`，仅监听 `127.0.0.1:8799`。
6. 为 MCP 使用独立 HTTPS 主机名并应用 `deploy/nginx.mcp.example`。在网关层进一步限制来源时，以 ChatGPT 官方当前出口和认证要求为准。
7. 在网页版 ChatGPT 中登记 HTTPS `/mcp` 地址和独立 token，逐项调用工具，检查审计记录与响应脱敏。

生产上线前必须确认 ChatGPT 当前远程 MCP 认证要求。若平台要求 OAuth，应在 Nginx 前增加符合要求的 OAuth 网关，并将 MCP Bearer token 保留为网关到服务的内部凭据。不得为了兼容平台而公开无认证端点。

## 本地验证

测试只使用合成 SQLite 数据，不连接生产数据库：

```bash
cd services/seecut-server
PYTHONPATH=src python3 -m unittest tests.test_readonly_mcp -v
```
