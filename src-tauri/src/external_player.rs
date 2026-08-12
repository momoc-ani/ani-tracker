use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ani_contracts::{
    PlayerDetectionCandidate, PlayerDetectionResult, PlayerRuntimePlatform,
    SelectPlayerExecutableInput,
};
use ani_domain::AppSettings;
use serde::Deserialize;
use serde_json::Value;
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::media::AppMediaState;

const IPC_CONNECT_ATTEMPTS: usize = 40;
const IPC_RETRY_DELAY: Duration = Duration::from_millis(250);
static ENDPOINT_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExternalPlayerProfile {
    id: String,
    name: String,
    executable_path: String,
    argument_template: String,
    #[serde(default = "default_profile_platform")]
    platform: String,
}

#[derive(Debug)]
enum PlaybackMonitorKind {
    Mpv {
        endpoint: String,
    },
    #[cfg(target_os = "windows")]
    PotPlayer,
}

#[derive(Debug, PartialEq)]
enum MpvPlaybackEvent {
    Path(PathBuf),
    Percent(f64),
}

trait AsyncIpcStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T> AsyncIpcStream for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

/// 探测当前平台全部外部播放器配置。
pub(crate) fn detect_players(
    settings: &AppSettings,
    overrides: Option<Vec<Value>>,
) -> Result<PlayerDetectionResult, String> {
    let profiles = parse_profiles(settings, overrides)?;
    let candidates = profiles
        .iter()
        .filter(|profile| supports_current_platform(profile))
        .map(detect_profile)
        .collect::<Vec<_>>();
    let detected = candidates.iter().find(|candidate| candidate.available);
    let result = PlayerDetectionResult {
        platform: runtime_platform(),
        detected_profile_id: detected.map(|candidate| candidate.profile_id.clone()),
        detected_executable_path: detected.and_then(|candidate| candidate.resolved_path.clone()),
        candidates,
    };
    log::info!(
        "Tauri 外部播放器探测完成 platform={:?} candidates={} detected={:?}",
        result.platform,
        result.candidates.len(),
        result.detected_profile_id
    );
    Ok(result)
}

/// 打开原生文件选择器并返回播放器可执行文件。
pub(crate) async fn select_player_executable(
    app: AppHandle,
    input: SelectPlayerExecutableInput,
) -> Result<Option<String>, String> {
    validate_identifier(&input.profile_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app
            .dialog()
            .file()
            .set_title(format!("选择 {} 播放器程序", input.profile_id));
        if let Some(parent) = input
            .current_path
            .as_deref()
            .map(Path::new)
            .and_then(Path::parent)
            .filter(|path| path.is_dir())
        {
            dialog = dialog.set_directory(parent);
        }
        #[cfg(target_os = "windows")]
        {
            dialog = dialog.add_filter("播放器程序", &["exe"]);
        }
        let Some(selected) = dialog.blocking_pick_file() else {
            return Ok(None);
        };
        let path = selected
            .into_path()
            .map_err(|error| format!("播放器选择结果不是本地文件：{error}"))?;
        if !path.is_file() {
            return Err(format!("播放器程序不存在：{}", path.display()));
        }
        let canonical = crate::path_utils::canonicalize(&path)
            .unwrap_or_else(|_| crate::path_utils::simplify(path));
        log::info!(
            "Tauri 播放器程序已选择 profile_id={} path={}",
            input.profile_id,
            canonical.display()
        );
        Ok(Some(canonical.to_string_lossy().into_owned()))
    })
    .await
    .map_err(|error| format!("播放器文件选择任务失败：{error}"))?
}

