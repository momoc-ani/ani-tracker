use std::path::{Path, PathBuf};

use ani_contracts::PlayerVideoEnhancement;
use ani_media::player::PlayerTransportError;

/// 画质增强策略的稳定标识，用于运行时选择和诊断名称。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnhancementStrategyId {
    Legacy,
    Anime4kUltra,
    FsrcnnxFidelity,
    ArtCnn,
}

impl EnhancementStrategyId {
    /// 返回可通过环境变量传入的策略标识。
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Anime4kUltra => "anime4k-ultra",
            Self::FsrcnnxFidelity => "fsrcnnx-fidelity",
            Self::ArtCnn => "artcnn",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "legacy" | "anime4k" | "anime4k-legacy" => Some(Self::Legacy),
            "anime4k-ultra" | "ultra" => Some(Self::Anime4kUltra),
            "fsrcnnx" | "fsrcnnx-fidelity" => Some(Self::FsrcnnxFidelity),
            "artcnn" | "art-cnn" => Some(Self::ArtCnn),
            _ => None,
        }
    }
}

/// 一个增强策略只负责自己的资源探测和 shader 管线，不参与播放状态管理。
trait EnhancementStrategy: Send + Sync {
    fn id(&self) -> EnhancementStrategyId;
    fn pipeline_name(&self) -> &'static str;
    fn available(&self) -> bool;
    fn describe(&self) -> String;
    fn shaders_for(
        &self,
        preset: PlayerVideoEnhancement,
    ) -> Result<Vec<&Path>, PlayerTransportError>;
}

/// 当前发布版本的兼容策略，保留原有 Anime4K 两份 shader 行为。
struct Anime4kLegacyStrategy {
    clamp: Option<PathBuf>,
    upscale: Option<PathBuf>,
}

impl Anime4kLegacyStrategy {
    fn resolve(roots: &[PathBuf]) -> Self {
        Self {
            clamp: find_resource(roots, "Anime4K_Clamp_Highlights.glsl"),
            upscale: find_resource(roots, "Anime4K_Upscale_Original_x2.glsl"),
        }
    }
}

impl EnhancementStrategy for Anime4kLegacyStrategy {
    fn id(&self) -> EnhancementStrategyId {
        EnhancementStrategyId::Legacy
    }

    fn pipeline_name(&self) -> &'static str {
        "anime4k"
    }

    fn available(&self) -> bool {
        self.upscale.is_some()
    }

    fn describe(&self) -> String {
        format!(
            "{}:clamp={} upscale={}",
            self.id().as_str(),
            describe_path(self.clamp.as_deref()),
            describe_path(self.upscale.as_deref())
        )
    }

    fn shaders_for(
        &self,
        preset: PlayerVideoEnhancement,
    ) -> Result<Vec<&Path>, PlayerTransportError> {
        if preset == PlayerVideoEnhancement::Off {
            return Ok(Vec::new());
        }
        let upscale = self.upscale.as_deref().ok_or_else(|| {
            PlayerTransportError::Unavailable("Anime4K legacy shader 资源缺失".to_owned())
        })?;
        let mut shaders = vec![upscale];
        if preset == PlayerVideoEnhancement::Clear {
            if let Some(clamp) = self.clamp.as_deref() {
                shaders.push(clamp);
            }
        }
        Ok(shaders)
    }
}

/// 单文件 Anime4K Ultra 策略，适合显式测试高质量 shader 链路。
struct Anime4kUltraStrategy {
    shader: Option<PathBuf>,
}

impl Anime4kUltraStrategy {
    fn resolve(roots: &[PathBuf]) -> Self {
        Self {
            shader: find_resource(roots, "Anime4K-Ultra.glsl"),
        }
    }
}

impl EnhancementStrategy for Anime4kUltraStrategy {
    fn id(&self) -> EnhancementStrategyId {
        EnhancementStrategyId::Anime4kUltra
    }

