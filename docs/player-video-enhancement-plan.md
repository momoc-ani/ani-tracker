# PC 内置播放器与画质增强执行计划

> 当前实现边界（2026-08-14）：桌面 Windows/Linux/macOS 已统一使用 libmpv，接入 gpu-next/Render API、平台硬解路径、Anime4K Shader、字幕后合成、掉帧自动降级和双窗口同步；PC 播放路径不再保留 libVLC。远程 HLS 已按 NVENC/AMF/QSV/libx264 探测普通转码编码器；模型链已接入 RIFE 插帧和 Real-ESRGAN 2x 动画超分，采用 `FFmpeg RGB24 解码 -> 可选 RIFE -> 可选 Real-ESRGAN -> FFmpeg 编码`，并通过受认证会话诊断报告实际模型与降级原因。RIFE 已增加按源帧率、模型滚动 P95、显存、60 FPS 输出上限、80% 利用率和 5 ms 链路余量计算的容量门禁，当前协议因 timestep 固定为 `0.5` 而硬限制为 2x；运行时超预算会关闭模型并维持既定时间轴。macOS Intel + AMD RX 6750 XT + MoltenVK 已完成真实 sidecar 握手：Real-ESRGAN 最新 warmup `24.46 ms`；RIFE 最新 warmup `25.99 ms`。这些低分辨率 warmup 只可作为启动样本，真实分辨率滚动 P95 和完整播放矩阵未通过前不能标记为实时插帧验收通过。F5-A WebCodecs/WebGPU 能力探测已接入但仍只记录诊断；本地 libmpv 模型链、HDR 原生输出、跨 GPU 真机证据和远端直传 shader 管线仍未完成。

正式模型包、HDR、远程输出和真实 GPU 的逐项执行与证据要求见 [播放器终版发布验收清单](./player-video-enhancement-acceptance.md)。

## 目标架构

首版播放链路：

```text
本地视频 -> libmpv 硬件解码 -> gpu-next -> Anime4K shader -> 字幕/OSD -> 原生窗口
```

PC 播放路径只运行 libmpv：Windows 使用原生 HWND `wid`、D3D11 和 D3D11VA，Linux 使用 X11/XWayland、Vulkan 和 VAAPI，macOS 使用原生 Render API 表面；不把 macOS 的 `wid` 嵌入限制扩散到其他平台。动态库或初始化失败时报告结构化错误，不能偷偷启动第二个 PC 播放内核。Android、iOS 和远程 Web 播放器继续使用各自的实现。

最终本地链路：

```text
输入 -> 硬件解码 -> 能力/负载调度 -> Anime4K 或模型增强 -> 可选插帧/HDR -> 字幕/OSD -> 显示
```

当前远程模型链路：

```text
输入 -> FFmpeg RGB24 解码 -> 可选 RIFE -> 可选 Real-ESRGAN 2x -> FFmpeg 编码 -> HLS -> 远程播放器
```

远程增强的模式边界：

- `direct` 只传输原文件，远端服务不解码视频像素，因此不能执行 Anime4K、FFmpeg 画质滤镜、Real-ESRGAN 或 RIFE；增强字段必须保持关闭。
- `transcode` 在主机侧解码并重新编码为 HLS，才可以执行 FFmpeg 滤镜或模型帧管线。后端会再次校验模式，伪造直传增强请求会被拒绝。
- F5 将新增浏览器端“直传 + 终端增强”路径：WebCodecs 负责视频解码，WebGPU/WGSL 负责 Anime4K 类 shader，字幕在增强后的画布上叠加；能力不满足时回退原始直传或实时转码。
- F5 首批不在浏览器端运行 RIFE/Real-ESRGAN；浏览器模型推理需在 F5 的音视频时钟、显存预算和 30 分钟稳定性通过后另立模型阶段。

目标远程链路：

```text
输入 -> 本地硬件解码 -> 增强/插帧/HDR -> 厂商硬件编码 -> HLS/SRT/WebRTC -> 远程播放器
```

目标远端直传终端增强链路：

```text
原文件 Range -> 容器解复用 -> WebCodecs VideoDecoder -> WebGPU/WGSL shader -> Canvas/WebGPU
                                                                                     -> DOM 字幕/控制层
```

