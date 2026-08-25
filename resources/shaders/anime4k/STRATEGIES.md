# 桌面画质增强策略资源

本目录当前只接入 Windows、macOS、Linux 的 libmpv 播放器。Android GPU shader
具备后续评估价值，但本阶段不修改 Android libVLC 播放链，也不随移动包声明可用。

桌面 libmpv 通过 `ANI_MPV_ENHANCEMENT_STRATEGY` 选择增强策略，未设置时使用
`legacy`，保持 Ani Tracker 当前默认行为。资源目录由 `ANI_ANIME4K_SHADER_DIR`
覆盖时，策略文件名保持不变。

| 策略 | 环境变量值 | 资源文件 | 说明 |
| --- | --- | --- | --- |
| Anime4K legacy | `legacy` | `Anime4K_Upscale_Original_x2.glsl`、`Anime4K_Clamp_Highlights.glsl` | 默认兼容链路 |
| Anime4K Ultra | `anime4k-ultra` | `Anime4K-Ultra.glsl`（可选，当前未内置） | 单文件高质量 Anime4K 管线 |
| FSRCNNX | `fsrcnnx-fidelity` | `FSRCNNX_x2_8-0-4-1.glsl` | 2x 细节恢复 |
| ArtCNN | `artcnn` | `ArtCNN_C4F16.glsl`、`ArtCNN_C4F32.glsl` | 动画 2x ArtCNN shader，当前默认 C4F16 |

外部策略资源缺失或不完整时，运行时自动回退 `legacy`，不会阻止原视频播放。
字幕和 OSD 仍由 libmpv 在 shader 后合成；策略只负责 `glsl-shaders` 列表。

外部资源来源：

- Anime4K Ultra（未内置）：<https://github.com/Chinna95P/mpv-anime-build>，`a13bee632180bf0bdaac8025c0d61b4388f07013`
- FSRCNNX：<https://github.com/deus0ww/mpv-conf/tree/master/shaders/igv>
- ArtCNN：<https://github.com/Artoriuz/ArtCNN>
- 本地纳入 FSRCNNX：`c831d602e28b2bd880e3ffa61f80f9537ce88dcd4ea3ea6ce35a49f4607f969b`
- 本地纳入 ArtCNN C4F16：`1706bddf4350643b34815c1baa72d26bfebd30e1f0473cf5352507c312757dfd`
- 本地纳入 ArtCNN C4F32：`b4181db4baecab6669d69d3618f3ade554ffbba5210ba437fe947387e4acf487`

资源未通过应用运行时联网下载，发布包只使用源码树或构建阶段显式提供的文件。
