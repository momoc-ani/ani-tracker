# PC 内置播放器与画质增强执行计划

> 当前实现边界（2026-08-13）：桌面 Windows/Linux 已接入 libmpv、gpu-next、硬解路径、Anime4K Shader、字幕后合成、掉帧自动降级和 libVLC 回退；远程 HLS 已按 NVENC/AMF/QSV/libx264 顺序尝试并报告实际编码器。RIFE sidecar 已固定上游提交、校验可执行文件与权重、完成 Vulkan 握手和真实 warmup，并已接入远程 RGB24 双帧队列、模型中间帧和 rawvideo 编码回退。Real-CUGAN/Real-ESRGAN 的单帧模型端口已完成，但正式上游模型资产、真实超分 sidecar 构建和本地播放器接入仍未完成；HDR 原生输出和跨 GPU 真机证据也未完成。没有模型运行时和权重时不得宣称可用。

正式模型包、HDR、远程输出和真实 GPU 的逐项执行与证据要求见 [播放器终版发布验收清单](./player-video-enhancement-acceptance.md)。

## 目标架构

首版播放链路：

```text
本地视频 -> libmpv 硬件解码 -> gpu-next -> Anime4K shader -> 字幕/OSD -> 原生窗口
```

迁移期只运行一个 PC 播放内核：Windows 与 Linux 优先初始化 libmpv；动态库缺失、初始化失败或首次加载失败时，释放 libmpv 后回退到现有 libVLC。macOS 首版继续使用已经完成窗口实机验证的 libVLC，下一阶段通过 libmpv render API + Metal 接入，不能使用官方未承诺的 NSView `wid` 嵌入。Android、iOS 和远程 Web 播放器继续使用原实现。

最终本地链路：

```text
输入 -> 硬件解码 -> 能力/负载调度 -> Anime4K 或模型增强 -> 可选插帧/HDR -> 字幕/OSD -> 显示
```

最终远程链路：

```text
输入 -> 本地硬件解码 -> 增强/插帧/HDR -> 厂商硬件编码 -> HLS/SRT/WebRTC -> 远程播放器
```

远程编码按 NVIDIA NVENC、AMD AMF、Intel QSV 选择，全部不可用时才回退 libx264。

## 首版阶段

### P0：播放器内核迁移

- 扩展统一契约，增加 `mpv` 后端和画质增强预设。
- 动态加载 libmpv C API，不让 Rust 编译依赖具体 GPU SDK。
- 保留现有视频窗口和透明控制窗口结构，以及 macOS 全屏、最大化和拖动同步。
- 覆盖加载、播放/暂停、跳转、音量/静音、倍速、音轨、字幕轨、字幕大小、画面比例、快照、重试和关闭。
- 首次加载失败时自动释放 libmpv 并回退 libVLC；回退后快照能力同步到前端。

验收：现有播放器功能无回归，单次会话不会同时运行 libmpv 和 libVLC。

### P1：跨厂商 GPU 路径

- Windows：`gpu-next + d3d11 + d3d11va`，覆盖 NVIDIA、AMD、Intel。
- macOS 首版：维持 libVLC + VideoToolbox；下一阶段使用 libmpv render API + Metal，不能把 `wid` 当作已支持能力。
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

验收：Windows 安装包在未安装 mpv/IINA/VLC 的干净系统启动；Linux DEB/RPM 由包管理器补齐 libmpv，AppImage 在宿主缺少 libmpv 时回退 libVLC；回退路径至少保留一个稳定版。

## 最终阶段

### 终版落地顺序

1. **能力与诊断契约**：统一声明 GPU 厂商、渲染器、解码器、模型后端、掉帧数、帧耗时和降级原因；旧快照字段必须可默认解码。
2. **GPU 零重编码链**：Windows 使用 D3D11/D3D11VA，Linux 使用 Vulkan/VA-API；Anime4K 在字幕/OSD 之前运行，禁止 CPU 视频重编码。
3. **模型超分适配器**：以独立 `ModelEnhancer` 端口接入 Real-CUGAN/Real-ESRGAN 类模型；加载前校验模型摘要、显存预算和目标分辨率，失败只回退 Shader/原画。
4. **模型插帧适配器**：以独立 `FrameInterpolator` 端口接入 RIFE；使用有界双帧队列，模型延迟超过帧预算或连续掉帧时自动关闭，字幕不得进入模型输入。
5. **HDR 能力**：同时满足源视频色彩元数据、渲染器和显示器能力后才开启；不满足条件时保持 SDR，不用滤镜伪装 HDR。
6. **远程增强输出**：基础 HLS 已按 NVENC/AMF/QSV/libx264 探测并报告降级；RIFE 增强帧已进入独立 rawvideo 编码管线并保留独立音轨，终端字幕能力探测和断线恢复仍待增强。
7. **双版本稳定期**：至少两个版本完成 Windows NVIDIA/AMD/Intel、macOS Apple Silicon/Intel、Linux AMD/Intel/NVIDIA 的基础播放与 Shader 矩阵后，才评估移除桌面 libVLC。

