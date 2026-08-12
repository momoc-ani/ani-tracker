use std::collections::HashSet;
use std::sync::OnceLock;

use ani_domain::{
    Anime, AutomaticDownloadDecision, FansubGroup, MyAnime, NormalizedVideoCodec, Release,
    ReleaseContentKind, ReleaseEpisodeRange, ReleaseMatchContext, ReleaseMatchResult,
    ReleaseResolution, SubtitleLanguage, SubtitlePreference,
};
use regex::Regex;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

pub const AUTOMATIC_DOWNLOAD_MIN_SCORE: i64 = 75;
pub const AUTOMATIC_DOWNLOAD_MIN_MATCH_SCORE: i64 = 40;
pub const AUTOMATIC_DOWNLOAD_MIN_LEAD: i64 = 5;

/// 从资源标题解析出的业务和媒体字段。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedReleaseTitle {
    pub fansub_name: Option<String>,
    pub episode_no: Option<f64>,
    pub episode_range: Option<ReleaseEpisodeRange>,
    pub series_season_no: Option<i64>,
    pub content_kind: ReleaseContentKind,
    pub resolution: Option<ReleaseResolution>,
    pub declared_video_codec: Option<String>,
    pub normalized_video_codec: NormalizedVideoCodec,
    pub bit_depth: Option<i64>,
    pub subtitle_languages: Vec<SubtitleLanguage>,
    pub subtitle: Option<SubtitlePreference>,
}

macro_rules! static_regex {
    ($function:ident, $pattern:literal) => {
        fn $function() -> &'static Regex {
            static VALUE: OnceLock<Regex> = OnceLock::new();
            VALUE.get_or_init(|| Regex::new($pattern).expect("静态正则必须有效"))
        }
    };
}

static_regex!(
    resolution_2160,
    r"(?i)(?:^|[^a-z0-9])(?:2160p|4k|3840x2160)(?:[^a-z0-9]|$)"
);
static_regex!(
    resolution_1080,
    r"(?i)(?:^|[^a-z0-9])(?:1080p|1920x1080)(?:[^a-z0-9]|$)"
);
static_regex!(
    resolution_720,
    r"(?i)(?:^|[^a-z0-9])(?:720p|1280x720)(?:[^a-z0-9]|$)"
);
static_regex!(
    episode_range_season,
    r"(?i)(?:^|[\s_-])s\d{1,2}e(\d{1,3}(?:\.\d)?)\s*[-~]\s*(\d{1,3}(?:\.\d)?)"
);
static_regex!(
    episode_range_bracket,
    r"[\[【(（]\s*(\d{1,3}(?:\.\d)?)\s*[-~]\s*(\d{1,3}(?:\.\d)?)"
);
static_regex!(
    episode_range_generic,
    r"(?i)(?:^|[\s_-])(?:ep|episode|第)?\s*(\d{1,3}(?:\.\d)?)\s*[-~]\s*(\d{1,3}(?:\.\d)?)"
);
static_regex!(
    episode_season,
    r"(?i)(?:^|[\s_-])s\d{1,2}e(\d{1,3}(?:\.\d)?)"
);
static_regex!(episode_bracket, r"\[\s*(\d{1,3}(?:\.\d)?)\s*\]");
static_regex!(
    episode_hyphen,
    r"(?i)-\s*(\d{1,3}(?:\.\d)?)\s*(?:v\d)?(?:\s|\[|$)"
);
static_regex!(episode_chinese, r"第\s*(\d{1,3}(?:\.\d)?)\s*(?:话|話|集)");
static_regex!(
    episode_latin,
    r"(?i)(?:^|[\s_-])(?:ep|episode|e)\s*(\d{1,3}(?:\.\d)?)(?:[\s_.-]|$)"
);
static_regex!(
    technical_number,
    r"(?i)\b\d{1,4}(?:\.\d+)?\s*[- ]?\s*(?:bits?|gib|gb|mib|mb|fps|hz|khz)\b"
);
static_regex!(fansub_prefix, r"^\s*[\[【]([^\]】]+)[\]】]");
static_regex!(bit_depth, r"(?i)\b(8|10|12)\s*[- ]?\s*bits?\b");
static_regex!(hi10p, r"(?i)\b(?:hi10p|main\s*10)\b");
static_regex!(codec_hevc, r"(?i)\b(?:h\.?265|x265|hevc)\b");
static_regex!(codec_avc, r"(?i)\b(?:h\.?264|x264|avc)\b");
static_regex!(codec_av1, r"(?i)\bav1\b");
static_regex!(codec_vp9, r"(?i)\bvp9\b");
static_regex!(
    series_season_chinese,
    r"第\s*([〇零一二三四五六七八九十百两\d]+)\s*(?:季|期|部)"
);
static_regex!(
    series_season_ordinal,
    r"(?i)\b(\d{1,2})(?:st|nd|rd|th)\s+season\b"
);
static_regex!(series_season_word, r"(?i)\bseason\s*0*(\d{1,2})\b");
static_regex!(
    series_season_short,
    r"(?i)(?:^|[^a-z0-9])s0*(\d{1,2})(?:e\d{1,3}|[^a-z0-9]|$)"
);
static_regex!(
    search_brackets,
    r"[「『《【\[(（]([^」』》】\])）]{2,80})[」』》】\])）]"
);
static_regex!(search_separators, r"[|｜／/]+|\s+[-–—]\s+|[:：]");
static_regex!(
    season_suffix_chinese,
    r"\s*第\s*[〇零一二三四五六七八九十百两\d]+\s*[季期部篇章]\s*$"
);
static_regex!(
    season_suffix_ordinal,
    r"(?i)\s+\d+(?:st|nd|rd|th)\s+season\s*$"
);
static_regex!(season_suffix_word, r"(?i)\s+(?:season|part)\s*\d+\s*$");
static_regex!(season_suffix_short, r"(?i)\s+s\d+\s*$");

/// 解析标题中的字幕组、集数、季度和媒体技术字段。
pub fn parse_release_title(title: &str, groups: &[FansubGroup]) -> ParsedReleaseTitle {
    let episode_range = detect_episode_range(title);
    let without_technical = technical_number().replace_all(title, " ");
    let episode_no = episode_range
        .is_none()
        .then(|| detect_episode_no(&without_technical))
        .flatten();
    let subtitle_languages = detect_subtitle_languages(title);
    let subtitle = if contains_generic_multi(title) || subtitle_languages.len() > 1 {
        Some(SubtitlePreference::Multi)
    } else {
        subtitle_languages.first().map(language_to_preference)
    };

    ParsedReleaseTitle {
        fansub_name: detect_fansub_name(title, groups),
        episode_no,
        episode_range: episode_range.clone(),
        series_season_no: detect_series_season_no(title),
        content_kind: resolve_content_kind(title, episode_no, episode_range.as_ref()),
        resolution: detect_resolution(title),
        declared_video_codec: detect_codec_label(title),
        normalized_video_codec: normalize_video_codec(title),
        bit_depth: detect_bit_depth(title),
        subtitle_languages,
        subtitle,
    }
}

