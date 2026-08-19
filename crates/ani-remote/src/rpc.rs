use async_trait::async_trait;
use serde_json::{json, Map, Value};

const HIDDEN_LOCAL_PATH: &str = "本机路径已隐藏";
pub const REMOTE_SECRET_PLACEHOLDER: &str = "********";

#[derive(Clone, Copy)]
struct MethodDefinition {
    name: &'static str,
    scope: &'static str,
    effect: RpcEffect,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RpcEffect {
    Read,
    Write,
}

const METHODS: &[MethodDefinition] = &[
    method("getDashboard", "dashboard.read", RpcEffect::Read),
    method("listNotifications", "notifications.read", RpcEffect::Read),
    method(
        "getUnreadNotificationCount",
        "notifications.read",
        RpcEffect::Read,
    ),
    method(
        "markNotificationRead",
        "notifications.write",
        RpcEffect::Write,
    ),
    method(
        "markAllNotificationsRead",
        "notifications.write",
        RpcEffect::Write,
    ),
    method("listMyAnime", "library.read", RpcEffect::Read),
    method("upsertMyAnime", "library.write", RpcEffect::Write),
    method("followBangumiAnime", "library.write", RpcEffect::Write),
    method("removeMyAnime", "library.write", RpcEffect::Write),
    method("listMyAnimeWatchProgress", "library.read", RpcEffect::Read),
    method("setAnimeWatchProgress", "library.write", RpcEffect::Write),
    method("reportPlaybackProgress", "library.write", RpcEffect::Write),
    method("savePlaybackCheckpoint", "library.write", RpcEffect::Write),
    method("listAnimeCatalog", "catalog.read", RpcEffect::Read),
    method("getAnimeDetail", "catalog.read", RpcEffect::Read),
    method("searchAnimeCatalog", "catalog.read", RpcEffect::Read),
    method("browseBangumiAnime", "catalog.read", RpcEffect::Read),
    method("listFansubs", "library.read", RpcEffect::Read),
    method("listEpisodes", "library.read", RpcEffect::Read),
    method("upsertEpisode", "library.write", RpcEffect::Write),
    method("listEpisodePreferences", "library.read", RpcEffect::Read),
    method("upsertEpisodePreference", "library.write", RpcEffect::Write),
    method("removeEpisodePreference", "library.write", RpcEffect::Write),
    method("previewEpisodeReleases", "sources.read", RpcEffect::Read),
    method("searchReleases", "sources.read", RpcEffect::Read),
    method("searchAnimeReleases", "sources.read", RpcEffect::Read),
    method(
        "searchRssSubscriptionReleases",
        "sources.read",
        RpcEffect::Read,
    ),
    method(
        "getAnimeSourceBindingState",
        "sources.read",
        RpcEffect::Read,
    ),
    method(
        "confirmAnimeSourceBinding",
        "sources.write",
        RpcEffect::Write,
    ),
    method(
        "reportAnimeSourceCandidateMismatch",
        "sources.write",
        RpcEffect::Write,
    ),
    method(
        "removeAnimeSourceCandidateMismatch",
        "sources.write",
        RpcEffect::Write,
    ),
    method("setAnimeSourceExcluded", "sources.write", RpcEffect::Write),
    method(
        "removeAnimeSourceBinding",
        "sources.write",
        RpcEffect::Write,
    ),
    method("listDownloads", "downloads.read", RpcEffect::Read),
    method("refreshDownloads", "downloads.control", RpcEffect::Write),
    method("pauseDownload", "downloads.control", RpcEffect::Write),
    method("resumeDownload", "downloads.control", RpcEffect::Write),
    method("removeDownload", "downloads.control", RpcEffect::Write),
    method(
        "setDownloadFilePriority",
        "downloads.control",
        RpcEffect::Write,
    ),
    method("addDownloadUrl", "downloads.control", RpcEffect::Write),
    method("addReleaseDownload", "downloads.control", RpcEffect::Write),
    method("listSources", "sources.read", RpcEffect::Read),
    method("setSourceEnabled", "sources.write", RpcEffect::Write),
    method("upsertSource", "sources.write", RpcEffect::Write),
    method("getSourceSyncStatus", "sources.read", RpcEffect::Read),
    method("getSettings", "settings.read", RpcEffect::Read),
    method("updateSettings", "settings.write", RpcEffect::Write),
    method("testQbittorrent", "host.control", RpcEffect::Read),
    method(
        "getAutomationSchedulerStatus",
        "settings.read",
        RpcEffect::Read,
    ),
    method(
        "getQbittorrentManagedStatus",
        "host.control",
        RpcEffect::Read,
    ),
    method("startQbittorrentManaged", "host.control", RpcEffect::Write),
    method("stopQbittorrentManaged", "host.control", RpcEffect::Write),
    method("getEmbeddedTorrentStatus", "host.control", RpcEffect::Read),
    method("startEmbeddedTorrent", "host.control", RpcEffect::Write),
    method("stopEmbeddedTorrent", "host.control", RpcEffect::Write),
    method("restartEmbeddedTorrent", "host.control", RpcEffect::Write),
];

const fn method(name: &'static str, scope: &'static str, effect: RpcEffect) -> MethodDefinition {
    MethodDefinition {
        name,
        scope,
        effect,
    }
}

/// Tauri 宿主对远程核心开放的显式业务调用端口。
#[async_trait]
pub trait RemoteRpcHandler: Send + Sync {
    /// 执行已完成协议校验的方法并返回可序列化结果。
    async fn call(&self, method: &str, args: Vec<Value>) -> Result<Value, String>;
}

/// 远程 RPC 的稳定协议错误。
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct RemoteRpcError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

impl RemoteRpcError {
    fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

/// 对固定方法执行请求校验、scope 授权、调用与返回值脱敏。
pub struct RemoteRpcService {
    handler: std::sync::Arc<dyn RemoteRpcHandler>,
}

impl RemoteRpcService {
    /// 使用显式业务端口创建 RPC 服务。
    pub fn new(handler: std::sync::Arc<dyn RemoteRpcHandler>) -> Self {
        Self { handler }
    }

    /// 返回方法的读写效果，供 HTTP 层应用不同限流。
    pub(crate) fn is_write_method(&self, request: &Value) -> bool {
        request
            .get("method")
            .and_then(Value::as_str)
            .and_then(find_method)
            .is_some_and(|definition| definition.effect == RpcEffect::Write)
    }

    /// 分发一个 JSON RPC 请求，并返回完成字段脱敏的结果。
    pub async fn dispatch(
        &self,
        request: Value,
        granted_scopes: &[String],
    ) -> Result<Value, RemoteRpcError> {
        let object = request
            .as_object()
            .ok_or_else(|| RemoteRpcError::new(400, "INVALID_REQUEST", "远程请求格式无效"))?;
        if object.keys().any(|key| key != "method" && key != "args") {
            return Err(RemoteRpcError::new(
                400,
                "INVALID_REQUEST",
                "远程请求包含未知字段",
            ));
        }
        let method_name = object
            .get("method")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty() && name.len() <= 80)
            .ok_or_else(|| RemoteRpcError::new(400, "INVALID_REQUEST", "远程方法名格式无效"))?;
        let definition = find_method(method_name)
            .ok_or_else(|| RemoteRpcError::new(404, "METHOD_NOT_FOUND", "远程方法不存在"))?;
        if !granted_scopes.iter().any(|scope| scope == definition.scope) {
            return Err(RemoteRpcError::new(
                403,
                "FORBIDDEN",
                "设备未获得此操作权限",
            ));
        }
        let args = match object.get("args") {
            None => Vec::new(),
            Some(Value::Array(args)) if args.len() <= 4 => args.clone(),
            _ => {
                return Err(RemoteRpcError::new(
                    400,
                    "INVALID_REQUEST",
                    "远程参数必须是最多四项的数组",
                ))
            }
        };
        let args = validate_args(method_name, args)?;
        let result = self
            .handler
            .call(method_name, args)
            .await
            .map_err(|error| {
                log::error!("Rust 远程 RPC 调用失败 method={method_name} error={error}");
                RemoteRpcError::new(500, "HANDLER_FAILED", "远程操作执行失败")
            })?;
        if definition.effect == RpcEffect::Write {
            log::info!("Rust 远程 RPC 写操作完成 method={method_name}");
        }
        sanitize_result(method_name, result)
    }
}

fn find_method(name: &str) -> Option<&'static MethodDefinition> {
    METHODS.iter().find(|definition| definition.name == name)
}

