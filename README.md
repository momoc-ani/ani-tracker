# Ani Tracker

[中文](README.md) · [English](README.en.md)

Ani Tracker 是基于 Tauri 2 的本地优先追番、资源搜索、BT 下载与媒体播放应用，支持 Windows、macOS、Linux、Android 和 iOS/iPadOS。

桌面端提供完整业务、媒体扫描、转码、远程 HTTPS 网关和系统集成；移动端保留发现、追番、来源、搜索、内置下载、原生播放、提醒、设置与主题，仅排除远程 Web/网关、FFmpeg/FFprobe、转码和无移动语义的桌面能力。

> Copyright (c) 2026 Ani Tracker contributors. 本项目源码免费公开，仅限个人及其他非商业用途；未经版权所有者书面许可，禁止商业使用。

## 核心能力

- 新番发现：合并 Bangumi、AniList、Mikan 元数据，支持季度、月份、搜索与详情刷新。
- 追番管理：状态、单集、字幕组、自动下载、画质、编码、字幕语言和目录偏好。
- 资源搜索：RSS、Torznab、DMHY、Mikan、AniBT、ACGNX、Nyaa、ACG.RIP，含限流、缓存、熔断和候选评分。
- 下载：内置 libtorrent `torrent-core`、外部 qBittorrent Web API；桌面额外支持托管 qBittorrent-nox。
- 播放：桌面统一使用 `libmpv`，支持硬件解码、`gpu-next`、字幕与音轨、倍速、比例、续播、已看和自动下一集；Android/iOS 使用各自的原生播放器适配层。
- 画质增强：桌面支持 Anime4K、FSRCNNX、ArtCNN Shader、RIFE 插帧、Real-ESRGAN 动画超分、HDR 能力探测和实时掉帧降级；远程 PWA 支持独立 HLS/直传增强路径并在能力不足时回退原画。
- 自动化：来源增量同步、自动扫描、自动下载、本地通知和提醒中心。
- 主题：跟随系统、浅色、深色、内置主题及自定义主题导入导出，桌面和移动共用语义令牌。
- 桌面专属：FFmpeg/FFprobe、媒体扫描、远程 HTTPS 网关、远程 PWA、托盘、开机启动、外部播放器和文件管理器。

## 播放与画质增强架构

桌面本地播放已从 libVLC 调整为单一 `libmpv` 内核，避免同一会话在多个 PC 播放内核之间切换。播放链路如下：

```text
本地媒体
  -> libmpv 硬件解码
  -> gpu-next
  -> Anime4K / FSRCNNX / ArtCNN Shader 或模型增强
  -> 可选 RIFE 插帧与 HDR
  -> 字幕 / OSD 后合成
  -> 原生视频窗口
```

- Windows 使用原生窗口、D3D11 和 D3D11VA；macOS 使用 libmpv Render API；Linux 首期支持 X11/XWayland、Vulkan 和 VAAPI。
- `balanced` 与 `clear` 画质预设会根据资源可用性和播放负载启用；持续掉帧时按清晰度档位自动降级，播放器快照会记录实际档位与原因。
- RIFE 与 Real-ESRGAN 通过独立模型 sidecar 接入，实时插帧当前以 2x 为上限；模型、编码器或硬件能力不足时保持原始时间轴并回退。
- 远程 PWA 使用独立 ArtPlayer/HLS 会话。直传增强使用浏览器 WebCodecs/WebGPU 路径，当前仅在能力探测通过时尝试，失败会回到原始直传或 HLS。
- Android/iOS 继续使用平台原生播放器适配层，不加载桌面远程页面，也不打包桌面 FFmpeg/FFprobe 与转码资源。

## 架构

```text
React / TypeScript / Tailwind / shadcn UI
                  |
               AppClient
                  |
          Tauri invoke / events
                  |
ani-contracts / ani-domain / ani-repository
ani-storage / ani-sources / ani-downloads
ani-media / ani-automation / ani-remote
                  |
SQLite / torrent-core / libmpv / platform adapters
                  |
 Anime4K / model sidecars / remote HLS
```

业务服务依赖 `ani-repository` 中的 Repository Ports 与 UnitOfWork，不依赖 SQLite 类型。SQLite 是桌面与移动的默认本地 Adapter；未来 MySQL 应作为独立 Adapter 或服务端存储接入，不改变 Tauri commands、`AppClient` 或页面。

React 页面只通过 `AppClient` 访问业务能力。移动端不会回退到桌面远程页面，桌面远程 PWA 也不会获得本地命令权限。

## 技术栈

- Tauri 2、Rust 1.97、React 18、TypeScript、Vite
- Tailwind CSS、shadcn/ui 风格组件、lucide-react
- SQLite、Repository Ports、版本化迁移与备份
- libtorrent-rasterbar、qBittorrent Web API、qBittorrent-nox
- 桌面 `libmpv`、`gpu-next`、D3D11/VAAPI/VideoToolbox；Android/iOS 原生播放器适配层
- Anime4K、FSRCNNX、ArtCNN、RIFE、Real-ESRGAN；浏览器端 WebCodecs/WebGPU 直传增强
- 桌面 FFmpeg/FFprobe、ArtPlayer、hls.js
- pnpm、Cargo、Node.js `node:test`

## 关键目录

```text
src-tauri                         Tauri 宿主、commands、生命周期与平台装配
crates/ani-*                     Rust 契约、领域、仓库、存储、来源、下载和媒体核心
crates/tauri-plugin-ani-*        Android/iOS torrent、播放器和移动平台插件
src/renderer/src                 桌面与移动 React UI，以及桌面远程 PWA 页面
src/shared                       TypeScript 共享领域模型与契约
native/torrent-core              桌面 sidecar 与移动原生核心共用的 C++ 运行时
resources                        libmpv、增强模型、Shader、FFmpeg、qBittorrent 和许可证资源
archive/legacy-hosts             已退役 Electron/Capacitor 宿主，只读归档
docs                             架构、启动、发布、进度和专项计划
```