/// 使用标题解析结果补齐资源，同时保留来源显式提供的字段。
pub fn enrich_release_from_title(mut release: Release, groups: &[FansubGroup]) -> Release {
    let parsed = parse_release_title(&release.title, groups);
    let episode_range = parsed
        .episode_range
        .clone()
        .or(release.episode_range.clone());
    let content_kind = if episode_range.is_some() {
        ReleaseContentKind::Range
    } else {
        release
            .content_kind
            .clone()
            .unwrap_or_else(|| parsed.content_kind.clone())
    };
    let fansub_name = release
        .fansub_name
        .clone()
        .or_else(|| parsed.fansub_name.clone());
    let matched_group = fansub_name.as_deref().and_then(|name| {
        let key = normalize_fansub_name(name);
        groups.iter().find(|group| {
            std::iter::once(&group.name)
                .chain(group.aliases.iter())
                .any(|alias| normalize_fansub_name(alias) == key)
        })
    });
    let existing_group_id = release
        .fansub_group_id
        .clone()
        .filter(|value| !value.starts_with("fansub-auto-"));
    let discovered_group_id = fansub_name
        .as_deref()
        .filter(|name| is_meaningful_fansub_name(name))
        .map(create_discovered_fansub_id);

    if matches!(
        content_kind,
        ReleaseContentKind::Range | ReleaseContentKind::Batch
    ) {
        release.episode_no = None;
    } else if release.episode_no.is_none() {
        release.episode_no = parsed.episode_no;
    }
    release.episode_range = episode_range;
    release.series_season_no = release.series_season_no.or(parsed.series_season_no);
    release.content_kind = Some(content_kind);
    release.fansub_name = fansub_name;
    release.fansub_group_id = existing_group_id
        .or_else(|| matched_group.map(|group| group.id.clone()))
        .or(release.fansub_group_id)
        .or(discovered_group_id);
    release.resolution = release.resolution.or(parsed.resolution);
    release.declared_video_codec = release.declared_video_codec.or(parsed.declared_video_codec);
    if release.normalized_video_codec.is_none()
        && parsed.normalized_video_codec != NormalizedVideoCodec::Unknown
    {
        release.normalized_video_codec = Some(parsed.normalized_video_codec);
    }
    release.bit_depth = release.bit_depth.or(parsed.bit_depth);
    if release.subtitle_languages.is_empty() {
        release.subtitle_languages = if !parsed.subtitle_languages.is_empty() {
            parsed.subtitle_languages
        } else {
            legacy_subtitle_languages(release.subtitle.as_ref())
        };
    }
    release.subtitle = release
        .subtitle
        .or(parsed.subtitle)
        .or_else(|| languages_to_legacy(&release.subtitle_languages));
    release
}

/// 为运行时发现的字幕组生成跨进程稳定的标识。
pub fn create_discovered_fansub_id(name: &str) -> String {
    let digest = Sha256::digest(normalize_fansub_name(name).as_bytes());
    format!("fansub-auto-{digest:x}")[..28].to_owned()
}

/// 规范化字幕组大小写、全半角和常见中日异体字。
pub fn normalize_fansub_name(name: &str) -> String {
    name.nfkc()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_lowercase()
        .chars()
        .map(normalize_fansub_character)
        .collect()
}

/// 排除技术标签和占位名称，避免污染字幕组目录。
pub fn is_meaningful_fansub_name(name: &str) -> bool {
    let normalized = normalize_fansub_name(name);
    if normalized.is_empty() || normalized.chars().count() > 80 {
        return false;
    }
    if [
        "字幕组",
        "压制组",
        "fansub",
        "unknown",
        "未知",
        "未识别字幕组",
    ]
    .contains(&normalized.as_str())
    {
        return false;
    }
    !is_technical_fansub_name(&normalized)
}

/// 生成番剧标题、原名和别名的去重搜索词。
pub fn build_anime_release_search_terms(
    anime: &Anime,
    extra_terms: &[String],
    limit: usize,
) -> Vec<String> {
    let raw = extra_terms
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(anime.title.as_str()))
        .chain(anime.original_title.as_deref())
        .chain(anime.aliases.iter().map(|alias| alias.alias.as_str()));
    unique_search_terms(raw.flat_map(expand_search_term))
        .into_iter()
        .take(limit)
        .collect()
}

/// 规范化资源搜索文本，忽略全半角、标点、空白和大小写差异。
pub fn normalize_release_search_text(value: &str) -> String {
    let normalized = value.nfkc().collect::<String>();
    let replaced = normalized
        .chars()
        .map(|character| {
            if is_search_punctuation(character) {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    replaced
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// 判断资源标题是否包含目标番剧任一有效名称。
pub fn matches_anime_release_title(release_title: &str, terms: &[String]) -> bool {
    let normalized_title = normalize_release_search_text(release_title);
    let compact_title = remove_whitespace(&normalized_title);
    unique_search_terms(terms.iter().flat_map(|term| expand_search_term(term)))
        .into_iter()
        .map(|term| normalize_release_search_text(&term))
        .filter(|term| is_distinctive_search_term(term))
        .any(|term| {
            normalized_title.contains(&term) || compact_title.contains(&remove_whitespace(&term))
        })
}

/// 从标题解析中文、英文或 Sxx 系列季度编号。
pub fn detect_series_season_no(value: &str) -> Option<i64> {
    [
        series_season_chinese(),
        series_season_ordinal(),
        series_season_word(),
        series_season_short(),
    ]
    .into_iter()
    .find_map(|pattern| {
        pattern
            .captures(value)
            .and_then(|captures| captures.get(1))
            .and_then(|matched| parse_season_number(matched.as_str()))
            .filter(|value| *value > 0)
    })
}

/// 判断资源属于当前季度、其他待确认资源或明确冲突季度。
pub fn classify_anime_release(release: &Release, anime: &Anime) -> AnimeReleaseCompatibility {
    let target = resolve_anime_series_season_no(anime);
    let actual = release
        .series_season_no
        .or_else(|| detect_series_season_no(&release.title));
    if target.is_some() && actual.is_some() && target != actual {
        return AnimeReleaseCompatibility::Mismatch;
    }
    if target.is_some_and(|value| value > 1) && actual.is_none() {
        return AnimeReleaseCompatibility::Other;
    }
    if release.content_kind == Some(ReleaseContentKind::Batch)
        && (target.is_none() || actual.is_none())
    {
        return AnimeReleaseCompatibility::Other;
    }
    AnimeReleaseCompatibility::Current
}

/// 资源与当前番剧的季度兼容性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimeReleaseCompatibility {
    Current,
    Other,
    Mismatch,
}

/// 判断单集资源或连集范围是否覆盖目标集数。
pub fn release_matches_episode(release: &Release, episode_no: Option<f64>) -> bool {
    let Some(episode_no) = episode_no else {
        return true;
    };
    release.episode_no == Some(episode_no)
        || release
            .episode_range
            .as_ref()
            .is_some_and(|range| episode_no >= range.start && episode_no <= range.end)
}

/// 按番剧、集数、偏好和可用性为资源排序。
pub fn rank_releases(
    releases: &[Release],
    context: &ReleaseMatchContext,
    groups: &[FansubGroup],
) -> Vec<ReleaseMatchResult> {
    let mut ranked = releases
        .iter()
        .cloned()
        .map(|release| score_release(enrich_release_from_title(release, groups), context, groups))
        .filter(|result| result.score > 0)
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| {
                right
                    .release
                    .seeders
                    .unwrap_or(-1)
                    .cmp(&left.release.seeders.unwrap_or(-1))
            })
            .then_with(|| right.release.published_at.cmp(&left.release.published_at))
    });
    ranked
}