fn validate_args(method: &str, args: Vec<Value>) -> Result<Vec<Value>, RemoteRpcError> {
    match method {
        "getDashboard"
        | "listNotifications"
        | "getUnreadNotificationCount"
        | "markAllNotificationsRead"
        | "listMyAnime"
        | "listMyAnimeWatchProgress"
        | "listDownloads"
        | "refreshDownloads"
        | "listSources"
        | "getSourceSyncStatus"
        | "getSettings"
        | "testQbittorrent"
        | "getAutomationSchedulerStatus"
        | "getQbittorrentManagedStatus"
        | "startQbittorrentManaged"
        | "stopQbittorrentManaged"
        | "getEmbeddedTorrentStatus"
        | "startEmbeddedTorrent"
        | "stopEmbeddedTorrent"
        | "restartEmbeddedTorrent" => require_count(args, 0),
        "markNotificationRead"
        | "getAnimeDetail"
        | "listEpisodes"
        | "listEpisodePreferences"
        | "pauseDownload"
        | "resumeDownload"
        | "removeMyAnime"
        | "removeEpisodePreference" => {
            let args = require_count(args, 1)?;
            parse_id(&args[0], "标识")?;
            Ok(args)
        }
        "listFansubs" => {
            if args.is_empty() {
                return Ok(args);
            }
            let args = require_count(args, 1)?;
            if !args[0].is_null() {
                parse_id(&args[0], "番剧标识")?;
            }
            Ok(args)
        }
        "searchAnimeCatalog" => {
            if !(1..=2).contains(&args.len()) {
                return Err(invalid_args("新番搜索参数数量无效"));
            }
            let keyword = args[0]
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty() && value.chars().count() <= 120)
                .filter(|value| !value.chars().any(char::is_control))
                .ok_or_else(|| invalid_args("搜索关键词长度必须为 1-120 个字符"))?;
            if args.len() == 2 && !args[1].is_null() && !args[1].is_boolean() {
                return Err(invalid_args("在线搜索开关必须是布尔值"));
            }
            let mut normalized = vec![Value::String(keyword.to_owned())];
            if let Some(include_online) = args.get(1) {
                normalized.push(include_online.clone());
            }
            Ok(normalized)
        }
        "browseBangumiAnime" => validate_domain_object::<ani_domain::BangumiBrowseQuery>(
            args,
            &["keyword", "sort", "filters", "page", "pageSize"],
            |object| {
                validate_optional_text(object.get("keyword"), "Bangumi 搜索关键词", 120)?;
                validate_number(object.get("page"), "Bangumi 页码", 1.0, 100_000.0)?;
                validate_number(object.get("pageSize"), "Bangumi 每页数量", 1.0, 50.0)?;
                Ok(())
            },
        ),
        "listAnimeCatalog" => validate_year_month(args),
        "upsertMyAnime" | "followBangumiAnime" => validate_domain_object::<ani_domain::MyAnime>(
            args,
            &[
                "id",
                "anime",
                "status",
                "defaultFansubGroupId",
                "autoDownload",
                "rssSubscriptions",
                "preferredResolution",
                "preferredCodec",
                "preferredBitDepth",
                "preferredSubtitleLanguages",
                "preferredSubtitle",
                "addedAt",
                "updatedAt",
            ],
            |object| {
                parse_id(object.get("id").unwrap_or(&Value::Null), "追番标识")?;
                let anime = object
                    .get("anime")
                    .and_then(Value::as_object)
                    .ok_or_else(|| invalid_args("番剧信息格式无效"))?;
                parse_id(anime.get("id").unwrap_or(&Value::Null), "番剧标识")?;
                validate_text(anime.get("title"), "番剧名称", 1, 300)?;
                if let Some(Value::Array(subscriptions)) = object.get("rssSubscriptions") {
                    if subscriptions.len() > 32 {
                        return Err(invalid_args("单部追番最多允许 32 条 RSS 订阅"));
                    }
                    for subscription in subscriptions {
                        let subscription = subscription
                            .as_object()
                            .ok_or_else(|| invalid_args("RSS 订阅格式无效"))?;
                        parse_id(
                            subscription.get("id").unwrap_or(&Value::Null),
                            "RSS 订阅标识",
                        )?;
                        validate_http_url(subscription.get("url"), "RSS 地址")?;
                    }
                }
                Ok(())
            },
        ),
        "upsertEpisode" => validate_domain_object::<ani_domain::Episode>(
            args,
            &["id", "animeId", "episodeNo", "title", "airTime", "status"],
            |object| {
                parse_id(object.get("id").unwrap_or(&Value::Null), "单集标识")?;
                parse_id(object.get("animeId").unwrap_or(&Value::Null), "番剧标识")?;
                validate_number(object.get("episodeNo"), "集数", 0.0, 100_000.0)?;
                Ok(())
            },
        ),
        "upsertEpisodePreference" => validate_domain_object::<ani_domain::EpisodePreference>(
            args,
            &[
                "id",
                "animeId",
                "episodeId",
                "fansubGroupId",
                "releaseId",
                "isManualOverride",
            ],
            |object| {
                for (key, label) in [
                    ("id", "偏好标识"),
                    ("animeId", "番剧标识"),
                    ("episodeId", "单集标识"),
                ] {
                    parse_id(object.get(key).unwrap_or(&Value::Null), label)?;
                }
                Ok(())
            },
        ),
        "previewEpisodeReleases" | "removeAnimeSourceBinding" => {
            let args = require_count(args, 2)?;
            parse_id(&args[0], "番剧标识")?;
            parse_id(&args[1], "关联标识")?;
            Ok(args)
        }
        "getAnimeSourceBindingState" => {
            if !(1..=2).contains(&args.len()) {
                return Err(invalid_args("来源绑定查询参数数量无效"));
            }
            parse_id(&args[0], "番剧标识")?;
            if args.len() == 2 && !args[1].is_null() && !args[1].is_boolean() {
                return Err(invalid_args("候选发现开关必须是布尔值"));
            }
            Ok(args)
        }
        "searchReleases" => validate_domain_object::<ani_domain::ReleaseQuery>(
            args,
            &[
                "keyword",
                "animeId",
                "episodeNo",
                "fansubGroupId",
                "preferredResolution",
                "limit",
                "cacheTtlMs",
                "forceRefresh",
            ],
            validate_release_query,
        ),
        "searchAnimeReleases" => validate_domain_object::<ani_domain::AnimeReleaseQuery>(
            args,
            &[
                "animeId",
                "episodeNo",
                "fansubGroupId",
                "preferredResolution",
                "limit",
                "cacheTtlMs",
                "forceRefresh",
            ],
            |object| {
                parse_id(object.get("animeId").unwrap_or(&Value::Null), "番剧标识")?;
                validate_search_limits(object)
            },
        ),
        "searchRssSubscriptionReleases" => {
            validate_domain_object::<ani_domain::RssSubscriptionReleaseQuery>(
                args,
                &["animeId", "subscriptionId", "preferredResolution", "limit"],
                |object| {
                    parse_id(object.get("animeId").unwrap_or(&Value::Null), "番剧标识")?;
                    parse_id(
                        object.get("subscriptionId").unwrap_or(&Value::Null),
                        "RSS 订阅标识",
                    )?;
                    validate_limit(object.get("limit"))
                },
            )
        }
        "confirmAnimeSourceBinding" => {
            validate_domain_object::<ani_domain::ConfirmAnimeSourceBindingInput>(
                args,
                &[
                    "animeId",
                    "sourceId",
                    "sourceAnimeId",
                    "sourceAnimeTitle",
                    "sourceUrl",
                    "confidence",
                ],
                |object| validate_binding_ids(object, &["animeId", "sourceId", "sourceAnimeId"]),
            )
        }
        "reportAnimeSourceCandidateMismatch" => {
            validate_domain_object::<ani_domain::ReportAnimeSourceCandidateMismatchInput>(
                args,
                &[
                    "animeId",
                    "sourceId",
                    "sourceAnimeId",
                    "sourceAnimeTitle",
                    "score",
                    "reasons",
                ],
                |object| validate_binding_ids(object, &["animeId", "sourceId", "sourceAnimeId"]),
            )
        }
        "removeAnimeSourceCandidateMismatch" => {
            validate_domain_object::<ani_domain::RemoveAnimeSourceCandidateMismatchInput>(
                args,
                &["animeId", "sourceId", "sourceAnimeId"],
                |object| validate_binding_ids(object, &["animeId", "sourceId", "sourceAnimeId"]),
            )
        }
        "setAnimeSourceExcluded" => {
            validate_domain_object::<ani_domain::SetAnimeSourceExclusionInput>(
                args,
                &["animeId", "sourceId", "excluded"],
                |object| validate_binding_ids(object, &["animeId", "sourceId"]),
            )
        }
        "removeDownload" => {
            let args = require_count(args, 2)?;
            parse_id(&args[0], "下载任务标识")?;
            if args[1].as_bool().is_none() {
                return Err(invalid_args("删除文件参数必须是布尔值"));
            }
            Ok(args)
        }
        "setDownloadFilePriority" => {
            let args = require_count(args, 3)?;
            parse_id(&args[0], "下载任务标识")?;
            let indexes = args[1]
                .as_array()
                .filter(|items| !items.is_empty() && items.len() <= 2_048)
                .ok_or_else(|| invalid_args("文件索引列表格式无效"))?;
            if indexes
                .iter()
                .any(|value| value.as_i64().is_none_or(|index| index < 0))
            {
                return Err(invalid_args("文件索引必须是非负整数"));
            }
            if args[2]
                .as_i64()
                .is_none_or(|priority| !(0..=7).contains(&priority))
            {
                return Err(invalid_args("文件优先级必须是 0 到 7 的整数"));
            }
            Ok(args)
        }
        "addDownloadUrl" => validate_object_input(args, &["url", "name", "paused"], |object| {
            validate_download_url(object.get("url"))?;
            validate_optional_text(object.get("name"), "任务名称", 300)?;
            validate_optional_bool(object.get("paused"), "暂停状态")
        }),
        "addReleaseDownload" => validate_object_input(
            args,
            &[
                "release",
                "animeId",
                "episodeId",
                "episodeNo",
                "fansubGroupId",
                "paused",
                "confirmUnknownSeason",
            ],
            validate_add_release_download,
        ),
        "setSourceEnabled" => {
            let args = require_count(args, 2)?;
            parse_id(&args[0], "下载源标识")?;
            if !args[1].is_boolean() {
                return Err(invalid_args("下载源启用状态必须是布尔值"));
            }
            Ok(args)
        }
        "upsertSource" => validate_domain_object::<ani_domain::ReleaseSourceConfig>(
            args,
            &[
                "id",
                "name",
                "kind",
                "enabled",
                "useProxy",
                "requestIntervalMs",
                "baseUrl",
                "apiKey",
                "rssUrl",
                "tags",
            ],
            validate_source,
        ),
        "updateSettings" => validate_object_input(
            args,
            &["download", "automation", "sourceSync", "network"],
            validate_settings_patch,
        ),
        "setAnimeWatchProgress" => {
            validate_object_input(args, &["animeId", "watchedEpisodeCount"], |object| {
                parse_id(object.get("animeId").unwrap_or(&Value::Null), "番剧标识")?;
                let count = object
                    .get("watchedEpisodeCount")
                    .and_then(Value::as_i64)
                    .filter(|value| (0..=10_000).contains(value))
                    .ok_or_else(|| invalid_args("观看进度必须是 0 到 10000 之间的整数"))?;
                if object.get("watchedEpisodeCount").and_then(Value::as_f64) != Some(count as f64) {
                    return Err(invalid_args("观看进度必须是整数"));
                }
                Ok(())
            })
        }
        "reportPlaybackProgress" => {
            validate_object_input(args, &["taskId", "fileIndex", "percent"], |object| {
                parse_id(object.get("taskId").unwrap_or(&Value::Null), "下载任务标识")?;
                validate_optional_file_index(object.get("fileIndex"))?;
                object
                    .get("percent")
                    .and_then(Value::as_f64)
                    .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
                    .ok_or_else(|| invalid_args("播放进度必须是 0 到 100 之间的数值"))?;
                Ok(())
            })
        }
        "savePlaybackCheckpoint" => validate_object_input(
            args,
            &[
                "taskId",
                "fileIndex",
                "positionSeconds",
                "durationSeconds",
                "completed",
            ],
            |object| {
                parse_id(object.get("taskId").unwrap_or(&Value::Null), "下载任务标识")?;
                validate_optional_file_index(object.get("fileIndex"))?;
                for key in ["positionSeconds", "durationSeconds"] {
                    object
                        .get(key)
                        .and_then(Value::as_f64)
                        .filter(|value| value.is_finite() && (0.0..=2_678_400.0).contains(value))
                        .ok_or_else(|| invalid_args("播放位置和时长必须是有效的非负秒数"))?;
                }
                if object
                    .get("completed")
                    .is_some_and(|value| !value.is_boolean())
                {
                    return Err(invalid_args("播放完成状态必须是布尔值"));
                }
                Ok(())
            },
        ),
        _ => Err(invalid_args("远程参数校验失败")),
    }
}

