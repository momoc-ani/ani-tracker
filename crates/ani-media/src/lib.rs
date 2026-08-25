use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ani_domain::{
    DownloadTask, MediaAvailability, MediaContentKind, MediaFile, MediaOrigin, TorrentFile,
};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tokio::process::Command;

pub mod model_sidecar;
pub mod player;

const DEFAULT_VIDEO_EXTENSIONS: &[&str] = &[".mkv", ".mp4", ".avi"];
const MAX_FFPROBE_OUTPUT_BYTES: usize = 10 * 1024 * 1024;

/// 媒体探测失败的稳定错误，不暴露具体进程实现。
#[derive(Debug, thiserror::Error)]
pub enum MediaProbeError {
    #[error("媒体文件不可访问：{0}")]
    File(String),
    #[error("FFprobe 不可用：{0}")]
    Unavailable(String),
    #[error("FFprobe 输出无效：{0}")]
    InvalidOutput(String),
}

/// 创建媒体记录时附带的下载业务关联。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaProbeContext {
    pub anime_id: Option<String>,
    pub episode_id: Option<String>,
    pub download_task_id: Option<String>,
    pub declared_video_codec: Option<String>,
    pub normalized_video_codec: Option<String>,
    pub size: Option<i64>,
    pub downloaded_at: Option<String>,
}

/// 可替换的媒体文件探测端口，桌面由 FFprobe 实现，移动端由 libVLC 实现。
#[async_trait]
pub trait MediaProbe: Send + Sync {
    /// 探测单个本地媒体文件并生成可持久化记录。
    async fn probe(
        &self,
        file_path: &Path,
        context: &MediaProbeContext,
    ) -> Result<MediaFile, MediaProbeError>;
}

/// 单个被跳过的下载文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaScanSkippedFile {
    pub name: String,
    pub reason: String,
}

/// 单个媒体扫描错误。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaScanError {
    pub file_path: String,
    pub message: String,
}

/// 下载任务媒体扫描结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaScanResult {
    pub task_id: String,
    pub media_files: Vec<MediaFile>,
    pub skipped_files: Vec<MediaScanSkippedFile>,
    pub errors: Vec<MediaScanError>,
}

/// 按下载选择、完成度和视频扩展名筛选并探测媒体文件。
#[derive(Clone)]
pub struct DownloadMediaScanner {
    probe: Arc<dyn MediaProbe>,
    video_extensions: Arc<HashSet<String>>,
}

impl DownloadMediaScanner {
    /// 使用平台探测器和用户视频扩展名创建扫描服务。
    pub fn new(probe: Arc<dyn MediaProbe>, video_extensions: &[String]) -> Self {
        let configured = if video_extensions.is_empty() {
            DEFAULT_VIDEO_EXTENSIONS
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>()
        } else {
            video_extensions.to_vec()
        };
        Self {
            probe,
            video_extensions: Arc::new(
                configured
                    .iter()
                    .map(|value| normalize_extension(value))
                    .collect(),
            ),
        }
    }

