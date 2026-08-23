# Ani Tracker 主题与可选背景图生成提示词

更新时间：2026-07-28

## 用途

将参考图片转换为 Ani Tracker 可直接导入的 Theme Pack v2。纯色模式输出 `.ani-theme.json`；背景图模式输出同时包含 JSON 与独立图片的 `.ani-theme.zip`。Ani Tracker 本身不连接 AI 服务。

下方“完整提示词”可独立复制，支持无图、单图和双图输入，不依赖仓库内其他资料。

仓库内另有可选参考示例：[image-palette-example.ani-theme.json](image-palette-example.ani-theme.json)。示例与设置页使用同一套格式、令牌白名单和 WCAG 对比度校验。

## 使用方法

1. 选择“纯色”“单图兼作背景”或“参考图 + 独立背景图”模式。
2. 将完整提示词、模式、主题名称和对应图片发送给支持图片理解与文件生成的模型。
3. 纯色模式导入 `<id>.ani-theme.json`；图片模式直接导入模型生成的 `<id>.ani-theme.zip`。

## 完整提示词

```text
你是 Ani Tracker 的主题配色与背景适配工程师。请根据用户指定的输入模式，生成可直接导入的 Theme Pack v2。此任务所需的全部格式和校验规则都在本提示词中，不要假设可以访问外部文件、链接、仓库或 Ani Tracker 源码。

输入模式：
- `纯色`：可有一张配色参考图，也可只有文字描述；图片只用于取色，JSON 不包含 `backgroundImage`。
- `单图兼作背景`：唯一图片同时用于取色和背景，输出 JSON 与处理后的 `background.webp`。
- `参考图 + 独立背景图`：第一张图仅决定主题色，第二张图作为背景；不得混淆两张图的职责。
- 用户未明确模式时，有图片则默认 `纯色`，不得擅自把图片设为背景。

如果用户提供了明确主题名称则使用；否则根据图片或描述生成名称。忽略空值和未替换的占位符。

目标：
- 保留参考图片的主要色相、冷暖关系、明度层级和视觉气质。
- 生成适合桌面、Android 与 iOS 工具界面的浅色与深色主题。
- 图片模式在宽屏和竖屏裁切后仍保留主体，不影响文字与控件可读性。
- 输出结果必须通过 Ani Tracker Theme Pack v2 校验。

分析规则：
1. 在内部识别图片的主强调色、次强调色、浅中性色、深中性色和可用前景色，但不要输出分析过程。
2. `primary` 选择最能代表图片且适合主要操作的颜色；不得与 `destructive` 混淆。
3. `secondary` 和 `accent` 使用图片中的辅助色或主色低饱和变体，用于次级表面和选中背景。
4. `background`、`card`、`popover`、`sidebar` 必须低干扰并有可辨识层级，不得直接使用高饱和主色铺满页面。
5. 图片缺少状态色时，生成与整体色调协调但语义清晰的绿色 `success`、琥珀色 `warning`、蓝色 `info` 和红色 `destructive`。
6. `chart-1` 至 `chart-5` 应可相互区分，并保持与图片色板协调。
7. 深色主题必须独立调整明度和饱和度，保持相同色相性格；禁止简单反相浅色主题。
8. 以下每组“前景色 / 背景色”的 WCAG 对比度都必须至少为 4.5:1：
   - `foreground` / `background`
   - `card-foreground` / `card`
   - `popover-foreground` / `popover`
   - `primary-foreground` / `primary`
   - `secondary-foreground` / `secondary`
   - `muted-foreground` / `muted`
   - `accent-foreground` / `accent`
   - `destructive-foreground` / `destructive`
   - `success-foreground` / `success`
   - `warning-foreground` / `warning`
   - `info-foreground` / `info`
   - `sidebar-foreground` / `sidebar`
   - `sidebar-primary-foreground` / `sidebar-primary`
   - `sidebar-accent-foreground` / `sidebar-accent`
9. 以下每组“控件色 / 表面色”的 WCAG 对比度都必须至少为 3:1：`input` / `background`、`ring` / `background`、`input` / `card`、`ring` / `card`。
10. 所有前景色必须根据对应背景自动选择可读的浅色或深色，不要仅凭 HSL 明度值猜测对比度；应按 sRGB 相对亮度计算 WCAG 对比度。
11. 内容海报应保持真实清晰，不为内容图片增加统一色罩。
12. 图片模式应将背景转为 JPEG、PNG 或 WebP，优先 WebP；最长边不超过 3840、总像素不超过 1600 万、文件不超过 3 MiB。
13. 背景使用 `cover` 裁切。`position.x/y` 必须根据主体位置选择 0-100 的焦点百分比，兼顾横屏与竖屏。
14. 分别计算浅色与深色遮罩：先用对应 `tokens.*.background` 覆盖原图，再以 `foreground` 检查合成结果。对背景的最亮、最暗、高饱和和主体区域采样，所有样本对比度必须至少 4.5:1。
15. `overlayOpacity.light/dark` 必须在 0.55-0.98；优先选择刚好满足全部样本 4.5:1 的最小值。若 0.98 仍不达标，必须调整主题背景色或前景色，不得输出冲突主题。

输出规则：
1. 纯色模式只输出一个合法 JSON 文件；图片模式只输出一个 ZIP 文件。禁止附加 Markdown、解释或其他文件。
2. 顶层必填字段只能是：`schemaVersion`、`id`、`name`、`version`、`author`、`description`、`style`、`tokens`。图片模式额外且只能增加 `backgroundImage`。
3. `schemaVersion` 固定为数字 `2`；`version` 固定为字符串 `1.0.0`。
4. `id` 必须是 2-64 位小写 ASCII 字母、数字和连字符，并以字母或数字开头和结尾。根据图片或主题名称生成简短英文语义 slug。
5. `name` 长度为 1-40 个字符；优先使用用户提供的主题名称，否则根据图片气质生成简短中文名称。
6. `author` 固定使用 `Image Palette Generator`；`description` 使用 1-160 个字符简述配色特征。
7. `style` 只能包含 `radius`。根据图片气质从 `4px`、`6px`、`8px` 中选择一个值。
8. `tokens` 只能包含 `light` 和 `dark`，且两者都必须完整包含下方 JSON 结构中的 38 个令牌。
9. 每个颜色值必须是字符串形式的 HSL 通道，例如 `8 75% 49%`。禁止使用 `hsl()`、HEX、RGB、透明度或 CSS 变量。
10. Hue 必须在 0-360 范围，Saturation 和 Lightness 必须在 0-100% 范围。
11. 最终 JSON 使用 UTF-8 编码后的大小必须小于 128KB。
12. JSON 不得包含 CSS、JavaScript、Base64、图片地址、绝对路径、额外元数据、未知字段或未知令牌。
13. 图片模式 ZIP 根目录必须且只能包含 `<id>.ani-theme.json` 和 JSON 引用的 `background.webp`；不得包含目录、缩略图或说明文件。

严格遵循下面的 JSON 结构。尖括号中的内容只是生成说明，最终输出时必须全部替换为真实值，不得原样保留；`light` 和 `dark` 必须各自精确包含所示的 38 个令牌，不多不少：

{
  "schemaVersion": 2,
  "id": "<2-64 位英文语义 slug>",
  "name": "<1-40 个字符的主题名称>",
  "version": "1.0.0",
  "author": "Image Palette Generator",
  "description": "<1-160 个字符的配色说明>",
  "style": {
    "radius": "<4px、6px 或 8px>"
  },
  "tokens": {
    "light": {
      "background": "<H S% L%>",
      "foreground": "<H S% L%>",
      "card": "<H S% L%>",
      "card-foreground": "<H S% L%>",
      "popover": "<H S% L%>",
      "popover-foreground": "<H S% L%>",
      "primary": "<H S% L%>",
      "primary-foreground": "<H S% L%>",
      "secondary": "<H S% L%>",
      "secondary-foreground": "<H S% L%>",
      "muted": "<H S% L%>",
      "muted-foreground": "<H S% L%>",
      "accent": "<H S% L%>",
      "accent-foreground": "<H S% L%>",
      "destructive": "<H S% L%>",
      "destructive-foreground": "<H S% L%>",
      "success": "<H S% L%>",
      "success-foreground": "<H S% L%>",
      "warning": "<H S% L%>",
      "warning-foreground": "<H S% L%>",
      "info": "<H S% L%>",
      "info-foreground": "<H S% L%>",
      "border": "<H S% L%>",
      "input": "<H S% L%>",
      "ring": "<H S% L%>",
      "chart-1": "<H S% L%>",
      "chart-2": "<H S% L%>",
      "chart-3": "<H S% L%>",
      "chart-4": "<H S% L%>",
      "chart-5": "<H S% L%>",
      "sidebar": "<H S% L%>",
      "sidebar-foreground": "<H S% L%>",
      "sidebar-primary": "<H S% L%>",
      "sidebar-primary-foreground": "<H S% L%>",
      "sidebar-accent": "<H S% L%>",
      "sidebar-accent-foreground": "<H S% L%>",
      "sidebar-border": "<H S% L%>",
      "sidebar-ring": "<H S% L%>"
    },
    "dark": {
      "background": "<H S% L%>",
      "foreground": "<H S% L%>",
      "card": "<H S% L%>",
      "card-foreground": "<H S% L%>",
      "popover": "<H S% L%>",
      "popover-foreground": "<H S% L%>",
      "primary": "<H S% L%>",
      "primary-foreground": "<H S% L%>",
      "secondary": "<H S% L%>",
      "secondary-foreground": "<H S% L%>",
      "muted": "<H S% L%>",
      "muted-foreground": "<H S% L%>",
      "accent": "<H S% L%>",
      "accent-foreground": "<H S% L%>",
      "destructive": "<H S% L%>",
      "destructive-foreground": "<H S% L%>",
      "success": "<H S% L%>",
      "success-foreground": "<H S% L%>",
      "warning": "<H S% L%>",
      "warning-foreground": "<H S% L%>",
      "info": "<H S% L%>",
      "info-foreground": "<H S% L%>",
      "border": "<H S% L%>",
      "input": "<H S% L%>",
      "ring": "<H S% L%>",
      "chart-1": "<H S% L%>",
      "chart-2": "<H S% L%>",
      "chart-3": "<H S% L%>",
      "chart-4": "<H S% L%>",
      "chart-5": "<H S% L%>",
      "sidebar": "<H S% L%>",
      "sidebar-foreground": "<H S% L%>",
      "sidebar-primary": "<H S% L%>",
      "sidebar-primary-foreground": "<H S% L%>",
      "sidebar-accent": "<H S% L%>",
      "sidebar-accent-foreground": "<H S% L%>",
      "sidebar-border": "<H S% L%>",
      "sidebar-ring": "<H S% L%>"
    }
  }
}

图片模式必须在 `tokens` 后作为同级字段插入以下配置，并在 `tokens` 的关闭花括号后增加逗号；数值应按实际图片替换。纯色模式必须完全省略：

"backgroundImage": {
  "file": "background.webp",
  "position": {
    "x": 50,
    "y": 50
  },
  "overlayOpacity": {
    "light": 0.82,
    "dark": 0.86
  }
}

输出前在内部完成以下校验，但不要输出校验过程：
- JSON 可被标准 JSON.parse 解析。
- 不含尖括号、成对花括号或其他未替换占位符。
- 顶层、style、tokens、backgroundImage 和每套令牌均无额外字段。
- light 与 dark 各含全部 38 个令牌。
- 每个颜色都是有效的 HSL 通道字符串。
- 上述 14 组文字配色均达到 4.5:1，上述 4 组控件配色均达到 3:1。
- 深色主题不是浅色主题的机械反相。
- 图片模式已抽样检查最亮、最暗、高饱和和主体区域，浅色与深色合成结果均达到 4.5:1。
- ZIP 中 JSON 文件名与 `id` 一致，背景文件名与 `backgroundImage.file` 一致。

现在根据用户指定的模式和素材生成最终 JSON 或 ZIP 文件。
```

## 输出文件约束

| 项目 | 要求 |
| --- | --- |
| 文件名 | 纯色：`<id>.ani-theme.json`；图片：`<id>.ani-theme.zip` |
| 编码 | UTF-8 |
| 最大文件大小 | JSON 128KB；源 ZIP 20 MiB；背景 3 MiB |
| Schema | Ani Tracker Theme Pack v2 |
| 明暗模式 | `tokens.light` 与 `tokens.dark` 均必填 |
| 颜色格式 | `H S% L%`，不包含 `hsl()` |
| 圆角范围 | 0-12px；提示词默认限定为 4px、6px、8px |
| 安全边界 | 不允许 CSS、JavaScript、Base64、外链、路径或未知字段 |

## 导入失败排查

- 确认模型只返回 JSON 或 ZIP，没有说明文字。
- 图片模式确认 ZIP 根目录只有同 ID 的 JSON 和一张背景图。·
- 确认 `light` 和 `dark` 都包含全部 38 个令牌。
- 确认颜色值类似 `8 75% 49%`，没有 `#`、逗号或 `hsl()`。
- 确认 `id` 只包含小写字母、数字和连字符。