fn validate_domain_object<T: serde::de::DeserializeOwned>(
    args: Vec<Value>,
    allowed_keys: &[&str],
    validate: impl FnOnce(&Map<String, Value>) -> Result<(), RemoteRpcError>,
) -> Result<Vec<Value>, RemoteRpcError> {
    let args = validate_object_input(args, allowed_keys, validate)?;
    serde_json::from_value::<T>(args[0].clone())
        .map_err(|_| invalid_args("远程对象字段类型无效"))?;
    Ok(args)
}

fn validate_text(
    value: Option<&Value>,
    label: &str,
    minimum: usize,
    maximum: usize,
) -> Result<(), RemoteRpcError> {
    let text = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| {
            let length = text.chars().count();
            (minimum..=maximum).contains(&length) && !text.chars().any(char::is_control)
        })
        .ok_or_else(|| invalid_args(format!("{label}长度或格式无效")))?;
    if text.is_empty() && minimum > 0 {
        return Err(invalid_args(format!("{label}不能为空")));
    }
    Ok(())
}

fn validate_optional_text(
    value: Option<&Value>,
    label: &str,
    maximum: usize,
) -> Result<(), RemoteRpcError> {
    match value {
        None | Some(Value::Null) => Ok(()),
        Some(value) => validate_text(Some(value), label, 0, maximum),
    }
}