## 插帧倍率与硬件容量策略

当前 RIFE sidecar 调用固定 `rife.process(previous, next, 0.5)`，每对源帧只能生成一个正中间帧，因此实时 AI 插帧硬上限为 2x。3x 需要协议支持 `1/3`、`2/3` timestep；4x 需要递归三次推理。当前不使用递归 4x，原因是成本至少约为 2x 的三倍，连续递归还会放大遮挡、场景切换和线条抖动伪影。

容量不按 AMD、NVIDIA、Intel 型号表硬编码，而按当前设备实测的滚动 P95 计算：

```text
source_interval_ms = 1000 / source_fps
safe_budget_ms = source_interval_ms * 0.8
cost(m) = (m - 1) * rife_p95_ms
        + m * enhancer_p95_ms
        + decode_p95_ms + encode_p95_ms
        + safety_margin_ms
```

只有 `cost(m) <= safe_budget_ms`、模型总显存不超过可用预算、`source_fps * m <= output_fps_cap` 且协议支持该倍率时才可选择 `m`。当前远程实现固定 `output_fps_cap=60`、`safety_margin_ms=5`、`hard_max_multiplier=2`；解码和编码独立 P95 尚未接入前由利用率与固定余量保守覆盖。sidecar 保留最近 120 个样本并持续重算 P95，真实分辨率或并发负载使预算超限时优先关闭 RIFE。

首版目标档位：

| 源帧率 | AI 目标 | 策略 |
| --- | --- | --- |
| 23.976/24 | 47.952/48 | P95、显存和输出门禁通过时使用 RIFE 2x |
| 25 | 50 | P95、显存和输出门禁通过时使用 RIFE 2x |
| 29.97/30 | 59.94/60 | P95、显存和输出门禁通过时使用 RIFE 2x |
| 大于 30 | 保持源帧率 | 60 FPS 输出上限下不启用 AI 插帧 |

更高刷新率显示器不直接提高 AI 倍率。首选输出 48/50/60 FPS 后由显示链做轻量帧呈现；本地 libmpv 可用 `display-resample` 作为低成本兜底，远程可选择 FFmpeg `minterpolate` 的 60 FPS 运动补偿。未来只有在任意 timestep 协议、场景切换检测、真实 P95 和质量矩阵全部完成后，才开放 3x；4x 及以上优先作为离线转码档位，不作为实时默认。

普通 HLS 与模型 rawvideo 链都按 NVIDIA NVENC、AMD AMF、Intel QSV 选择，全部不可用时回退 libx264。模型链当前使用探测通过的首个编码器；跨厂商真机证据和模型编码器逐候选重试仍属于后续发布门禁。

## 首版阶段

### P0：播放器内核迁移

- 扩展统一契约，增加 `mpv` 后端和画质增强预设。
- 动态加载 libmpv C API，不让 Rust 编译依赖具体 GPU SDK。
- 保留现有视频窗口和透明控制窗口结构，以及 macOS 全屏、最大化和拖动同步。
- 覆盖加载、播放/暂停、跳转、音量/静音、倍速、音轨、字幕轨、字幕大小、画面比例、快照、重试和关闭。
- 首次加载失败时释放 libmpv 并返回结构化错误；前端显示可恢复的重试/关闭动作。

验收：现有播放器功能无回归，单次会话只运行 libmpv。

### P1：跨厂商 GPU 路径

- Windows：`gpu-next + d3d11 + d3d11va`，覆盖 NVIDIA、AMD、Intel。
- macOS：libmpv Render API 原生表面 + VideoToolbox；不得把 `wid` 当作 macOS 原生渲染能力。
- Linux：`gpu-next + vulkan + vaapi`，首版窗口继续使用 X11/XWayland，并由系统 `libmpv1` 提供运行时。
- 不引入 CUDA、TensorRT 或任何单一显卡厂商作为必需依赖。

验收：播放日志可确认实际后端；Windows 三厂商分别完成硬解与 shader 实机播放；macOS 不回归现有双窗口行为。

### P2：零重编码增强

- `关闭`：清空 shader 列表。
- `均衡`：单次 Anime4K 2x 上采样。
- `清晰`：Anime4K 2x 上采样并增加高光去振铃。
- 字幕继续由 libmpv 在 shader 之后合成，避免字幕被锐化。
- 预设持久化；仅在 libmpv 和 shader 均可用时展示。