/// 通过配置的外部播放器启动受控媒体文件。
pub(crate) fn play_media(
    media_state: &AppMediaState,
    settings: &AppSettings,
    file_path: &str,
    profile_id: Option<&str>,
) -> Result<(), String> {
    let file_path = media_state.authorize_media_path(file_path)?;
    let profiles = parse_profiles(settings, None)?;
    if profiles.is_empty() {
        open::that_detached(&file_path).map_err(|error| format!("系统播放器启动失败：{error}"))?;
        log::info!(
            "Tauri 已使用系统默认播放器启动媒体 path={}",
            file_path.display()
        );
        return Ok(());
    }

    let profile = resolve_profile(settings, &profiles, profile_id)?;
    let executable = resolve_executable(&profile)
        .ok_or_else(|| missing_player_message(settings, profile_id, &profile.name))?;
    let mut arguments = parse_player_arguments(&profile.argument_template, &file_path)?;
    let monitor = build_monitor(&profile, &mut arguments);
    spawn_detached(&executable, &arguments).map_err(|error| {
        format!(
            "启动 {} 失败：{error}。请前往“设置 > 播放器配置”检查可执行文件路径。",
            profile.name
        )
    })?;
    log::info!(
        "Tauri 外部播放器已启动 profile_id={} executable={} media={}",
        profile.id,
        executable.display(),
        file_path.display()
    );
    if let Some(monitor) = monitor {
        let media_state = media_state.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = monitor_playback(monitor, media_state, file_path.clone()).await {
                log::warn!(
                    "Tauri 外部播放器进度监控结束 media={} error={error}",
                    file_path.display()
                );
            }
        });
    } else {
        log::info!(
            "Tauri 当前外部播放器未声明进度协议 profile_id={}",
            profile.id
        );
    }
    Ok(())
}

/// 在当前平台文件管理器中定位受控媒体文件。
pub(crate) fn reveal_media(media_state: &AppMediaState, file_path: &str) -> Result<(), String> {
    let file_path = media_state.authorize_media_path(file_path)?;
    #[cfg(target_os = "windows")]
    let result = ProcessCommand::new("explorer.exe")
        .arg(format!("/select,{}", file_path.display()))
        .spawn();
    #[cfg(target_os = "macos")]
    let result = ProcessCommand::new("open")
        .arg("-R")
        .arg(&file_path)
        .spawn();
    #[cfg(target_os = "linux")]
    let result = open::that_detached(
        file_path
            .parent()
            .ok_or_else(|| "媒体文件没有父目录".to_owned())?,
    );
    result.map_err(|error| format!("文件管理器定位媒体失败：{error}"))?;
    log::info!("Tauri 文件管理器已定位媒体 path={}", file_path.display());
    Ok(())
}

/// 从公共设置或 Renderer 临时配置中解析播放器列表。
fn parse_profiles(
    settings: &AppSettings,
    overrides: Option<Vec<Value>>,
) -> Result<Vec<ExternalPlayerProfile>, String> {
    let value = overrides
        .map(Value::Array)
        .or_else(|| settings.pointer("/players").cloned())
        .unwrap_or_else(|| Value::Array(Vec::new()));
    serde_json::from_value(value).map_err(|error| format!("播放器配置格式无效：{error}"))
}

/// 按自动选择或指定标识解析当前可用播放器。
fn resolve_profile(
    settings: &AppSettings,
    profiles: &[ExternalPlayerProfile],
    requested: Option<&str>,
) -> Result<ExternalPlayerProfile, String> {
    let target = requested
        .or_else(|| {
            settings
                .pointer("/defaultPlayerProfileId")
                .and_then(Value::as_str)
        })
        .unwrap_or("auto");
    let mut candidates = profiles
        .iter()
        .filter(|profile| supports_current_platform(profile));
    let selected = if target == "auto" {
        candidates.find(|profile| resolve_executable(profile).is_some())
    } else {
        candidates.find(|profile| profile.id == target)
    };
    selected.cloned().ok_or_else(|| {
        let name = profiles
            .iter()
            .find(|profile| profile.id == target)
            .map(|profile| profile.name.as_str())
            .unwrap_or(target);
        missing_player_message(settings, requested, name)
    })
}

/// 返回未找到播放器时的稳定设置提示。
fn missing_player_message(_settings: &AppSettings, requested: Option<&str>, name: &str) -> String {
    let target = requested.map_or("可用播放器", |_| name);
    format!("未找到{target}，请前往“设置 > 播放器配置”选择播放器或设置可执行文件路径。")
}

/// 探测单条播放器配置的实际文件路径。
fn detect_profile(profile: &ExternalPlayerProfile) -> PlayerDetectionCandidate {
    let resolved = resolve_executable(profile);
    PlayerDetectionCandidate {
        profile_id: profile.id.clone(),
        name: profile.name.clone(),
        configured_path: profile.executable_path.clone(),
        available: resolved.is_some(),
        resolved_path: resolved.map(|path| path.to_string_lossy().into_owned()),
    }
}