fn validate_optional_bool(value: Option<&Value>, label: &str) -> Result<(), RemoteRpcError> {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(_)) => Ok(()),
        _ => Err(invalid_args(format!("{label}必须是布尔值"))),
    }
}

fn validate_number(
    value: Option<&Value>,
    label: &str,
    minimum: f64,
    maximum: f64,
) -> Result<(), RemoteRpcError> {
    value
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite() && (minimum..=maximum).contains(number))
        .map(|_| ())
        .ok_or_else(|| invalid_args(format!("{label}范围无效")))
}

fn validate_http_url(value: Option<&Value>, label: &str) -> Result<(), RemoteRpcError> {
    let raw = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 4_096)
        .ok_or_else(|| invalid_args(format!("{label}格式无效")))?;
    let parsed = url::Url::parse(raw).map_err(|_| invalid_args(format!("{label}格式无效")))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(invalid_args(format!("{label}只允许 HTTP(S) 地址")));
    }
    Ok(())
}

fn validate_download_url(value: Option<&Value>) -> Result<(), RemoteRpcError> {
    let raw = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 8_192)
        .ok_or_else(|| invalid_args("下载地址格式无效"))?;
    let parsed = url::Url::parse(raw).map_err(|_| invalid_args("下载地址格式无效"))?;
    if !matches!(parsed.scheme(), "magnet" | "http" | "https") {
        return Err(invalid_args("下载地址只允许 magnet 或 HTTP(S)"));
    }
    Ok(())
}

fn validate_limit(value: Option<&Value>) -> Result<(), RemoteRpcError> {
    match value {
        None | Some(Value::Null) => Ok(()),
        Some(value)
            if value
                .as_u64()
                .is_some_and(|limit| (1..=200).contains(&limit)) =>
        {
            Ok(())
        }
        _ => Err(invalid_args("搜索数量必须是 1 到 200 的整数")),
    }
}

fn validate_search_limits(object: &Map<String, Value>) -> Result<(), RemoteRpcError> {
    validate_limit(object.get("limit"))?;
    if let Some(value) = object.get("episodeNo") {
        validate_number(Some(value), "集数", 0.0, 100_000.0)?;
    }
    if let Some(value) = object.get("cacheTtlMs") {
        value
            .as_u64()
            .filter(|ttl| *ttl <= 31_536_000_000)
            .ok_or_else(|| invalid_args("缓存时长范围无效"))?;
    }
    Ok(())
}

fn validate_release_query(object: &Map<String, Value>) -> Result<(), RemoteRpcError> {
    validate_text(object.get("keyword"), "搜索关键词", 1, 300)?;
    validate_search_limits(object)
}

fn validate_binding_ids(object: &Map<String, Value>, keys: &[&str]) -> Result<(), RemoteRpcError> {
    for key in keys {
        parse_id(object.get(*key).unwrap_or(&Value::Null), "来源绑定标识")?;
    }
    Ok(())
}

fn validate_add_release_download(object: &Map<String, Value>) -> Result<(), RemoteRpcError> {
    let release = object
        .get("release")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_args("资源信息格式无效"))?;
    parse_id(release.get("id").unwrap_or(&Value::Null), "资源标识")?;
    let source = ["magnetUrl", "torrentUrl"]
        .iter()
        .filter_map(|key| release.get(*key))
        .find(|value| value.as_str().is_some_and(|url| !url.trim().is_empty()));
    validate_download_url(source)?;
    for key in ["animeId", "episodeId", "fansubGroupId"] {
        if let Some(value) = object.get(key).filter(|value| !value.is_null()) {
            parse_id(value, "下载关联标识")?;
        }
    }
    if let Some(value) = object.get("episodeNo") {
        validate_number(Some(value), "集数", 0.0, 100_000.0)?;
    }
    validate_optional_bool(object.get("paused"), "暂停状态")?;
    validate_optional_bool(object.get("confirmUnknownSeason"), "季度确认状态")
}

fn validate_source(object: &Map<String, Value>) -> Result<(), RemoteRpcError> {
    parse_id(object.get("id").unwrap_or(&Value::Null), "下载源标识")?;
    validate_text(object.get("name"), "下载源名称", 1, 120)?;
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_args("下载源类型无效"))?;
    match kind {
        "rss" => validate_http_url(object.get("rssUrl"), "RSS 地址")?,
        "torznab" | "site_adapter" => validate_http_url(object.get("baseUrl"), "服务地址")?,
        "manual" => {}
        _ => return Err(invalid_args("下载源类型无效")),
    }
    object
        .get("requestIntervalMs")
        .and_then(Value::as_i64)
        .filter(|interval| (250..=86_400_000).contains(interval))
        .ok_or_else(|| invalid_args("下载源请求间隔范围无效"))?;
    validate_optional_text(object.get("apiKey"), "访问凭据", 2_048)
}

/// 校验远程可修改的完整下载配置边界。
fn validate_download_settings(value: &Value) -> Result<(), RemoteRpcError> {
    let download = validate_nested_object(
        value,
        &[
            "defaultDownloadDir",
            "createAnimeFolder",
            "animeFolderPattern",
            "temporaryDownloadDir",
            "defaultTorrentEngine",
            "allowMeteredDownloads",
            "embedded",
            "qbittorrent",
        ],
        "下载设置",
    )?;
    if download.contains_key("defaultDownloadDir") {
        validate_text(download.get("defaultDownloadDir"), "默认下载目录", 1, 4_096)?;
    }
    if download.contains_key("animeFolderPattern") {
        validate_text(download.get("animeFolderPattern"), "番剧目录模板", 1, 512)?;
    }
    validate_optional_text(download.get("temporaryDownloadDir"), "临时下载目录", 4_096)?;
    validate_optional_bool(download.get("createAnimeFolder"), "创建番剧目录开关")?;
    validate_optional_bool(download.get("allowMeteredDownloads"), "移动网络下载开关")?;
    if let Some(engine) = download.get("defaultTorrentEngine") {
        engine
            .as_str()
            .filter(|value| matches!(*value, "embedded" | "qbittorrent"))
            .ok_or_else(|| invalid_args("默认下载引擎无效"))?;
    }
    if let Some(embedded) = download.get("embedded") {
        validate_embedded_download_settings(embedded)?;
    }
    if let Some(qbittorrent) = download.get("qbittorrent") {
        validate_qbittorrent_settings(qbittorrent)?;
    }
    Ok(())
}

/// 校验内置 torrent-core 的远程配置。
fn validate_embedded_download_settings(value: &Value) -> Result<(), RemoteRpcError> {
    let embedded = validate_nested_object(
        value,
        &[
            "enabled",
            "listenPort",
            "dhtEnabled",
            "upnpEnabled",
            "maxActiveDownloads",
            "maxDownloadSpeed",
            "maxUploadSpeed",
            "seedingLimits",
        ],
        "内置下载核心设置",
    )?;
    for key in ["enabled", "dhtEnabled", "upnpEnabled"] {
        validate_optional_bool(embedded.get(key), "内置下载核心开关")?;
    }
    validate_optional_integer(embedded.get("listenPort"), "监听端口", 1_024, 65_535)?;
    validate_optional_integer(embedded.get("maxActiveDownloads"), "活动下载数", 1, 100)?;
    for key in ["maxDownloadSpeed", "maxUploadSpeed"] {
        validate_optional_integer(embedded.get(key), "内置核心限速", 0, 10_000_000)?;
    }
    if let Some(seeding) = embedded.get("seedingLimits") {
        validate_seeding_limits(seeding)?;
    }
    Ok(())
}