    fn pipeline_name(&self) -> &'static str {
        "anime4k-ultra"
    }

    fn available(&self) -> bool {
        self.shader.is_some()
    }

    fn describe(&self) -> String {
        format!(
            "{}:shader={}",
            self.id().as_str(),
            describe_path(self.shader.as_deref())
        )
    }

    fn shaders_for(
        &self,
        preset: PlayerVideoEnhancement,
    ) -> Result<Vec<&Path>, PlayerTransportError> {
        if preset == PlayerVideoEnhancement::Off {
            return Ok(Vec::new());
        }
        self.shader
            .as_deref()
            .map(|shader| vec![shader])
            .ok_or_else(|| {
                PlayerTransportError::Unavailable("Anime4K Ultra shader 资源缺失".to_owned())
            })
    }
}

/// FSRCNNX 策略，优先使用 LineArt 版本，兼容 Fidelity 文件名变体。
struct FsrcnnxFidelityStrategy {
    shader: Option<PathBuf>,
}

impl FsrcnnxFidelityStrategy {
    fn resolve(roots: &[PathBuf]) -> Self {
        Self {
            shader: find_first_resource(
                roots,
                &[
                    "FSRCNNX_x2_8-0-4-1_LineArt.glsl",
                    "FSRCNNX_x2_8-0-4-1.glsl",
                    "FSRCNNX_x2_8-0-4-1_Fidelity.glsl",
                ],
            ),
        }
    }
}

impl EnhancementStrategy for FsrcnnxFidelityStrategy {
    fn id(&self) -> EnhancementStrategyId {
        EnhancementStrategyId::FsrcnnxFidelity
    }

    fn pipeline_name(&self) -> &'static str {
        "fsrcnnx-fidelity"
    }

    fn available(&self) -> bool {
        self.shader.is_some()
    }

    fn describe(&self) -> String {
        format!(
            "{}:shader={}",
            self.id().as_str(),
            describe_path(self.shader.as_deref())
        )
    }

    fn shaders_for(
        &self,
        preset: PlayerVideoEnhancement,
    ) -> Result<Vec<&Path>, PlayerTransportError> {
        if preset == PlayerVideoEnhancement::Off {
            return Ok(Vec::new());
        }
        self.shader
            .as_deref()
            .map(|shader| vec![shader])
            .ok_or_else(|| PlayerTransportError::Unavailable("FSRCNNX shader 资源缺失".to_owned()))
    }
}

/// ArtCNN 策略，默认选择动画 2x 版本，避免清晰档意外放大到 4x。
struct ArtCnnStrategy {
    shader: Option<PathBuf>,
}

impl ArtCnnStrategy {
    fn resolve(roots: &[PathBuf]) -> Self {
        Self {
            shader: find_first_resource(
                roots,
                &[
                    "ArtCNN_C4F16.glsl",
                    "ArtCNN_C4F32.glsl",
                    "Ani4Kv2_ArtCNN_C4F32_i2.glsl",
                    "Ani4Kv2_ArtCNN_C4F32_i2_CMP.glsl",
                ],
            ),
        }
    }
}

impl EnhancementStrategy for ArtCnnStrategy {
    fn id(&self) -> EnhancementStrategyId {
        EnhancementStrategyId::ArtCnn
    }

    fn pipeline_name(&self) -> &'static str {
        "artcnn"
    }

    fn available(&self) -> bool {
        self.shader.is_some()
    }

    fn describe(&self) -> String {
        format!(
            "{}:shader={}",
            self.id().as_str(),
            describe_path(self.shader.as_deref())
        )
    }

    fn shaders_for(
        &self,
        preset: PlayerVideoEnhancement,
    ) -> Result<Vec<&Path>, PlayerTransportError> {
        if preset == PlayerVideoEnhancement::Off {
            return Ok(Vec::new());
        }
        self.shader
            .as_deref()
            .map(|shader| vec![shader])
            .ok_or_else(|| PlayerTransportError::Unavailable("ArtCNN shader 资源缺失".to_owned()))
    }
}

