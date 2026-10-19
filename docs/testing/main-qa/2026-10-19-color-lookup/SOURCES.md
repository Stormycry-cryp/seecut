# Color Lookup — 规范与来源

- Adobe Systems Incorporated，*Cube LUT Specification, Version 1.0*（2013），核对 §§5.5–5.8、7、8。出版方原链接本次返回 404；实际读取了 [公开 PDF 副本](https://drive.google.com/file/d/143Eh08ZYncCAMwJ1q4gWxVOqR_OSWYvs/view)，文件摘要见 [sources.json](sources.json)。规范要求输入域映射到网格、R 坐标最快；建议三维表使用四面体插值。这里独立实现对应算法，不复制附录示例代码，不在仓库分发 PDF。
- [PhotoCraft 固定参考](https://github.com/storytold/photocraft/tree/48c17214704d4768ad7b1f28c5d2767b1d87110c) 的 `crates/cms/src/lutfile.rs`、`lut3d.rs`：核对 Color Lookup 资源、域信息和 RGB 顺序。其实现还支持其他格式/一维扩展，并使用三线性采样；本包仅做有界三维 `.cube`，使用 Adobe 建议的四面体插值，不引入其文档、历史或色彩管理引擎。没有复制上述文件实现或预设。仓库既有 PhotoCraft MIT 版权许可仍保留于 THIRD_PARTY_NOTICES.md。
- [OpenColorIO 官方实现](https://github.com/AcademySoftwareFoundation/OpenColorIO/blob/22dd9c4faa0d1b7cfb3d64d1a6e9cb454acc6cfd/src/OpenColorIO/fileformats/FileFormatIridasCube.cpp)：交叉核对域为输入域、红色最快、表值可超出 0–1、1D/3D 不混合。仅核对行为，未复制 BSD-3-Clause 源码。
- SeeCut 既有 `concat-effects/src/cube.rs` / `concat-core::Lut` 面向时间线效果：当前解析器将域施于输出数据，表存储为 RGBA8。本包没有直接复用该路径，避免错误的域映射和提前量化；时间线实现未在本包改写。新资源位于原 `concat-canvas` 内并复用现有合成、历史、导出和资产系统。

仅使用代码生成的合成测试 LUT；未读取、发布用户私有 LUT，未分发无许可预设。