/// 校验外部和托管 qBittorrent 的远程配置。
fn validate_qbittorrent_settings(value: &Value) -> Result<(), RemoteRpcError> {
    let qbittorrent = validate_nested_object(
        value,
        &[
            "baseUrl",
            "username",
            "password",
            "autoConnect",
            "downloadLimitKiBps",
            "uploadLimitKiBps",
            "seedingLimits",
            "managed",
        ],
        "qBittorrent 设置",
    )?;
    if qbittorrent.contains_key("baseUrl") {
        validate_http_url(qbittorrent.get("baseUrl"), "qBittorrent WebUI 地址")?;
    }
    validate_optional_text(qbittorrent.get("username"), "qBittorrent 用户名", 256)?;
    validate_optional_text(qbittorrent.get("password"), "qBittorrent 密码", 2_048)?;
    validate_optional_bool(qbittorrent.get("autoConnect"), "qBittorrent 自动连接开关")?;
    for key in ["downloadLimitKiBps", "uploadLimitKiBps"] {
        validate_optional_integer(qbittorrent.get(key), "qBittorrent 限速", 0, 10_000_000)?;
    }
    if let Some(seeding) = qbittorrent.get("seedingLimits") {
        validate_seeding_limits(seeding)?;
    }
    if let Some(managed) = qbittorrent.get("managed") {
        let managed = validate_nested_object(
            managed,
            &["enabled", "startupTimeoutMs"],
            "托管 qBittorrent 设置",
        )?;
        validate_optional_bool(managed.get("enabled"), "托管 qBittorrent 开关")?;
        validate_optional_integer(
            managed.get("startupTimeoutMs"),
            "托管 qBittorrent 启动超时",
            1_000,
            120_000,
        )?;
    }
    Ok(())
}

/// 校验两种下载引擎共用的做种限制。
fn validate_seeding_limits(value: &Value) -> Result<(), RemoteRpcError> {
    let limits = validate_nested_object(
        value,
        &[
            "enabled",
            "ratioEnabled",
            "ratioLimit",
            "timeEnabled",
            "timeLimitMinutes",
        ],
        "做种限制",
    )?;
    for key in ["enabled", "ratioEnabled", "timeEnabled"] {
        validate_optional_bool(limits.get(key), "做种限制开关")?;
    }
    if let Some(ratio) = limits.get("ratioLimit") {
        validate_number(Some(ratio), "分享率", 0.1, 1_000.0)?;
    }
    validate_optional_integer(limits.get("timeLimitMinutes"), "做种时间", 0, 525_600)
}

/// 校验可选整数是否位于允许范围。
fn validate_optional_integer(
    value: Option<&Value>,
    label: &str,
    minimum: i64,
    maximum: i64,
) -> Result<(), RemoteRpcError> {
    match value {
        None | Some(Value::Null) => Ok(()),
        Some(value)
            if value
                .as_i64()
                .is_some_and(|number| (minimum..=maximum).contains(&number)) =>
        {
            Ok(())
        }
        _ => Err(invalid_args(format!("{label}范围无效"))),
    }
}

fn validate_settings_patch(object: &Map<String, Value>) -> Result<(), RemoteRpcError> {
    if object.is_empty() {
        return Err(invalid_args("设置更新不能为空"));
    }
    if let Some(download) = object.get("download") {
        validate_download_settings(download)?;
    }
    if let Some(automation) = object.get("automation") {
        let automation = validate_nested_object(
            automation,
            &[
                "scheduledCheckEnabled",
                "checkIntervalMinutes",
                "notifyOnNewEpisode",
                "autoDownloadEnabledGlobally",
                "fallbackWhenDefaultFansubMissing",
                "candidateFansubNames",
            ],
            "自动化设置",
        )?;
        for key in [
            "scheduledCheckEnabled",
            "notifyOnNewEpisode",
            "autoDownloadEnabledGlobally",
        ] {
            validate_optional_bool(automation.get(key), "自动化开关")?;
        }
        if let Some(interval) = automation.get("checkIntervalMinutes") {
            interval
                .as_i64()
                .filter(|value| (5..=10_080).contains(value))
                .ok_or_else(|| invalid_args("自动检查间隔必须是 5 到 10080 分钟"))?;
        }
        if let Some(fallback) = automation.get("fallbackWhenDefaultFansubMissing") {
            fallback
                .as_str()
                .filter(|value| matches!(*value, "wait" | "candidate" | "notify_only"))
                .ok_or_else(|| invalid_args("默认字幕组缺失策略无效"))?;
        }
        if let Some(Value::Array(names)) = automation.get("candidateFansubNames") {
            if names.len() > 64 {
                return Err(invalid_args("候选字幕组名称数量过多"));
            }
            for name in names {
                validate_text(Some(name), "候选字幕组名称", 1, 120)?;
            }
        } else if automation.contains_key("candidateFansubNames") {
            return Err(invalid_args("候选字幕组名称格式无效"));
        }
    }
    if let Some(source_sync) = object.get("sourceSync") {
        let source_sync =
            validate_nested_object(source_sync, &["enabled", "dailyTime"], "来源同步设置")?;
        validate_optional_bool(source_sync.get("enabled"), "来源同步开关")?;
        if let Some(time) = source_sync.get("dailyTime") {
            let time = time
                .as_str()
                .filter(|value| is_daily_time(value))
                .ok_or_else(|| invalid_args("每日同步时间格式无效"))?;
            if time.len() != 5 {
                return Err(invalid_args("每日同步时间格式无效"));
            }
        }
    }
    if let Some(network) = object.get("network") {
        let network = validate_nested_object(network, &["metadataProxy"], "网络设置")?;
        let proxy = network
            .get("metadataProxy")
            .ok_or_else(|| invalid_args("元数据代理设置缺失"))?;
        let proxy = validate_nested_object(proxy, &["mode", "url", "timeoutMs"], "元数据代理")?;
        let mode = proxy
            .get("mode")
            .and_then(Value::as_str)
            .filter(|mode| matches!(*mode, "off" | "system" | "manual"))
            .ok_or_else(|| invalid_args("元数据代理模式无效"))?;
        if mode == "manual" {
            validate_proxy_url(proxy.get("url"))?;
        } else {
            validate_optional_text(proxy.get("url"), "代理地址", 2_048)?;
        }
        proxy
            .get("timeoutMs")
            .and_then(Value::as_u64)
            .filter(|timeout| (1_000..=120_000).contains(timeout))
            .ok_or_else(|| invalid_args("代理超时必须是 1 到 120 秒"))?;
    }
    Ok(())
}

fn validate_nested_object<'a>(
    value: &'a Value,
    allowed_keys: &[&str],
    label: &str,
) -> Result<&'a Map<String, Value>, RemoteRpcError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_args(format!("{label}格式无效")))?;
    if object
        .keys()
        .any(|key| !allowed_keys.contains(&key.as_str()))
    {
        return Err(invalid_args(format!("{label}包含不允许的字段")));
    }
    Ok(object)
}