验收：切换预设不中断播放，字幕边缘不参与增强，CPU 不发生视频重编码。

### P3：实时降级

- 轮询 mpv 丢帧计数并使用累积阈值，过滤单次抖动。
- 持续掉帧时按 `清晰 -> 均衡 -> 关闭` 降级。
- 快照标记自动降级，UI 显示当前生效档位。
- 用户再次手动选择预设时清除降级状态并重新评估。

验收：压力视频下自动恢复流畅；正常视频不因偶发单帧丢失降级。

### P4：发布资源和实机门禁

- Windows 固定可重定位 libmpv 运行时来源、摘要和依赖清单，并整理到 `out/libmpv/win32-x64` 随安装包发布。
- Linux 首版通过 DEB/RPM 系统依赖安装 libmpv，不创建空的伪打包目录；后续再评估完全自带运行时。
- macOS 在 render API + Metal 输出完成后再纳入正式运行时。
- CI smoke 必须动态加载、创建实例并验证 `gpu-next` 初始化。
- 实机矩阵：Windows NVIDIA/AMD/Intel、Apple Silicon/Intel macOS、Linux AMD/Intel/NVIDIA。
- 完成矩阵后，将 libmpv 从可选资源提升为发布必需资源。

验收：Windows 安装包在未安装 mpv/IINA/VLC 的干净系统启动；Linux DEB/RPM 由包管理器补齐 libmpv，AppImage 明确报告缺少 libmpv 的安装错误；不再维护桌面 libVLC 回退路径。

## 最终阶段

### 终版落地顺序

1. **能力与诊断契约**：统一声明 GPU 厂商、渲染器、解码器、模型后端、掉帧数、帧耗时和降级原因；旧快照字段必须可默认解码。
2. **GPU 零重编码链**：Windows 使用 D3D11/D3D11VA，Linux 使用 Vulkan/VA-API；Anime4K 在字幕/OSD 之前运行，禁止 CPU 视频重编码。
3. **模型超分适配器**：独立 `ModelEnhancer` 端口和 Real-ESRGAN `realesr-animevideov3-x2` 已接入远程链；加载前校验模型摘要、2x 输出、显存预算和目标帧时间，运行失败保持固定输出尺寸并关闭模型诊断。
4. **模型插帧适配器**：独立 `FrameInterpolator` 端口和 RIFE `rife-v4.6` 已接入远程链；使用有界双帧队列，运行失败后重复帧维持双倍输出帧率和原始时长，字幕不得进入模型输入。
5. **HDR 能力**：同时满足源视频色彩元数据、渲染器和显示器能力后才开启；不满足条件时保持 SDR，不用滤镜伪装 HDR。
6. **远程增强输出**：基础 HLS 已按 NVENC/AMF/QSV/libx264 探测并报告降级；RIFE 与 Real-ESRGAN 已进入独立 rawvideo 管线，音轨和软字幕不经过模型，实际模型后端与运行时降级通过 2 秒诊断轮询展示。模型链硬件编码、终端字幕能力探测和断线恢复仍待增强。
7. **远端直传终端增强**：新增 WebCodecs/WebGPU 能力探测、受控解复用、Anime4K WGSL、音视频时钟、关键帧跳转、字幕后合成和无能力回退；不改变服务端直传文件。
8. **跨平台稳定期**：至少两个版本完成 Windows NVIDIA/AMD/Intel、macOS Apple Silicon/Intel、Linux AMD/Intel/NVIDIA 的基础播放与 Shader 矩阵，以及远端主流浏览器直传增强矩阵后，才将 libmpv 标记为稳定发布后端。

### 终版能力开关规则

- `supportsModelEnhancement`、`supportsFrameInterpolation` 和 `supportsHdr` 只有在真实后端初始化、资源校验和实时预算检查全部通过后才能为 `true`。
- 新增的 `supportsDirectEnhancement` 只有在 WebCodecs 解码器、WebGPU 设备、shader 资源和音视频时钟全部通过后才能为 `true`；它不等同于 `supportsVideoEnhancement` 的服务端转码能力。
- UI 只根据能力字段显示入口；命令在后端再次校验，避免伪造客户端绕过能力门禁。
- 模型与 Shader 不得无条件叠加：总帧预算、显存预算或掉帧阈值任一超限，优先关闭插帧，再关闭模型超分，最后回退 Shader/原画。远程组合预算按一次 RIFE 和两次 Real-ESRGAN 单帧处理计算。

