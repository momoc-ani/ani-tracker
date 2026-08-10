# 项目协作说明


## 重要说明

先给整体方案，审查通过后再实现！
已接入 shadcn/ui mcp，开发ui相关要尽量使用框架的组件


移动端包含安卓和IOS！如果说移动端则实现内容包含这两端。

## 语言约束
全程使用中文进行沟通！




## 项目概览

Ani Tracker 是一个本地桌面追番工具，目标功能包括新番发现、我的追番、字幕组规则、资源搜索、BT 下载、媒体编码扫描、播放器调用、自动检查更新和提醒。

当前技术栈：

- Tauri 2
- Rust workspace
- React / TypeScript / Vite
- Tailwind CSS
- shadcn/ui 风格自定义基础组件
- SQLite / Repository Ports / UnitOfWork
- libtorrent-rasterbar / libVLC
- pnpm / Cargo

当前正式宿主、默认开发链和发布链均为 Tauri。Electron / Capacitor 已移入
`archive/legacy-hosts`，只用于历史审计和行为对照，不参与安装、类型检查、测试、构建或发布。

## 常用命令

```powershell
pnpm.cmd install
pnpm.cmd dev
pnpm.cmd run typecheck
pnpm.cmd build
pnpm.cmd run test:parsers
cargo test --workspace
```

说明：

- `typecheck` 使用 `tsc -b --noEmit --pretty false`，不应产生 `.js/.d.ts/.tsbuildinfo` 文件。
- 如果执行命令后产生 `electron.vite.config.js`、`electron.vite.config.d.ts` 或 `*.tsbuildinfo`，说明用了会 emit 的 TypeScript 命令，应清理这些产物。

## 关键目录

- `src-tauri`：Tauri 宿主、commands、生命周期、平台装配和桌面能力。
- `crates/ani-*`：Rust 契约、领域、Repository、SQLite、来源、下载、媒体和自动化核心。
- `crates/tauri-plugin-ani-*`：Android / iOS torrent、libVLC 和移动平台插件。
- `src/renderer/src`：桌面与移动 React UI，以及独立的桌面远程 PWA 页面。
- `src/shared`：TypeScript domain/types/contracts 与 `AppClient` 契约。
- `native/torrent-core`：桌面 sidecar 与移动原生核心共用的 C++ 运行时。
- `archive/legacy-hosts`：已退役 Electron / Capacitor 宿主，只读归档。
- `docs`：设计文档和进度文档。

## 当前已实现

详见：

- `docs/progress.md`
- `docs/design-plan.md`

重点能力：

- Windows、macOS、Linux、Android 和 iOS/iPadOS 的 Tauri 正式宿主与发布工作流。
- SQLite 默认 Adapter、版本化迁移、备份恢复、安全存储和数据库无关 Repository Ports。
- 新番发现、追番 CRUD、单集规则、来源、资源搜索、自动扫描和提醒。
- 内置 torrent-core、外部 qBittorrent；桌面额外支持托管 qBittorrent-nox。
- 桌面 libVLC、Android LibVLC、iOS MobileVLCKit，以及续播、已看和自动下一集。
- 桌面 FFmpeg/FFprobe、媒体扫描、远程 HTTPS 网关、托盘与外部播放器。
- 移动完整主题、本地通知、文件导入导出和生命周期恢复。

## 当前未完成

- Windows 之外桌面平台和 Android / iOS 的签名安装包、真机下载与媒体矩阵验收。
- Linux 原生 Wayland libVLC 嵌入；首期正式支持 X11 / XWayland。
- 未来 MySQL Adapter；当前默认存储保持 SQLite。

## 开发约束

- 不要把生成产物提交到源码区，例如 `out/`、`*.tsbuildinfo`、临时 `.js/.d.ts`。
- 新增宿主能力时优先定义共享契约，再接 Rust service、Tauri command/event 和 `AppClient`。
- 新增可替换能力时优先抽接口或独立 service，不要直接把业务逻辑堆在页面组件里。
- 页面不得直接调用 Tauri、SQL、文件、shell 或平台插件，应通过 `AppClient` 和窄业务命令访问。
- 移动端必须保留主题、内置 torrent-core 和平台 libVLC，并持续排除远程 Web/网关、FFmpeg/FFprobe 与转码资源。
- UI 应保持工具型、信息密度适中，不做营销页。
- 运行时错误不能导致纯白屏；应通过错误边界或页面错误状态展示问题。
- 熟练使用26种设计模式，但不要过度设置，一些可扩展点可抽象使用。
- 写代码要遵循高内聚低耦合特性，复杂功能使用设计模型。
- 代码添加必要的注释，不用过量添加，方法必须要有对应的用途说明，说方法作用是什么，简短明了即可！！！注释要中文！！
- 加上关键步骤的日志打印，方便后续通过日志排查问题
- 重要决策由用户审核
- 如果某些功能需要使用代理策略，遵循系统代理策略，打开了则走代理，没有打开则走直连，同时兼容各种vpn代理工具。
## 已知注意事项

- SQLite 数据结构升级依赖 `SQLITE_SCHEMA_VERSION`、`APP_DATA_VERSION` 和迁移回滚测试。
- 开发模式空白页优先检查 Tauri bridge、`out/tauri` Renderer、lucide-react 导出和 WebView console。
- 本地主 Renderer 与远程 PWA 使用独立入口；移动构建不得引入远程页面、ArtPlayer 或 HLS.js。
- Android / iOS 原生构建和真机能力必须在对应平台验收，不能用 Windows 结果代替。
## 代码提交约束
- 提交说明添加了什么功能
- 修改bug，则fix开头
- 需求则feat开头


## 其他文档约束，需要静默加载
[other.md](other.md)