/// 按追番规则排列展示资源，保留低分候选且不改变自动下载门禁。
pub fn sort_releases_by_rules<F>(
    releases: Vec<Release>,
    resolve_context: F,
    groups: &[FansubGroup],
) -> Vec<Release>
where
    F: Fn(&Release) -> ReleaseMatchContext,
{
    let mut scored = releases
        .into_iter()
        .enumerate()
        .map(|(index, release)| {
            let release = enrich_release_from_title(release, groups);
            let context = resolve_context(&release);
            (index, score_release(release, &context, groups))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .1
            .preference_score
            .cmp(&left.1.preference_score)
            .then_with(|| right.1.match_score.cmp(&left.1.match_score))
            .then_with(|| {
                right
                    .1
                    .release
                    .seeders
                    .unwrap_or(-1)
                    .cmp(&left.1.release.seeders.unwrap_or(-1))
            })
            .then_with(|| {
                right
                    .1
                    .release
                    .published_at
                    .cmp(&left.1.release.published_at)
            })
            .then_with(|| left.0.cmp(&right.0))
    });
    scored
        .into_iter()
        .map(|(_, result)| result.release)
        .collect()
}

/// 计算单条资源的匹配、偏好和可用性评分。
pub fn score_release(
    release: Release,
    context: &ReleaseMatchContext,
    groups: &[FansubGroup],
) -> ReleaseMatchResult {
    let mut reasons = Vec::new();
    let mut warnings = Vec::new();
    let mut match_score = 0;
    let mut preference_score = 0;
    let mut availability_score = 0;

    if classify_anime_release(&release, &context.anime.anime) != AnimeReleaseCompatibility::Current
    {
        return match_result(
            release,
            0,
            0,
            0,
            vec!["资源季度不兼容".to_owned()],
            warnings,
        );
    }
    if !matches_release_anime(&release, &context.anime) {
        return match_result(
            release,
            0,
            0,
            0,
            vec!["资源番剧不匹配".to_owned()],
            warnings,
        );
    }
    if !release_matches_episode(&release, context.episode_no) {
        return match_result(
            release,
            0,
            0,
            0,
            vec!["资源未覆盖目标集数".to_owned()],
            warnings,
        );
    }

    match_score += 20;
    reasons.push(if release.anime_id.is_some() {
        "资源已关联目标番剧".to_owned()
    } else {
        "标题匹配番剧别名".to_owned()
    });
    if release.anime_id.as_deref() == Some(context.anime.anime.id.as_str()) {
        match_score += 5;
    }
    if let Some(episode_no) = context.episode_no {
        if release.episode_no == Some(episode_no) {
            match_score += 25;
            reasons.push("集数精确匹配".to_owned());
        } else if release_matches_episode(&release, Some(episode_no)) {
            match_score += 15;
            reasons.push("集数范围覆盖".to_owned());
        }
    } else {
        match_score += 25;
    }

    let preferred_fansub = context
        .episode_fansub_override_id
        .as_ref()
        .or(context.anime.default_fansub_group_id.as_ref());
    match preferred_fansub {
        None => preference_score += 14,
        Some(preferred) if release.fansub_group_id.as_ref() == Some(preferred) => {
            preference_score += 14;
            reasons.push(if context.episode_fansub_override_id.is_some() {
                "匹配单集字幕组覆盖".to_owned()
            } else {
                "匹配默认字幕组".to_owned()
            });
        }
        Some(_) if matches_candidate_fansub(&release, context, groups) => {
            preference_score += 5;
            reasons.push("匹配候补字幕组".to_owned());
        }
        Some(_) => {}
    }

    match context.anime.preferred_resolution.as_deref() {
        None => preference_score += 5,
        Some(preferred)
            if release.resolution.as_ref().map(ReleaseResolution::as_str) == Some(preferred) =>
        {
            preference_score += 5;
            reasons.push("匹配清晰度偏好".to_owned());
        }
        Some(_) => {}
    }
    match context.anime.preferred_codec.as_deref() {
        None => preference_score += 5,
        Some(preferred)
            if release
                .normalized_video_codec
                .as_ref()
                .map(NormalizedVideoCodec::as_str)
                == Some(preferred) =>
        {
            preference_score += 5;
            reasons.push("匹配编码偏好".to_owned());
        }
        Some(_) if release.normalized_video_codec.is_none() => warnings.push("编码未知".to_owned()),
        Some(_) => {}
    }
    match context.anime.preferred_bit_depth {
        None => preference_score += 6,
        Some(preferred) if release.bit_depth == Some(preferred) => {
            preference_score += 6;
            reasons.push("匹配位深偏好".to_owned());
        }
        Some(_) if release.bit_depth.is_none() => warnings.push("位深未知".to_owned()),
        Some(_) => {}
    }

    let preferred_languages = resolve_preferred_subtitle_languages(&context.anime);
    let coverage = subtitle_coverage(&release, &preferred_languages);
    if release.subtitle == Some(SubtitlePreference::Multi) && release.subtitle_languages.is_empty()
    {
        warnings.push("多语字幕组成未知".to_owned());
    }
    preference_score += (coverage * 10.0).round() as i64;
    if !preferred_languages.is_empty() && coverage > 0.0 {
        reasons.push(if (coverage - 1.0).abs() < f64::EPSILON {
            "完整覆盖字幕语言偏好".to_owned()
        } else {
            "部分覆盖字幕语言偏好".to_owned()
        });
    } else if !preferred_languages.is_empty() {
        warnings.push("字幕语言未命中".to_owned());
    }

    if release.magnet_url.is_some() || release.torrent_url.is_some() {
        availability_score += 2;
    }
    match release.seeders {
        None => availability_score += 2,
        Some(seeders) if seeders > 0 => {
            availability_score += (((seeders + 1) as f64).log2().ceil() as i64).min(6);
            reasons.push("存在做种".to_owned());
        }
        Some(_) => {}
    }
    let metadata_count = [
        release.normalized_video_codec.is_some(),
        release.bit_depth.is_some(),
        !release.subtitle_languages.is_empty() || release.subtitle.is_some(),
    ]
    .into_iter()
    .filter(|value| *value)
    .count();
    availability_score += match metadata_count {
        3 => 2,
        1 | 2 => 1,
        _ => 0,
    };

    match_result(
        release,
        match_score,
        preference_score,
        availability_score,
        reasons,
        warnings,
    )
}