### 终版验收指标

| 场景 | 必须满足 |
| --- | --- |
| 基础播放 | 1080p H.264/H.265 10-bit 连续 30 分钟无崩溃，首帧 P95 <= 2 秒 |
| Shader | 切换预设不中断；字幕边缘不被锐化；CPU 不出现视频重编码进程 |
| 模型超分 | 模型摘要校验通过；显存不足自动关闭；P95 帧耗时不超过目标帧间隔 |
| 插帧 | 双帧队列有界；运行失败后关闭模型；重复帧保持既定双倍帧率和源时长 |
| HDR | 仅在源、渲染器、显示器三者能力齐全时开启；SDR 不被错误提升 |
| 远程 | 编码器降级可观测；断线可恢复；软字幕与烧录字幕路径可区分 |
| 远端直传增强 | WebCodecs + WebGPU 无 FFmpeg 转码；字幕不进入 shader；音视频时钟、拖动和回退可验证 |

### 当前未完成项

- 本地 libmpv 的模型超分/插帧渲染链；当前模型仅用于远程 HLS。
- Windows/Linux Release Runner 的真实 RIFE、Real-ESRGAN Vulkan 构建、握手、warmup 和帧耗时证据；macOS 已完成 sidecar 级别验证，但尚未完成 30 分钟播放矩阵。
- 解码、模型 rawvideo 编码的独立 P95，显示刷新率/终端 FPS 上限探测，以及按分辨率和 GPU 缓存的容量基线；当前远程门禁使用 80% 利用率和 5 ms 固定余量保守覆盖未拆分成本。
- RIFE 任意 timestep 协议、场景切换检测和 3x 质量矩阵；完成前实时 AI 硬上限保持 2x，4x 及以上只评估离线转码。
- 模型 rawvideo 链的跨厂商真机证据、逐候选编码器重试、终端字幕能力探测、自适应码率和断线恢复。
- WebGPU/WebCodecs 直传增强：F5-A 能力探测已完成；容器解复用、音频时钟、关键帧跳转、浏览器能力矩阵和 GPU 资源回收仍待实现。
- 浏览器端 RIFE/Real-ESRGAN 模型推理和端侧显存/帧预算调度。
- HDR 源元数据、渲染器、显示器能力探测和原生输出。
- Windows NVIDIA/AMD/Intel、macOS、Linux 真机矩阵与两个正式版本稳定期。

### 阶段完成状态

| 阶段 | 代码状态 | 发布验收状态 |
| --- | --- | --- |
| 首版 P0-P3 | 已完成：libmpv 单内核、跨厂商硬解配置、Anime4K、字幕后合成和掉帧降级 | 各平台真机需持续回归 |
| 首版 P4 | 已完成发布资源门禁和实机矩阵声明 | Windows/AMD/NVIDIA/Intel、macOS、Linux 实机结果待采集 |
| 终版 F1 | 已完成能力、诊断、按源帧率/滚动 P95/显存/输出上限计算的 2x 容量门禁、权重/可执行文件摘要校验和安全降级契约 | 真实 GPU 显存值仍使用配置预算；解码/编码 P95 和真机数据待采集 |
| 终版 F2 | 已完成 RIFE 与 Real-ESRGAN 端口、长驻 sidecar、内存帧协议、2x 固定尺寸回退、双倍帧率时间轴保护和运行时 P95 降级；固定提交和子模块校验已接入 | macOS 两个模型的低分辨率 warmup 已通过；真实分辨率完整链路尚未达到发布验收，本地播放器模型链、HDR 原生输出待完成 |
| 终版 F3 | 已完成普通 HLS 编码器回退、远程 RIFE + Real-ESRGAN rawvideo 管线、独立音轨/软字幕和受认证诊断刷新 | 模型链硬件编码、终端字幕探测、断线恢复和跨厂商证据待完成 |
| 终版 F4 | PC libVLC 已从播放器、安装包和工作流移除 | 仍需两个正式版本完成跨平台真机矩阵，确认 libmpv 发布稳定性 |
| 终版 F5 | F5-A 能力探测、codec 去重和失败原因已实现；不修改原文件和服务端直传协议 | F5-B 容器解复用、时钟、shader、字幕和浏览器矩阵待完成 |

