use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ani_contracts::{
    PlayerFrameInterpolation, PlayerVideoEnhancement, RemotePlaybackDiagnostics,
    RemotePlaybackEnhancement, RemotePlaybackSession, RemotePlaybackSubtitle,
};
use ani_domain::{AppSettings, DownloadTask, MediaFile, PlaybackCheckpoint};
use ani_media::model_sidecar::{ModelSidecarConfig, ModelSidecarRuntime};
use ani_media::player::{FrameInterpolator, RawVideoFrame};
use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{SecondsFormat, Utc};
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::Deserialize;
use std::process::Stdio;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

const SESSION_TTL: Duration = Duration::from_secs(30 * 60);
const TRANSCODER_START_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_SESSIONS: usize = 2;
const RIFE_FRAME_QUEUE_CAPACITY: usize = 2;
const ENCODER_CANDIDATES: &[(&str, &str)] = &[
    ("h264_nvenc", "nvenc"),
    ("h264_amf", "amf"),
    ("h264_qsv", "qsv"),
    ("libx264", "libx264"),
];
const MEDIA_FILE_SEGMENT_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// 远程媒体核心读取下载、媒体、设置和续播状态的公共端口。
#[async_trait]
pub trait RemoteMediaRepository: Send + Sync {
    /// 读取指定下载任务。
    async fn get_download_task(&self, task_id: &str) -> Result<Option<DownloadTask>, String>;

    /// 读取全部已登记媒体文件。
    async fn list_media_files(&self) -> Result<Vec<MediaFile>, String>;

    /// 读取当前桌面媒体设置。
    async fn get_settings(&self) -> Result<AppSettings, String>;

    /// 读取指定下载文件的续播检查点。
    async fn get_playback_checkpoint(
        &self,
        task_id: &str,
        file_index: Option<i64>,
    ) -> Result<Option<PlaybackCheckpoint>, String>;
}

/// 桌面 FFmpeg 与 FFprobe 的受控候选路径。
#[derive(Debug, Clone)]
pub struct RemoteMediaTools {
    pub ffprobe_paths: Vec<PathBuf>,
    pub ffmpeg_path: PathBuf,
    pub timeout: Duration,
    pub rife_sidecar_root: Option<PathBuf>,
    pub rife_available_vram_bytes: u64,
}

/// 网关可输出的受控媒体文件。
#[derive(Debug, Clone)]
pub struct RemoteMediaAsset {
    pub file_path: PathBuf,
    pub content_type: String,
    pub direct: bool,
    pub file_name: Option<String>,
}

/// 远程媒体会话的稳定协议错误。
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct RemoteMediaError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

impl RemoteMediaError {
    fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SessionAccess {
    Browser,
    External,
}

struct SessionRecord {
    public: Mutex<RemotePlaybackSession>,
    access: SessionAccess,
    access_token: Option<String>,
    device_id: String,
    source_path: PathBuf,
    content_type: String,
    temporary_directory: PathBuf,
    process: Mutex<Option<MediaProcess>>,
    last_accessed_at_millis: Mutex<i64>,
}

struct MediaProcess {
    encoder: Child,
    decoder: Option<Child>,
    pipeline: Option<JoinHandle<Result<(), String>>>,
}

impl Drop for MediaProcess {
    fn drop(&mut self) {
        if let Some(pipeline) = self.pipeline.take() {
            pipeline.abort();
        }
    }
}

impl MediaProcess {
    async fn stop(mut self) {
        if let Some(pipeline) = self.pipeline.take() {
            pipeline.abort();
            let _ = pipeline.await;
        }
        if let Some(mut decoder) = self.decoder.take() {
            let _ = decoder.kill().await;
            let _ = tokio::time::timeout(Duration::from_secs(2), decoder.wait()).await;
        }
        let _ = self.encoder.kill().await;
        let _ = tokio::time::timeout(Duration::from_secs(2), self.encoder.wait()).await;
    }
}

struct HlsStart {
    process: MediaProcess,
    encoder: &'static str,
    encoder_degraded: bool,
    actual_interpolation: PlayerFrameInterpolation,
    degradation_reason: Option<String>,
}

struct ResolvedMedia {
    path: PathBuf,
    file_name: String,
    file_index: Option<i64>,
    duration_seconds: Option<i64>,
    content_type: String,
}

/// 管理绑定设备的短期直传、字幕和实时 HLS 会话。
pub struct RemoteMediaSessionService {
    repository: Arc<dyn RemoteMediaRepository>,
    tools: RemoteMediaTools,
    temporary_root: PathBuf,
    sessions: Mutex<HashMap<String, Arc<SessionRecord>>>,
    rife_runtime: Mutex<Option<Arc<ModelSidecarRuntime>>>,
}

impl RemoteMediaSessionService {
    /// 使用数据库端口、媒体工具和受控临时目录创建服务。
    pub fn new(
        repository: Arc<dyn RemoteMediaRepository>,
        tools: RemoteMediaTools,
        temporary_root: PathBuf,
    ) -> Self {
        Self {
            repository,
            tools,
            temporary_root,
            sessions: Mutex::new(HashMap::new()),
            rife_runtime: Mutex::new(None),
        }
    }

    /// 创建浏览器使用的 Bearer/Cookie 绑定会话。
    pub async fn create_session(
        self: &Arc<Self>,
        task_id: &str,
        device_id: &str,
        requested_mode: &str,
        file_index: Option<i64>,
        enhancement: RemotePlaybackEnhancement,
    ) -> Result<RemotePlaybackSession, RemoteMediaError> {
        self.create_session_record(
            task_id,
            device_id,
            requested_mode,
            file_index,
            enhancement,
            SessionAccess::Browser,
        )
        .await
    }

    /// 创建带高熵 URL 票据的外部播放器会话。
    pub async fn create_external_session(
        self: &Arc<Self>,
        task_id: &str,
        device_id: &str,
        requested_mode: &str,
        file_index: Option<i64>,
        enhancement: RemotePlaybackEnhancement,
    ) -> Result<RemotePlaybackSession, RemoteMediaError> {
        self.create_session_record(
            task_id,
            device_id,
            requested_mode,
            file_index,
            enhancement,
            SessionAccess::External,
        )
        .await
    }