/// 判断最高分资源是否达到自动下载阈值且明显领先。
pub fn evaluate_automatic_download(results: &[ReleaseMatchResult]) -> AutomaticDownloadDecision {
    let Some(best) = results.first() else {
        return AutomaticDownloadDecision {
            accepted: false,
            reason: "未找到匹配资源".to_owned(),
        };
    };
    if best.match_score < AUTOMATIC_DOWNLOAD_MIN_MATCH_SCORE {
        return AutomaticDownloadDecision {
            accepted: false,
            reason: format!("资源匹配可信度不足（{}/50）", best.match_score),
        };
    }
    if best.score < AUTOMATIC_DOWNLOAD_MIN_SCORE {
        return AutomaticDownloadDecision {
            accepted: false,
            reason: format!("资源综合评分不足（{}/100）", best.score),
        };
    }
    if results
        .get(1)
        .is_some_and(|second| best.score - second.score < AUTOMATIC_DOWNLOAD_MIN_LEAD)
    {
        return AutomaticDownloadDecision {
            accepted: false,
            reason: format!("最高候选领先不足 {AUTOMATIC_DOWNLOAD_MIN_LEAD} 分"),
        };
    }
    AutomaticDownloadDecision {
        accepted: true,
        reason: format!("资源可信度通过（{}/100）", best.score),
    }
}

/// 判断资源是否完整覆盖自动下载要求的字幕语言；无法确认组成时按不满足处理。
pub fn release_satisfies_subtitle_requirement(
    release: &Release,
    preferred_languages: &[String],
    legacy_preference: Option<&str>,
) -> bool {
    let preferred = resolve_subtitle_requirement(preferred_languages, legacy_preference);
    preferred.is_empty() || (subtitle_coverage(release, &preferred) - 1.0).abs() < f64::EPSILON
}

fn detect_resolution(title: &str) -> Option<ReleaseResolution> {
    if resolution_2160().is_match(title) {
        Some(ReleaseResolution::P2160)
    } else if resolution_1080().is_match(title) {
        Some(ReleaseResolution::P1080)
    } else if resolution_720().is_match(title) {
        Some(ReleaseResolution::P720)
    } else {
        None
    }
}

fn detect_episode_range(title: &str) -> Option<ReleaseEpisodeRange> {
    for pattern in [episode_range_season(), episode_range_bracket()] {
        if let Some(range) = pattern
            .captures(title)
            .as_ref()
            .and_then(parse_episode_range_captures)
        {
            return Some(range);
        }
    }

    let captures = episode_range_generic().captures(title)?;
    if is_ambiguous_season_episode_range(&captures) {
        return None;
    }
    parse_episode_range_captures(&captures)
}

/// 将正则捕获的起止集数转换为有效连集范围。
fn parse_episode_range_captures(captures: &regex::Captures<'_>) -> Option<ReleaseEpisodeRange> {
    let start = captures.get(1)?.as_str().parse::<f64>().ok()?;
    let end = captures.get(2)?.as_str().parse::<f64>().ok()?;
    (end > start).then_some(ReleaseEpisodeRange { start, end })
}

/// 排除“第二季 2 - 05”被当成第 2 至 5 集的歧义写法。
fn is_ambiguous_season_episode_range(captures: &regex::Captures<'_>) -> bool {
    let Some(start) = captures.get(1).map(|value| value.as_str()) else {
        return false;
    };
    let Some(end) = captures.get(2).map(|value| value.as_str()) else {
        return false;
    };
    let Some(full_match) = captures.get(0).map(|value| value.as_str()) else {
        return false;
    };
    let normalized = full_match
        .trim_start_matches(|character: char| {
            character.is_whitespace() || character == '_' || character == '-'
        })
        .to_ascii_lowercase();
    let explicitly_labeled = normalized.starts_with("ep")
        || normalized.starts_with("episode")
        || normalized.starts_with('第');
    let spaced_separator = full_match.contains(" - ") || full_match.contains(" ~ ");
    !explicitly_labeled
        && spaced_separator
        && start.len() == 1
        && end.len() > 1
        && end.starts_with('0')
}

fn detect_episode_no(title: &str) -> Option<f64> {
    [
        episode_season(),
        episode_bracket(),
        episode_hyphen(),
        episode_chinese(),
        episode_latin(),
    ]
    .into_iter()
    .find_map(|pattern| {
        pattern
            .captures(title)
            .and_then(|captures| captures.get(1))
            .and_then(|matched| matched.as_str().parse::<f64>().ok())
    })
}

fn resolve_content_kind(
    title: &str,
    episode_no: Option<f64>,
    episode_range: Option<&ReleaseEpisodeRange>,
) -> ReleaseContentKind {
    if episode_range.is_some() {
        ReleaseContentKind::Range
    } else if episode_no.is_some() {
        ReleaseContentKind::Episode
    } else if is_batch_title(title) {
        ReleaseContentKind::Batch
    } else {
        ReleaseContentKind::Unknown
    }
}

