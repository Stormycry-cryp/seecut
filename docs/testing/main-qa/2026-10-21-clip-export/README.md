# 视频导出固定效果表：基础接口验证

状态：本地实现与相关无窗口检查通过。本批仅固定一次导出所用的效果表，尚未接通Agent视频导出。

## 已实现

`flatten_timeline_in_with_catalogue`使用调用方的同一Catalogue生成音频、普通视频和Layer滤镜链。`render_with_catalogue`继续使用该表构建画面效果、GPU计划及逐帧关键帧shader参数。原flatten与render接口保留，默认入口各捕获一次全局表；预览接口保持原协议。

没有复制第二套渲染主体，没有改UI、原生适配、依赖或MCP工具范围。模型尚不能通过新入口导出。

## 实际验证

- 工作区格式检查与concat-export all-targets严格Clippy通过。
- 同一实际测试程序全模块28项通过，0失败、0忽略；新增5项包含在28项内，不相加。
- 独立、未安装到全局的自定义Catalogue用于音频/视频/Layer解析与alias核对；空表反证可发现错误回退全局表。真实Project经原命令层构造，路径、时间轴、轨道、遮罩、文字排除及原工程不变分别检查。
- 实际编码12帧源短片，前6帧普通clip、后6帧Layer施加反色；显式表输出与旧render对照均完整软件解码，逐帧核尺寸、中心像素及总帧数。没有以编码能力不足跳过。
- 预取消、首个真实输出帧后取消、发布竞争与既有文件保护在两个入口检查，源文件字节不变，原临时目录清理继续有效。

这里的CPU证据指默认非GPU合成路径。源fixture显式关闭硬件编码；实际导出沿原hardware=true的硬件首选及软件回退，没有独立确定其具体编码器身份。GPU只核计划和shader uniform，没有实际GPU像素验收。

主代理核对三份live源码、实际程序与28项列表、全部命令日志和摘要。程序SHA256：`8fd5edc82a747d4adf121b1e7ac322db67ebf7dd62410c05a3e959881c56c867`。三份最终源码：

| 文件 | SHA256 |
| --- | --- |
| concat-export/src/lib.rs | c575bec609ba32f8c4e58905eaa3fb9397a2c9bc2ab08c37c11f9d4fb2c8fd76 |
| concat-export/src/resolve.rs | 571c50906027cccd4321b7fc3cf764289cbecdc9e6477af7a1477c924fc03bfa |
| concat-export/src/flatten.rs | 4a7681ca05641d9ef3b101fdf0e6bf13b766c82d0c00757101dc71e2d9245138 |

本机详细命令、退出码与原日志在`.toonflow/clip-export-catalogue-draft`，不作为源码清单的仓库文件引用。

## 未验证与下一步

本批没有冻结媒体、标题、遮罩、字体、LUT或自定义FFmpeg模板的外部文件；固定Catalogue不等于固定这些字节。尚未增加typed导出操作、原回执、成片读回证明、资产登记交付或Task长任务查询。

未运行App、操作鼠标/键盘/焦点、调用模型或验收实际作品/分发包。原生保存433项的证据属于此前保存版本，不能与本模块28项相加声称全应用测试通过。后续按[导出计划](../../../plans/2026-10-21-agent-clip-export.md)逐步接线，Android暂缓。
