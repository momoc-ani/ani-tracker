# 播放器终版发布验收清单

本清单用于补齐不能由普通 CI 代替的模型、HDR、远程编码和真实 GPU 验收。代码门禁通过不等于真机通过；每个正式版本必须保存对应平台的日志、快照和结果记录。

## 1. 通用代码门禁

在候选版本的同一个 Git SHA 上执行：

```bash
pnpm run typecheck
pnpm run test:parsers
pnpm run test:desktop-gates
pnpm run test:player-matrix
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

任何命令失败都停止发布，不使用其他提交的测试结果代替。

## 2. 模型包验收

每个 Real-CUGAN、Real-ESRGAN 或 RIFE 模型必须提供独立清单，至少包含模型标识、推理后端、操作类型、输出倍率、权重 SHA-256、输入宽高、所需显存和预计单帧耗时。

当前固定模型：

| 模型 | 上游提交 | 后端/操作 | 资源摘要 |
| --- | --- | --- | --- |
| RIFE `rife-v4.6` | `a7532fc3f9f8f008cd6eecd6f2ffe2a9698e0cf7` | `ncnn-vulkan` / 插帧 / 1x | NCNN `b4ba207c18d3103d6df890c0e3a97b469b196b26`；libwebp `5abb55823bb6196a918dd87202b2f32bbaff4c18`；glslang `86ff4bca1ddc7e2262f119c16e7228d0efb67610`；权重以脚本清单为准 |
| Real-ESRGAN `realesr-animevideov3-x2` | `37026f49824c5cf84062e7c6a5dd71445dcf610f` | `ncnn-vulkan` / 单帧增强 / 2x | NCNN `6125c9f47cd14b589de0521350668cf9d3d37e3c`；libwebp `8ea81561d2fdd382da60f57958741a7c23a18eb6`；glslang `4afd69177258d0636f78d2c4efb823ab6382a187`；`.bin` `548a36f9c3f4ab8da56cd3b13badf23968bee207b396dad14d04b830e5f2ab2d`；`.param` `b88ff4f00ebf019a7fdac17fdd45a7fd3665d37509efc5baf2e4da2e24420a04` |

Real-ESRGAN 模型归档固定为 `v0.2.5.0/realesrgan-ncnn-vulkan-20220424-windows.zip`，大小 `45474481`，SHA-256 `abc02804e17982a3be33675e4d471e91ea374e65b70167abc09e31acb412802d`。

- 启动前读取实际权重文件并比对 SHA-256，摘要不一致时能力保持关闭。
- 显存或帧时间超过当前会话预算时，不创建推理会话。
- 模型初始化失败只允许回退 Shader 或原画，不能中断基础播放。
- 超分和插帧不得无条件同时满负载；远程组合预算按一次 RIFE 与两次 Real-ESRGAN 处理计算，超限时优先关闭 RIFE。
- RIFE 倍率必须由源帧率、滚动 P95、显存、输出 FPS 上限和协议硬上限共同决定；不得按 GPU 品牌或型号直接判定可用。
- 当前协议固定中间时刻 `0.5`，AI 实时上限必须保持 2x。未经任意 timestep、场景切换和质量矩阵验收，不得开放 3x；不得用递归 RIFE 冒充已验收的实时 4x。
- 远程会话必须报告 `sourceFrameRate`、`targetFrameRate`、`selectedMultiplier`、`maxFeasibleMultiplier`、安全预算、预计成本、模型 P95 和样本数；运行中容量超限后 2 秒内应显示 1x 与实际关闭状态。
- 字幕和 OSD 不得进入模型输入帧，必须在增强后合成。
- RIFE 运行中失败后，每个源帧继续重复输出并补齐尾帧，维持编码器既定双倍帧率和原始时长。
- Real-ESRGAN 运行中失败后，使用最近邻 2x 保持编码器固定输入尺寸，同时实际增强状态变为关闭。

结果记录：模型版本、权重摘要、后端版本、GPU/驱动、输入分辨率、目标帧率、显存峰值、P50/P95 帧耗时、累计丢帧和实际降级原因。

### macOS 本机 Vulkan 证据（2026-08-14）

测试主机为 macOS `26.4.1`、Intel `x86_64`、AMD Radeon RX 6750 XT，LunarG Vulkan SDK `1.3.296.0`，Vulkan 通过 MoltenVK/Metal。

- Real-ESRGAN `realesr-animevideov3-x2`：真实握手和 warmup 通过，最新实测 `24.46 ms`，满足 `33 ms` 单帧预算。
- RIFE `rife-v4.6`：使用固定 NCNN/libwebp/glslang 提交重新构建后，真实握手和 warmup 通过，最新实测 `25.99 ms`。2x 每个源帧区间只执行一次 RIFE，不能再用固定 `16 ms` 输出帧间隔作为所有源帧率的判断；24 FPS 在 80% 利用率下的模型链安全窗口为 `33.33 ms`。但当前 warmup 分辨率和单样本仍不足以证明真实视频 P95，因此不能标记为实时插帧通过。
- 上述结果只证明 Vulkan sidecar、模型加载、协议和单次推理链路，不代表 30 分钟播放、HDR、字幕、远程编码或 Windows/Linux 真机矩阵已完成。
- RIFE 旧缓存曾使用错误 libwebp 提交，已由固定提交的重建 bundle 和新实测结果替换。

## 3. 桌面 GPU 矩阵

每个目标使用 1080p H.264、1080p H.265 10-bit 和至少一个带 ASS 字幕的样本连续播放 30 分钟，并切换关闭、均衡、清晰三个预设。

| 平台 | 架构 | GPU | 必须确认 |
| --- | --- | --- | --- |
| Windows | x64 | NVIDIA | D3D11、D3D11VA、gpu-next、Anime4K、字幕后合成 |
| Windows | x64 | AMD | D3D11、D3D11VA、gpu-next、Anime4K、字幕后合成 |
| Windows | x64 | Intel | D3D11、D3D11VA、gpu-next、Anime4K、字幕后合成 |
| macOS | arm64 | Apple | Render API、VideoToolbox、Anime4K、字幕后合成 |
| macOS | x64 | Intel/AMD | Render API、VideoToolbox、Anime4K、字幕后合成 |
| Linux | x64 | AMD/Intel/NVIDIA | Vulkan、VA-API 或明确记录的实际硬解后端、gpu-next、Anime4K |

结果记录：安装包 SHA-256、系统版本、GPU 型号、驱动版本、实际渲染器、实际解码器、首帧耗时、30 分钟丢帧、崩溃次数、自动降级次数和 libmpv 初始化错误。Windows 安装包还必须在未安装 mpv、IINA、VLC 的干净系统启动。

## 4. HDR 验收

只有以下三项均为真时才允许 `supportsHdr=true`：

1. FFprobe 或播放器属性确认源视频包含受支持的 HDR 色彩元数据。
2. 当前渲染器确认支持对应色深、色域和输出格式。
3. 当前显示器与操作系统输出链路确认 HDR 已启用。

使用一个 HDR 样本和一个 SDR 样本交叉验证。任一能力缺失时 `set-hdr:auto` 必须被结构化拒绝，SDR 样本不得被滤镜伪装成 HDR。记录源元数据、渲染器输出格式、显示器能力、系统 HDR 状态和最终快照三项能力值。

## 5. 远程增强输出验收

- `direct` 模式只发送原文件，不经过 FFmpeg 编码；服务端不改变视频码流，画质上限等于原文件和终端解码能力。
- `transcode` 模式必须重新编码为 HLS，当前普通和模型管线使用 H.264 视频候选、`yuv420p` 像素格式和 AAC 160 kbps 音频，因此属于有损输出，可能降低码率、色深和 HDR 元数据保留能力。模型超分只增加处理后的像素，不会恢复原始编码中已丢失的细节。
- 原始下载文件始终保持不变；压缩只发生在远端会话的输出流。需要无损保真时选择直传，接受转码损失后再启用远端画质增强或插帧。
- WebGPU/WebCodecs 直传增强按本清单第 6 节单独验收；在该阶段完成前，远端浏览器不宣称支持终端本地增强。
- 分别在 NVIDIA、AMD、Intel 环境确认 NVENC、AMF、QSV 的实际编码器诊断。
- 禁用全部硬件编码器后确认回退 `libx264`，且界面显示编码降级。
- RIFE 或 Real-ESRGAN 只有在摘要、预算、真实 Vulkan 握手和 warmup 通过后才能出现在 `modelBackend`；运行中失败后 2 秒内通过受认证会话状态读取更新 `degradationReason`、实际增强和插帧状态。
- 对 23.976/24、25、29.97/30 和 60 FPS 源分别核对容量诊断：目标只能为 47.952/48、50、59.94/60 和保持 60；任何 P95、显存或输出上限不满足的场景必须显示 1x 并关闭 RIFE。
- 至少采集 120 个真实分辨率样本后记录滚动 P95；启动 warmup 只能作为第一个临时样本，不能单独作为 `device-passed` 证据。
- 请求 `clear` 时优先使用 Real-ESRGAN 2x；不可用时回退 FFmpeg 清晰滤镜。请求 `balanced` 时保持轻量 FFmpeg 滤镜。
- 当前远程链固定软字幕并保持字幕不进入模型输入；终端能力探测和烧录字幕尚未完成，不得标记为已验收。
- 普通 HLS 和模型 rawvideo 链都验收 NVENC/AMF/QSV/libx264；模型链使用探测通过的首个候选，未完成对应 GPU 真机证据前不得记录跨厂商模型编码通过。
- 播放中断网后恢复，确认会话、HLS 清单、播放位置和字幕状态可恢复。

结果记录：输入管线、实际编码器、是否软件回退、字幕模式、输出分辨率/帧率/码率、首段耗时、重连耗时和失败原因。

## 6. WebCodecs/WebGPU 直传增强验收

- F5-A 能力探测已实现并有共享单元测试，覆盖 Audio/Video WebCodecs、AudioContext、WebGPU、候选 codec 和内置 WGSL SHA-256；F5-B/C 已加入受控 MP4/WebM Range demux、实际 decoder config、关键帧、绝对时钟、外部 VideoFrame shader 工厂及 1x1 OffscreenCanvas 首帧 smoke test；F5-D 已加入连续 VideoDecoder、有界帧队列、可见画布、关键帧拖动、预设切换和原画回退；F5-E 已加入 device-lost、源帧率帧预算、GPU P95、丢帧、连续漂移和无帧门禁；F5-F 已加入独立 AudioDecoder/AudioContext 音轨调度、暂停、拖动、倍速、音量、静音、结束态和独立 ASS/VTT cue 时钟。增强激活后卸载 ArtPlayer 原媒体源，不再连续重复 Range/视频解码；尚未完成真机矩阵，因此 `supportsDirectEnhancement` 必须保持 `false`。
- 能力探测必须同时确认 `VideoDecoder`、`AudioDecoder`、目标 codec 的 `isConfigSupported`、WebGPU adapter/device、WGSL shader 摘要和音视频时钟；任一项失败都不能展示“直传增强已启用”。
- 直传增强必须从原文件的 HTTP Range 读取并在终端解码，不得启动服务端 FFmpeg/模型转码进程；网络面板和服务端会话诊断必须能区分 `direct` 与 `direct-enhanced`。
- 客户端诊断上报必须绑定已配对设备和当前直传会话，并覆盖能力结果、实际档位、AudioContext 主时钟、帧/GPU/漂移/Range 指标和降级原因；伪造 `active`、越界指标、HLS 会话上报和迟到序号不得覆盖服务端快照。增强回退后服务端 `playbackPath` 必须从 `direct-enhanced` 恢复为 `direct`。
- 首批只验收 `mediabunny 1.53.1` 受控适配器支持的 MP4/WebM 组合；诊断阶段必须遵守 32 MiB 缓存、2 个并发、24 次 Range、64 MiB 累计响应和 8 秒窗口。H.265、MKV、HDR 和浏览器不支持的 codec 必须明确回退原始直传或 HLS，不能输出黑帧或假增强状态。
- 人为中断一个 Range 建连和一个响应体，确认客户端最多重试两次、从已交付的下一字节继续、严格核对 `Content-Range`，且重试仍计入请求/字节总预算；401/403/416 和取消操作不得重试。恢复成功时播放时间轴不能倒退或重复音频，恢复失败必须回退原画。
- WebGPU shader 只处理视频帧；F5-D 只有在增强首帧成功后才显示画布，并通过 ArtPlayer DOM 保留字幕层。F5-F 将 ASS 转为 VTT cue，再由 AudioContext 主时钟直接选择活动字幕；仍须用真实 ASS/VTT 样本确认默认字幕、切换、拖动和缩放不会被锐化、遮挡、重复绘制或丢失。
- 连续播放 30 分钟，确认首帧、暂停/恢复、关键帧拖动、全屏、音画同步、GPU 资源释放和页面重新进入；核对页面诊断的源帧率预算、GPU P95、估算工作集/资源预算、丢帧率和漂移值。清晰档超限必须先显示均衡实际档位；均衡持续超限、连续漂移、无帧或 WebGPU OOM 时必须回退原画直传。WebGPU 不提供真实 VRAM 遥测，验收记录不得把估算工作集写成显存占用。
- Chrome/Edge、Safari macOS、Firefox（若能力未开放）分别记录 `device-passed` 或结构化回退原因；不能以桌面浏览器支持代替移动浏览器证据。
- WGSL 必须来自应用内置资源并校验摘要，不能从远端 URL 加载可执行 shader 或模型代码。

结果记录：浏览器版本、GPU/驱动、WebGPU adapter、codec、容器、Range 请求、实际渲染路径、首帧、P95 帧耗时、音画漂移、估算 GPU 工作集/资源预算、回退次数和字幕截图；不得把浏览器估算值写成真实显存峰值。

### 2026-08-14 Intel Mac Chrome 受控回退记录

- 候选代码：`f467f52`；macOS `26.4.1 (25E253)`、`x86_64`、AMD Radeon RX 6750 XT 12 GB（Metal 3）、Google Chrome `151.0.7922.138`。
- 已配对浏览器通过 `http://127.0.0.1:18083` 创建真实直传会话，样本为 1080p H.264/AAC MKV，包含 15 条内封 ASS 字幕。
- 修复前原生 `Window.fetch` 作为裸函数调用会抛出 `Illegal invocation`，并被 Range 恢复逻辑重试两次；`f467f52` 将底层 fetch 绑定到 `globalThis`，重建远程 Renderer 后同一路径不再产生该错误或伪网络重试。
- MKV 按 F5 首批容器边界返回 `Input has an unsupported or unrecognizable format.`，页面保持 `data-remote-playback-path="direct"` 并显示“浏览器增强不可用，已使用原画”；原画播放推进到 `01:42`，暂停控制有效，默认中文 ASS 轨道保持选中且字幕实际显示，字幕缩放保持 `150%`。
- 本记录只证明浏览器原生 fetch 修复和不支持容器的安全回退，不标记为 `device-passed`。仍需 MP4/WebM 正向样本验证 `direct-enhanced`、WebGPU 指标、30 分钟音画同步、拖动、Range 断流恢复和资源释放；完成前 `supportsDirectEnhancement` 继续保持 `false`。