/// 按用户路径、平台已知路径和 PATH 环境变量解析播放器程序。
fn resolve_executable(profile: &ExternalPlayerProfile) -> Option<PathBuf> {
    let mut candidates = executable_candidates(&profile.executable_path);
    #[cfg(target_os = "windows")]
    candidates.extend(known_windows_paths(&profile.id));
    if profile.id.eq_ignore_ascii_case("mpv") {
        candidates.extend(executable_candidates("mpv"));
    }
    unique_paths(candidates)
        .into_iter()
        .find(|path| path.is_file())
        .map(|path| {
            crate::path_utils::canonicalize(&path)
                .unwrap_or_else(|_| crate::path_utils::simplify(path))
        })
}

/// 将绝对路径或命令名展开为可验证的文件候选。
fn executable_candidates(configured: &str) -> Vec<PathBuf> {
    let configured = configured.trim().trim_matches('"');
    if configured.is_empty() {
        return Vec::new();
    }
    let configured_path = PathBuf::from(configured);
    if configured_path.is_absolute() {
        return vec![configured_path];
    }
    #[cfg(target_os = "windows")]
    let names = if configured_path.extension().is_none() {
        vec![configured_path, PathBuf::from(format!("{configured}.exe"))]
    } else {
        vec![configured_path]
    };
    #[cfg(not(target_os = "windows"))]
    let names = [configured_path];
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .flat_map(|directory| names.iter().map(move |name| directory.join(name)))
        .collect()
}

#[cfg(target_os = "windows")]
/// 返回 Windows 常用播放器的稳定安装路径。
fn known_windows_paths(profile_id: &str) -> Vec<PathBuf> {
    match profile_id.to_ascii_lowercase().as_str() {
        "pure-codec-potplayer" => {
            vec![PathBuf::from(
                r"C:\Program Files\Pure Codec\x64\PotPlayerMini64.exe",
            )]
        }
        "potplayer" => vec![PathBuf::from(
            r"C:\Program Files\DAUM\PotPlayer\PotPlayerMini64.exe",
        )],
        "mpv" => vec![PathBuf::from(r"C:\Program Files\mpv\mpv.exe")],
        _ => Vec::new(),
    }
}