    /// 扫描一个下载任务并隔离单文件失败。
    pub async fn scan_task(&self, task: &DownloadTask) -> MediaScanResult {
        let mut result = MediaScanResult {
            task_id: task.id.clone(),
            media_files: Vec::new(),
            skipped_files: Vec::new(),
            errors: Vec::new(),
        };
        for file in &task.files {
            if let Some(reason) = self.skip_reason(task, file) {
                result.skipped_files.push(MediaScanSkippedFile {
                    name: file.name.clone(),
                    reason: reason.to_owned(),
                });
                continue;
            }
            let file_path = match resolve_torrent_file_path(task, file).await {
                Ok(path) => path,
                Err(error) => {
                    result.errors.push(MediaScanError {
                        file_path: display_task_file_path(task, file),
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            let context = MediaProbeContext {
                anime_id: task.anime_id.clone(),
                episode_id: file.episode_id.clone().or_else(|| task.episode_id.clone()),
                download_task_id: Some(task.id.clone()),
                declared_video_codec: task.declared_video_codec.clone(),
                normalized_video_codec: task.normalized_video_codec.clone(),
                size: Some(file.size),
                downloaded_at: task.completed_at.clone().or_else(|| Some(now_iso())),
            };
            match self.probe.probe(&file_path, &context).await {
                Ok(media_file) => result.media_files.push(media_file),
                Err(error) => result.errors.push(MediaScanError {
                    file_path: file_path.to_string_lossy().into_owned(),
                    message: error.to_string(),
                }),
            }
        }
        result
    }

    /// 返回任务中应被媒体化的文件路径，用于自动扫描幂等判断。
    pub fn candidate_paths(&self, task: &DownloadTask) -> Vec<PathBuf> {
        task.files
            .iter()
            .filter(|file| self.skip_reason(task, file).is_none())
            .map(|file| unresolved_task_file_path(task, file))
            .collect()
    }

    fn skip_reason(&self, task: &DownloadTask, file: &TorrentFile) -> Option<&'static str> {
        if !file.selected {
            return Some("未选择下载");
        }
        if !is_video_file(&file.name, &self.video_extensions) {
            return Some("非视频文件");
        }
        if !task.is_completed() && file.progress < 1.0 {
            return Some("文件尚未下载完成");
        }
        None
    }
}

/// 通过一个或多个 FFprobe 命令探测桌面媒体信息。
pub struct FfprobeMediaProbe {
    commands: Vec<PathBuf>,
    timeout: Duration,
}

impl FfprobeMediaProbe {
    /// 创建带候选命令和受限超时的桌面探测器。
    pub fn new(commands: Vec<PathBuf>, timeout: Duration) -> Result<Self, MediaProbeError> {
        let commands = unique_paths(commands);
        if commands.is_empty() {
            return Err(MediaProbeError::Unavailable("没有可用命令".to_owned()));
        }
        Ok(Self {
            commands,
            timeout: timeout.clamp(Duration::from_secs(1), Duration::from_secs(60)),
        })
    }

    /// 依次执行候选命令并返回首个有效 FFprobe JSON。
    async fn run(&self, file_path: &Path) -> Result<FfprobeOutput, MediaProbeError> {
        let mut last_error = None;
        for command_path in &self.commands {
            let mut command = Command::new(command_path);
            command
                .args([
                    "-v",
                    "quiet",
                    "-print_format",
                    "json",
                    "-show_format",
                    "-show_streams",
                ])
                .arg(file_path)
                .kill_on_drop(true);
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::process::CommandExt;
                command.as_std_mut().creation_flags(0x0800_0000);
            }
            let output = match tokio::time::timeout(self.timeout, command.output()).await {
                Ok(Ok(output)) => output,
                Ok(Err(error)) => {
                    last_error = Some(format!("{}：{error}", command_path.display()));
                    continue;
                }
                Err(_) => {
                    last_error = Some(format!("{}：执行超时", command_path.display()));
                    continue;
                }
            };
            if !output.status.success() {
                last_error = Some(format!(
                    "{}：退出状态 {}",
                    command_path.display(),
                    output.status
                ));
                continue;
            }
            if output.stdout.len() > MAX_FFPROBE_OUTPUT_BYTES {
                last_error = Some(format!("{}：输出超过 10 MiB", command_path.display()));
                continue;
            }
            match serde_json::from_slice(&output.stdout) {
                Ok(parsed) => return Ok(parsed),
                Err(error) => {
                    last_error = Some(format!("{}：{error}", command_path.display()));
                }
            }
        }
        Err(MediaProbeError::Unavailable(
            last_error.unwrap_or_else(|| "所有候选命令均失败".to_owned()),
        ))
    }
}

#[async_trait]
impl MediaProbe for FfprobeMediaProbe {
    /// 读取文件属性并合并 FFprobe 结果；探测不可用时保留基础媒体关联。
    async fn probe(
        &self,
        file_path: &Path,
        context: &MediaProbeContext,
    ) -> Result<MediaFile, MediaProbeError> {
        let metadata = tokio::fs::metadata(file_path)
            .await
            .map_err(|error| MediaProbeError::File(error.to_string()))?;
        if !metadata.is_file() {
            return Err(MediaProbeError::File("目标不是普通文件".to_owned()));
        }
        let output = match self.run(file_path).await {
            Ok(output) => Some(output),
            Err(error) => {
                log::warn!(
                    "FFprobe 媒体探测失败，保留基础关联 path={} error={error}",
                    file_path.display()
                );
                None
            }
        };
        Ok(build_media_file(
            file_path,
            metadata.len(),
            context,
            output.as_ref(),
        ))
    }
}

#[derive(Debug, Deserialize)]
struct FfprobeOutput {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
    format: Option<FfprobeFormat>,
}

#[derive(Debug, Deserialize)]
struct FfprobeFormat {
    format_name: Option<String>,
    duration: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
    pix_fmt: Option<String>,
    bits_per_raw_sample: Option<serde_json::Value>,
    bits_per_sample: Option<serde_json::Value>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

fn build_media_file(
    file_path: &Path,
    metadata_size: u64,
    context: &MediaProbeContext,
    output: Option<&FfprobeOutput>,
) -> MediaFile {
    let video = output.and_then(|value| {
        value
            .streams
            .iter()
            .find(|stream| stream.codec_type.as_deref() == Some("video"))
    });
    let detected_video_codec = video.and_then(|stream| stream.codec_name.clone());
    let normalized_video_codec = detected_video_codec
        .as_deref()
        .map(normalize_video_codec)
        .or_else(|| context.normalized_video_codec.clone())
        .unwrap_or_else(|| "Unknown".to_owned());
    let file_name = file_path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_path.to_string_lossy().into_owned());
    MediaFile {
        id: create_media_file_id(file_path),
        anime_id: context
            .anime_id
            .clone()
            .unwrap_or_else(|| "unmatched".to_owned()),
        episode_id: context.episode_id.clone(),
        download_task_id: context.download_task_id.clone(),
        content_kind: if context.episode_id.is_some() {
            MediaContentKind::Episode
        } else {
            MediaContentKind::Unknown
        },
        special_no: None,
        file_path: file_path.to_string_lossy().into_owned(),
        file_name,
        size: i64::try_from(metadata_size)
            .unwrap_or(i64::MAX)
            .max(context.size.unwrap_or_default()),
        container: Some(detect_container(file_path, output)),
        declared_video_codec: context.declared_video_codec.clone(),
        detected_video_codec,
        normalized_video_codec,
        resolution: video.and_then(|stream| match (stream.width, stream.height) {
            (Some(width), Some(height)) if width > 0 && height > 0 => {
                Some(format!("{width}x{height}"))
            }
            _ => None,
        }),
        bit_depth: video.and_then(detect_bit_depth),
        audio_codecs: unique_strings(
            output
                .map(|value| {
                    value
                        .streams
                        .iter()
                        .filter(|stream| stream.codec_type.as_deref() == Some("audio"))
                        .filter_map(|stream| stream.codec_name.as_deref())
                        .map(normalize_audio_codec)
                        .collect()
                })
                .unwrap_or_default(),
        ),
        subtitle_tracks: unique_strings(
            output
                .map(|value| {
                    value
                        .streams
                        .iter()
                        .filter(|stream| stream.codec_type.as_deref() == Some("subtitle"))
                        .filter_map(format_subtitle_track)
                        .collect()
                })
                .unwrap_or_default(),
        ),
        duration_seconds: output
            .and_then(|value| value.format.as_ref())
            .and_then(|format| parse_duration(format.duration.as_deref())),
        downloaded_at: context.downloaded_at.clone(),
        probed_at: Some(now_iso()),
        origin: MediaOrigin::Download,
        source_root: None,
        fingerprint: None,
        file_modified_at: None,
        availability: MediaAvailability::Available,
        last_verified_at: Some(now_iso()),
        availability_error: None,
    }
}

async fn resolve_torrent_file_path(
    task: &DownloadTask,
    file: &TorrentFile,
) -> Result<PathBuf, MediaProbeError> {
    let unresolved = unresolved_task_file_path(task, file);
    let resolved = tokio::fs::canonicalize(&unresolved)
        .await
        .map_err(|error| MediaProbeError::File(error.to_string()))?;
    let resolved = dunce::simplified(&resolved).to_path_buf();
    if !Path::new(&file.name).is_absolute() {
        let save_root = tokio::fs::canonicalize(&task.save_path)
            .await
            .map_err(|error| MediaProbeError::File(error.to_string()))?;
        let save_root = dunce::simplified(&save_root).to_path_buf();
        if !resolved.starts_with(&save_root) {
            return Err(MediaProbeError::File(
                "下载文件路径超出任务保存目录".to_owned(),
            ));
        }
    }
    Ok(resolved)
}

fn unresolved_task_file_path(task: &DownloadTask, file: &TorrentFile) -> PathBuf {
    let file_path = Path::new(&file.name);
    if file_path.is_absolute() {
        file_path.to_path_buf()
    } else {
        Path::new(&task.save_path).join(file_path)
    }
}

fn display_task_file_path(task: &DownloadTask, file: &TorrentFile) -> String {
    unresolved_task_file_path(task, file)
        .to_string_lossy()
        .into_owned()
}

fn is_video_file(file_name: &str, extensions: &HashSet<String>) -> bool {
    let normalized = file_name.to_lowercase();
    extensions
        .iter()
        .any(|extension| normalized.ends_with(extension))
}

fn normalize_extension(value: &str) -> String {
    let value = value.trim().to_lowercase();
    if value.starts_with('.') {
        value
    } else {
        format!(".{value}")
    }
}

fn detect_container(file_path: &Path, output: Option<&FfprobeOutput>) -> String {
    let extension = file_path
        .extension()
        .map(|value| value.to_string_lossy().to_lowercase());
    if let Some(extension @ ("mkv" | "mp4" | "avi")) = extension.as_deref() {
        return extension.to_owned();
    }
    let format_name = output
        .and_then(|value| value.format.as_ref())
        .and_then(|format| format.format_name.as_deref())
        .unwrap_or_default()
        .to_lowercase();
    if format_name.contains("matroska") {
        "mkv".to_owned()
    } else if format_name.contains("mp4") || format_name.contains("mov") {
        "mp4".to_owned()
    } else if format_name.contains("avi") {
        "avi".to_owned()
    } else {
        "unknown".to_owned()
    }
}

fn normalize_video_codec(codec: &str) -> String {
    match codec.trim().to_lowercase().as_str() {
        "h264" | "avc" | "avc1" => "H.264/AVC".to_owned(),
        "h265" | "hevc" | "hev1" | "hvc1" => "H.265/HEVC".to_owned(),
        "av1" | "av01" => "AV1".to_owned(),
        "vp9" | "vp09" => "VP9".to_owned(),
        _ => "Unknown".to_owned(),
    }
}

fn normalize_audio_codec(codec: &str) -> String {
    match codec.to_lowercase().as_str() {
        "aac" => "AAC".to_owned(),
        "ac3" => "AC-3".to_owned(),
        "eac3" => "E-AC-3".to_owned(),
        "flac" => "FLAC".to_owned(),
        "mp3" => "MP3".to_owned(),
        "opus" => "OPUS".to_owned(),
        "truehd" => "TrueHD".to_owned(),
        "dts" => "DTS".to_owned(),
        value => value.to_uppercase(),
    }
}

fn format_subtitle_track(stream: &FfprobeStream) -> Option<String> {
    let language = stream
        .tags
        .get("language")
        .map(|value| value.to_lowercase());
    let title = stream.tags.get("title").cloned();
    let codec = stream.codec_name.as_ref().map(|value| value.to_uppercase());
    let value = [language, title, codec]
        .into_iter()
        .flatten()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" / ");
    (!value.is_empty()).then_some(value)
}

fn detect_bit_depth(stream: &FfprobeStream) -> Option<i64> {
    [
        stream.bits_per_raw_sample.as_ref(),
        stream.bits_per_sample.as_ref(),
    ]
    .into_iter()
    .flatten()
    .find_map(parse_positive_integer)
    .or_else(|| parse_pixel_format_bit_depth(stream.pix_fmt.as_deref()))
}

fn parse_positive_integer(value: &serde_json::Value) -> Option<i64> {
    let parsed = value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()));
    parsed.filter(|value| *value > 0)
}