### F1：能力调度层

- 收集 GPU 厂商、显存、解码格式、渲染 API、分辨率、帧率和电源模式。
- 根据能力与实时帧预算选择 shader、NCNN/Vulkan、Core ML、DirectML 或 ONNX Runtime 后端。
- 把增强链路、实际后端、帧耗时和降级原因写入统一快照与诊断页。

### F2：模型增强

- 动画超分：首版固定 `Real-ESRGAN-ncnn-vulkan` 提交 `37026f49824c5cf84062e7c6a5dd71445dcf610f` 与 `realesr-animevideov3-x2`，输出 2x；模型来源和 SHA-256 固定在准备脚本及验收清单。
- 插帧：固定 `rife-ncnn-vulkan` 提交 `a7532fc3f9f8f008cd6eecd6f2ffe2a9698e0cf7` 与 `rife-v4.6`，和超分共享显存/帧预算，组合超限时优先关闭插帧。
- HDR：色彩空间和显示能力完整探测后再启用，不以简单滤镜冒充 HDR。
- 模型下载、校验、版本切换和缓存独立于播放器内核。

### F3：远程增强输出

- 基础 HLS 已使用独立编码器探测；RIFE 与 Real-ESRGAN 处理内存 RGB24 帧，字幕保持独立软字幕，不进入模型输入。
- 当前固定软字幕；按终端能力选择烧录字幕仍待实现。
- 自适应码率、断线恢复、延迟模式和带宽探测纳入远程会话契约。
- 模型链按 NVENC/AMF/QSV 选择，最后回退 libx264，并在会话诊断中标记实际编码器；当前使用探测通过的首个候选，后续补齐逐候选启动重试。

### F4：PC libmpv 稳定发布

PC libVLC 已不再属于播放器、安装包或桌面工作流。剩余发布门禁是：

- libmpv 发布资源在全部桌面架构稳定可重定位。
- 基础播放与增强实机矩阵连续两个版本通过。
- 崩溃率、初始化失败率和首帧时间达到既定门槛。
- 缺失资源和初始化失败均有结构化错误与可恢复操作。

移动端 libVLC 不受此 PC 播放器迁移影响。

### F5：WebCodecs/WebGPU 直传终端增强

1. **F5-A 能力与安全边界**：探测 `VideoDecoder`、`VideoFrame`、`GPUAdapter`、`MediaCapabilities` 和 `OffscreenCanvas`；shader 使用应用内置且带摘要的 WGSL，禁止远端下发任意代码。能力不足时保持原始 `<video>` 直传，不自动伪装成增强。
2. **F5-B 容器与媒体时钟**：引入受控 demuxer 适配器，先覆盖可验证的 MP4/WebM + H.264/VP9/AV1 组合；通过 HTTP Range 获取关键帧和音频样本，建立绝对时间轴、拖动和断线恢复。MKV、H.265 及浏览器不支持的组合先回退直传或 HLS。
3. **F5-C WebGPU shader 链**：`EncodedVideoChunk -> VideoDecoder -> VideoFrame -> WebGPU/WGSL -> Canvas`；优先实现 Anime4K 等零重编码 shader，避免把字幕和 OSD 写入模型/shader 输入。
4. **F5-D 控制层接入**：扩展现有 `UnifiedPlayerAdapter`，保留播放/暂停、音量、倍速、拖动、全屏、进度上报和默认字幕；字幕在增强画布之后通过 DOM/独立字幕层合成。
5. **F5-E 降级与验收**：WebGPU 初始化失败、解码器不支持、帧预算超限、音画漂移或 GPU 资源回收失败时，按“终端原画直传 -> 服务端 HLS 转码”顺序降级，并在快照中报告实际路径和原因。

F5 完成后再评估浏览器端 RIFE/Real-ESRGAN。模型端侧推理不能复用远程 HLS 的 rawvideo 管道，必须另行验证模型包、WebGPU compute、显存上限、断帧策略和浏览器兼容性。