fn validate_proxy_url(value: Option<&Value>) -> Result<(), RemoteRpcError> {
    let raw = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 2_048)
        .ok_or_else(|| invalid_args("手动代理地址格式无效"))?;
    let parsed = url::Url::parse(raw).map_err(|_| invalid_args("手动代理地址格式无效"))?;
    if !matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h") {
        return Err(invalid_args("手动代理协议不受支持"));
    }
    Ok(())
}

fn is_daily_time(value: &str) -> bool {
    let Some((hour, minute)) = value.split_once(':') else {
        return false;
    };
    hour.len() == 2
        && minute.len() == 2
        && hour.parse::<u8>().is_ok_and(|hour| hour < 24)
        && minute.parse::<u8>().is_ok_and(|minute| minute < 60)
}

fn require_count(args: Vec<Value>, expected: usize) -> Result<Vec<Value>, RemoteRpcError> {
    if args.len() != expected {
        return Err(invalid_args(format!("参数数量无效，预期 {expected} 个")));
    }
    Ok(args)
}

fn parse_id<'a>(value: &'a Value, label: &str) -> Result<&'a str, RemoteRpcError> {
    let value = value
        .as_str()
        .map(str::trim)
        .ok_or_else(|| invalid_args(format!("{label}必须是字符串")))?;
    if value.is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
    {
        return Err(invalid_args(format!("{label}格式无效")));
    }
    Ok(value)
}

fn validate_year_month(args: Vec<Value>) -> Result<Vec<Value>, RemoteRpcError> {
    if args.is_empty() {
        return Ok(args);
    }
    let args = require_count(args, 2)?;
    if args.iter().all(Value::is_null) {
        return Ok(Vec::new());
    }
    let year = args[0]
        .as_i64()
        .filter(|value| (1900..=2200).contains(value))
        .ok_or_else(|| invalid_args("年份必须为 1900-2200 的整数"))?;
    let month = args[1]
        .as_i64()
        .filter(|value| (1..=12).contains(value))
        .ok_or_else(|| invalid_args("月份必须为 1-12 的整数"))?;
    Ok(vec![json!(year), json!(month)])
}

fn validate_object_input(
    args: Vec<Value>,
    allowed_keys: &[&str],
    validate: impl FnOnce(&Map<String, Value>) -> Result<(), RemoteRpcError>,
) -> Result<Vec<Value>, RemoteRpcError> {
    let args = require_count(args, 1)?;
    let object = args[0]
        .as_object()
        .ok_or_else(|| invalid_args("远程参数格式无效"))?;
    if object
        .keys()
        .any(|key| !allowed_keys.contains(&key.as_str()))
    {
        return Err(invalid_args("远程参数包含未知字段"));
    }
    validate(object)?;
    Ok(args)
}

fn validate_optional_file_index(value: Option<&Value>) -> Result<(), RemoteRpcError> {
    if let Some(value) = value {
        value
            .as_i64()
            .filter(|value| *value >= 0)
            .ok_or_else(|| invalid_args("播放文件索引必须是非负整数"))?;
    }
    Ok(())
}

fn invalid_args(message: impl Into<String>) -> RemoteRpcError {
    RemoteRpcError::new(400, "INVALID_ARGUMENTS", message)
}

fn sanitize_result(method: &str, mut value: Value) -> Result<Value, RemoteRpcError> {
    match method {
        "listMyAnime" | "upsertMyAnime" | "followBangumiAnime" | "removeMyAnime" => {
            sanitize_array(&mut value, sanitize_my_anime)?
        }
        "listDownloads"
        | "refreshDownloads"
        | "pauseDownload"
        | "resumeDownload"
        | "removeDownload"
        | "setDownloadFilePriority"
        | "addDownloadUrl"
        | "addReleaseDownload" => sanitize_array(&mut value, sanitize_download)?,
        "listSources" | "setSourceEnabled" | "upsertSource" => {
            sanitize_array(&mut value, sanitize_source)?
        }
        "getSettings" | "updateSettings" => sanitize_settings(&mut value)?,
        "getQbittorrentManagedStatus" | "startQbittorrentManaged" | "stopQbittorrentManaged" => {
            sanitize_qbittorrent_status(&mut value)?
        }
        "getEmbeddedTorrentStatus"
        | "startEmbeddedTorrent"
        | "stopEmbeddedTorrent"
        | "restartEmbeddedTorrent" => sanitize_embedded_status(&mut value)?,
        "getDashboard" => {
            let object = require_result_object(&mut value, "首页看板")?;
            if let Some(downloads) = object.get_mut("activeDownloads") {
                sanitize_array(downloads, sanitize_download)?;
            }
            if let Some(media) = object.get_mut("recentCompleted") {
                sanitize_array(media, sanitize_media)?;
            }
        }
        "listNotifications" | "markNotificationRead" | "markAllNotificationsRead" => {
            let items = require_result_array(&mut value, "通知列表")?;
            for item in items {
                let object = item
                    .as_object_mut()
                    .ok_or_else(|| invalid_result("通知记录格式无效"))?;
                for key in ["title", "body"] {
                    if let Some(Value::String(text)) = object.get_mut(key) {
                        *text = redact_free_text(text);
                    }
                }
            }
        }
        "getUnreadNotificationCount" if value.as_u64().is_none() => {
            return Err(invalid_result("未读数量返回格式无效"));
        }
        "getAnimeDetail" => {
            let object = require_result_object(&mut value, "番剧详情")?;
            if let Some(my_anime) = object.get_mut("myAnime") {
                sanitize_my_anime(my_anime)?;
            }
            redact_error_list(object.get_mut("partialErrors"));
        }
        "searchAnimeCatalog" => {
            let object = require_result_object(&mut value, "新番搜索结果")?;
            redact_string(object.get_mut("keyword"));
            redact_string(object.get_mut("source"));
            if let Some(Value::Array(errors)) = object.get_mut("errors") {
                for error in errors {
                    redact_string(Some(error));
                }
            }
        }
        "browseBangumiAnime" => {
            let object = require_result_object(&mut value, "Bangumi 浏览结果")?;
            redact_string(object.get_mut("source"));
            if let Some(Value::Object(query)) = object.get_mut("query") {
                redact_string(query.get_mut("keyword"));
            }
        }
        "searchReleases" | "searchAnimeReleases" | "searchRssSubscriptionReleases" => {
            let object = require_result_object(&mut value, "资源搜索结果")?;
            redact_error_list(object.get_mut("errors"));
        }
        "previewEpisodeReleases" | "getAnimeSourceBindingState" => {
            let object = require_result_object(&mut value, "来源操作结果")?;
            redact_error_list(object.get_mut("errors"));
        }
        "reportPlaybackProgress" if !value.is_boolean() => {
            return Err(invalid_result("远程处理结果格式无效"));
        }
        _ => {}
    }
    Ok(value)
}

fn sanitize_array(
    value: &mut Value,
    sanitizer: fn(&mut Value) -> Result<(), RemoteRpcError>,
) -> Result<(), RemoteRpcError> {
    for item in require_result_array(value, "远程列表")? {
        sanitizer(item)?;
    }
    Ok(())
}

fn sanitize_my_anime(value: &mut Value) -> Result<(), RemoteRpcError> {
    let object = require_result_object(value, "追番记录")?;
    object.remove("downloadDir");
    Ok(())
}

fn sanitize_source(value: &mut Value) -> Result<(), RemoteRpcError> {
    let object = require_result_object(value, "下载源")?;
    if object
        .get("apiKey")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty())
    {
        object.insert(
            "apiKey".to_owned(),
            Value::String(REMOTE_SECRET_PLACEHOLDER.to_owned()),
        );
    } else {
        object.remove("apiKey");
    }
    Ok(())
}