    async fn create_session_record(
        self: &Arc<Self>,
        task_id: &str,
        device_id: &str,
        requested_mode: &str,
        file_index: Option<i64>,
        enhancement: RemotePlaybackEnhancement,
        access: SessionAccess,
    ) -> Result<RemotePlaybackSession, RemoteMediaError> {
        if !matches!(requested_mode, "direct" | "transcode") {
            return Err(RemoteMediaError::new(
                400,
                "MEDIA_MODE_INVALID",
                "播放模式无效",
            ));
        }
        if file_index.is_some_and(|value| value < 0) {
            return Err(RemoteMediaError::new(
                400,
                "MEDIA_FILE_INVALID",
                "媒体文件标识无效",
            ));
        }
        validate_remote_enhancement(requested_mode, enhancement)?;
        self.cleanup_expired().await;
        let task = self
            .repository
            .get_download_task(task_id)
            .await
            .map_err(internal_media_error)?
            .ok_or_else(|| RemoteMediaError::new(404, "MEDIA_TASK_NOT_FOUND", "下载任务不存在"))?;
        let media = self.resolve_media(&task, file_index).await?;
        let checkpoint = self
            .repository
            .get_playback_checkpoint(&task.id, media.file_index)
            .await
            .map_err(internal_media_error)?;
        self.close_matching(device_id, task_id, access).await;
        self.reserve_slot().await;

        tokio::fs::create_dir_all(&self.temporary_root)
            .await
            .map_err(|error| internal_media_error(error.to_string()))?;
        let id = random_token(24);
        let temporary_directory = self.temporary_root.join(format!("session-{id}"));
        tokio::fs::create_dir(&temporary_directory)
            .await
            .map_err(|error| internal_media_error(error.to_string()))?;
        let access_token = (access == SessionAccess::External).then(|| random_token(32));
        let asset_base = access_token.as_ref().map_or_else(
            || format!("/api/media/sessions/{id}"),
            |token| format!("/api/media/external/{token}/sessions/{id}"),
        );
        let subtitles = self
            .prepare_subtitles(&media.path, &temporary_directory, &asset_base)
            .await;
        let mode = if requested_mode == "transcode" {
            "hls"
        } else {
            "direct"
        };
        let mut process = None;
        let mut diagnostics = RemotePlaybackDiagnostics {
            subtitle_mode: Some("soft".to_owned()),
            enhanced_frame_input: enhancement.video_enhancement != PlayerVideoEnhancement::Off
                || enhancement.frame_interpolation != PlayerFrameInterpolation::Off,
            video_enhancement: enhancement.video_enhancement,
            frame_interpolation: enhancement.frame_interpolation,
            ..Default::default()
        };
        if mode == "hls" {
            let started = self
                .start_hls(&media.path, &temporary_directory, enhancement)
                .await
                .inspect_err(|_| {
                    let _ = std::fs::remove_dir_all(&temporary_directory);
                })?;
            process = Some(started.process);
            diagnostics.encoder = Some(started.encoder.to_owned());
            diagnostics.encoder_degraded = started.encoder_degraded;
            diagnostics.frame_interpolation = started.actual_interpolation;
            diagnostics.enhanced_frame_input = diagnostics.video_enhancement
                != PlayerVideoEnhancement::Off
                || diagnostics.frame_interpolation != PlayerFrameInterpolation::Off;
            diagnostics.degradation_reason = started.degradation_reason;
        }
        let now = Utc::now();
        let direct_asset_name =
            utf8_percent_encode(&media.file_name, MEDIA_FILE_SEGMENT_ENCODE_SET).to_string();
        let public = RemotePlaybackSession {
            id: id.clone(),
            task_id: task.id.clone(),
            file_index: media.file_index,
            file_name: media.file_name,
            mode: mode.to_owned(),
            stream_url: if mode == "hls" {
                format!("{asset_base}/hls/index.m3u8")
            } else if access == SessionAccess::External {
                format!("{asset_base}/{direct_asset_name}")
            } else {
                format!("{asset_base}/file")
            },
            expires_at: (now
                + chrono::Duration::from_std(SESSION_TTL).unwrap_or(chrono::Duration::minutes(30)))
            .to_rfc3339_opts(SecondsFormat::Millis, true),
            duration_seconds: media.duration_seconds,
            start_position_seconds: resolve_resume_position(checkpoint.as_ref()),
            subtitles,
            diagnostics,
        };
        let record = Arc::new(SessionRecord {
            public: Mutex::new(public.clone()),
            access,
            access_token,
            device_id: device_id.to_owned(),
            source_path: media.path,
            content_type: media.content_type,
            temporary_directory,
            process: Mutex::new(process),
            last_accessed_at_millis: Mutex::new(now.timestamp_millis()),
        });
        self.sessions.lock().await.insert(id.clone(), record);
        let service = Arc::clone(self);
        let expiration_id = id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(SESSION_TTL).await;
            service.close_if_expired(&expiration_id).await;
        });
        log::info!(
            "Rust 远程媒体会话已创建 session_id={id} task_id={} mode={mode}",
            task.id
        );
        Ok(public)
    }

    /// 返回设备拥有会话中的媒体资源。
    pub async fn get_asset(
        &self,
        session_id: &str,
        device_id: &str,
        asset_name: &str,
    ) -> Result<RemoteMediaAsset, RemoteMediaError> {
        let record = self
            .require_session(session_id, Some(device_id), None)
            .await?;
        self.resolve_asset(&record, asset_name).await
    }

    /// 使用外部会话专属票据返回媒体资源。
    pub async fn get_external_asset(
        &self,
        session_id: &str,
        access_token: &str,
        asset_name: &str,
    ) -> Result<RemoteMediaAsset, RemoteMediaError> {
        let record = self
            .require_session(session_id, None, Some(access_token))
            .await?;
        self.resolve_asset(&record, asset_name).await
    }

    async fn require_session(
        &self,
        session_id: &str,
        device_id: Option<&str>,
        access_token: Option<&str>,
    ) -> Result<Arc<SessionRecord>, RemoteMediaError> {
        let record = self
            .sessions
            .lock()
            .await
            .get(session_id)
            .cloned()
            .ok_or_else(session_not_found)?;
        if device_id.is_some_and(|device| record.device_id != device)
            || access_token.is_some_and(|token| {
                record.access != SessionAccess::External
                    || !secure_token_equals(record.access_token.as_deref(), token)
            })
        {
            return Err(session_not_found());
        }
        self.refresh_expiration(&record).await;
        Ok(record)
    }

    async fn resolve_asset(
        &self,
        record: &SessionRecord,
        asset_name: &str,
    ) -> Result<RemoteMediaAsset, RemoteMediaError> {
        let public = record.public.lock().await;
        if let Some(subtitle) = public
            .subtitles
            .iter()
            .find(|subtitle| subtitle.url.ends_with(&format!("/subtitles/{asset_name}")))
        {
            if !is_subtitle_asset(asset_name) {
                return Err(asset_not_found());
            }
            let path = canonical_asset(&record.temporary_directory, asset_name).await?;
            return Ok(RemoteMediaAsset {
                file_path: path,
                content_type: if subtitle.subtitle_type == "ass" {
                    "text/x-ssa; charset=utf-8"
                } else {
                    "text/vtt; charset=utf-8"
                }
                .to_owned(),
                direct: false,
                file_name: None,
            });
        }
        if public.mode == "direct" {
            if asset_name != "file" && asset_name != public.file_name {
                return Err(asset_not_found());
            }
            return Ok(RemoteMediaAsset {
                file_path: record.source_path.clone(),
                content_type: record.content_type.clone(),
                direct: true,
                file_name: Some(public.file_name.clone()),
            });
        }
        if !is_hls_asset(asset_name) {
            return Err(asset_not_found());
        }
        Ok(RemoteMediaAsset {
            file_path: canonical_asset(&record.temporary_directory, asset_name).await?,
            content_type: if asset_name.ends_with(".m3u8") {
                "application/vnd.apple.mpegurl"
            } else {
                "video/mp2t"
            }
            .to_owned(),
            direct: false,
            file_name: None,
        })
    }

    /// 关闭设备拥有的会话并回收 FFmpeg 与临时资源。
    pub async fn close_session(&self, session_id: &str, device_id: &str) -> bool {
        let record = self.sessions.lock().await.get(session_id).cloned();
        if record
            .as_ref()
            .is_none_or(|record| record.device_id != device_id)
        {
            return false;
        }
        self.remove_session(session_id).await;
        true
    }

    /// 关闭全部会话，供网关停止和应用退出时回收资源。
    pub async fn stop_all(&self) {
        let ids = self
            .sessions
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            self.remove_session(&id).await;
        }
    }

    /// 关闭指定设备拥有的全部会话，供吊销令牌时立即回收媒体资源。
    pub async fn close_device_sessions(&self, device_id: &str) -> usize {
        let snapshot = self.sessions.lock().await.clone();
        let ids = snapshot
            .into_iter()
            .filter_map(|(id, record)| (record.device_id == device_id).then_some(id))
            .collect::<Vec<_>>();
        for id in &ids {
            self.remove_session(id).await;
        }
        ids.len()
    }

    async fn remove_session(&self, session_id: &str) {
        let record = self.sessions.lock().await.remove(session_id);
        let Some(record) = record else {
            return;
        };
        if let Some(process) = record.process.lock().await.take() {
            process.stop().await;
        }
        if let Err(error) = tokio::fs::remove_dir_all(&record.temporary_directory).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!("清理远程媒体缓存失败 session_id={session_id} error={error}");
            }
        }
        log::info!("Rust 远程媒体会话已关闭 session_id={session_id}");
    }

    async fn close_matching(&self, device_id: &str, task_id: &str, access: SessionAccess) {
        let snapshot = self.sessions.lock().await.clone();
        for (id, record) in snapshot {
            if record.device_id == device_id
                && record.access == access
                && record.public.lock().await.task_id == task_id
            {
                self.remove_session(&id).await;
            }
        }
    }

    async fn reserve_slot(&self) {
        let snapshot = self.sessions.lock().await.clone();
        if snapshot.len() < MAX_SESSIONS {
            return;
        }
        let mut oldest = None;
        for (id, record) in snapshot {
            let last_accessed = *record.last_accessed_at_millis.lock().await;
            if oldest
                .as_ref()
                .is_none_or(|(_, timestamp)| last_accessed < *timestamp)
            {
                oldest = Some((id, last_accessed));
            }
        }
        if let Some((id, _)) = oldest {
            self.remove_session(&id).await;
        }
    }

    async fn cleanup_expired(&self) {
        let now = Utc::now().timestamp_millis();
        let snapshot = self.sessions.lock().await.clone();
        for (id, record) in snapshot {
            let public = record.public.lock().await;
            let expired = chrono::DateTime::parse_from_rfc3339(&public.expires_at)
                .map(|value| value.timestamp_millis() <= now)
                .unwrap_or(true);
            drop(public);
            if expired {
                self.remove_session(&id).await;
            }
        }
    }

    async fn close_if_expired(&self, session_id: &str) {
        let record = self.sessions.lock().await.get(session_id).cloned();
        let Some(record) = record else {
            return;
        };
        let expires_at = record.public.lock().await.expires_at.clone();
        if chrono::DateTime::parse_from_rfc3339(&expires_at)
            .map(|value| value <= Utc::now())
            .unwrap_or(true)
        {
            self.remove_session(session_id).await;
        }
    }

    async fn refresh_expiration(&self, record: &SessionRecord) {
        let now = Utc::now();
        *record.last_accessed_at_millis.lock().await = now.timestamp_millis();
        record.public.lock().await.expires_at = (now
            + chrono::Duration::from_std(SESSION_TTL).unwrap_or(chrono::Duration::minutes(30)))
        .to_rfc3339_opts(SecondsFormat::Millis, true);
    }

    async fn resolve_media(
        &self,
        task: &DownloadTask,
        requested_file_index: Option<i64>,
    ) -> Result<ResolvedMedia, RemoteMediaError> {
        let settings = self
            .repository
            .get_settings()
            .await
            .map_err(internal_media_error)?;
        let extensions = video_extensions(&settings);
        let media_files = self
            .repository
            .list_media_files()
            .await
            .map_err(internal_media_error)?;
        let mut task_files = task
            .files
            .iter()
            .filter(|file| {
                file.selected && file.progress >= 1.0 && extensions.contains(&extension(&file.name))
            })
            .collect::<Vec<_>>();
        task_files.sort_by_key(|file| Reverse(file.size));
        if requested_file_index
            .is_some_and(|index| !task_files.iter().any(|file| file.index == index))
        {
            return Err(RemoteMediaError::new(
                409,
                "MEDIA_FILE_UNAVAILABLE",
                "指定媒体文件不存在或尚未写入完成",
            ));
        }
        let mut candidates = Vec::new();
        if let Some(index) = requested_file_index {
            let file = task_files
                .iter()
                .find(|file| file.index == index)
                .expect("file index checked");
            candidates.push((
                task_file_path(task, &file.name),
                Path::new(&file.name)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                Some(file.index),
                None,
            ));
        } else {
            let mut registered = media_files
                .into_iter()
                .filter(|media| media.download_task_id.as_deref() == Some(&task.id))
                .collect::<Vec<_>>();
            registered.sort_by_key(|media| Reverse(media.size));
            candidates.extend(registered.into_iter().map(|media| {
                (
                    PathBuf::from(&media.file_path),
                    media.file_name.clone(),
                    task.files.iter().find_map(|file| {
                        (task_file_path(task, &file.name).as_path() == Path::new(&media.file_path))
                            .then_some(file.index)
                    }),
                    Some(media),
                )
            }));
            candidates.extend(task_files.into_iter().map(|file| {
                (
                    task_file_path(task, &file.name),
                    Path::new(&file.name)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    Some(file.index),
                    None,
                )
            }));
        }

        for (path, file_name, file_index, media) in candidates {
            let Some(path) = validate_media_path(Path::new(&task.save_path), &path).await else {
                continue;
            };
            if !extensions.contains(&extension(&path.to_string_lossy())) {
                continue;
            }
            let duration_seconds = media
                .as_ref()
                .and_then(|media| media.duration_seconds)
                .or(self.probe_duration(&path).await);
            return Ok(ResolvedMedia {
                content_type: direct_content_type(&path),
                path,
                file_name,
                file_index,
                duration_seconds,
            });
        }
        Err(RemoteMediaError::new(
            409,
            "MEDIA_FILE_UNAVAILABLE",
            "已完成的媒体文件不存在或尚未写入完成",
        ))
    }

    async fn probe_duration(&self, path: &Path) -> Option<i64> {
        for command_path in &self.tools.ffprobe_paths {
            let output = run_command(
                command_path,
                &[
                    "-v",
                    "quiet",
                    "-show_entries",
                    "format=duration",
                    "-of",
                    "default=noprint_wrappers=1:nokey=1",
                ],
                Some(path),
                self.tools.timeout,
            )
            .await;
            if let Ok(output) = output {
                if let Some(duration) = String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite() && *value >= 0.0)
                {
                    return Some(duration.round() as i64);
                }
            }
        }
        None
    }

    async fn prepare_subtitles(
        &self,
        source_path: &Path,
        output_directory: &Path,
        asset_base: &str,
    ) -> Vec<RemotePlaybackSubtitle> {
        let streams = match self.probe_subtitles(source_path).await {
            Ok(streams) => streams,
            Err(error) => {
                log::warn!("远程字幕探测失败 error={error}");
                return Vec::new();
            }
        };
        let supported = streams
            .into_iter()
            .filter_map(SupportedSubtitle::from_stream)
            .collect::<Vec<_>>();
        let mut subtitles = Vec::new();
        for (order, stream) in supported.into_iter().enumerate() {
            let asset_name = format!("subtitle-{order:03}.{}", stream.output_type);
            let output_path = output_directory.join(&asset_name);
            let mut command = hidden_command(&self.tools.ffmpeg_path);
            command
                .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
                .arg(source_path)
                .args([
                    "-map",
                    &format!("0:{}", stream.index),
                    "-c:s",
                    if stream.output_type == "ass" {
                        "ass"
                    } else {
                        "webvtt"
                    },
                    "-y",
                ])
                .arg(&output_path)
                .kill_on_drop(true);
            let completed = tokio::time::timeout(self.tools.timeout, command.output()).await;
            let valid = matches!(completed, Ok(Ok(ref output)) if output.status.success())
                && tokio::fs::metadata(&output_path)
                    .await
                    .is_ok_and(|metadata| metadata.len() > 0);
            if valid {
                let language = normalize_language(stream.tags.get("language").map(String::as_str));
                let title = stream.tags.get("title").map(String::as_str);
                subtitles.push(RemotePlaybackSubtitle {
                    id: format!("subtitle-{}", stream.index),
                    label: subtitle_label(title, language.as_deref(), order),
                    language,
                    subtitle_type: stream.output_type.to_owned(),
                    url: format!("{asset_base}/subtitles/{asset_name}"),
                    default: stream.disposition.default == 1,
                });
            }
        }
        subtitles
    }

    async fn probe_subtitles(&self, source_path: &Path) -> Result<Vec<SubtitleStream>, String> {
        let mut last_error = "没有可用 FFprobe".to_owned();
        for command_path in &self.tools.ffprobe_paths {
            match run_command(
                command_path,
                &[
                    "-v",
                    "quiet",
                    "-print_format",
                    "json",
                    "-show_streams",
                    "-select_streams",
                    "s",
                ],
                Some(source_path),
                self.tools.timeout,
            )
            .await
            {
                Ok(output) => {
                    let parsed: SubtitleProbeOutput = serde_json::from_slice(&output.stdout)
                        .map_err(|error| format!("FFprobe 字幕 JSON 无效：{error}"))?;
                    return Ok(parsed.streams);
                }
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }

    async fn start_hls(
        &self,
        source_path: &Path,
        output_directory: &Path,
        enhancement: RemotePlaybackEnhancement,
    ) -> Result<HlsStart, RemoteMediaError> {
        if enhancement.frame_interpolation == PlayerFrameInterpolation::RifeRealtime {
            match self.ensure_rife_runtime().await {
                Ok(runtime) => match self
                    .start_rife_hls(source_path, output_directory, enhancement, runtime)
                    .await
                {
                    Ok(started) => return Ok(started),
                    Err(error) => {
                        log::warn!("RIFE 远程管线启动失败，回退 FFmpeg error={}", error.message);
                        return self
                            .start_hls_ffmpeg(
                                source_path,
                                output_directory,
                                RemotePlaybackEnhancement {
                                    frame_interpolation: PlayerFrameInterpolation::Off,
                                    ..enhancement
                                },
                            )
                            .await
                            .map(|mut started| {
                                started.degradation_reason = Some(error.message);
                                started.encoder_degraded = true;
                                started
                            });
                    }
                },
                Err(reason) => {
                    log::warn!("RIFE sidecar 不可用，回退 FFmpeg reason={reason}");
                    return self
                        .start_hls_ffmpeg(
                            source_path,
                            output_directory,
                            RemotePlaybackEnhancement {
                                frame_interpolation: PlayerFrameInterpolation::Off,
                                ..enhancement
                            },
                        )
                        .await
                        .map(|mut started| {
                            started.degradation_reason = Some(reason);
                            started.encoder_degraded = true;
                            started
                        });
                }
            }
        }
        self.start_hls_ffmpeg(source_path, output_directory, enhancement)
            .await
    }

    async fn ensure_rife_runtime(&self) -> Result<Arc<ModelSidecarRuntime>, String> {
        let mut guard = self.rife_runtime.lock().await;
        if let Some(runtime) = guard.as_ref() {
            if runtime.ready() {
                return Ok(Arc::clone(runtime));
            }
            *guard = None;
        }
        let root = self
            .tools
            .rife_sidecar_root
            .clone()
            .ok_or_else(|| "未找到 RIFE sidecar 资源".to_owned())?;
        let config = ModelSidecarConfig::new(root, self.tools.rife_available_vram_bytes, 33.0);
        let runtime = Arc::new(ModelSidecarRuntime::launch(config).await?);
        *guard = Some(Arc::clone(&runtime));
        Ok(runtime)
    }

    async fn start_hls_ffmpeg(
        &self,
        source_path: &Path,
        output_directory: &Path,
        enhancement: RemotePlaybackEnhancement,
    ) -> Result<HlsStart, RemoteMediaError> {
        let playlist = output_directory.join("index.m3u8");
        let segments = output_directory.join("segment-%06d.ts");
        let mut last_error = None;
        for (index, (codec, encoder)) in ENCODER_CANDIDATES.iter().enumerate() {
            let _ = tokio::fs::remove_file(&playlist).await;
            let mut command = hidden_command(&self.tools.ffmpeg_path);
            command
                .args(["-nostdin", "-hide_banner", "-loglevel", "warning", "-i"])
                .arg(source_path)
                .args(["-map", "0:v:0", "-map", "0:a:0?", "-c:v"])
                .args(encoder_video_args(codec))
                .args(remote_video_filter_args(enhancement))
                .args([
                    "-pix_fmt",
                    "yuv420p",
                    "-c:a",
                    "aac",
                    "-b:a",
                    "160k",
                    "-ac",
                    "2",
                    "-f",
                    "hls",
                    "-hls_time",
                    "4",
                    "-hls_list_size",
                    "0",
                    "-hls_playlist_type",
                    "event",
                    "-hls_segment_filename",
                ])
                .arg(&segments)
                .arg(&playlist)
                .kill_on_drop(true);
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(error) => {
                    last_error = Some(error.to_string());
                    continue;
                }
            };
            let started = tokio::time::Instant::now();
            loop {
                if tokio::fs::try_exists(&playlist).await.unwrap_or(false) {
                    return Ok(HlsStart {
                        process: MediaProcess {
                            encoder: child,
                            decoder: None,
                            pipeline: None,
                        },
                        encoder,
                        encoder_degraded: index > 0,
                        actual_interpolation: enhancement.frame_interpolation,
                        degradation_reason: None,
                    });
                }
                if let Ok(Some(status)) = child.try_wait() {
                    last_error = Some(format!("编码器 {encoder} 提前退出：{status}"));
                    break;
                }
                if started.elapsed() >= TRANSCODER_START_TIMEOUT {
                    let _ = child.kill().await;
                    last_error = Some(format!("编码器 {encoder} 启动超时"));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        Err(RemoteMediaError::new(
            503,
            "TRANSCODER_UNAVAILABLE",
            last_error.unwrap_or_else(|| "没有可用的 HLS 编码器".to_owned()),
        ))
    }

    async fn start_rife_hls(
        &self,
        source_path: &Path,
        output_directory: &Path,
        enhancement: RemotePlaybackEnhancement,
        runtime: Arc<ModelSidecarRuntime>,
    ) -> Result<HlsStart, RemoteMediaError> {
        let video = self
            .probe_video(source_path)
            .await
            .map_err(|error| RemoteMediaError::new(503, "RIFE_VIDEO_PROBE_FAILED", error))?;
        let playlist = output_directory.join("index.m3u8");
        let segments = output_directory.join("segment-%06d.ts");
        let output_fps = (video.frame_rate * 2.0).clamp(1.0, 240.0);
        let dimensions = format!("{}x{}", video.width, video.height);
        let mut decoder = hidden_command(&self.tools.ffmpeg_path);
        decoder
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(source_path)
            .args([
                "-map", "0:v:0", "-an", "-vsync", "0", "-f", "rawvideo", "-pix_fmt", "rgb24",
                "pipe:1",
            ])
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        let mut decoder = decoder.spawn().map_err(|error| {
            RemoteMediaError::new(503, "RIFE_DECODER_UNAVAILABLE", error.to_string())
        })?;
        let decoder_stdout = decoder.stdout.take().ok_or_else(|| {
            RemoteMediaError::new(
                503,
                "RIFE_DECODER_OUTPUT_UNAVAILABLE",
                "FFmpeg RGB 输出不可用",
            )
        })?;

        let mut encoder = hidden_command(&self.tools.ffmpeg_path);
        let fps_arg = format!("{output_fps:.6}");
        encoder
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "warning",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "-s",
                &dimensions,
                "-r",
                &fps_arg,
                "-i",
                "pipe:0",
                "-i",
            ])
            .arg(source_path)
            .args(["-map", "0:v:0", "-map", "1:a:0?", "-c:v"])
            .args(encoder_video_args("libx264"))
            .args(remote_video_filter_args(RemotePlaybackEnhancement {
                frame_interpolation: PlayerFrameInterpolation::Off,
                ..enhancement
            }))
            .args([
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                "160k",
                "-ac",
                "2",
                "-shortest",
                "-f",
                "hls",
                "-hls_time",
                "4",
                "-hls_list_size",
                "0",
                "-hls_playlist_type",
                "event",
                "-hls_segment_filename",
            ])
            .arg(&segments)
            .arg(&playlist)
            .stdin(Stdio::piped())
            .kill_on_drop(true);
        let mut encoder = encoder.spawn().map_err(|error| {
            RemoteMediaError::new(503, "RIFE_ENCODER_UNAVAILABLE", error.to_string())
        })?;
        let encoder_stdin = encoder.stdin.take().ok_or_else(|| {
            RemoteMediaError::new(
                503,
                "RIFE_ENCODER_INPUT_UNAVAILABLE",
                "FFmpeg rawvideo 输入不可用",
            )
        })?;
        let pipeline = tokio::spawn(run_rife_pipeline(
            decoder_stdout,
            encoder_stdin,
            runtime,
            video.width,
            video.height,
            video.frame_rate,
        ));
        let started = tokio::time::Instant::now();
        loop {
            if tokio::fs::try_exists(&playlist).await.unwrap_or(false) {
                return Ok(HlsStart {
                    process: MediaProcess {
                        encoder,
                        decoder: Some(decoder),
                        pipeline: Some(pipeline),
                    },
                    encoder: "libx264",
                    encoder_degraded: true,
                    actual_interpolation: PlayerFrameInterpolation::RifeRealtime,
                    degradation_reason: None,
                });
            }
            if let Ok(Some(status)) = encoder.try_wait() {
                let _ = decoder.kill().await;
                pipeline.abort();
                return Err(RemoteMediaError::new(
                    503,
                    "RIFE_ENCODER_EXITED",
                    format!("RIFE 编码器提前退出：{status}"),
                ));
            }
            if started.elapsed() >= TRANSCODER_START_TIMEOUT {
                pipeline.abort();
                let _ = decoder.kill().await;
                let _ = encoder.kill().await;
                return Err(RemoteMediaError::new(
                    503,
                    "RIFE_PIPELINE_START_TIMEOUT",
                    "RIFE 远程管线启动超时",
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn probe_video(&self, source_path: &Path) -> Result<VideoProbe, String> {
        let mut last_error = "没有可用 FFprobe".to_owned();
        for command_path in &self.tools.ffprobe_paths {
            match run_command(
                command_path,
                &[
                    "-v",
                    "quiet",
                    "-print_format",
                    "json",
                    "-show_entries",
                    "stream=width,height,avg_frame_rate",
                    "-select_streams",
                    "v:0",
                ],
                Some(source_path),
                self.tools.timeout,
            )
            .await
            {
                Ok(output) => {
                    let parsed: VideoProbeOutput = serde_json::from_slice(&output.stdout)
                        .map_err(|error| format!("FFprobe 视频 JSON 无效：{error}"))?;
                    let stream = parsed
                        .streams
                        .into_iter()
                        .next()
                        .ok_or_else(|| "媒体没有视频流".to_owned())?;
                    let width = stream.width.ok_or_else(|| "视频宽度缺失".to_owned())?;
                    let height = stream.height.ok_or_else(|| "视频高度缺失".to_owned())?;
                    let frame_rate =
                        parse_frame_rate(stream.avg_frame_rate.as_deref().unwrap_or("0/1"))?;
                    return Ok(VideoProbe {
                        width,
                        height,
                        frame_rate,
                    });
                }
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }
}

struct VideoProbe {
    width: u32,
    height: u32,
    frame_rate: f64,
}

#[derive(Default, Deserialize)]
struct VideoProbeOutput {
    #[serde(default)]
    streams: Vec<VideoProbeStream>,
}

#[derive(Default, Deserialize)]
struct VideoProbeStream {
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
}

fn parse_frame_rate(value: &str) -> Result<f64, String> {
    let (numerator, denominator) = value
        .split_once('/')
        .ok_or_else(|| "视频帧率格式无效".to_owned())?;
    let numerator = numerator
        .parse::<f64>()
        .map_err(|_| "视频帧率无效".to_owned())?;
    let denominator = denominator
        .parse::<f64>()
        .map_err(|_| "视频帧率无效".to_owned())?;
    let frame_rate = numerator / denominator;
    if !frame_rate.is_finite() || frame_rate <= 0.0 || frame_rate > 120.0 {
        return Err("视频帧率超出 RIFE 管线限制".to_owned());
    }
    Ok(frame_rate)
}

async fn run_rife_pipeline(
    mut decoder_stdout: ChildStdout,
    mut encoder_stdin: ChildStdin,
    runtime: Arc<ModelSidecarRuntime>,
    width: u32,
    height: u32,
    frame_rate: f64,
) -> Result<(), String> {
    let frame_size = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(3))
        .and_then(|stride| {
            usize::try_from(height)
                .ok()
                .and_then(|height| stride.checked_mul(height))
        })
        .ok_or_else(|| "RIFE 视频帧大小溢出".to_owned())?;
    let frame_interval = (1_000_000.0 / frame_rate).round() as i64;
    let (sender, mut receiver) = mpsc::channel::<RawVideoFrame>(RIFE_FRAME_QUEUE_CAPACITY);
    let reader = tokio::spawn(async move {
        let mut index = 0_i64;
        loop {
            let mut data = vec![0_u8; frame_size];
            match decoder_stdout.read_exact(&mut data).await {
                Ok(_) => {
                    let frame = RawVideoFrame {
                        width,
                        height,
                        stride: width.saturating_mul(3),
                        pts_micros: index.saturating_mul(frame_interval),
                        data,
                    };
                    if sender.send(frame).await.is_err() {
                        return Ok::<(), String>(());
                    }
                    index = index.saturating_add(1);
                }
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(error) => return Err(format!("读取 FFmpeg RGB 帧失败：{error}")),
            }
        }
    });
    let Some(mut previous) = receiver.recv().await else {
        let _ = reader.await;
        return Err("FFmpeg 没有输出视频帧".to_owned());
    };
    while let Some(next) = receiver.recv().await {
        encoder_stdin
            .write_all(&previous.data)
            .await
            .map_err(|error| format!("写入 RIFE 原始帧失败：{error}"))?;
        let middle = runtime.interpolate(previous.clone(), next.clone()).await?;
        encoder_stdin
            .write_all(&middle.data)
            .await
            .map_err(|error| format!("写入 RIFE 中间帧失败：{error}"))?;
        previous = next;
    }
    encoder_stdin
        .write_all(&previous.data)
        .await
        .map_err(|error| format!("写入 RIFE 最后一帧失败：{error}"))?;
    encoder_stdin
        .shutdown()
        .await
        .map_err(|error| format!("关闭 RIFE 编码输入失败：{error}"))?;
    reader
        .await
        .map_err(|error| format!("读取 RIFE 帧任务失败：{error}"))??;
    Ok(())
}

fn validate_remote_enhancement(
    requested_mode: &str,
    enhancement: RemotePlaybackEnhancement,
) -> Result<(), RemoteMediaError> {
    let enabled = enhancement.video_enhancement != PlayerVideoEnhancement::Off
        || enhancement.frame_interpolation != PlayerFrameInterpolation::Off;
    if requested_mode != "transcode" && enabled {
        return Err(RemoteMediaError::new(
            400,
            "MEDIA_ENHANCEMENT_REQUIRES_TRANSCODE",
            "远程画质增强和插帧只能在实时转码模式使用",
        ));
    }
    if enhancement.frame_interpolation == PlayerFrameInterpolation::DisplayResample {
        return Err(RemoteMediaError::new(
            400,
            "MEDIA_INTERPOLATION_UNAVAILABLE",
            "远程转码不使用播放器显示刷新率重采样",
        ));
    }
    Ok(())
}

fn remote_video_filter_args(enhancement: RemotePlaybackEnhancement) -> Vec<String> {
    let mut filters = Vec::new();
    match enhancement.video_enhancement {
        PlayerVideoEnhancement::Off => {}
        PlayerVideoEnhancement::Balanced => {
            filters.push("hqdn3d=1.2:1.2:4:4");
            filters.push("unsharp=5:5:0.45:5:5:0");
        }
        PlayerVideoEnhancement::Clear => {
            filters.push("hqdn3d=0.8:0.8:3:3");
            filters.push("unsharp=7:7:0.75:5:5:0");
        }
    }
    if enhancement.frame_interpolation == PlayerFrameInterpolation::MotionCompensated {
        filters.push("minterpolate=fps=60:mi_mode=mci:mc_mode=aobmc:me_mode=bidir:vsbmc=1");
    }
    if filters.is_empty() {
        Vec::new()
    } else {
        vec!["-vf".to_owned(), filters.join(",")]
    }
}

/// 返回各编码器合法的视频参数，避免把 libx264 参数误传给硬件编码器。
fn encoder_video_args(codec: &str) -> &'static [&'static str] {
    match codec {
        "h264_nvenc" => &["h264_nvenc", "-preset", "p4", "-rc", "vbr", "-cq", "23"],
        "h264_amf" => &[
            "h264_amf", "-quality", "balanced", "-qp_i", "23", "-qp_p", "23",
        ],
        "h264_qsv" => &["h264_qsv", "-global_quality", "23"],
        _ => &["libx264", "-preset", "veryfast", "-crf", "23"],
    }
}

#[cfg(test)]
mod encoder_tests {
    use super::{
        encoder_video_args, remote_video_filter_args, validate_remote_enhancement,
        ENCODER_CANDIDATES,
    };
    use ani_contracts::{
        PlayerFrameInterpolation, PlayerVideoEnhancement, RemotePlaybackEnhancement,
    };

    #[test]
    fn selects_vendor_specific_video_options() {
        assert_eq!(encoder_video_args("h264_nvenc")[0], "h264_nvenc");
        assert!(encoder_video_args("h264_nvenc").contains(&"-cq"));
        assert!(encoder_video_args("h264_amf").contains(&"-qp_i"));
        assert!(encoder_video_args("h264_qsv").contains(&"-global_quality"));
        assert!(encoder_video_args("libx264").contains(&"-crf"));
    }

    #[test]
    fn tries_nvidia_amd_intel_before_software_fallback() {
        assert_eq!(
            ENCODER_CANDIDATES,
            &[
                ("h264_nvenc", "nvenc"),
                ("h264_amf", "amf"),
                ("h264_qsv", "qsv"),
                ("libx264", "libx264"),
            ]
        );
    }

    #[test]
    fn builds_actual_remote_enhancement_filter_chain() {
        let args = remote_video_filter_args(RemotePlaybackEnhancement {
            video_enhancement: PlayerVideoEnhancement::Clear,
            frame_interpolation: PlayerFrameInterpolation::MotionCompensated,
        });
        assert_eq!(args[0], "-vf");
        assert!(args[1].contains("unsharp=7:7"));
        assert!(args[1].contains("minterpolate=fps=60"));
    }

    #[test]
    fn rejects_enhancement_for_direct_and_unready_rife() {
        assert!(validate_remote_enhancement(
            "direct",
            RemotePlaybackEnhancement {
                video_enhancement: PlayerVideoEnhancement::Balanced,
                ..Default::default()
            }
        )
        .is_err());
        assert!(validate_remote_enhancement(
            "transcode",
            RemotePlaybackEnhancement {
                frame_interpolation: PlayerFrameInterpolation::DisplayResample,
                ..Default::default()
            }
        )
        .is_err());
        assert!(validate_remote_enhancement(
            "transcode",
            RemotePlaybackEnhancement {
                frame_interpolation: PlayerFrameInterpolation::RifeRealtime,
                ..Default::default()
            }
        )
        .is_ok());
    }

    #[test]
    fn accepts_normal_frame_rates_and_rejects_unsafe_values() {
        assert!((super::parse_frame_rate("24000/1001").unwrap() - 23.976).abs() < 0.01);
        assert!(super::parse_frame_rate("0/1").is_err());
        assert!(super::parse_frame_rate("121/1").is_err());
        assert!(super::parse_frame_rate("nan/1").is_err());
    }
}

#[derive(Default, Deserialize)]
struct SubtitleProbeOutput {
    #[serde(default)]
    streams: Vec<SubtitleStream>,
}

#[derive(Default, Deserialize)]
struct SubtitleStream {
    index: Option<i64>,
    codec_name: Option<String>,
    #[serde(default)]
    disposition: SubtitleDisposition,
    #[serde(default)]
    tags: HashMap<String, String>,
}

#[derive(Default, Deserialize)]
struct SubtitleDisposition {
    default: i64,
}

struct SupportedSubtitle {
    index: i64,
    output_type: &'static str,
    disposition: SubtitleDisposition,
    tags: HashMap<String, String>,
}

impl SupportedSubtitle {
    fn from_stream(stream: SubtitleStream) -> Option<Self> {
        let index = stream.index?;
        let codec = stream.codec_name.as_deref()?.to_ascii_lowercase();
        let output_type = if matches!(codec.as_str(), "ass" | "ssa") {
            "ass"
        } else if matches!(
            codec.as_str(),
            "subrip" | "srt" | "webvtt" | "mov_text" | "text"
        ) {
            "vtt"
        } else {
            return None;
        };
        Some(Self {
            index,
            output_type,
            disposition: stream.disposition,
            tags: stream.tags,
        })
    }
}

async fn run_command(
    command_path: &Path,
    args: &[&str],
    trailing_path: Option<&Path>,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let mut command = hidden_command(command_path);
    command.args(args);
    if let Some(path) = trailing_path {
        command.arg(path);
    }
    command.kill_on_drop(true);
    match tokio::time::timeout(timeout, command.output()).await {
        Ok(Ok(output)) if output.status.success() => Ok(output),
        Ok(Ok(output)) => Err(format!("退出状态 {}", output.status)),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("执行超时".to_owned()),
    }
}

fn hidden_command(path: &Path) -> Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;

        let mut command = Command::new(path);
        command.as_std_mut().creation_flags(0x0800_0000);
        command
    }
    #[cfg(not(target_os = "windows"))]
    {
        Command::new(path)
    }
}

async fn validate_media_path(root: &Path, candidate: &Path) -> Option<PathBuf> {
    let (Ok(root), Ok(candidate)) = (
        tokio::fs::canonicalize(root).await,
        tokio::fs::canonicalize(candidate).await,
    ) else {
        return None;
    };
    if !candidate.starts_with(&root) {
        log::warn!("拒绝下载目录外的远程媒体路径");
        return None;
    }
    tokio::fs::metadata(&candidate)
        .await
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|_| candidate)
}

async fn canonical_asset(root: &Path, name: &str) -> Result<PathBuf, RemoteMediaError> {
    let candidate = root.join(name);
    let path = tokio::fs::canonicalize(&candidate)
        .await
        .map_err(|_| asset_not_found())?;
    if !path.starts_with(root) || !path.is_file() {
        return Err(asset_not_found());
    }
    Ok(path)
}

fn task_file_path(task: &DownloadTask, name: &str) -> PathBuf {
    let path = Path::new(name);
    if path.is_absolute() {
        path.to_owned()
    } else {
        Path::new(&task.save_path).join(path)
    }
}

fn video_extensions(settings: &AppSettings) -> HashSet<String> {
    settings
        .pointer("/media/videoExtensions")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(normalize_extension)
                .collect()
        })
        .filter(|values: &HashSet<_>| !values.is_empty())
        .unwrap_or_else(|| {
            [".mkv", ".mp4", ".avi"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        })
}

fn normalize_extension(value: &str) -> String {
    let value = value.trim().to_ascii_lowercase();
    if value.starts_with('.') {
        value
    } else {
        format!(".{value}")
    }
}

fn extension(value: &str) -> String {
    Path::new(value)
        .extension()
        .map(|value| format!(".{}", value.to_string_lossy().to_ascii_lowercase()))
        .unwrap_or_default()
}

fn direct_content_type(path: &Path) -> String {
    match extension(&path.to_string_lossy()).as_str() {
        ".webm" => "video/webm",
        ".mp4" | ".m4v" => "video/mp4",
        ".mkv" => "video/x-matroska",
        ".avi" => "video/x-msvideo",
        ".mov" => "video/quicktime",
        ".mpg" | ".mpeg" => "video/mpeg",
        _ => "application/octet-stream",
    }
    .to_owned()
}

fn resolve_resume_position(checkpoint: Option<&PlaybackCheckpoint>) -> Option<f64> {
    let checkpoint = checkpoint?;
    if checkpoint.completed
        || checkpoint.position_seconds < 5.0
        || (checkpoint.duration_seconds > 0.0
            && checkpoint.duration_seconds - checkpoint.position_seconds <= 30.0)
    {
        return None;
    }
    Some(checkpoint.position_seconds)
}

fn random_token(size: usize) -> String {
    let mut bytes = vec![0_u8; size];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn secure_token_equals(expected: Option<&str>, actual: &str) -> bool {
    expected.is_some_and(|expected| {
        expected.len() == actual.len()
            && expected.as_bytes().ct_eq(actual.as_bytes()).unwrap_u8() == 1
    })
}

fn is_hls_asset(value: &str) -> bool {
    value == "index.m3u8"
        || value
            .strip_prefix("segment-")
            .and_then(|value| value.strip_suffix(".ts"))
            .is_some_and(|digits| {
                digits.len() == 6 && digits.bytes().all(|byte| byte.is_ascii_digit())
            })
}

fn is_subtitle_asset(value: &str) -> bool {
    value
        .strip_prefix("subtitle-")
        .and_then(|value| {
            value
                .strip_suffix(".ass")
                .or_else(|| value.strip_suffix(".vtt"))
        })
        .is_some_and(|digits| digits.len() == 3 && digits.bytes().all(|byte| byte.is_ascii_digit()))
}

fn normalize_language(value: Option<&str>) -> Option<String> {
    let normalized = value?.trim().to_ascii_lowercase();
    if normalized.is_empty() || normalized == "und" {
        return None;
    }
    Some(match normalized.as_str() {
        "zh" | "zho" | "chi" => "中文".to_owned(),
        "chs" => "简体中文".to_owned(),
        "cht" => "繁体中文".to_owned(),
        "ja" | "jpn" => "日语".to_owned(),
        "en" | "eng" => "英语".to_owned(),
        _ => sanitize_label(&normalized).unwrap_or_else(|| "未知".to_owned()),
    })
}

fn subtitle_label(title: Option<&str>, language: Option<&str>, order: usize) -> String {
    let title = title.and_then(sanitize_label);
    let mut parts = Vec::new();
    if let Some(title) = title {
        parts.push(title);
    }
    if let Some(language) = language {
        if !parts.iter().any(|part| part == language) {
            parts.push(language.to_owned());
        }
    }
    if parts.is_empty() {
        format!("字幕 {}", order + 1)
    } else {
        parts.join(" / ")
    }
}

fn sanitize_label(value: &str) -> Option<String> {
    let value = value
        .chars()
        .filter(|character| !character.is_control() && *character != '<' && *character != '>')
        .take(120)
        .collect::<String>();
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn internal_media_error(error: impl Into<String>) -> RemoteMediaError {
    log::error!("Rust 远程媒体服务失败 error={}", error.into());
    RemoteMediaError::new(500, "MEDIA_SERVICE_FAILED", "远程媒体服务内部错误")
}

fn session_not_found() -> RemoteMediaError {
    RemoteMediaError::new(404, "MEDIA_SESSION_NOT_FOUND", "播放会话不存在或已过期")
}

fn asset_not_found() -> RemoteMediaError {
    RemoteMediaError::new(404, "MEDIA_ASSET_NOT_FOUND", "媒体资源不存在")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证媒体资源路由只接受固定 HLS 与字幕文件名。
    #[test]
    fn validates_asset_names() {
        assert!(is_hls_asset("index.m3u8"));
        assert!(is_hls_asset("segment-000001.ts"));
        assert!(!is_hls_asset("../segment-000001.ts"));
        assert!(is_subtitle_asset("subtitle-001.ass"));
        assert!(!is_subtitle_asset("subtitle-1.ass"));
    }

    /// 验证已完成或临近片尾的检查点不会恢复。
    #[test]
    fn guards_resume_position() {
        let checkpoint = PlaybackCheckpoint {
            task_id: "task-1".to_owned(),
            file_index: Some(0),
            position_seconds: 90.0,
            duration_seconds: 100.0,
            completed: false,
            watched_reported: true,
            updated_at: "2026-07-25T00:00:00.000Z".to_owned(),
        };
        assert_eq!(resolve_resume_position(Some(&checkpoint)), None);
    }
}