### 终版能力开关规则

- `supportsModelEnhancement`、`supportsFrameInterpolation` 和 `supportsHdr` 只有在真实后端初始化、资源校验和实时预算检查全部通过后才能为 `true`。
- UI 只根据能力字段显示入口；命令在后端再次校验，避免伪造客户端绕过能力门禁。
- 模型与 Shader 不得无条件叠加：总帧预算、显存预算或掉帧阈值任一超限，按“插帧 -> 模型超分 -> Shader”顺序降级。

### 终版验收指标

| 场景 | 必须满足 |
| --- | --- |
| 基础播放 | 1080p H.264/H.265 10-bit 连续 30 分钟无崩溃，首帧 P95 <= 2 秒 |
| Shader | 切换预设不中断；字幕边缘不被锐化；CPU 不出现视频重编码进程 |
| 模型超分 | 模型摘要校验通过；显存不足自动关闭；P95 帧耗时不超过目标帧间隔 |
| 插帧 | 双帧队列有界；连续掉帧 3 个采样周期内关闭；关闭后恢复原始时间轴 |
| HDR | 仅在源、渲染器、显示器三者能力齐全时开启；SDR 不被错误提升 |
| 远程 | 编码器降级可观测；断线可恢复；软字幕与烧录字幕路径可区分 |

### 当前未完成项

- Real-CUGAN/Real-ESRGAN 正式模型运行时、权重管理和 GPU 推理后端。
- RIFE sidecar、摘要/显存/帧预算校验和远程有界双帧队列已完成；真实 Vulkan 构建、warmup 和 Windows/macOS/Linux GPU 证据仍需 CI/真机记录。
- libmpv render API + Metal 的 macOS 原生输出。
- HDR 元数据探测、显示器能力探测和远程硬件编码管线。
- Windows NVIDIA/AMD/Intel、macOS、Linux 真机矩阵与两个版本稳定期。

### 阶段完成状态

| 阶段 | 代码状态 | 发布验收状态 |
| --- | --- | --- |
| 首版 P0-P3 | 已完成：libmpv/libVLC 单内核切换、跨厂商硬解配置、Anime4K、字幕后合成和掉帧降级 | macOS 仍使用 libVLC；各平台真机需持续回归 |
| 首版 P4 | 已完成发布资源门禁和实机矩阵声明 | Windows/AMD/NVIDIA/Intel、macOS、Linux 实机结果待采集 |
| 终版 F1 | 已完成能力、诊断、预算、权重摘要文件校验和安全降级契约 | 真实 GPU/显存探测需随模型后端装配 |
| 终版 F2 | 已完成模型超分/插帧端口、RIFE sidecar 协议、HDR 三方门禁和有界队列 | Real-ESRGAN 正式权重/sidecar、真实 Vulkan warmup、HDR 原生输出待接入 |
| 终版 F3 | 已完成 HLS 编码器回退、RIFE 增强帧 rawvideo 管线和独立音轨诊断 | 终端字幕探测、断线恢复和跨厂商实际编码证据待采集 |
| 终版 F4 | 删除条件和双版本稳定门槛已固化 | 必须经过两个正式版本，不在当前阶段删除 libVLC |

### F1：能力调度层

- 收集 GPU 厂商、显存、解码格式、渲染 API、分辨率、帧率和电源模式。
- 根据能力与实时帧预算选择 shader、NCNN/Vulkan、Core ML、DirectML 或 ONNX Runtime 后端。
- 把增强链路、实际后端、帧耗时和降级原因写入统一快照与诊断页。

### F2：模型增强

- 动画超分：Real-CUGAN/Real-ESRGAN 类模型，按平台选择通用 ncnn/Vulkan 推理后端；未取得可验证模型资产前保持关闭。
- 插帧：RIFE 类模型，和超分共享帧预算，禁止两者无条件同时满负载。
- HDR：色彩空间和显示能力完整探测后再启用，不以简单滤镜冒充 HDR。
- 模型下载、校验、版本切换和缓存独立于播放器内核。

### F3：远程增强输出

- 基础 HLS 已使用独立编码器探测；RIFE 增强后的 RGB 帧已经进入本地编码管线，不复用字幕已经烧录的源画面。
- 支持软字幕传递和按终端能力选择烧录字幕。
- 自适应码率、断线恢复、延迟模式和带宽探测纳入远程会话契约。
- 编码器按 NVENC/AMF/QSV 选择，最后回退 libx264，并在会话诊断中标记降级。

### F4：移除 PC libVLC

只有在以下条件全部满足后删除桌面 libVLC：

- libmpv 发布资源在全部桌面架构稳定可重定位。
- 基础播放与增强实机矩阵连续两个版本通过。
- 崩溃率、初始化失败率和首帧时间达到既定门槛。
- 回退遥测显示不再依赖 libVLC 处理常见媒体。

移动端 libVLC 不受此删除计划影响。