/// 根据播放器类型添加进度协议启动参数。
fn build_monitor(
    profile: &ExternalPlayerProfile,
    arguments: &mut Vec<String>,
) -> Option<PlaybackMonitorKind> {
    let executable_name = Path::new(&profile.executable_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if profile.id.eq_ignore_ascii_case("iina") || executable_name == "iina-cli" {
        let endpoint = create_playback_endpoint();
        arguments.insert(0, format!("--mpv-input-ipc-server={endpoint}"));
        if !arguments
            .iter()
            .any(|argument| matches!(argument.as_str(), "--stdin" | "--no-stdin"))
        {
            arguments.insert(1, "--no-stdin".to_owned());
        }
        return Some(PlaybackMonitorKind::Mpv { endpoint });
    }
    if profile.id.eq_ignore_ascii_case("mpv")
        || matches!(executable_name.as_str(), "mpv" | "mpv.exe")
    {
        let endpoint = create_playback_endpoint();
        arguments.insert(0, format!("--input-ipc-server={endpoint}"));
        return Some(PlaybackMonitorKind::Mpv { endpoint });
    }
    #[cfg(target_os = "windows")]
    if profile.id.to_ascii_lowercase().contains("potplayer")
        || executable_name.contains("potplayer")
    {
        return Some(PlaybackMonitorKind::PotPlayer);
    }
    None
}

/// 将参数模板解析为无需 shell 的进程参数。
fn parse_player_arguments(template: &str, file_path: &Path) -> Result<Vec<String>, String> {
    let rendered = template.replace("{file}", &file_path.to_string_lossy());
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut started = false;
    for character in rendered.chars() {
        match (quote, character) {
            (Some(active), value) if value == active => quote = None,
            (Some(_), value) => current.push(value),
            (None, '"' | '\'') => {
                quote = Some(character);
                started = true;
            }
            (None, value) if value.is_whitespace() => {
                if started {
                    arguments.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, value) => {
                current.push(value);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("播放器参数模板包含未闭合引号".to_owned());
    }
    if started {
        arguments.push(current);
    }
    Ok(arguments)
}

/// 启动并立即分离外部播放器进程。
fn spawn_detached(executable: &Path, arguments: &[String]) -> std::io::Result<()> {
    let mut command = ProcessCommand::new(executable);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0008 | 0x0800_0000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn().map(|_| ())
}

/// 运行播放器特定进度监控，并在达到阈值后停止。
async fn monitor_playback(
    monitor: PlaybackMonitorKind,
    media_state: AppMediaState,
    file_path: PathBuf,
) -> Result<(), String> {
    match monitor {
        PlaybackMonitorKind::Mpv { endpoint } => {
            monitor_mpv(endpoint, media_state, file_path).await
        }
        #[cfg(target_os = "windows")]
        PlaybackMonitorKind::PotPlayer => monitor_potplayer(media_state, file_path).await,
    }
}

/// 连接 mpv/IINA JSON IPC 并观察 percent-pos。
async fn monitor_mpv(
    endpoint: String,
    media_state: AppMediaState,
    file_path: PathBuf,
) -> Result<(), String> {
    let stream = connect_mpv_endpoint(&endpoint).await?;
    let (reader, mut writer) = tokio::io::split(stream);
    writer
        .write_all(
            b"{\"command\":[\"observe_property\",1,\"percent-pos\"]}\n{\"command\":[\"observe_property\",2,\"path\"]}\n",
        )
        .await
        .map_err(|error| format!("订阅 mpv 播放状态失败：{error}"))?;
    let mut lines = BufReader::new(reader).lines();
    let mut current_file_path = file_path;
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|error| format!("读取 mpv 播放进度失败：{error}"))?
    {
        match parse_mpv_playback_event(&line) {
            Some(MpvPlaybackEvent::Path(path)) => {
                log::info!(
                    "Tauri 外部播放器切换媒体 old={} new={}",
                    current_file_path.display(),
                    path.display()
                );
                current_file_path = path;
            }
            Some(MpvPlaybackEvent::Percent(percent)) => {
                match media_state.report_external_playback_progress(&current_file_path, percent) {
                    Ok(true) | Ok(false) => {}
                    Err(error) => log::warn!("Tauri mpv 进度回写失败 error={error}"),
                }
            }
            None => {}
        }
    }
    #[cfg(unix)]
    let _ = tokio::fs::remove_file(endpoint).await;
    Ok(())
}

/// 在 Windows 上通过 GSMTC 轮询 PotPlayer 播放百分比。
#[cfg(target_os = "windows")]
async fn monitor_potplayer(media_state: AppMediaState, file_path: PathBuf) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use tokio::process::Command;

    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            GSMTC_MONITOR_SCRIPT,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command.as_std_mut().creation_flags(0x0800_0000);
    let mut child = command
        .spawn()
        .map_err(|error| format!("启动 PotPlayer GSMTC 监控失败：{error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "PotPlayer GSMTC 监控没有标准输出".to_owned())?;
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|error| format!("读取 PotPlayer GSMTC 进度失败：{error}"))?
    {
        let Some((percent, title)) = parse_potplayer_progress_line(&line) else {
            continue;
        };
        match media_state.report_external_playback_progress_with_title(
            &file_path,
            title.as_deref(),
            percent,
        ) {
            Ok(true) | Ok(false) => {}
            Err(error) => log::warn!("Tauri PotPlayer 进度回写失败 error={error}"),
        }
    }
    let _ = child.kill().await;
    Ok(())
}

/// 有限重试连接播放器创建的 Unix Socket 或 Windows Named Pipe。
async fn connect_mpv_endpoint(endpoint: &str) -> Result<Box<dyn AsyncIpcStream>, String> {
    let mut last_error = None;
    for attempt in 1..=IPC_CONNECT_ATTEMPTS {
        #[cfg(target_os = "windows")]
        let result = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(endpoint)
            .map(|stream| Box::new(stream) as Box<dyn AsyncIpcStream>);
        #[cfg(unix)]
        let result = tokio::net::UnixStream::connect(endpoint)
            .await
            .map(|stream| Box::new(stream) as Box<dyn AsyncIpcStream>);
        match result {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error.to_string()),
        }
        if attempt < IPC_CONNECT_ATTEMPTS {
            tokio::time::sleep(IPC_RETRY_DELAY).await;
        }
    }
    Err(format!(
        "播放器进度 IPC 连接失败：{}",
        last_error.unwrap_or_else(|| "未知错误".to_owned())
    ))
}

/// 解析 mpv property-change 或 GSMTC JSON 行中的播放百分比。
fn parse_progress_line(line: &str, mpv: bool) -> Option<f64> {
    let value: Value = serde_json::from_str(line).ok()?;
    if mpv
        && (value.get("event")?.as_str()? != "property-change"
            || value.get("name")?.as_str()? != "percent-pos")
    {
        return None;
    }
    let percent = value.get(if mpv { "data" } else { "percent" })?.as_f64()?;
    percent.is_finite().then(|| percent.clamp(0.0, 100.0))
}

/// 解析 mpv 当前文件或播放百分比事件。
fn parse_mpv_playback_event(line: &str) -> Option<MpvPlaybackEvent> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("event")?.as_str()? != "property-change" {
        return None;
    }
    match value.get("name")?.as_str()? {
        "path" => parse_local_media_path(value.get("data")?.as_str()?).map(MpvPlaybackEvent::Path),
        "percent-pos" => parse_progress_line(line, true).map(MpvPlaybackEvent::Percent),
        _ => None,
    }
}

/// 将 mpv 的本地路径或 file URI 转换为本地路径。
fn parse_local_media_path(value: &str) -> Option<PathBuf> {
    if value.trim().is_empty() {
        return None;
    }
    if value.starts_with("file://") {
        return url::Url::parse(value).ok()?.to_file_path().ok();
    }
    if value.contains("://") {
        return None;
    }
    Some(PathBuf::from(value))
}

/// 解析 PotPlayer GSMTC 的百分比与当前媒体标题。
#[cfg(any(target_os = "windows", test))]
fn parse_potplayer_progress_line(line: &str) -> Option<(f64, Option<String>)> {
    let value: Value = serde_json::from_str(line).ok()?;
    let percent = value
        .get("percent")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())?
        .clamp(0.0, 100.0);
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    Some((percent, title))
}