### 2026-08-14 Intel Mac Chrome MP4 正向验收

- 代码：`a93fbcb`（48 帧有界预解码窗口）与 `9937d60`（页面退出会话回收）。主机为 macOS `26.4.1 (25E253)`、Intel `x86_64`、AMD Radeon RX 6750 XT 12 GB（Metal 3），浏览器为 Google Chrome `151.0.7922.138`。
- 样本为 `acceptance-direct-enhancement-long-hq.mp4`：35:00、1920x1080 H.264、约 7.5 Mbps、AAC、两条字幕轨、约 2.00 GiB。样本由本地媒体以 `-c copy` 生成，不重新编码。
- 正式连续播放通过 `30:40`：`playbackPath=direct-enhanced`、实际档位 `clear`、1920x1080 canvas、`AudioContext` 主时钟、渲染帧 `43620`、滚动丢帧率约 `2.44%`、GPU 队列 P95 约 `1.3 ms`、音画漂移约 `12.1 ms`、Range 重试/恢复/网络失败均为 `0`。丢帧率是最近窗口指标，不得写成累计值；`207548416` bytes 是 WebGPU 工作集估算，资源预算为 1 GiB，不是 VRAM 遥测。
- 高码率样本约 7 分钟处曾因旧的 8 帧缓冲无法覆盖 Range 读取抖动而触发无增强帧回退；48 帧上限对应约 2 秒预解码窗口，1080p/4 GiB 门禁通过，4K/4 GiB 被工作集门禁拒绝。修复后跨越原失败点并完成 30 分钟持续播放。
- 应用级全屏进入、退出均通过，截图中的画布保持 1920x1080，控件和字幕层可用。该模式是播放器的网页全屏布局；浏览器 `document.fullscreenElement` 仍为 `null`，不能把它记为浏览器原生全屏证明。
- 关闭验收页后重新进入同一任务，首帧再次为 `direct-enhanced / clear`，AudioContext 与双字幕轨仍存在；手动开始后 3 秒内渲染 60 帧。页面退出回收已验证：网关日志记录 `DELETE /api/media/sessions/<id> -> 204`，对应 `remote-media/session-<id>` 目录立即消失。
- 此项是指定 macOS Intel x64 + RX 6750 XT + Chrome 的 `device-passed`，不代表 Windows、Safari、Firefox、Apple Silicon、Intel/NVIDIA Windows GPU、H.265、MKV、HDR、模型侧车或远端 HLS 已通过。此前页面重载不是会话 TTL；会话 TTL 为 30 分钟，播放期间每 2 秒刷新，原问题是 8 帧缓冲覆盖不了高码率 Range 抖动。

## 7. 证据状态规则

- `implemented`：源码、单元/集成测试和静态门禁已通过，不代表真实硬件通过。
- `release-runner`：同一候选 SHA 的正式 Release Runner 已完成构建、打包、真实 sidecar 握手和 warmup。
- `device-passed`：指定系统、GPU、驱动和安装包完成持续播放及降级场景。
- `stable-version`：正式版本完成全部适用矩阵；连续两个版本均为此状态后，才可将 libmpv 标记为稳定发布后端。

`scripts/player-enhancement-matrix.mjs` 只校验必须登记的目标和证据字段，不把任何目标自动标记为通过。

## 8. PC libmpv 稳定发布门槛

PC 播放器、安装包和桌面工作流已经移除 libVLC。连续两个正式版本仍需满足：

- libmpv 发布资源在各桌面架构可重定位，干净系统可启动。
- 常见媒体的初始化失败率、崩溃率和首帧 P95 达标。
- 缺失资源和初始化失败均有结构化错误与可恢复操作。
- macOS 使用 Render API 原生表面，不以未承诺的 `wid` 嵌入替代。

移动端 libVLC 不属于本迁移范围。