fn parse_pixel_format_bit_depth(pixel_format: Option<&str>) -> Option<i64> {
    let value = pixel_format?;
    [10_i64, 12, 14, 16]
        .into_iter()
        .find(|bit_depth| value.contains(&bit_depth.to_string()))
}

fn parse_duration(value: Option<&str>) -> Option<i64> {
    value?
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| value.round() as i64)
}

fn create_media_file_id(file_path: &Path) -> String {
    let normalized = file_path.to_string_lossy().to_lowercase();
    let digest = Sha1::digest(normalized.as_bytes());
    format!("media-{:x}", digest)[..22].to_owned()
}

fn unique_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

fn unique_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ani_contracts::ContractFixture;
    use ani_domain::{DownloadStatus, TorrentEngineKind};
    use serde::Deserialize;
    use tempfile::tempdir;

    struct StubProbe;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct MediaFixture {
        scan_result: MediaScanResult,
    }

    #[async_trait]
    impl MediaProbe for StubProbe {
        async fn probe(
            &self,
            file_path: &Path,
            context: &MediaProbeContext,
        ) -> Result<MediaFile, MediaProbeError> {
            Ok(build_media_file(file_path, 4, context, None))
        }
    }

    /// 验证扫描器只处理已选且完成的视频文件，并保留下载关联。
    #[tokio::test]
    async fn scans_completed_selected_video_files() {
        let directory = tempdir().expect("create temp directory");
        let video_path = directory.path().join("episode-01.mkv");
        std::fs::write(&video_path, b"test").expect("write media file");
        let task = completed_task(directory.path(), "episode-01.mkv");
        let scanner = DownloadMediaScanner::new(Arc::new(StubProbe), &["mkv".to_owned()]);

        let result = scanner.scan_task(&task).await;

        assert_eq!(result.media_files.len(), 1);
        assert!(result.errors.is_empty());
        assert_eq!(result.media_files[0].anime_id, "anime-1");
        assert_eq!(
            result.media_files[0].episode_id.as_deref(),
            Some("episode-1")
        );
        assert_eq!(
            result.media_files[0].download_task_id.as_deref(),
            Some("download-1")
        );
    }

    /// 验证相对路径不能逃逸下载任务保存目录。
    #[tokio::test]
    async fn rejects_relative_path_outside_save_directory() {
        let directory = tempdir().expect("create temp directory");
        let outside = directory.path().join("outside.mkv");
        std::fs::write(&outside, b"test").expect("write media file");
        let save_path = directory.path().join("downloads");
        std::fs::create_dir_all(&save_path).expect("create save directory");
        let task = completed_task(&save_path, "../outside.mkv");
        let scanner = DownloadMediaScanner::new(Arc::new(StubProbe), &[]);

        let result = scanner.scan_task(&task).await;

        assert!(result.media_files.is_empty());
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].message.contains("超出任务保存目录"));
    }

    /// 验证 FFprobe 字段映射兼容编码、位深、音轨和字幕轨。
    #[test]
    fn maps_ffprobe_output() {
        let output: FfprobeOutput = serde_json::from_value(serde_json::json!({
            "streams": [
                { "codec_type": "video", "codec_name": "hevc", "width": 1920, "height": 1080, "pix_fmt": "yuv420p10le" },
                { "codec_type": "audio", "codec_name": "aac" },
                { "codec_type": "subtitle", "codec_name": "ass", "tags": { "language": "chi", "title": "简体" } }
            ],
            "format": { "format_name": "matroska,webm", "duration": "1439.6" }
        }))
        .expect("decode ffprobe output");
        let context = MediaProbeContext {
            anime_id: Some("anime-1".to_owned()),
            ..MediaProbeContext::default()
        };

        let media = build_media_file(Path::new("episode.mkv"), 4, &context, Some(&output));

        assert_eq!(media.normalized_video_codec, "H.265/HEVC");
        assert_eq!(media.resolution.as_deref(), Some("1920x1080"));
        assert_eq!(media.bit_depth, Some(10));
        assert_eq!(media.audio_codecs, ["AAC"]);
        assert_eq!(media.subtitle_tracks, ["chi / 简体 / ASS"]);
        assert_eq!(media.duration_seconds, Some(1440));
    }

    /// 验证 Rust 媒体扫描结果能严格解码 TypeScript 共用金样。
    #[test]
    fn decodes_media_scan_contract_fixture() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p4-media-model.v1.json"
        ));
        let decoded: ContractFixture<MediaFixture> =
            serde_json::from_str(fixture).expect("media scan fixture must decode");

        assert_eq!(decoded.schema_version, 1);
        assert_eq!(decoded.kind, "p4-media-model");
        assert_eq!(decoded.payload.scan_result.media_files.len(), 1);
        assert_eq!(
            decoded.payload.scan_result.media_files[0].normalized_video_codec,
            "H.265/HEVC"
        );
        assert_eq!(decoded.payload.scan_result.skipped_files.len(), 1);
    }

    fn completed_task(save_path: &Path, file_name: &str) -> DownloadTask {
        DownloadTask {
            id: "download-1".to_owned(),
            release_id: None,
            anime_id: Some("anime-1".to_owned()),
            episode_id: Some("episode-1".to_owned()),
            anime_title: Some("测试番".to_owned()),
            episode_no: Some(1.0),
            fansub_group_id: None,
            fansub_name: None,
            resolution: None,
            declared_video_codec: None,
            normalized_video_codec: None,
            bit_depth: None,
            subtitle_languages: Vec::new(),
            subtitle: None,
            correlation_tag: None,
            engine: TorrentEngineKind::Embedded,
            torrent_hash: None,
            name: "测试任务".to_owned(),
            status: DownloadStatus::Completed,
            progress: 1.0,
            download_speed: 0,
            upload_speed: 0,
            eta_seconds: Some(0),
            save_path: save_path.to_string_lossy().into_owned(),
            files: vec![TorrentFile {
                id: "file-1".to_owned(),
                index: 0,
                name: file_name.to_owned(),
                episode_id: None,
                episode_no: Some(1.0),
                size: 4,
                progress: 1.0,
                priority: 1,
                selected: true,
            }],
            created_at: "2026-07-25T00:00:00.000Z".to_owned(),
            completed_at: Some("2026-07-25T00:10:00.000Z".to_owned()),
        }
    }
}