/// 按显式偏好选择一个策略；选择失败时保留 legacy 兼容行为。
pub(crate) struct EnhancementRegistry {
    selected: Box<dyn EnhancementStrategy>,
    selected_id: EnhancementStrategyId,
    fallback_reason: Option<String>,
    descriptions: Vec<String>,
}

impl EnhancementRegistry {
    /// 探测应用资源并构造增强策略注册表。
    pub(crate) fn resolve(roots: &[PathBuf], requested: Option<&str>) -> Self {
        let strategies: Vec<Box<dyn EnhancementStrategy>> = vec![
            Box::new(Anime4kLegacyStrategy::resolve(roots)),
            Box::new(Anime4kUltraStrategy::resolve(roots)),
            Box::new(FsrcnnxFidelityStrategy::resolve(roots)),
            Box::new(ArtCnnStrategy::resolve(roots)),
        ];
        let descriptions = strategies
            .iter()
            .map(|strategy| strategy.describe())
            .collect();
        let legacy_index = 0;
        let requested_id = requested.and_then(EnhancementStrategyId::parse);
        let requested_index =
            requested_id.and_then(|id| strategies.iter().position(|strategy| strategy.id() == id));

        let (selected_index, fallback_reason) = match (requested, requested_id, requested_index) {
            (None, _, _) => (legacy_index, None),
            (Some(raw), None, _) => (
                legacy_index,
                Some(format!("未知增强策略 {raw}，已回退 legacy")),
            ),
            (Some(raw), Some(id), Some(index)) if strategies[index].available() => {
                log::info!(
                    "libmpv 选择画质增强策略 requested={raw} strategy={}",
                    id.as_str()
                );
                (index, None)
            }
            (Some(raw), Some(id), Some(_)) => (
                legacy_index,
                Some(format!(
                    "增强策略 {raw}({}) 资源不完整，已回退 legacy",
                    id.as_str()
                )),
            ),
            (Some(raw), Some(id), None) => (
                legacy_index,
                Some(format!(
                    "增强策略 {raw}({}) 不存在，已回退 legacy",
                    id.as_str()
                )),
            ),
        };

        let selected_id = strategies[selected_index].id();
        if let Some(reason) = &fallback_reason {
            log::warn!("libmpv 画质增强策略回退: {reason}");
        }
        let selected = strategies
            .into_iter()
            .nth(selected_index)
            .expect("增强策略注册表必须包含 legacy 策略");
        Self {
            selected,
            selected_id,
            fallback_reason,
            descriptions,
        }
    }

    /// 返回当前选中的策略标识。
    pub(crate) fn selected_id(&self) -> EnhancementStrategyId {
        self.selected_id
    }

    /// 返回当前策略的诊断名称。
    pub(crate) fn pipeline_name(&self) -> &'static str {
        self.selected.pipeline_name()
    }

    /// 返回当前策略是否具备完整资源。
    pub(crate) fn available(&self) -> bool {
        self.selected.available()
    }

    /// 返回策略探测摘要，供初始化日志使用。
    pub(crate) fn describe(&self) -> String {
        let fallback = self
            .fallback_reason
            .as_deref()
            .map_or_else(|| "none".to_owned(), ToOwned::to_owned);
        format!(
            "selected={} fallback={} [{}]",
            self.selected_id().as_str(),
            fallback,
            self.descriptions.join(", ")
        )
    }

    /// 将播放器预设转换为当前策略负责的 shader 列表。
    pub(crate) fn shaders_for(
        &self,
        preset: PlayerVideoEnhancement,
    ) -> Result<Vec<&Path>, PlayerTransportError> {
        self.selected.shaders_for(preset)
    }
}