fn sanitize_settings(value: &mut Value) -> Result<(), RemoteRpcError> {
    let object = require_result_object(value, "应用设置")?;
    object.retain(|key, _| {
        matches!(
            key.as_str(),
            "download" | "automation" | "sourceSync" | "network"
        )
    });
    if let Some(download) = object.get_mut("download") {
        let download = require_result_object(download, "下载设置")?;
        if let Some(qbittorrent) = download.get_mut("qbittorrent") {
            let qbittorrent = require_result_object(qbittorrent, "qBittorrent 设置")?;
            if qbittorrent
                .get("password")
                .and_then(Value::as_str)
                .is_some_and(|password| !password.is_empty())
            {
                qbittorrent.insert(
                    "password".to_owned(),
                    Value::String(REMOTE_SECRET_PLACEHOLDER.to_owned()),
                );
            }
            if let Some(managed) = qbittorrent.get_mut("managed") {
                let managed = require_result_object(managed, "托管 qBittorrent 设置")?;
                managed.remove("binaryPath");
                managed.remove("profileDir");
            }
        }
    }
    if let Some(network) = object.get_mut("network") {
        let network = require_result_object(network, "网络设置")?;
        network.retain(|key, _| key == "metadataProxy");
    }
    Ok(())
}

fn sanitize_qbittorrent_status(value: &mut Value) -> Result<(), RemoteRpcError> {
    let object = require_result_object(value, "qBittorrent 状态")?;
    object.remove("binaryPath");
    object.remove("profileDir");
    object.insert("webUiUrl".to_owned(), Value::String(String::new()));
    redact_string(object.get_mut("lastError"));
    Ok(())
}

fn sanitize_embedded_status(value: &mut Value) -> Result<(), RemoteRpcError> {
    let object = require_result_object(value, "内置下载核心状态")?;
    object.remove("binaryPath");
    object.remove("dataDir");
    redact_string(object.get_mut("lastError"));
    Ok(())
}

fn sanitize_download(value: &mut Value) -> Result<(), RemoteRpcError> {
    let object = require_result_object(value, "下载记录")?;
    object.remove("torrentHash");
    object.remove("correlationTag");
    object.insert(
        "savePath".to_owned(),
        Value::String(HIDDEN_LOCAL_PATH.to_owned()),
    );
    Ok(())
}

fn sanitize_media(value: &mut Value) -> Result<(), RemoteRpcError> {
    let object = require_result_object(value, "媒体记录")?;
    object.insert(
        "filePath".to_owned(),
        Value::String(HIDDEN_LOCAL_PATH.to_owned()),
    );
    Ok(())
}

fn require_result_object<'a>(
    value: &'a mut Value,
    label: &str,
) -> Result<&'a mut Map<String, Value>, RemoteRpcError> {
    value
        .as_object_mut()
        .ok_or_else(|| invalid_result(format!("{label}返回格式无效")))
}

fn require_result_array<'a>(
    value: &'a mut Value,
    label: &str,
) -> Result<&'a mut Vec<Value>, RemoteRpcError> {
    value
        .as_array_mut()
        .ok_or_else(|| invalid_result(format!("{label}返回格式无效")))
}

fn invalid_result(message: impl Into<String>) -> RemoteRpcError {
    RemoteRpcError::new(500, "HANDLER_FAILED", message)
}

fn redact_error_list(value: Option<&mut Value>) {
    if let Some(Value::Array(items)) = value {
        for item in items {
            if let Some(object) = item.as_object_mut() {
                redact_string(object.get_mut("source"));
                redact_string(object.get_mut("message"));
            }
        }
    }
}

fn redact_string(value: Option<&mut Value>) {
    if let Some(Value::String(text)) = value {
        *text = redact_free_text(text);
    }
}

