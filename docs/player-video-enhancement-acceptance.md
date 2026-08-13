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
- 字幕和 OSD 不得进入模型输入帧，必须在增强后合成。
- RIFE 运行中失败后，每个源帧继续重复输出并补齐尾帧，维持编码器既定双倍帧率和原始时长。
- Real-ESRGAN 运行中失败后，使用最近邻 2x 保持编码器固定输入尺寸，同时实际增强状态变为关闭。

结果记录：模型版本、权重摘要、后端版本、GPU/驱动、输入分辨率、目标帧率、显存峰值、P50/P95 帧耗时、累计丢帧和实际降级原因。

### macOS 本机 Vulkan 证据（2026-08-14）

测试主机为 macOS `26.4.1`、Intel `x86_64`、AMD Radeon RX 6750 XT，LunarG Vulkan SDK `1.3.296.0`，Vulkan 通过 MoltenVK/Metal。

- Real-ESRGAN `realesr-animevideov3-x2`：真实握手和 warmup 通过，最新实测 `24.46 ms`，满足 `33 ms` 单帧预算。
- RIFE `rife-v4.6`：真实握手和 warmup 通过，`118.57 ms`，未满足 `16 ms` 实时预算；不能标记为实时插帧通过。
- 上述结果只证明 Vulkan sidecar、模型加载、协议和单次推理链路，不代表 30 分钟播放、HDR、字幕、远程编码或 Windows/Linux 真机矩阵已完成。
- RIFE 旧缓存曾使用错误 libwebp 提交；脚本现已固定并校验 NCNN、libwebp、glslang 提交，需网络恢复后重新构建并复测 RIFE 才能替换旧证据。

## 3. 桌面 GPU 矩阵

每个目标使用 1080p H.264、1080p H.265 10-bit 和至少一个带 ASS 字幕的样本连续播放 30 分钟，并切换关闭、均衡、清晰三个预设。

| 平台 | 架构 | GPU | 必须确认 |
| --- | --- | --- | --- |
| Windows | x64 | NVIDIA | D3D11、D3D11VA、gpu-next、Anime4K、字幕后合成 |
| Windows | x64 | AMD | D3D11、D3D11VA、gpu-next、Anime4K、字幕后合成 |
| Windows | x64 | Intel | D3D11、D3D11VA、gpu-next、Anime4K、字幕后合成 |
| macOS | arm64 | Apple | VideoToolbox；完成 render API 后再验收 Metal 增强输出 |
| macOS | x64 | Intel/AMD | VideoToolbox；完成 render API 后再验收 Metal 增强输出 |
| Linux | x64 | AMD/Intel/NVIDIA | Vulkan、VA-API 或明确记录的实际硬解后端、gpu-next、Anime4K |

结果记录：安装包 SHA-256、系统版本、GPU 型号、驱动版本、实际渲染器、实际解码器、首帧耗时、30 分钟丢帧、崩溃次数、自动降级次数和 libVLC 回退原因。Windows 安装包还必须在未安装 mpv、IINA、VLC 的干净系统启动。

## 4. HDR 验收

只有以下三项均为真时才允许 `supportsHdr=true`：

1. FFprobe 或播放器属性确认源视频包含受支持的 HDR 色彩元数据。
2. 当前渲染器确认支持对应色深、色域和输出格式。
3. 当前显示器与操作系统输出链路确认 HDR 已启用。

使用一个 HDR 样本和一个 SDR 样本交叉验证。任一能力缺失时 `set-hdr:auto` 必须被结构化拒绝，SDR 样本不得被滤镜伪装成 HDR。记录源元数据、渲染器输出格式、显示器能力、系统 HDR 状态和最终快照三项能力值。

## 5. 远程增强输出验收

- 分别在 NVIDIA、AMD、Intel 环境确认 NVENC、AMF、QSV 的实际编码器诊断。
- 禁用全部硬件编码器后确认回退 `libx264`，且界面显示编码降级。
- RIFE 或 Real-ESRGAN 只有在摘要、预算、真实 Vulkan 握手和 warmup 通过后才能出现在 `modelBackend`；运行中失败后 2 秒内通过受认证会话状态读取更新 `degradationReason`、实际增强和插帧状态。
- 请求 `clear` 时优先使用 Real-ESRGAN 2x；不可用时回退 FFmpeg 清晰滤镜。请求 `balanced` 时保持轻量 FFmpeg 滤镜。
- 当前远程链固定软字幕并保持字幕不进入模型输入；终端能力探测和烧录字幕尚未完成，不得标记为已验收。
- 普通 HLS 验收 NVENC/AMF/QSV/libx264；模型 rawvideo 链当前使用 libx264，硬件编码接入前不得记录跨厂商模型编码通过。
- 播放中断网后恢复，确认会话、HLS 清单、播放位置和字幕状态可恢复。

结果记录：输入管线、实际编码器、是否软件回退、字幕模式、输出分辨率/帧率/码率、首段耗时、重连耗时和失败原因。

## 6. 证据状态规则

- `implemented`：源码、单元/集成测试和静态门禁已通过，不代表真实硬件通过。
- `release-runner`：同一候选 SHA 的正式 Release Runner 已完成构建、打包、真实 sidecar 握手和 warmup。
- `device-passed`：指定系统、GPU、驱动和安装包完成持续播放及降级场景。
- `stable-version`：正式版本完成全部适用矩阵；只有连续两个版本均为此状态才能删除桌面 libVLC。

`scripts/player-enhancement-matrix.mjs` 只校验必须登记的目标和证据字段，不把任何目标自动标记为通过。

## 7. libVLC 移除门槛

桌面 libVLC 只能在连续两个正式版本完成全部适用真机矩阵后移除。两个版本都必须满足：

- libmpv 发布资源在各桌面架构可重定位，干净系统可启动。
- 常见媒体的初始化失败率、崩溃率和首帧 P95 达标。
- 回退记录不再显示 libVLC 承担常见媒体播放。
- macOS render API + Metal 路径已经完成，不以未承诺的 `wid` 嵌入替代。

移动端 libVLC 不属于本删除范围。任何一项缺证据都继续保留桌面回退。