/// 创建长度受控的本地播放器 IPC 地址。
fn create_playback_endpoint() -> String {
    let sequence = ENDPOINT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let id = format!("{}-{sequence}", std::process::id());
    if cfg!(target_os = "windows") {
        format!(r"\\.\pipe\ani-playback-{id}")
    } else {
        std::env::temp_dir()
            .join(format!("ani-playback-{id}.sock"))
            .to_string_lossy()
            .into_owned()
    }
}

/// 判断播放器配置是否适用于当前平台。
fn supports_current_platform(profile: &ExternalPlayerProfile) -> bool {
    profile.platform == "any"
        || profile.platform
            == match runtime_platform() {
                PlayerRuntimePlatform::Windows => "windows",
                PlayerRuntimePlatform::Macos => "macos",
                PlayerRuntimePlatform::Linux => "linux",
                PlayerRuntimePlatform::Other => "other",
            }
}

/// 返回当前 Rust 编译目标对应的平台枚举。
fn runtime_platform() -> PlayerRuntimePlatform {
    if cfg!(target_os = "windows") {
        PlayerRuntimePlatform::Windows
    } else if cfg!(target_os = "macos") {
        PlayerRuntimePlatform::Macos
    } else if cfg!(target_os = "linux") {
        PlayerRuntimePlatform::Linux
    } else {
        PlayerRuntimePlatform::Other
    }
}

/// 保留候选优先级并移除重复路径。
fn unique_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.to_string_lossy().to_ascii_lowercase()))
        .collect()
}

/// 校验 Renderer 提交的播放器配置标识。
fn validate_identifier(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err("播放器配置标识无效".to_owned());
    }
    Ok(())
}

fn default_profile_platform() -> String {
    "any".to_owned()
}

