# PC 画质增强模型资源

这些模型仅供桌面 PC 端后续 `FrameInterpolator` / `ModelEnhancer` 策略使用，当前
libmpv 首版仍只启用 GPU shader，Android/iOS 不读取此目录。

| 模型 | 目录 | 后端 | 用途 | 输出 |
| --- | --- | --- | --- | --- |
| RIFE v4.6 | `rife-v4.6/` | `ncnn-vulkan` | 插帧 | 1x，当前实时协议上限 2x |
| Real-ESRGAN AnimeVideo v3 x2 | `realesr-animevideov3-x2/` | `ncnn-vulkan` | 动画超分 | 2x |

每个目录包含 `manifest.json` 和 `SOURCE.json`，权重摘要必须在模型运行时再次校验；
摘要不一致时保持能力关闭。模型文件不是 shader，不能直接放入 libmpv 的
`glsl-shaders` 列表。

第三方依赖及来源：

- RIFE：`nihui/rife-ncnn-vulkan`，固定提交见 `rife-v4.6/SOURCE.json`。
- Real-ESRGAN：`xinntao/Real-ESRGAN-ncnn-vulkan`，固定提交见
  `realesr-animevideov3-x2/SOURCE.json`。
- 运行时依赖：NCNN + Vulkan；Windows 使用 Vulkan 驱动，macOS 使用 MoltenVK，
  Linux 使用 Vulkan 驱动。不得把 Android 运行时直接复用到此目录。

许可证文件位于 `licenses/`。本目录权重来自已经通过准备脚本校验的桌面构建缓存，
仅作为应用资源预置，不表示本阶段已经完成本地模型实时播放验收。
