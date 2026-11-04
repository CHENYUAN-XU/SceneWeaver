# SceneWeaver 打包说明

## 目标

- Windows 一键安装或便携版。
- 最终用户无需安装 Node.js、Python、PostgreSQL 或 Docker。
- 原始素材保留在用户本地。

## 桌面安装包

使用 Tauri v2 内置打包器：

```powershell
npm run tauri build
```

输出：

- `src-tauri/target/release/bundle/msi/*.msi`
- `src-tauri/target/release/bundle/nsis/*.exe`

2026-11-04 已生成 Windows x64 MSI 和 NSIS 未签名安装包。MSI 行政解包成功，NSIS 归档检查通过。两个包均含应用 EXE、WebView2 加载器和 ONNX Runtime。干净系统的安装、卸载、升级仍待验证。

当前验证状态：当前代码的 `src-tauri/target/release/sceneweaver.exe` 与最新 `src-tauri/target/release/bundle/nsis/SceneWeaver_0.1.0_x64-setup.exe` 已成功生成。`src-tauri/icons/sceneweaver.svg` 是版本控制的发布图标源；Tauri 由它生成多分辨率 `icons/icon.ico` 并在 Windows 打包中显式使用。`build.rs` 会在 Tauri 校验资源前，从 Cargo.lock 锁定的 `webview2-com-sys` x64 依赖暂存官方 `WebView2Loader.dll` 到构建资源路径，因此干净工作目录的 `cargo test --no-run` 和打包不再依赖先前残留的 `target` 文件；`bundle.resources` 再将它安装到 EXE 同级目录。同一资源清单也会携带官方 ONNX Runtime Windows x64 CPU 的 `onnxruntime.dll`，供可选本地语义模型使用。2026-07-26 已将带新图标的当前安装器静默安装到专用临时目录（退出码 0），确认 EXE（58,777,785 bytes）、`WebView2Loader.dll`（160,320 bytes）和 `onnxruntime.dll`（14,897,976 bytes）三者同级，应用隐藏启动后存活超过 8 秒；测试安装目录已清理。该验证没有下载或使用 Edge。尚未在干净 Windows 环境完成交互式安装、卸载、升级和 MSI 验证。

## 侧车依赖

### FFmpeg / ffprobe

- 运行时依次检查应用同级的 `media-tools`、PATH 和常见 Windows 安装目录。媒体工具缺失时，本地素材库和关键词搜索仍可使用。
- 自行制作包含 FFmpeg 的安装包时，将 Windows x64 的 `ffmpeg.exe`、`ffprobe.exe`、所需 DLL 和该构建的分发资料放入 `src-tauri/vendor/ffmpeg/`，然后运行 `npm.cmd run tauri -- build --config src-tauri/tauri.with-ffmpeg.conf.json`。应用会从安装目录的 `media-tools` 读取它们。
- 视频短预览需要支持 `libx264` 的构建。语音转写需要支持 `whisper` 滤镜的构建及用户选定的本地模型。发布者应根据所选构建检查 [FFmpeg 分发说明](https://ffmpeg.org/legal.html)。

### 本地模型

- 默认模型体积应控制在可接受范围（< 500 MB）。
- 只有用户在设置页明确点击“下载模型”时才按需下载，提供状态和失败提示。
- SceneWeaver 当前使用配对的 CLIP 图文模型；ONNX Runtime 随安装器资源携带，模型权重不随安装器下载。
- 模型文件存放于 `{app_data_dir}/cache/models/`。

## 数据目录

- 数据库：`%LOCALAPPDATA%/SceneWeaver/sceneweaver.db`
- 缓存：`%LOCALAPPDATA%/SceneWeaver/cache/`
- 模型：`%LOCALAPPDATA%/SceneWeaver/cache/models/`
- 日志：`%LOCALAPPDATA%/SceneWeaver/logs/`

## 便携版

- 通过环境变量 `SCENEWEAVER_PORTABLE=1` 将数据目录置于可执行文件同级 `data/` 目录。
- 可在 USB 或外接硬盘上运行。

## 更新

- 使用 Tauri 更新器（可选）。
- 更新时不删除用户数据库和缓存。