fn find_resource(roots: &[PathBuf], name: &str) -> Option<PathBuf> {
    roots
        .iter()
        .map(|root| root.join(name))
        .find(|path| path.is_file())
}

fn find_first_resource(roots: &[PathBuf], names: &[&str]) -> Option<PathBuf> {
    names.iter().find_map(|name| find_resource(roots, name))
}

fn describe_path(path: Option<&Path>) -> String {
    path.map_or_else(|| "missing".to_owned(), |value| value.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn defaults_to_legacy_without_requested_strategy() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/shaders/anime4k");
        let registry = EnhancementRegistry::resolve(&[root], None);
        assert_eq!(registry.selected_id(), EnhancementStrategyId::Legacy);
        assert!(registry.available());
        assert_eq!(
            registry
                .shaders_for(PlayerVideoEnhancement::Balanced)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn unknown_strategy_falls_back_to_legacy() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/shaders/anime4k");
        let registry = EnhancementRegistry::resolve(&[root], Some("does-not-exist"));
        assert_eq!(registry.selected_id(), EnhancementStrategyId::Legacy);
        assert!(registry.describe().contains("未知增强策略"));
    }

    #[test]
    fn missing_external_strategy_falls_back_without_disabling_legacy() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/shaders/anime4k");
        let registry = EnhancementRegistry::resolve(&[root], Some("anime4k-ultra"));
        assert_eq!(registry.selected_id(), EnhancementStrategyId::Legacy);
        assert!(registry.available());
        assert!(registry.describe().contains("anime4k-ultra"));
    }

    #[test]
    fn selects_external_strategy_when_its_resource_is_present() {
        let root =
            std::env::temp_dir().join(format!("ani-enhancement-strategy-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Anime4K_Clamp_Highlights.glsl"), "legacy").unwrap();
        std::fs::write(root.join("Anime4K_Upscale_Original_x2.glsl"), "legacy").unwrap();
        std::fs::write(root.join("Anime4K-Ultra.glsl"), "ultra").unwrap();

        let registry =
            EnhancementRegistry::resolve(std::slice::from_ref(&root), Some("anime4k-ultra"));
        assert_eq!(registry.selected_id(), EnhancementStrategyId::Anime4kUltra);
        assert_eq!(registry.pipeline_name(), "anime4k-ultra");
        assert_eq!(
            registry
                .shaders_for(PlayerVideoEnhancement::Clear)
                .unwrap()
                .len(),
            1
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolves_bundled_fsrcnnx_and_artcnn_strategies() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/shaders/anime4k");
        let fsrcnnx = EnhancementRegistry::resolve(std::slice::from_ref(&root), Some("fsrcnnx"));
        assert_eq!(
            fsrcnnx.selected_id(),
            EnhancementStrategyId::FsrcnnxFidelity
        );
        assert_eq!(
            fsrcnnx
                .shaders_for(PlayerVideoEnhancement::Balanced)
                .unwrap()
                .len(),
            1
        );

        let artcnn = EnhancementRegistry::resolve(&[root], Some("artcnn"));
        assert_eq!(artcnn.selected_id(), EnhancementStrategyId::ArtCnn);
        assert_eq!(
            artcnn
                .shaders_for(PlayerVideoEnhancement::Clear)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn parses_supported_strategy_aliases() {
        assert_eq!(
            EnhancementStrategyId::parse("anime4k"),
            Some(EnhancementStrategyId::Legacy)
        );
        assert_eq!(
            EnhancementStrategyId::parse("ultra"),
            Some(EnhancementStrategyId::Anime4kUltra)
        );
        assert_eq!(
            EnhancementStrategyId::parse("fsrcnnx"),
            Some(EnhancementStrategyId::FsrcnnxFidelity)
        );
        assert_eq!(
            EnhancementStrategyId::parse("art-cnn"),
            Some(EnhancementStrategyId::ArtCnn)
        );
    }
}
