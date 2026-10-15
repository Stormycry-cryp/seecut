# GrsAI 媒体接入与凭据交接

本轮从 PR8 merge `6c9da07cc27a744783f8f38bafac62956229a881` 建立独立分支。PR8 的最终源提交 `6b587b67f153749ef19d133da5e2f0692b05c744` 是该 merge 的第二父提交。旧 C03 混合工作树、已安装应用、个人素材与历史工程不迁移、不清理。本文件记录实现和交接，不替代协调计划第 42 节。

## R0 覆盖核对

| 既有能力 | R1 使用方式 |
| --- | --- |
| 新 Toonflow 项目列表、保存及快照同步 | 保留 PR8 实现，未另建工程系统 |
| 原生认证、目录、报价、任务提交与原请求恢复 | 在同一服务和白名单桥增加 GrsAI；旧 Image2/Xiangxin 路由保留 |
| SeeCut 全局个人库与项目结果登记 | 继续使用原任务结果登记，下载失败只恢复原结果 |
| 服务端钱包、冻结与捕获账本 | 使用原累计计费父记录；节点只显示实际捕获数字 |
| 模型与强度策略 | 一种规范模型一个候选；策略独立选择，旧 route ID 可恢复 |
| 旧 C03 / Agent / MCP 混合 WIP | 不移植，不当成本轮已覆盖；R2/R3/R4 不在此提交内 |

## 模型与请求契约

目录加入 13 个图片型号和 2 个视频型号。`nano-banana-fast` 归一到 `nano-banana-2-lite`；CL/VIP/尺寸专用型号保留独立身份。维护中的 GrsAI Flare/Sunburst 不开放；既有 Image2 Flare 是另一条已配置路由，不因同名维护信息被改写。若显式 Image2 配置与新目录碰撞，保留既有路由，不静默换供应商。

- Nano：`nano-banana-2.1`、`nano-banana-pro`、`nano-banana-2` 为 1K/2K/4K；Lite 不发送 `imageSize`。CL/VIP 的尺寸按独立型号限定。`aspectRatio` 与尺寸独立。
- GPT：`gpt-image-2` / `gpt-image-2.5` 使用 1K 契约及 `quality=auto`；`gpt-image-2-vip` 使用目录列出的像素尺寸及 `quality=medium`。不向 GPT 发送 `imageSize`。
- `minimax-h3`：480p/768p 为 1–15 秒，1080p 为 1–10 秒；16:9/9:16 转为 landscape/portrait。
- `agnes-video-2.5`：720p/1080p/2k、4–12 秒、六种目录比例。
- 图片参考最多 16 张是本地保护上限；视频参考按当前契约最多 9 张图片、3 段音频，不接收视频参考。不宣称首尾帧角色、口型或生成音轨支持。
- 透明背景及 mask 虽见于供应商文档，本轮通用 UI/上传协议尚未开放；请求会拒绝未开放字段，不悄悄丢弃。

仅允许模型凭据发送到 `https://grsai.dakka.com.cn` 或 `https://grsaiapi.com`。提交为 `POST /v1/api/generate`、`replyType: async`；查询原任务为 `GET /v1/api/result?id=...`。下载结果不附带 GrsAI 或其他供应商凭据。

提交前写入状态，收到 ID 后先持久化再下载。没有可靠 ID 的未知结果不重发付费 POST；收到 ID 只查原编号。成功回执保留，下载失败不重新生成。失败/违规响应缺少可信退款凭证时保留待核对和冻结，不自动假设退款。

真实成本必须通过已有 `SECUT_MODEL_PRICE_CATALOG_JSON` 的 verified/verified_free 采购记录提供。GrsAI 不接受零价回退或销售价覆盖；按成本 ×3、1 元=10 积分，父任务累计精确金额取整后只捕获差额。UI 的 0 表示账本尚未捕获，不表示供应商免费。API 回执未提供实际采购扣费/退款凭证，真实对账仍需另行验收。

公开依据（2026-10-15 获取，模型 Skill 核对日期 2026-10-14）：[目录](https://grsai.com/zh/dashboard/models)、[Nano](https://qmy27nhsd9.apifox.cn/452392911e0.md)、[GPT](https://qmy27nhsd9.apifox.cn/452409160e0.md)、[Minimax](https://qmy27nhsd9.apifox.cn/514679297e0.md)、[Agnes](https://qmy27nhsd9.apifox.cn/523334530e0.md)、[异步结果](https://qmy27nhsd9.apifox.cn/452409577e0.md)。本地 Skill 的积分估算不能换算成已核实人民币采购成本。

## 非秘密部署交接

2026-10-15 只读确认：腾讯云上海 Lighthouse `lhins-rfa0t3du`（Stormycry，122.51.140.181），`seecut.service` 运行中；账户/组 `seecut`；工作目录 `/opt/seecut/server`；服务定义 `/etc/systemd/system/seecut.service`；环境文件 `/etc/seecut/server.env`，所有者 root:root，权限 600。当时工作目录指向 release `84f1165955cf12b5c6f84557a57d6a2d70bf9459`。这是目标核对，不表示部署本分支。

凭据、附件、私有 profile 均未读取。本轮未修改远端配置、重启或部署。后续需要用户/管理员：

1. 在已确认服务器的受信任秘密输入渠道中向该环境文件注入 `SECUT_GRSAI_API_KEY`，保持 root:root/600；不要粘贴到聊天、提交 Git 或从本机 Skill 自动搬运。
2. 选择 `SECUT_GRSAI_BASE_URL` 的官方 origin；补充各型号及尺寸/时长的已核实采购记录，并显式开启 `SECUT_MODEL_COST_PRICING_ENABLED`。本轮未代填采购金额。
3. 复核 `SECUT_PROVIDER_RESULT_HOSTS`；当前公共示例结果主机为 `file1.aitohumanize.com`，不使用任意通配域名。参考素材依赖既有可公网读取的 HTTPS 签名存储地址。
4. 另行确认部署、迁移与重启窗口；发布前备份数据库，核对当前线上 release。本轮只验证本地旧表约束迁移，不执行线上迁移。
5. 用户批准真实测试型号、素材和预算后，才做最小图片/视频调用、真实扣费及退款对账。此时重新读取官方七天额度；剩余不高于 50% 或不可读即暂停。

本机 Skill 的 `GRSAI_API_KEY` 与服务器的 `SECUT_GRSAI_API_KEY` 是不同配置边界；桌面和 Flow 网页不接收供应商 Key。

## 可复现的无秘密验证

`services/seecut-server/tests/test_grsai.py` 覆盖目录别名、能力拒绝、一次 POST、原任务恢复、下载重试、累计计费、凭据隔离和旧库迁移。`integrations/toonflow/tests/grsai-contract.test.ts` 覆盖一个模型多个显式策略及父任务积分去重。

`integrations/toonflow/tests/grsai-ui-fixture.py` 是仅绑定回环的合成验收服务，必须使用新的独立数据目录；不读取配置文件、不调用供应商。它使用测试账户、假采购价格和单像素图片，不能用来证明真实模型质量、实际费用、视频生成或生产部署成功。