fn redact_free_text(value: &str) -> String {
    value
        .split_whitespace()
        .map(|token| {
            let lower = token.to_ascii_lowercase();
            if lower.starts_with("http://")
                || lower.starts_with("https://")
                || lower.starts_with("ftp://")
            {
                "[链接已隐藏]".to_owned()
            } else if looks_like_local_path(token) {
                "[本机路径已隐藏]".to_owned()
            } else {
                token.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn looks_like_local_path(value: &str) -> bool {
    (value.len() >= 3
        && value.as_bytes()[0].is_ascii_alphabetic()
        && value.as_bytes()[1] == b':'
        && matches!(value.as_bytes()[2], b'\\' | b'/'))
        || value.starts_with("\\\\")
        || [
            "/Users/",
            "/home/",
            "/var/",
            "/private/",
            "/Volumes/",
            "/mnt/",
            "/media/",
        ]
        .iter()
        .any(|prefix| value.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoHandler;

    #[async_trait]
    impl RemoteRpcHandler for EchoHandler {
        async fn call(&self, method: &str, args: Vec<Value>) -> Result<Value, String> {
            match method {
                "listDownloads" => Ok(json!([{
                    "id": "task-1",
                    "torrentHash": "secret-hash",
                    "correlationTag": "secret-tag",
                    "savePath": "C:\\Downloads",
                    "files": []
                }])),
                "listMyAnime" => Ok(json!([{
                    "id": "my-1",
                    "downloadDir": "/Users/test/Downloads",
                    "rssSubscriptions": [{ "id": "rss-1", "url": "https://example.com/feed.xml" }]
                }])),
                "listSources" => Ok(json!([{
                    "id": "source-1",
                    "name": "受控来源",
                    "apiKey": "real-secret"
                }])),
                "getSettings" => Ok(json!({
                    "appearance": { "themeMode": "dark" },
                    "download": {
                        "defaultDownloadDir": "/Users/test/Downloads",
                        "createAnimeFolder": true,
                        "animeFolderPattern": "{title}",
                        "defaultTorrentEngine": "qbittorrent",
                        "embedded": { "enabled": false },
                        "qbittorrent": {
                            "baseUrl": "http://127.0.0.1:18080",
                            "username": "admin",
                            "password": "real-password",
                            "autoConnect": true,
                            "downloadLimitKiBps": 0,
                            "uploadLimitKiBps": 0,
                            "seedingLimits": {},
                            "managed": {
                                "enabled": true,
                                "binaryPath": "/usr/bin/qbittorrent-nox",
                                "profileDir": "/Users/test/qbittorrent",
                                "startupTimeoutMs": 15000
                            }
                        }
                    },
                    "automation": { "scheduledCheckEnabled": true },
                    "sourceSync": { "enabled": true, "dailyTime": "09:00" },
                    "network": {
                        "metadataProxy": { "mode": "system", "timeoutMs": 30000 },
                        "remoteAccess": { "lanEnabled": true, "port": 18083 }
                    },
                    "storage": { "userDataDir": "/Users/test" },
                    "players": [{ "id": "builtin" }]
                })),
                "pauseDownload" | "removeDownload" | "addReleaseDownload" => Ok(json!([])),
                _ => Ok(json!({ "args": args })),
            }
        }
    }

    /// 验证未知方法、scope 不足和未知参数字段均被拒绝。
    #[tokio::test]
    async fn validates_method_scope_and_arguments() {
        let service = RemoteRpcService::new(std::sync::Arc::new(EchoHandler));
        let scopes = vec!["downloads.read".to_owned()];
        let unknown = service
            .dispatch(
                json!({ "method": "runAutomationOnce", "args": [] }),
                &scopes,
            )
            .await
            .expect_err("unknown method");
        assert_eq!(unknown.code, "METHOD_NOT_FOUND");

        let forbidden = service
            .dispatch(
                json!({ "method": "pauseDownload", "args": ["task-1"] }),
                &scopes,
            )
            .await
            .expect_err("forbidden method");
        assert_eq!(forbidden.code, "FORBIDDEN");

        let invalid = service
            .dispatch(
                json!({ "method": "reportPlaybackProgress", "args": [{ "taskId": "task-1", "percent": 95, "path": "C:\\secret" }] }),
                &["library.write".to_owned()],
            )
            .await
            .expect_err("unknown argument");
        assert_eq!(invalid.code, "INVALID_ARGUMENTS");
    }

    /// 验证新番搜索的在线来源开关可选且仅接受布尔值。
    #[tokio::test]
    async fn accepts_remote_catalog_search_online_flag() {
        let service = RemoteRpcService::new(std::sync::Arc::new(EchoHandler));
        let local_only = service
            .dispatch(
                json!({ "method": "searchAnimeCatalog", "args": ["测试番", false] }),
                &["catalog.read".to_owned()],
            )
            .await
            .expect("local-only catalog search");
        assert_eq!(local_only["args"][0], "测试番");
        assert_eq!(local_only["args"][1], false);

        let invalid = service
            .dispatch(
                json!({ "method": "searchAnimeCatalog", "args": ["测试番", "false"] }),
                &["catalog.read".to_owned()],
            )
            .await
            .expect_err("non-boolean online search flag must fail");
        assert_eq!(invalid.code, "INVALID_ARGUMENTS");
    }

    /// 验证远程仅接受布尔类型的单条未知季度确认，不放宽其他下载参数。
    #[tokio::test]
    async fn accepts_remote_unknown_season_confirmation() {
        let service = RemoteRpcService::new(std::sync::Arc::new(EchoHandler));
        let release = json!({
            "id": "release-season-2-episode-5",
            "title": "[LoliHouse] 测试番 2 - 05 [简繁内封字幕]",
            "sourceId": "mikan",
            "sourceName": "蜜柑计划 RSS",
            "torrentUrl": "https://mikanani.me/Download/test.torrent",
            "publishedAt": "2026-08-06T13:15:00Z"
        });
        let confirmed = service
            .dispatch(
                json!({
                    "method": "addReleaseDownload",
                    "args": [{
                        "release": release.clone(),
                        "animeId": "bangumi-412144",
                        "confirmUnknownSeason": true
                    }]
                }),
                &["downloads.control".to_owned()],
            )
            .await;
        assert!(confirmed.is_ok());

        let invalid = service
            .dispatch(
                json!({
                    "method": "addReleaseDownload",
                    "args": [{
                        "release": release,
                        "animeId": "bangumi-412144",
                        "confirmUnknownSeason": "true"
                    }]
                }),
                &["downloads.control".to_owned()],
            )
            .await
            .expect_err("non-boolean confirmation must fail");
        assert_eq!(invalid.code, "INVALID_ARGUMENTS");
    }

    /// 验证远程可以保存下载目录、引擎和做种配置。
    #[tokio::test]
    async fn accepts_remote_download_settings() {
        let service = RemoteRpcService::new(std::sync::Arc::new(EchoHandler));
        let result = service
            .dispatch(
                json!({
                    "method": "updateSettings",
                    "args": [{
                        "download": {
                            "defaultDownloadDir": "/Users/test/Downloads",
                            "createAnimeFolder": true,
                            "animeFolderPattern": "{year}-{month}/{title}",
                            "temporaryDownloadDir": "/Users/test/Downloads/.incomplete",
                            "defaultTorrentEngine": "embedded",
                            "embedded": {
                                "enabled": true,
                                "listenPort": 51413,
                                "maxActiveDownloads": 3,
                                "seedingLimits": {
                                    "enabled": true,
                                    "ratioLimit": 2.0,
                                    "timeLimitMinutes": 120
                                }
                            }
                        }
                    }]
                }),
                &["settings.write".to_owned()],
            )
            .await;

        assert!(result.is_ok());
    }

    /// 验证远程下载删除允许显式删除文件，其他设置写入仍不能越过桌面字段边界。
    #[tokio::test]
    async fn allows_download_file_removal_but_rejects_desktop_mutations() {
        let service = RemoteRpcService::new(std::sync::Arc::new(EchoHandler));
        let remove_result = service
            .dispatch(
                json!({ "method": "removeDownload", "args": ["task-1", true] }),
                &["downloads.control".to_owned()],
            )
            .await;
        assert!(remove_result.is_ok());

        for request in [
            json!({ "method": "addDownloadUrl", "args": [{ "url": "magnet:?xt=urn:btih:abc", "savePath": "/tmp" }] }),
            json!({ "method": "updateSettings", "args": [{ "storage": { "userDataDir": "/tmp" } }] }),
            json!({ "method": "updateSettings", "args": [{ "network": { "remoteAccess": { "port": 1 } } }] }),
            json!({ "method": "updateSettings", "args": [{ "download": { "qbittorrent": { "managed": { "binaryPath": "/tmp/qb" } } } }] }),
        ] {
            let error = service
                .dispatch(
                    request,
                    &["downloads.control".to_owned(), "settings.write".to_owned()],
                )
                .await
                .expect_err("local mutation must fail");
            assert_eq!(error.code, "INVALID_ARGUMENTS");
        }
    }

    /// 验证下载结果不会向远程客户端泄漏哈希、关联标签和保存路径。
    #[tokio::test]
    async fn sanitizes_download_result() {
        let service = RemoteRpcService::new(std::sync::Arc::new(EchoHandler));
        let result = service
            .dispatch(
                json!({ "method": "listDownloads", "args": [] }),
                &["downloads.read".to_owned()],
            )
            .await
            .expect("dispatch downloads");
        let task = &result[0];
        assert!(task.get("torrentHash").is_none());
        assert!(task.get("correlationTag").is_none());
        assert_eq!(task["savePath"], HIDDEN_LOCAL_PATH);
    }

    /// 验证共享 RSS 保留，同时本地目录、来源秘密与宿主设置保持隐藏。
    #[tokio::test]
    async fn sanitizes_shared_business_data_without_removing_rss() {
        let service = RemoteRpcService::new(std::sync::Arc::new(EchoHandler));
        let anime = service
            .dispatch(
                json!({ "method": "listMyAnime", "args": [] }),
                &["library.read".to_owned()],
            )
            .await
            .expect("dispatch anime");
        assert!(anime[0].get("downloadDir").is_none());
        assert_eq!(anime[0]["rssSubscriptions"][0]["id"], "rss-1");

        let sources = service
            .dispatch(
                json!({ "method": "listSources", "args": [] }),
                &["sources.read".to_owned()],
            )
            .await
            .expect("dispatch sources");
        assert_eq!(sources[0]["apiKey"], REMOTE_SECRET_PLACEHOLDER);

        let settings = service
            .dispatch(
                json!({ "method": "getSettings", "args": [] }),
                &["settings.read".to_owned()],
            )
            .await
            .expect("dispatch settings");
        assert!(settings.get("storage").is_none());
        assert!(settings.get("players").is_none());
        assert!(settings.get("appearance").is_none());
        assert!(settings["network"].get("remoteAccess").is_none());
        assert!(settings["network"].get("metadataProxy").is_some());
        assert_eq!(
            settings["download"]["qbittorrent"]["password"],
            REMOTE_SECRET_PLACEHOLDER
        );
        assert!(settings["download"]["qbittorrent"]["managed"]
            .get("binaryPath")
            .is_none());
        assert!(settings["download"]["qbittorrent"]["managed"]
            .get("profileDir")
            .is_none());
    }
}