## 界面预览

### 新番发现

按季度、月份、评分和关键词浏览新番，查看 Bangumi、AniList、Mikan 等来源并直接加入追番。

![新番发现：季度新番浏览与搜索](assert/新番发现.png)

### 我的追番

集中管理追番状态、观看进度、下载进度、字幕组和单番规则。

![我的追番：观看与下载进度](assert/我的追番.png)

### 设置

统一管理主题、目录、语言与桌面集成、播放与媒体、下载核心和自动化规则。

![设置：主题与应用配置](assert/设置.png)

### 网页端

桌面应用可托管远程 HTTPS PWA，用于查看追番更新、下载任务、提醒和近期完成内容。

![网页端：远程 PWA 首页](assert/网页端.png)

## 环境准备

推荐 Node.js 22、pnpm 10.34.5、Rust 1.97.1。桌面原生依赖和移动工具链见 [启动说明](docs/startup.md)。

```powershell
pnpm.cmd install --frozen-lockfile
```

桌面播放运行时由 `libmpv` 提供。Windows 和 macOS 使用项目准备的固定运行时，Linux 首期使用系统 `libmpv`；开发机可按平台执行对应准备脚本。

## 常用命令

```powershell
# 桌面开发与构建
pnpm.cmd dev
pnpm.cmd build
pnpm.cmd run package:desktop

# Renderer
pnpm.cmd run dev:tauri:renderer
pnpm.cmd run build:tauri:desktop-renderers

# 桌面播放器与画质增强资源
pnpm.cmd run prepare:tauri:desktop-runtime
pnpm.cmd run verify:libmpv
pnpm.cmd run verify:pc-enhancement-resources
pnpm.cmd run test:player-matrix

# Android / iOS
pnpm.cmd run dev:tauri:android
pnpm.cmd run package:tauri:android
pnpm.cmd run dev:tauri:ios
pnpm.cmd run package:tauri:ios

# 门禁
pnpm.cmd run typecheck
pnpm.cmd run test:parsers
pnpm.cmd run test:theme
pnpm.cmd run test:rust
pnpm.cmd run lint:rust
```

`dev`、`build` 和 `package:desktop` 均以 Tauri 为唯一正式宿主。Electron/Capacitor 源码与依赖不参与当前构建；最后回退点和依赖清单见 [旧宿主归档](archive/legacy-hosts/README.md)。

## 平台边界

| 能力 | 桌面 | Android / iOS |
| --- | --- | --- |
| 本地 SQLite、追番、来源和搜索 | 支持 | 支持 |
| 内置 torrent-core | 支持 | 支持 |
| 外部 qBittorrent Web API | 支持 | 支持 |
| 托管 qBittorrent-nox | 支持 | 不适用 |
| 内置播放 | `libmpv` | 平台原生播放器适配层 |
| Shader、模型增强与实时降级 | 支持 | 按平台能力逐步适配 |
| 主题与本地通知 | 支持 | 支持 |
| FFmpeg、FFprobe、扫描与转码 | 支持 | 不打包 |
| 远程 HTTPS 网关与远程 PWA | 支持 | 不打包 |
| 托盘、开机启动和外部播放器路径 | 支持 | 不适用 |

移动应用在桌面离线时仍可独立完成发现、追番、搜索、下载、播放和进度回写。iOS 下载遵循系统后台限制，不承诺应用被挂起后持续传输。

## 远程访问

远程 PWA 仅由桌面 Tauri 应用托管。启用「设置 -> 远程设备」后，使用本地 CA、一次性配对码和设备令牌建立 HTTPS 连接。远程播放支持 Range、字幕、播放列表和 FFmpeg HLS 回退；移动安装包不会携带这套页面或网关。

## 发布

`.github/workflows` 提供 Windows x64、macOS x64/arm64、Linux x64、Android arm64 和 iOS arm64 发布工作流。正式发布要求对应签名凭据和平台运行时资源，详见 [发布说明](docs/release-build.md)。

## 当前验证边界

Rust 工作区、Clippy、格式、TypeScript、共享契约测试、主题检查和两个 Renderer 构建已纳入门禁。桌面 `libmpv` 动态加载、增强资源和策略门禁已有自动检查；指定 Intel Mac + AMD RX 6750 XT + Chrome 的 1080p MP4 直传增强已完成一次持续播放验收，但完整浏览器/GPU 矩阵仍待对应平台验证，不能用单机结果替代正式发布结论。

## 文档

- [总体设计](docs/design-plan.md)
- [实现进度](docs/progress.md)
- [启动与故障排查](docs/startup.md)
- [跨平台发布](docs/release-build.md)
- [播放器与画质增强计划](docs/player-video-enhancement-plan.md)
- [播放器画质增强验收](docs/player-video-enhancement-acceptance.md)
- [主题系统](docs/theme-system-progress.md)

## 版权、许可证与 MPV 来源

Ani Tracker 原创源码采用 [PolyForm Noncommercial License 1.0.0](LICENSE)。允许个人学习、研究、娱乐及其他非商业用途使用、修改和分发；必须保留 [NOTICE](NOTICE) 与许可证。第三方组件继续遵循各自许可证。

- MPV 项目：<https://github.com/mpv-player/mpv>
- libmpv 嵌入接口文档：<https://mpv.io/manual/stable/#embedding-into-other-programs>
- 本项目桌面运行时来源记录：[resources/licenses/mpv/SOURCE.md](resources/licenses/mpv/SOURCE.md)
