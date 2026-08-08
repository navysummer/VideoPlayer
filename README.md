# 流影 · 全格式视频播放器

> 一款古风唯美风格的全格式视频播放器，由 Rust / Tauri v2 与 FFmpeg SDK 驱动。
> 采用水墨金玉配色与衬线字体，内置「关于」页，可一键打开本地文件、网络地址、直播流（m3u8 / http / rtmp 等）。

---

## 功能

- **全格式播放**：基于 FFmpeg SDK 在进程内解码/转码，覆盖绝大多数视频与直播流格式。
- **本地文件**：经系统文件对话框打开任何格式文件。
- **网络流媒体**：粘贴 URL（`https://` / `http://` / `rtmp://` / m3u8）即可播放。
- **拖拽打开**：把视频文件或网络地址直接拖进窗口即可播放。
- **内联转码**：不可直接播放的源会在进程内自动转码后送回播放器，无需安装任何外部 FFmpeg。
- **古风 UI**：水墨 / 金玉配色、衬线字体、点状纹样，顶栏 / 控制条 / 转码进度 / 弹窗全风格化。
- **关于页面**：设置 → 关于，含功能介绍与作者信息（navysummer）。

---

## 技术架构

- **框架**：Tauri v2 + **Vite 8 + Vue 3** 前端（Composition API，组件化）。
- **包管理**：pnpm。
- **FFmpeg**：`ffmpeg-next` crate（SDK 绑定）在进程内完成解码、探测与转码。
- **流服务**：`tiny_http` 在本机起 HTTP 服务，`/stream` 接口向播放器推送后端转码后的媒体流。
- **打包**：默认开启 `embedded-ffmpeg` feature，从源码静态构建并链接 FFmpeg 7.1（含 GPL 全功能），发布包不依赖目标机器安装 FFmpeg。Vite 8 基于 Rolldown 与 Oxc（JS 转换/压缩默认即 Oxc，无需额外安装）。

```
frontend(src/*、*.vue) ──►  Vite 构建(dist/) ──►  Tauri 窗口(WebView)
                                                        │
                                                        ▼
                                           ┌─ src-tauri/src/lib.rs（命令层：打开文件/URL/设置/外链）
                                           │
                                           ├─ probe.rs         媒体探测与可播放判断
                                           ├─ transcode.rs     进程内转码（视频/音频）
                                           └─ stream.rs        tiny_http /stream 本地流服务
```

---

## 环境准备

- Rust 工具链（msvc / GNU / Apple clang 均可）
- [nasm](https://www.nasm.us/)（源码构建 FFmpeg 需要）
  在 macOS 上：`brew install nasm`

> FFmpeg 源码构建默认从 `ffmpeg.org` 官方下载 `ffmpeg-7.1.tar.xz`。
> 如需离线/本地包，可设置环境变量 `FFMPEG_TARBALL` 指向本地 tar 包路径。

---

## 构建与运行

```bash
# 安装前端依赖
pnpm install

# 开发检查（Rust）
cd src-tauri && cargo check && cd ..

# 开发运行（启动 Vite dev server + Tauri 窗口，自动先跑 pnpm dev）
pnpm tauri dev

# 发布构建（先自动 pnpm build 前端，再编译并打包）
pnpm tauri build
```

> 首次构建会下载并源码静态编译 FFmpeg 7.1，耗时较长（视机器配置约 10~30 分钟）。

### 构建跨平台产物

| 目标 | 命令 |
| --- | --- |
| macOS（当前架构） | `pnpm tauri build --bundles app,dmg` |
| macOS x86_64 | `pnpm tauri build --target x86_64-apple-darwin --bundles app,dmg` |
| macOS arm64 | `pnpm tauri build --target aarch64-apple-darwin --bundles app,dmg` |
| macOS 通用二进制 | `pnpm tauri build --target universal-apple-darwin --bundles app,dmg` |
| Windows x86_64 | `pnpm tauri build --target x86_64-pc-windows-msvc --bundles msi,nsis` |
| Windows arm64 | `pnpm tauri build --target aarch64-pc-windows-msvc --bundles msi,nsis` |
| Linux x86_64 | `pnpm tauri build --target x86_64-unknown-linux-gnu --bundles appimage,deb,rpm` |
| Linux arm64 | `pnpm tauri build --target aarch64-unknown-linux-gnu --bundles appimage,deb,rpm` |
| iOS | `pnpm tauri ios build --no-sign` |
| Android | `pnpm tauri android build --apk` |

### macOS 平台提示

若链接时找不到系统库，可先导出 pkg-config 路径：

```bash
export PKG_CONFIG_PATH=/usr/local/lib/pkgconfig:/usr/local/opt/x264/lib/pkgconfig
```

---

## 目录结构

```
├── src/                    前端（Tauri 前端根目录 frontendDist）
│   ├── index.html          页面结构（顶栏/播放器/设置-关于）
│   ├── style.css           古风样式（水墨金玉配色/衬线字体）
│   └── main.js             播放控制、拖拽、设置、转码进度等逻辑
└── src-tauri/              后端
    ├── src/lib.rs          命令注册、外链打开（opener 插件）
    ├── src/probe.rs        媒体探测（FFmpeg SDK）
    ├── src/transcode.rs    进程内转码
    ├── src/stream.rs       本地流服务（tiny_http）
    ├── capabilities/default.json  权限声明
    ├── tauri.conf.json     窗口/打包配置（productName：「流影视频播放器」）
    ├── Cargo.toml          依赖（ffmpeg 别名 = ffmpeg-next，tauri-plugin-opener）
    └── icons/              应用图标（png / icns / ico）
```

---

## 打包

`src-tauri/tauri.conf.json` 已配置 macOS / Windows 全部打包目标，图标已生成
（`icon.icns` / `icon.ico`）。如无特殊需求无需手动再生成：

```bash
pnpm tauri build
```

产物位于 `src-tauri/target/{target}/release/bundle/`。
CI 已配置 GitHub Actions，打 tag（`v*`）或手动触发后会自动构建 Windows / Linux / macOS（x86_64+arm64 + universal）/ iOS / Android，产物发布到 GitHub Releases。

---

## 作者

**navysummer**

- GitHub：[https://github.com/navysummer](https://github.com/navysummer)
- 博客：CNBlogs（博客园）
- 微信：navysummer1001

## 协议

MIT
