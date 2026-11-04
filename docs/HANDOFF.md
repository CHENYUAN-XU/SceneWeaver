# SceneWeaver v0.1.0 阶段收尾

截至 2026 年 11 月 4 日，SceneWeaver 暂停主动开发。这个版本保留为 Windows 桌面预览，可用来导入本地素材、检索镜头、整理选片并导出交接文件。

## 从哪里开始

1. 从 [GitHub Releases](https://github.com/CHENYUAN-XU/SceneWeaver/releases) 下载 Windows x64 安装包。提供 NSIS 和 MSI 两种格式，均未签名。
2. 启动后选择本地素材文件夹，设置快速、平衡或精确索引并开始扫描。素材库卡片可开启自动增量扫描。
3. 用关键词、参考图、实体或 Must／Should／Must Not 条件查找素材。视频可打开镜头面板查看时间范围，再把素材或镜头加入选片集合。
4. 在选片集合内调整顺序、标签、备注、评分和推荐入／出点，再导出 CSV、JSON、EDL、FCPXML、PNG 或离线 HTML 联系表。

原始媒体保留在原路径。应用把索引和选片数据写入本地 SQLite，缩略图和短预览写入本地缓存。

## 可选的媒体工具

- 安装本机 FFmpeg 和 ffprobe 可启用视频元数据、缩略图、镜头切分与预览。默认安装包依赖本机工具，包含 FFmpeg 的构建方法见 [打包说明](PACKAGING.md)。
- 素材卡片的“文字”面板使用本机 Tesseract 和语言包识别画面文字。语音转写需要支持 Whisper 滤镜的 FFmpeg，以及用户选择的本地模型。两类文字结果都可进入本地搜索。
- 本地 CLIP 图文检索需在设置中主动下载模型并重建语义索引。关键词、基础视觉相似和实体名称检索开箱可用。

## 验证记录

- 前端 lint、25 项测试和生产构建通过。Windows GNU 环境的 Rust 33 项单测通过。
- 核心测试覆盖 1,000 文件首次与增量扫描、SQLite 迁移、文字索引失效和视频派生。隔离桌面测试覆盖暂停与崩溃恢复、自动扫描、合成图片 OCR、合成语音转写、搜索和选片导出。
- FCPXML 1.10 通过 Apple DTD 结构校验。MSI 行政解包、NSIS 归档完整性及安装包内的 EXE、WebView2 加载器和 ONNX Runtime 检查通过。

## 恢复开发时

- 用有标注的真实素材评估中文图文检索、OCR、转写和 ACG 识别。自动角色与场景识别及智能选片助手仍待实现。
- 在 DaVinci Resolve、Premiere Pro 和 Final Cut Pro 中验证实际导入。在干净 Windows 环境验证安装、卸载和升级。
- 开发启动和验证命令见 [README](../README.md)，实现记录与边界见 [项目进度](PROGRESS.md) 和 [已知限制](KNOWN_LIMITATIONS.md)。
