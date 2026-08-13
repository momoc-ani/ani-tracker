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

每个 Real-CUGAN、Real-ESRGAN 或 RIFE 模型必须提供独立清单，至少包含模型标识、推理后端、权重 SHA-256、输入宽高、所需显存和预计单帧耗时。

- 启动前读取实际权重文件并比对 SHA-256，摘要不一致时能力保持关闭。
- 显存或帧时间超过当前会话预算时，不创建推理会话。
- 模型初始化失败只允许回退 Shader 或原画，不能中断基础播放。
- 超分和插帧不得无条件同时满负载；降级顺序固定为插帧、模型超分、Shader。
- 字幕和 OSD 不得进入模型输入帧，必须在增强后合成。

结果记录：模型版本、权重摘要、后端版本、GPU/驱动、输入分辨率、目标帧率、显存峰值、P50/P95 帧耗时、累计丢帧和实际降级原因。

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
- 在增强帧真正进入编码器前，`enhancedFrameInput` 必须保持 `false`；接入后使用画面对比和管线日志确认其为 `true`。
- 软字幕终端保持独立字幕轨；不支持软字幕的终端才允许烧录，并在诊断中标记模式。
- 播放中断网后恢复，确认会话、HLS 清单、播放位置和字幕状态可恢复。

结果记录：输入管线、实际编码器、是否软件回退、字幕模式、输出分辨率/帧率/码率、首段耗时、重连耗时和失败原因。

## 6. libVLC 移除门槛

桌面 libVLC 只能在连续两个正式版本完成全部适用真机矩阵后移除。两个版本都必须满足：

- libmpv 发布资源在各桌面架构可重定位，干净系统可启动。
- 常见媒体的初始化失败率、崩溃率和首帧 P95 达标。
- 回退记录不再显示 libVLC 承担常见媒体播放。
- macOS render API + Metal 路径已经完成，不以未承诺的 `wid` 嵌入替代。

移动端 libVLC 不属于本删除范围。任何一项缺证据都继续保留桌面回退。