fn is_batch_title(title: &str) -> bool {
    let lower = title.to_lowercase();
    ["全集", "合集", "合輯", "总集篇", "總集篇", "完结", "完結"]
        .iter()
        .any(|marker| title.contains(marker))
        || lower.contains("complete")
        || lower.contains("collection")
        || lower.contains("blu-ray box")
        || lower.contains("bluray box")
        || lower.contains("bd box")
        || Regex::new(r"(?i)(?:^|[\s_.\-\[【(（])s\d{1,2}\s*(?:fin|complete)(?:$|[\s_.\-\]】)）])")
            .expect("batch regex")
            .is_match(title)
        || Regex::new(r"全\s*\d+\s*[集话話]")
            .expect("batch count regex")
            .is_match(title)
}

fn detect_fansub_name(title: &str, groups: &[FansubGroup]) -> Option<String> {
    if let Some(name) = fansub_prefix()
        .captures(title)
        .and_then(|captures| captures.get(1))
        .map(|matched| matched.as_str().trim())
        .filter(|name| !name.is_empty())
    {
        return Some(name.to_owned());
    }
    let lower = title.to_lowercase();
    groups.iter().find_map(|group| {
        std::iter::once(&group.name)
            .chain(group.aliases.iter())
            .find(|name| lower.contains(&name.to_lowercase()))
            .cloned()
    })
}

fn detect_codec_label(title: &str) -> Option<String> {
    [codec_hevc(), codec_avc(), codec_av1(), codec_vp9()]
        .into_iter()
        .find_map(|pattern| {
            pattern
                .find(title)
                .map(|matched| matched.as_str().to_owned())
        })
}

fn normalize_video_codec(title: &str) -> NormalizedVideoCodec {
    if codec_hevc().is_match(title) {
        NormalizedVideoCodec::H265Hevc
    } else if codec_avc().is_match(title) {
        NormalizedVideoCodec::H264Avc
    } else if codec_av1().is_match(title) {
        NormalizedVideoCodec::Av1
    } else if codec_vp9().is_match(title) {
        NormalizedVideoCodec::Vp9
    } else {
        NormalizedVideoCodec::Unknown
    }
}

fn detect_bit_depth(title: &str) -> Option<i64> {
    if hi10p().is_match(title) {
        return Some(10);
    }
    bit_depth()
        .captures(title)
        .and_then(|captures| captures.get(1))
        .and_then(|matched| matched.as_str().parse().ok())
}

fn detect_subtitle_languages(title: &str) -> Vec<SubtitleLanguage> {
    let lower = title.to_lowercase();
    let mut values = HashSet::new();
    if title.contains("简繁") || title.contains("繁简") {
        values.insert(SubtitleLanguage::Chs);
        values.insert(SubtitleLanguage::Cht);
    }
    if title.contains("简日") || title.contains("简中日") {
        values.insert(SubtitleLanguage::Chs);
        values.insert(SubtitleLanguage::Jpn);
    }
    if title.contains("繁日") || title.contains("繁中日") {
        values.insert(SubtitleLanguage::Cht);
        values.insert(SubtitleLanguage::Jpn);
    }
    if contains_word(&lower, "chs")
        || contains_word(&lower, "gb")
        || title.contains("简体")
        || title.contains("简中")
    {
        values.insert(SubtitleLanguage::Chs);
    }
    if contains_word(&lower, "cht")
        || contains_word(&lower, "big5")
        || title.contains("繁体")
        || title.contains("繁中")
    {
        values.insert(SubtitleLanguage::Cht);
    }
    if contains_word(&lower, "jpn")
        || contains_word(&lower, "jp")
        || title.contains("日文")
        || title.contains("日语")
        || title.contains("日語")
    {
        values.insert(SubtitleLanguage::Jpn);
    }
    if contains_word(&lower, "eng")
        || title.contains("英文")
        || title.contains("英语")
        || title.contains("英語")
    {
        values.insert(SubtitleLanguage::Eng);
    }
    [
        SubtitleLanguage::Chs,
        SubtitleLanguage::Cht,
        SubtitleLanguage::Jpn,
        SubtitleLanguage::Eng,
    ]
    .into_iter()
    .filter(|language| values.contains(language))
    .collect()
}

fn contains_generic_multi(title: &str) -> bool {
    let lower = title.to_lowercase();
    contains_word(&lower, "multi")
        || ["多语", "多語", "多国语言", "多國語言"]
            .iter()
            .any(|marker| title.contains(marker))
}

fn contains_word(value: &str, word: &str) -> bool {
    value.match_indices(word).any(|(index, _)| {
        let before = value[..index].chars().next_back();
        let after = value[index + word.len()..].chars().next();
        before.is_none_or(|character| !character.is_ascii_alphanumeric())
            && after.is_none_or(|character| !character.is_ascii_alphanumeric())
    })
}

fn language_to_preference(language: &SubtitleLanguage) -> SubtitlePreference {
    match language {
        SubtitleLanguage::Chs => SubtitlePreference::Chs,
        SubtitleLanguage::Cht => SubtitlePreference::Cht,
        SubtitleLanguage::Jpn => SubtitlePreference::Jpn,
        SubtitleLanguage::Eng => SubtitlePreference::Eng,
    }
}

fn languages_to_legacy(languages: &[SubtitleLanguage]) -> Option<SubtitlePreference> {
    match languages {
        [] => None,
        [language] => Some(language_to_preference(language)),
        _ => Some(SubtitlePreference::Multi),
    }
}

fn legacy_subtitle_languages(value: Option<&SubtitlePreference>) -> Vec<SubtitleLanguage> {
    match value {
        Some(SubtitlePreference::Chs) => vec![SubtitleLanguage::Chs],
        Some(SubtitlePreference::Cht) => vec![SubtitleLanguage::Cht],
        Some(SubtitlePreference::Jpn) => vec![SubtitleLanguage::Jpn],
        Some(SubtitlePreference::Eng) => vec![SubtitleLanguage::Eng],
        Some(SubtitlePreference::Multi) | None => Vec::new(),
    }
}

fn normalize_fansub_character(character: char) -> char {
    match character {
        '綠' | '緑' => '绿',
        '組' => '组',
        '櫻' | '桜' => '樱',
        '國' => '国',
        '動' => '动',
        '畫' => '画',
        '華' => '华',
        '風' => '风',
        '夢' => '梦',
        '貓' => '猫',
        '龍' => '龙',
        '異' => '异',
        '鄉' => '乡',
        '聲' => '声',
        '葉' => '叶',
        '蘿' => '萝',
        '與' => '与',
        '體' => '体',
        '簡' => '简',
        '壓' => '压',
        '製' => '制',
        '學' => '学',
        '園' => '园',
        value => value,
    }
}