#[cfg(target_os = "windows")]
const GSMTC_MONITOR_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Runtime.WindowsRuntime
$asTaskMethod = [System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object { $_.Name -eq 'AsTask' -and $_.IsGenericMethod -and $_.GetParameters().Count -eq 1 } | Select-Object -First 1
function Await-WinRt($operation, [Type]$resultType) {
  $task = $asTaskMethod.MakeGenericMethod($resultType).Invoke($null, @($operation))
  $task.Wait()
  return $task.Result
}
[Windows.Media.Control.GlobalSystemMediaTransportControlsSessionManager, Windows.Media.Control, ContentType = WindowsRuntime] | Out-Null
$manager = Await-WinRt ([Windows.Media.Control.GlobalSystemMediaTransportControlsSessionManager]::RequestAsync()) ([Windows.Media.Control.GlobalSystemMediaTransportControlsSessionManager])
$seenSession = $false
$missingCount = 0
for ($attempt = 0; $attempt -lt 14400; $attempt++) {
  $session = @($manager.GetSessions()) | Where-Object { $_.SourceAppUserModelId -match '(?i)potplayer' } | Select-Object -First 1
  if ($null -eq $session) {
    if ($seenSession) { $missingCount++; if ($missingCount -ge 5) { break } }
    Start-Sleep -Seconds 2
    continue
  }
  $seenSession = $true
  $missingCount = 0
  $timeline = $session.GetTimelineProperties()
  $duration = ($timeline.EndTime - $timeline.StartTime).TotalSeconds
  if ($duration -gt 0) {
    $position = ($timeline.Position - $timeline.StartTime).TotalSeconds
    $percent = [Math]::Max(0, [Math]::Min(100, $position / $duration * 100))
    $title = $null
    try {
      $properties = Await-WinRt ($session.TryGetMediaPropertiesAsync()) ([Windows.Media.Control.GlobalSystemMediaTransportControlsSessionMediaProperties])
      $title = $properties.Title
    } catch {}
    $payload = @{ percent = $percent }
    if (-not [String]::IsNullOrWhiteSpace($title)) { $payload.title = $title }
    [Console]::Out.WriteLine(($payload | ConvertTo-Json -Compress))
  }
  Start-Sleep -Seconds 2
}
"#;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// 验证参数模板不会经过 shell 且保留带空格文件路径。
    #[test]
    fn parses_player_argument_template() {
        let arguments = parse_player_arguments(
            "--force-window=yes \"{file}\" --title 'Ani Tracker'",
            Path::new("C:/Anime/Test Episode.mkv"),
        )
        .expect("parse player arguments");
        assert_eq!(
            arguments,
            [
                "--force-window=yes",
                "C:/Anime/Test Episode.mkv",
                "--title",
                "Ani Tracker"
            ]
        );
    }

    /// 验证播放器配置按当前平台过滤并返回稳定结构。
    #[test]
    fn detects_current_platform_profiles() {
        let settings = json!({
            "players": [
                {
                    "id": "missing",
                    "name": "Missing Player",
                    "executablePath": "definitely-missing-player-command",
                    "argumentTemplate": "\"{file}\"",
                    "platform": "any"
                },
                {
                    "id": "other-platform",
                    "name": "Other Platform",
                    "executablePath": "missing",
                    "argumentTemplate": "\"{file}\"",
                    "platform": "other"
                }
            ]
        });
        let result = detect_players(&settings, None).expect("detect players");
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].profile_id, "missing");
        assert!(!result.candidates[0].available);
    }

    /// 验证 mpv 与 GSMTC 进度事件使用同一百分比范围。
    #[test]
    fn parses_external_player_progress() {
        assert_eq!(
            parse_progress_line(
                r#"{"event":"property-change","name":"percent-pos","data":92.5}"#,
                true
            ),
            Some(92.5)
        );
        assert_eq!(
            parse_progress_line(r#"{"percent":105}"#, false),
            Some(100.0)
        );
    }

    /// 验证 mpv 切集事件更新文件目标，并兼容 file URI。
    #[test]
    fn parses_mpv_media_switch_events() {
        let media_path = std::env::temp_dir().join("Episode 02.mkv");
        let media_uri = url::Url::from_file_path(&media_path)
            .expect("build media file URI")
            .to_string();
        assert_eq!(
            parse_mpv_playback_event(&format!(
                r#"{{"event":"property-change","name":"path","data":{}}}"#,
                serde_json::to_string(&media_uri).expect("encode media file URI")
            )),
            Some(MpvPlaybackEvent::Path(media_path))
        );
        assert_eq!(
            parse_mpv_playback_event(
                r#"{"event":"property-change","name":"percent-pos","data":92.5}"#
            ),
            Some(MpvPlaybackEvent::Percent(92.5))
        );
        assert_eq!(
            parse_mpv_playback_event(
                r#"{"event":"property-change","name":"path","data":"https://example.test/episode.mkv"}"#
            ),
            None
        );
    }

    /// 验证 PotPlayer 输出当前媒体标题，并保留百分比边界归一化。
    #[test]
    fn parses_potplayer_progress_with_title() {
        assert_eq!(
            parse_potplayer_progress_line(r#"{"percent":105,"title":"Episode 02.mkv"}"#),
            Some((100.0, Some("Episode 02.mkv".to_owned())))
        );
        assert_eq!(
            parse_potplayer_progress_line(r#"{"percent":91.25}"#),
            Some((91.25, None))
        );
    }
}