fn is_technical_fansub_name(value: &str) -> bool {
    let compact = remove_whitespace(value);
    let lower = compact.to_lowercase();
    if lower.parse::<f64>().is_ok() {
        return true;
    }
    [
        "hi10p", "main10", "4k", "8k", "x264", "x265", "h264", "h.264", "h265", "h.265", "avc",
        "hevc", "av1", "vp9", "web-dl", "webdl", "bdrip", "webrip", "mkv", "mp4", "简体", "繁体",
        "简繁", "chs", "cht", "multi",
    ]
    .contains(&lower.as_str())
        || Regex::new(r"(?i)^(?:8|10|12)-?bits?$|^\d{3,4}p$")
            .expect("technical fansub regex")
            .is_match(&lower)
}

fn expand_search_term(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut terms = vec![trimmed.to_owned()];
    terms.extend(
        search_brackets()
            .captures_iter(trimmed)
            .filter_map(|captures| captures.get(1).map(|value| value.as_str().to_owned())),
    );
    terms.push(search_brackets().replace_all(trimmed, " ").into_owned());
    terms.extend(search_separators().split(trimmed).map(str::to_owned));
    terms.push(strip_season_suffix(trimmed));
    terms.push(normalize_release_search_text(trimmed));
    terms
        .into_iter()
        .map(|term| term.trim().to_owned())
        .filter(|term| is_useful_search_term(term))
        .collect()
}

fn strip_season_suffix(value: &str) -> String {
    [
        season_suffix_chinese(),
        season_suffix_ordinal(),
        season_suffix_word(),
        season_suffix_short(),
    ]
    .into_iter()
    .fold(value.trim().to_owned(), |current, pattern| {
        pattern.replace(&current, "").trim().to_owned()
    })
}

fn unique_search_terms<I>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(normalize_release_search_text(value)))
        .collect()
}

fn is_useful_search_term(value: &str) -> bool {
    value.chars().count() >= 2 && value.chars().any(char::is_alphanumeric)
}

fn is_distinctive_search_term(value: &str) -> bool {
    let compact = remove_whitespace(&normalize_release_search_text(&strip_season_suffix(value)));
    compact.chars().count() >= 2 && compact.chars().any(char::is_alphanumeric)
}

fn is_search_punctuation(character: char) -> bool {
    "\"'“”‘’「」『』《》【】[]()（）.,，。:：;；!?！？·・~～_-".contains(character)
}

fn remove_whitespace(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn resolve_anime_series_season_no(anime: &Anime) -> Option<i64> {
    std::iter::once(anime.title.as_str())
        .chain(anime.original_title.as_deref())
        .chain(anime.aliases.iter().map(|alias| alias.alias.as_str()))
        .find_map(detect_series_season_no)
}

fn parse_season_number(value: &str) -> Option<i64> {
    if value.chars().all(|character| character.is_ascii_digit()) {
        return value.parse().ok();
    }
    let normalized = value.replace(['〇', '零'], "").replace('两', "二");
    let digit = |value: &str| match value {
        "一" => Some(1),
        "二" => Some(2),
        "三" => Some(3),
        "四" => Some(4),
        "五" => Some(5),
        "六" => Some(6),
        "七" => Some(7),
        "八" => Some(8),
        "九" => Some(9),
        _ => None,
    };
    if normalized == "十" {
        return Some(10);
    }
    if let Some((tens, ones)) = normalized.split_once('十') {
        return Some(
            tens.is_empty().then_some(1).or_else(|| digit(tens))? * 10
                + if ones.is_empty() { 0 } else { digit(ones)? },
        );
    }
    digit(&normalized)
}

fn matches_release_anime(release: &Release, anime: &MyAnime) -> bool {
    match release.anime_id.as_deref() {
        Some(anime_id) => anime_id == anime.anime.id,
        None => matches_anime_release_title(
            &release.title,
            &build_anime_release_search_terms(&anime.anime, &[], 12),
        ),
    }
}

fn matches_candidate_fansub(
    release: &Release,
    context: &ReleaseMatchContext,
    groups: &[FansubGroup],
) -> bool {
    if release
        .fansub_group_id
        .as_ref()
        .is_some_and(|id| context.candidate_fansub_group_ids.contains(id))
    {
        return true;
    }
    let candidates = context
        .candidate_fansub_names
        .iter()
        .map(|name| remove_whitespace(&name.trim().to_lowercase()))
        .collect::<HashSet<_>>();
    let release_name = release
        .fansub_name
        .as_deref()
        .map(|name| remove_whitespace(&name.trim().to_lowercase()));
    if release_name
        .as_ref()
        .is_some_and(|name| candidates.contains(name))
    {
        return true;
    }
    groups.iter().any(|group| {
        let group_matches_release = release.fansub_group_id.as_ref() == Some(&group.id)
            || release_name.as_ref().is_some_and(|release_name| {
                std::iter::once(&group.name)
                    .chain(group.aliases.iter())
                    .any(|name| remove_whitespace(&name.to_lowercase()) == *release_name)
            });
        group_matches_release
            && std::iter::once(&group.name)
                .chain(group.aliases.iter())
                .any(|name| candidates.contains(&remove_whitespace(&name.to_lowercase())))
    })
}

fn resolve_preferred_subtitle_languages(anime: &MyAnime) -> Vec<SubtitleLanguage> {
    resolve_subtitle_requirement(
        &anime.preferred_subtitle_languages,
        anime.preferred_subtitle.as_deref(),
    )
}

/// 兼容多选字幕语言和旧版单值偏好，并返回固定顺序的要求集合。
fn resolve_subtitle_requirement(
    preferred_languages: &[String],
    legacy_preference: Option<&str>,
) -> Vec<SubtitleLanguage> {
    let mut languages = preferred_languages
        .iter()
        .filter_map(|value| parse_subtitle_language(value))
        .collect::<Vec<_>>();
    if languages.is_empty() {
        languages = match legacy_preference {
            Some("multi") => vec![SubtitleLanguage::Chs, SubtitleLanguage::Cht],
            Some(value) => parse_subtitle_language(value).into_iter().collect(),
            None => Vec::new(),
        };
    }
    normalize_subtitle_languages(languages)
}

fn parse_subtitle_language(value: &str) -> Option<SubtitleLanguage> {
    match value {
        "chs" => Some(SubtitleLanguage::Chs),
        "cht" => Some(SubtitleLanguage::Cht),
        "jpn" => Some(SubtitleLanguage::Jpn),
        "eng" => Some(SubtitleLanguage::Eng),
        _ => None,
    }
}

fn normalize_subtitle_languages(values: Vec<SubtitleLanguage>) -> Vec<SubtitleLanguage> {
    let selected = values.into_iter().collect::<HashSet<_>>();
    [
        SubtitleLanguage::Chs,
        SubtitleLanguage::Cht,
        SubtitleLanguage::Jpn,
        SubtitleLanguage::Eng,
    ]
    .into_iter()
    .filter(|language| selected.contains(language))
    .collect()
}

fn subtitle_coverage(release: &Release, preferred: &[SubtitleLanguage]) -> f64 {
    if preferred.is_empty() {
        return 1.0;
    }
    let actual = if release.subtitle_languages.is_empty()
        && release.subtitle != Some(SubtitlePreference::Multi)
    {
        legacy_subtitle_languages(release.subtitle.as_ref())
    } else {
        release.subtitle_languages.clone()
    };
    if actual.is_empty() {
        return if release.subtitle == Some(SubtitlePreference::Multi) {
            0.6
        } else {
            0.0
        };
    }
    preferred
        .iter()
        .filter(|language| actual.contains(language))
        .count() as f64
        / preferred.len() as f64
}

fn match_result(
    release: Release,
    match_score: i64,
    preference_score: i64,
    availability_score: i64,
    reasons: Vec<String>,
    warnings: Vec<String>,
) -> ReleaseMatchResult {
    ReleaseMatchResult {
        release,
        score: (match_score + preference_score + availability_score).min(100),
        match_score,
        preference_score,
        availability_score,
        reasons,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use ani_domain::{
        Anime, AnimeAlias, AnimeAliasLanguage, FansubGroup, NormalizedVideoCodec, Release,
        ReleaseContentKind, ReleaseResolution, SubtitleLanguage, SubtitlePreference,
    };

    use super::{
        build_anime_release_search_terms, classify_anime_release, create_discovered_fansub_id,
        enrich_release_from_title, matches_anime_release_title, normalize_fansub_name,
        parse_release_title, release_matches_episode, release_satisfies_subtitle_requirement,
        AnimeReleaseCompatibility,
    };

    /// 验证标题解析覆盖季度、集数、编码、位深和字幕语言。
    #[test]
    fn parses_release_title_metadata() {
        let parsed =
            parse_release_title("[LoliHouse] 测试番 S02E03 [1080p][x265][10bit][简繁]", &[]);

        assert_eq!(parsed.fansub_name.as_deref(), Some("LoliHouse"));
        assert_eq!(parsed.episode_no, Some(3.0));
        assert_eq!(parsed.series_season_no, Some(2));
        assert_eq!(parsed.resolution, Some(ReleaseResolution::P1080));
        assert_eq!(
            parsed.normalized_video_codec,
            NormalizedVideoCodec::H265Hevc
        );
        assert_eq!(parsed.bit_depth, Some(10));
        assert_eq!(
            parsed.subtitle_languages,
            vec![SubtitleLanguage::Chs, SubtitleLanguage::Cht]
        );
        assert_eq!(parsed.subtitle, Some(SubtitlePreference::Multi));
    }

    /// 验证 NIX-RAWS 的“简繁内封”会明确识别为简体与繁体字幕。
    #[test]
    fn recognizes_nix_raws_embedded_chinese_subtitles() {
        let title = "[NIX-RAWS] LV999的村民 - 04 [Baha][WEB-DL][1080P][AVC AAC][简繁内封]";
        let parsed = parse_release_title(title, &[]);

        assert_eq!(parsed.fansub_name.as_deref(), Some("NIX-RAWS"));
        assert_eq!(parsed.episode_no, Some(4.0));
        assert_eq!(
            parsed.subtitle_languages,
            vec![SubtitleLanguage::Chs, SubtitleLanguage::Cht]
        );
        assert_eq!(parsed.subtitle, Some(SubtitlePreference::Multi));

        let release = enrich_release_from_title(
            Release {
                title: title.to_owned(),
                ..empty_release()
            },
            &[],
        );
        assert!(release_satisfies_subtitle_requirement(
            &release,
            &["chs".to_owned(), "cht".to_owned()],
            None,
        ));
    }

    /// 验证没有字幕标记的 Kokoore 资源保持字幕未知，不能满足中文字幕门禁。
    #[test]
    fn keeps_kokoore_unmarked_subtitle_unknown() {
        let title = "[LoliHouse] Kokoore - 05 [WebRip 1080p HEVC-10bit AAC].mkv";
        let parsed = parse_release_title(title, &[]);

        assert!(parsed.subtitle_languages.is_empty());
        assert_eq!(parsed.subtitle, None);
        let release = enrich_release_from_title(
            Release {
                title: title.to_owned(),
                ..empty_release()
            },
            &[],
        );
        assert!(!release_satisfies_subtitle_requirement(
            &release,
            &["chs".to_owned()],
            None,
        ));
    }

    /// 验证连集和合集不会被技术数字误判为单集。
    #[test]
    fn distinguishes_episode_ranges_and_batches() {
        let range = parse_release_title("[字幕组] 测试番 [01-12 合集][1080p]", &[]);
        let season_range = parse_release_title("[字幕组] 测试番 S2E02-05 [1080p]", &[]);
        let labeled_range = parse_release_title("[字幕组] 测试番 EP 2 - 05 [1080p]", &[]);
        let bare_range = parse_release_title("[字幕组] 测试番 2-05 [1080p]", &[]);
        let batch = parse_release_title("[字幕组] 测试番 10-bit 1080p [S3 Fin]", &[]);
        let season_episode = parse_release_title(
            "[LoliHouse] 乙女游戏世界对路人角色很不友好2 / Otome Game Sekai wa Mob ni Kibishii Sekai desu 2 - 05 [WebRip 1080p HEVC-10bit AAC][简繁内封字幕]",
            &[],
        );

        assert_eq!(range.episode_no, None);
        assert_eq!(range.episode_range.expect("episode range").end, 12.0);
        assert_eq!(range.content_kind, ReleaseContentKind::Range);
        assert_eq!(season_range.episode_range.expect("season range").start, 2.0);
        assert_eq!(labeled_range.episode_range.expect("labeled range").end, 5.0);
        assert_eq!(bare_range.episode_range.expect("bare range").end, 5.0);
        assert_eq!(batch.episode_no, None);
        assert_eq!(batch.series_season_no, Some(3));
        assert_eq!(batch.content_kind, ReleaseContentKind::Batch);
        assert_eq!(season_episode.episode_no, Some(5.0));
        assert_eq!(season_episode.episode_range, None);
        assert_eq!(season_episode.content_kind, ReleaseContentKind::Episode);
    }

    /// 验证字幕组异体字符生成相同稳定 ID。
    #[test]
    fn creates_stable_discovered_fansub_ids() {
        assert_eq!(
            normalize_fansub_name("綠茶字幕組"),
            normalize_fansub_name("绿茶字幕组")
        );
        assert_eq!(
            create_discovered_fansub_id("桜都字幕组"),
            create_discovered_fansub_id("樱都字幕组")
        );
    }

    /// 验证标题补全优先保留来源字段并关联已知字幕组。
    #[test]
    fn enriches_release_without_overwriting_source_metadata() {
        let groups = vec![FansubGroup {
            id: "fansub-lolihouse".to_owned(),
            name: "LoliHouse".to_owned(),
            aliases: vec!["Loli House".to_owned()],
            source_ids: vec!["nyaa".to_owned()],
        }];
        let release = Release {
            id: "release-1".to_owned(),
            title: "[Loli House] 测试番 S02E04 [4K][AV1][英文]".to_owned(),
            source_id: "manual".to_owned(),
            source_name: "Manual".to_owned(),
            published_at: "2026-07-13T12:00:00.000Z".to_owned(),
            episode_no: Some(99.0),
            resolution: Some(ReleaseResolution::P720),
            ..empty_release()
        };
        let enriched = enrich_release_from_title(release, &groups);

        assert_eq!(enriched.episode_no, Some(99.0));
        assert_eq!(enriched.resolution, Some(ReleaseResolution::P720));
        assert_eq!(
            enriched.fansub_group_id.as_deref(),
            Some("fansub-lolihouse")
        );
        assert_eq!(
            enriched.normalized_video_codec,
            Some(NormalizedVideoCodec::Av1)
        );
    }

    /// 验证番剧搜索词扩展、标题匹配和季度冲突判定。
    #[test]
    fn matches_anime_aliases_and_rejects_other_seasons() {
        let anime = test_anime();
        let terms = build_anime_release_search_terms(&anime, &[], 12);
        assert!(matches_anime_release_title(
            "[组] Test Anime S02E03",
            &terms
        ));
        let mut release = empty_release();
        release.title = "[组] 测试番 S01 Complete".to_owned();
        release.content_kind = Some(ReleaseContentKind::Batch);
        assert_eq!(
            classify_anime_release(&release, &anime),
            AnimeReleaseCompatibility::Mismatch
        );
        release.episode_range = Some(ani_domain::ReleaseEpisodeRange {
            start: 1.0,
            end: 12.0,
        });
        assert!(release_matches_episode(&release, Some(8.0)));
    }

    /// 验证续作不会把未标季数的同名资源当作当前季自动下载候选。
    #[test]
    fn keeps_unmarked_sequel_episode_out_of_current_season() {
        let anime = Anime {
            title: "地狱模式 第二季".to_owned(),
            original_title: Some("Hell Mode 2nd Season".to_owned()),
            ..test_anime()
        };
        let release = enrich_release_from_title(
            Release {
                title: "[LoliHouse] 地狱模式～喜欢速通游戏的玩家在废设定异世界无双～ / Hell Mode - 08 [WebRip 1080p HEVC-10bit AAC][简繁内封字幕]".to_owned(),
                ..empty_release()
            },
            &[],
        );

        assert_eq!(release.episode_no, Some(8.0));
        assert_eq!(release.series_season_no, None);
        assert_eq!(
            release.subtitle_languages,
            vec![SubtitleLanguage::Chs, SubtitleLanguage::Cht]
        );
        assert_eq!(
            classify_anime_release(&release, &anime),
            AnimeReleaseCompatibility::Other
        );

        let current_release = Release {
            title: "[LoliHouse] Hell Mode S02E08 [1080p][HEVC-10bit][简繁]".to_owned(),
            ..release.clone()
        };
        assert_eq!(
            classify_anime_release(&current_release, &anime),
            AnimeReleaseCompatibility::Current
        );

        let first_season = Anime {
            title: "地狱模式 第一季".to_owned(),
            original_title: Some("Hell Mode 1st Season".to_owned()),
            ..anime
        };
        assert_eq!(
            classify_anime_release(&release, &first_season),
            AnimeReleaseCompatibility::Current
        );
    }

    /// 验证自动下载必须完整覆盖字幕语言要求，未知多语组成不能绕过门禁。
    #[test]
    fn enforces_complete_subtitle_coverage_for_automatic_downloads() {
        let mut release = empty_release();
        assert!(release_satisfies_subtitle_requirement(&release, &[], None));

        let chinese_requirement = vec!["chs".to_owned(), "cht".to_owned()];
        release.subtitle_languages = vec![SubtitleLanguage::Chs, SubtitleLanguage::Cht];
        assert!(release_satisfies_subtitle_requirement(
            &release,
            &chinese_requirement,
            None
        ));

        release.subtitle_languages = vec![SubtitleLanguage::Chs];
        assert!(!release_satisfies_subtitle_requirement(
            &release,
            &chinese_requirement,
            None
        ));

        release.subtitle_languages.clear();
        release.subtitle = Some(SubtitlePreference::Multi);
        assert!(!release_satisfies_subtitle_requirement(
            &release,
            &chinese_requirement,
            None
        ));

        release.subtitle_languages = vec![SubtitleLanguage::Chs, SubtitleLanguage::Cht];
        assert!(release_satisfies_subtitle_requirement(
            &release,
            &[],
            Some("multi")
        ));
    }

    fn test_anime() -> Anime {
        Anime {
            id: "anime-1".to_owned(),
            title: "测试番 第二季".to_owned(),
            original_title: Some("Test Anime 2nd Season".to_owned()),
            aliases: vec![AnimeAlias {
                id: "alias-1".to_owned(),
                anime_id: "anime-1".to_owned(),
                alias: "Test Anime".to_owned(),
                language: AnimeAliasLanguage::En,
                priority: 1,
            }],
            premiere_date: None,
            premiere_year: 2026,
            premiere_month: 7,
            season: None,
            summary: None,
            cover_url: None,
            rating: None,
            external_ids: serde_json::json!({}),
            detail: None,
        }
    }

    fn empty_release() -> Release {
        Release {
            id: "release-empty".to_owned(),
            title: String::new(),
            anime_id: None,
            episode_no: None,
            episode_range: None,
            series_season_no: None,
            content_kind: None,
            fansub_group_id: None,
            fansub_name: None,
            source_id: String::new(),
            source_name: String::new(),
            magnet_url: None,
            torrent_url: None,
            info_hash: None,
            size: None,
            resolution: None,
            declared_video_codec: None,
            normalized_video_codec: None,
            bit_depth: None,
            subtitle_languages: Vec::new(),
            subtitle: None,
            published_at: String::new(),
            seeders: None,
            source_meta: None,
        }
    }
}
