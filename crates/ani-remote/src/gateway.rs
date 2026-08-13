use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ani_contracts::{
    RemoteCertificateInfo, RemoteGatewayStatus, RemotePairingChallenge, RemotePlaybackEnhancement,
};
use axum::body::{to_bytes, Body};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::{
    ACCEPT_RANGES, AUTHORIZATION, CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH,
    CONTENT_RANGE, CONTENT_TYPE, COOKIE, ETAG, HOST, IF_NONE_MATCH, ORIGIN, RANGE, SET_COOKIE,
};
use axum::http::{HeaderMap, HeaderValue, Method, Response, StatusCode};
use axum::routing::any;
use axum::Router;
use axum_server::Handle;
use percent_encoding::{percent_decode_str, utf8_percent_encode, NON_ALPHANUMERIC};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio_util::io::ReaderStream;

use crate::auth::{RemoteDeviceAuth, RemoteDeviceAuthError};
use crate::image_cache::{ImageCache, ImageCacheError};
use crate::media::{RemoteMediaAsset, RemoteMediaError, RemoteMediaSessionService};
use crate::network::{
    is_trusted_host, is_trusted_origin, list_private_ipv4_addresses, TrustedOrigin,
};
use crate::rpc::{RemoteRpcError, RemoteRpcService};
use crate::tls::{RemoteTlsBundle, RemoteTlsCertificateStore};

const DEFAULT_PORT: u16 = 18_083;
const MAX_BODY_BYTES: usize = 64 * 1024;
const MEDIA_STREAM_BUFFER_BYTES: usize = 1024 * 1024;

/// 设置中的远程网关监听选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewayConfig {
    pub lan_enabled: bool,
    pub port: u16,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            lan_enabled: true,
            port: DEFAULT_PORT,
        }
    }
}

impl GatewayConfig {
    /// 校验非特权端口后创建配置。
    pub fn new(lan_enabled: bool, port: u16) -> Result<Self, RemoteGatewayError> {
        if port < 1_024 {
            return Err(RemoteGatewayError::Configuration(
                "远程网关端口必须为 1024 至 65535".to_owned(),
            ));
        }
        Ok(Self { lan_enabled, port })
    }
}

/// 装配远程网关所需的独立 Rust 服务。
pub struct RemoteGatewayDependencies {
    pub auth: Arc<RemoteDeviceAuth>,
    pub rpc: Arc<RemoteRpcService>,
    pub media: Arc<RemoteMediaSessionService>,
    pub image_cache: Arc<ImageCache>,
    pub tls_store: Arc<RemoteTlsCertificateStore>,
    pub renderer_directory: PathBuf,
    pub trusted_origins: Vec<TrustedOrigin>,
}

/// 网关启动、停止和配置错误。
#[derive(Debug, thiserror::Error)]
pub enum RemoteGatewayError {
    #[error("远程网关配置无效：{0}")]
    Configuration(String),
    #[error("远程网关启动失败：{0}")]
    Startup(String),
    #[error("远程网关凭据失败：{0}")]
    Credentials(String),
}

#[derive(Clone)]
struct RuntimeStatus {
    running: bool,
    host: String,
    port: u16,
    protocol: &'static str,
    lan_enabled: bool,
    addresses: Vec<Ipv4Addr>,
    certificate: Option<RemoteTlsBundle>,
    last_error: Option<String>,
}

impl Default for RuntimeStatus {
    fn default() -> Self {
        Self {
            running: false,
            host: "127.0.0.1".to_owned(),
            port: DEFAULT_PORT,
            protocol: "http",
            lan_enabled: false,
            addresses: Vec::new(),
            certificate: None,
            last_error: None,
        }
    }
}

struct RateLimitEntry {
    started_at: Instant,
    count: u32,
}

struct GatewayCore {
    auth: Arc<RemoteDeviceAuth>,
    rpc: Arc<RemoteRpcService>,
    media: Arc<RemoteMediaSessionService>,
    image_cache: Arc<ImageCache>,
    renderer_directory: PathBuf,
    trusted_origins: Vec<TrustedOrigin>,
    runtime: RwLock<RuntimeStatus>,
    rate_limits: Mutex<HashMap<String, RateLimitEntry>>,
}

struct RunningServer {
    handle: Handle,
    task: JoinHandle<()>,
}

/// 桌面 Rust HTTP/TLS 网关，拥有监听器和全部远程会话生命周期。
pub struct RemoteGateway {
    core: Arc<GatewayCore>,
    tls_store: Arc<RemoteTlsCertificateStore>,
    server: Mutex<Option<RunningServer>>,
}

impl RemoteGateway {
    /// 使用显式依赖创建默认停止状态的网关。
    pub fn new(dependencies: RemoteGatewayDependencies) -> Self {
        Self {
            core: Arc::new(GatewayCore {
                auth: dependencies.auth,
                rpc: dependencies.rpc,
                media: dependencies.media,
                image_cache: dependencies.image_cache,
                renderer_directory: dependencies.renderer_directory,
                trusted_origins: dependencies.trusted_origins,
                runtime: RwLock::new(RuntimeStatus::default()),
                rate_limits: Mutex::new(HashMap::new()),
            }),
            tls_store: dependencies.tls_store,
            server: Mutex::new(None),
        }
    }

    /// 应用设置并切换回环 HTTP 或局域网 HTTPS，HTTPS 失败时恢复回环服务。
    pub async fn apply_config(
        &self,
        config: GatewayConfig,
    ) -> Result<RemoteGatewayStatus, RemoteGatewayError> {
        if config.port < 1_024 {
            return Err(RemoteGatewayError::Configuration(
                "远程网关端口必须为 1024 至 65535".to_owned(),
            ));
        }
        self.stop().await;
        self.core
            .auth
            .initialize()
            .await
            .map_err(|error| RemoteGatewayError::Credentials(error.to_string()))?;
        if !config.lan_enabled {
            self.start_listener(config.port, Vec::new(), None).await?;
            return Ok(self.status().await);
        }
        let addresses = list_private_ipv4_addresses();
        let lan_result = async {
            if addresses.is_empty() {
                return Err(RemoteGatewayError::Startup(
                    "未发现可用的局域网 IPv4 地址".to_owned(),
                ));
            }
            let certificate = self
                .tls_store
                .load_or_create(&addresses)
                .await
                .map_err(RemoteGatewayError::Startup)?;
            self.start_listener(config.port, addresses.clone(), Some(certificate))
                .await
        }
        .await;
        if let Err(error) = lan_result {
            let message = error.to_string();
            log::warn!("Rust 局域网 HTTPS 启动失败，恢复回环服务 error={message}");
            self.start_listener(config.port, Vec::new(), None).await?;
            self.core.runtime.write().await.last_error = Some(message);
        }
        Ok(self.status().await)
    }

    async fn start_listener(
        &self,
        port: u16,
        addresses: Vec<Ipv4Addr>,
        certificate: Option<RemoteTlsBundle>,
    ) -> Result<(), RemoteGatewayError> {
        let lan_enabled = certificate.is_some();
        let host = if lan_enabled {
            Ipv4Addr::UNSPECIFIED
        } else {
            Ipv4Addr::LOCALHOST
        };
        let listener = std::net::TcpListener::bind((host, port))
            .map_err(|error| RemoteGatewayError::Startup(error.to_string()))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| RemoteGatewayError::Startup(error.to_string()))?;
        let active_port = listener
            .local_addr()
            .map_err(|error| RemoteGatewayError::Startup(error.to_string()))?
            .port();
        let protocol = if lan_enabled { "https" } else { "http" };
        {
            let mut runtime = self.core.runtime.write().await;
            *runtime = RuntimeStatus {
                running: true,
                host: host.to_string(),
                port: active_port,
                protocol,
                lan_enabled,
                addresses,
                certificate: certificate.clone(),
                last_error: None,
            };
        }
        let router = Router::new()
            .fallback(any(dispatch_request))
            .with_state(Arc::clone(&self.core));
        let handle = Handle::new();
        let server_handle = handle.clone();
        let core = Arc::clone(&self.core);
        let task = if let Some(certificate) = certificate {
            let tls_config = certificate
                .rustls_config()
                .await
                .map_err(RemoteGatewayError::Startup)?;
            tokio::spawn(async move {
                let result = axum_server::from_tcp_rustls(listener, tls_config)
                    .handle(server_handle)
                    .serve(router.into_make_service_with_connect_info::<SocketAddr>())
                    .await;
                finish_server(core, result.map_err(|error| error.to_string())).await;
            })
        } else {
            tokio::spawn(async move {
                let result = axum_server::from_tcp(listener)
                    .handle(server_handle)
                    .serve(router.into_make_service_with_connect_info::<SocketAddr>())
                    .await;
                finish_server(core, result.map_err(|error| error.to_string())).await;
            })
        };
        *self.server.lock().await = Some(RunningServer { handle, task });
        log::info!("Rust 远程网关已启动 host={host} port={active_port} protocol={protocol}");
        Ok(())
    }

    /// 停止监听并回收连接、媒体会话和限流状态。
    pub async fn stop(&self) {
        let running = self.server.lock().await.take();
        if let Some(running) = running {
            running
                .handle
                .graceful_shutdown(Some(Duration::from_secs(2)));
            let _ = tokio::time::timeout(Duration::from_secs(3), running.task).await;
        }
        self.core.media.stop_all().await;
        self.core.rate_limits.lock().await.clear();
        self.core.runtime.write().await.running = false;
    }

    /// 返回设置页可展示的网关、证书和设备状态。
    pub async fn status(&self) -> RemoteGatewayStatus {
        let runtime = self.core.runtime.read().await.clone();
        let public_host = if runtime.lan_enabled {
            runtime
                .addresses
                .first()
                .copied()
                .unwrap_or(Ipv4Addr::LOCALHOST)
                .to_string()
        } else {
            Ipv4Addr::LOCALHOST.to_string()
        };
        RemoteGatewayStatus {
            running: runtime.running,
            host: runtime.host,
            port: runtime.port,
            protocol: runtime.protocol.to_owned(),
            lan_enabled: runtime.lan_enabled,
            base_url: format!("{}://{}:{}", runtime.protocol, public_host, runtime.port),
            addresses: runtime.addresses.iter().map(ToString::to_string).collect(),
            devices: self.core.auth.list_devices().await,
            certificate: runtime
                .certificate
                .map(|certificate| RemoteCertificateInfo {
                    fingerprint: certificate.fingerprint,
                    expires_at: certificate.expires_at,
                    authority_certificate_path: certificate
                        .authority_certificate_path
                        .to_string_lossy()
                        .into_owned(),
                }),
            last_error: runtime.last_error,
        }
    }

    /// 创建两分钟有效的一次性配对码。
    pub async fn create_pairing_code(&self) -> Result<RemotePairingChallenge, RemoteGatewayError> {
        if !self.core.runtime.read().await.running {
            return Err(RemoteGatewayError::Startup("远程网关尚未运行".to_owned()));
        }
        Ok(self.core.auth.create_pairing_code().await)
    }

    /// 吊销设备、令牌和其全部媒体会话。
    pub async fn revoke_device(
        &self,
        device_id: &str,
    ) -> Result<RemoteGatewayStatus, RemoteGatewayError> {
        self.core
            .auth
            .revoke(device_id)
            .await
            .map_err(|error| RemoteGatewayError::Credentials(error.to_string()))?;
        self.core.media.close_device_sessions(device_id).await;
        Ok(self.status().await)
    }
}

async fn finish_server(core: Arc<GatewayCore>, result: Result<(), String>) {
    let mut runtime = core.runtime.write().await;
    runtime.running = false;
    if let Err(error) = result {
        log::error!("Rust 远程网关监听异常退出 error={error}");
        runtime.last_error = Some("远程网关监听异常退出".to_owned());
    }
}

async fn dispatch_request(
    State(core): State<Arc<GatewayCore>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response<Body> {
    let request_id = uuid::Uuid::new_v4();
    let started = Instant::now();
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let result = handle_request(&core, peer, request).await;
    let response = match result {
        Ok(response) => response,
        Err(error) => error_response(error),
    };
    log::info!(
        "Rust 远程请求完成 request_id={request_id} method={method} path={path} status={} elapsed_ms={}",
        response.status(),
        started.elapsed().as_millis()
    );
    response
}

async fn handle_request(
    core: &Arc<GatewayCore>,
    peer: SocketAddr,
    request: Request,
) -> Result<Response<Body>, GatewayHttpError> {
    let host = request_authority(&request).map(str::to_owned);
    let origin = request
        .headers()
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    validate_host_origin(core, host.as_deref(), origin.as_deref()).await?;
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    if method == Method::GET && path == "/api/health" {
        return Ok(json_response(StatusCode::OK, json!({ "ok": true })));
    }
    if method == Method::GET && path == "/ani-tracker-ca.crt" {
        let certificate = core
            .runtime
            .read()
            .await
            .certificate
            .clone()
            .ok_or_else(|| {
                GatewayHttpError::new(404, "CERTIFICATE_NOT_AVAILABLE", "CA 证书不可用")
            })?;
        return response_with_headers(
            StatusCode::OK,
            Body::from(certificate.ca_pem),
            &[
                (CONTENT_TYPE, "application/x-x509-ca-cert"),
                (
                    CONTENT_DISPOSITION,
                    "attachment; filename=ani-tracker-ca.crt",
                ),
                (CACHE_CONTROL, "no-store"),
            ],
        );
    }
    if method == Method::POST && path == "/api/pair" {
        consume_rate_limit(
            core,
            format!("pair:{}", peer.ip()),
            5,
            Duration::from_secs(600),
        )
        .await?;
        let body: PairBody = parse_body(request).await?;
        if body.code.len() != 6 || !body.code.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(GatewayHttpError::new(
                400,
                "PAIRING_CODE_INVALID",
                "配对码格式无效",
            ));
        }
        let result = core
            .auth
            .pair_device(&body.code, &body.device_name)
            .await
            .map_err(GatewayHttpError::from_auth)?;
        return Ok(json_response(StatusCode::OK, serde_json::to_value(result)?));
    }
    if method == Method::POST && path == "/api/rpc" {
        let device = authenticate(core, request.headers(), false).await?;
        let body = parse_json_body(request).await?;
        let write = core.rpc.is_write_method(&body);
        consume_rate_limit(
            core,
            format!("rpc:{}:{}", device.id, if write { "write" } else { "read" }),
            if write { 30 } else { 120 },
            Duration::from_secs(60),
        )
        .await?;
        let result = core
            .rpc
            .dispatch(body, &device.scopes)
            .await
            .map_err(GatewayHttpError::from_rpc)?;
        return Ok(json_response(StatusCode::OK, json!({ "result": result })));
    }
    if method == Method::POST && path == "/api/images/resolve" {
        let device = authenticate(core, request.headers(), false).await?;
        consume_rate_limit(
            core,
            format!("images:{}:resolve", device.id),
            240,
            Duration::from_secs(60),
        )
        .await?;
        let body: ImageResolveBody = parse_body(request).await?;
        if body.url.len() > 2_048 {
            return Err(GatewayHttpError::new(
                400,
                "IMAGE_URL_INVALID",
                "图片地址无效",
            ));
        }
        let url = core
            .image_cache
            .create_remote_path(&body.url)
            .map_err(GatewayHttpError::from_image)?;
        return Ok(json_response(StatusCode::OK, json!({ "url": url })));
    }
    if matches!(method, Method::GET | Method::HEAD) {
        if let Some(token) = path
            .strip_prefix("/api/images/")
            .filter(|value| !value.contains('/'))
        {
            consume_rate_limit(
                core,
                format!("images:{}:read", peer.ip()),
                600,
                Duration::from_secs(60),
            )
            .await?;
            let asset = core
                .image_cache
                .get_by_token(token)
                .await
                .map_err(GatewayHttpError::from_image)?;
            let etag = format!("\"{}\"", asset.cache_key);
            if request
                .headers()
                .get(IF_NONE_MATCH)
                .and_then(|value| value.to_str().ok())
                == Some(&etag)
            {
                return Ok(empty_response(StatusCode::NOT_MODIFIED));
            }
            return stream_file(
                &method,
                &asset.file_path,
                &asset.content_type,
                None,
                &[
                    (CACHE_CONTROL, "private, max-age=86400, immutable"),
                    (ETAG, &etag),
                ],
            )
            .await;
        }
    }
    if method == Method::POST
        && matches!(
            path.as_str(),
            "/api/media/sessions" | "/api/media/external-sessions"
        )
    {
        let device = authenticate(core, request.headers(), false).await?;
        let authorization = bearer_token(request.headers()).map(str::to_owned);
        consume_rate_limit(
            core,
            format!("media:{}:write", device.id),
            20,
            Duration::from_secs(60),
        )
        .await?;
        let body: MediaSessionBody = parse_body(request).await?;
        let session = if path.ends_with("external-sessions") {
            core.media
                .create_external_session(
                    &body.task_id,
                    &device.id,
                    &body.mode,
                    body.file_index,
                    body.enhancement,
                )
                .await
        } else {
            core.media
                .create_session(
                    &body.task_id,
                    &device.id,
                    &body.mode,
                    body.file_index,
                    body.enhancement,
                )
                .await
        }
        .map_err(GatewayHttpError::from_media)?;
        let mut response = json_response(StatusCode::OK, serde_json::to_value(session)?);
        if path.ends_with("/sessions") {
            if let Some(token) = authorization {
                let secure = core.runtime.read().await.protocol == "https";
                let cookie = format!(
                    "ani_media_token={token}; HttpOnly; SameSite=Strict; Path=/api/media; Max-Age=2592000{}",
                    if secure { "; Secure" } else { "" }
                );
                response.headers_mut().insert(
                    SET_COOKIE,
                    HeaderValue::from_str(&cookie).map_err(|_| {
                        GatewayHttpError::new(500, "INTERNAL_ERROR", "远程服务内部错误")
                    })?,
                );
            }
        }
        return Ok(response);
    }
    if let Some(route) = parse_browser_media_route(&path) {
        if method == Method::DELETE && route.asset_name.is_none() {
            let device = authenticate(core, request.headers(), true).await?;
            if !core
                .media
                .close_session(&route.session_id, &device.id)
                .await
            {
                return Err(GatewayHttpError::new(
                    404,
                    "MEDIA_SESSION_NOT_FOUND",
                    "播放会话不存在",
                ));
            }
            return Ok(empty_response(StatusCode::NO_CONTENT));
        }
        if matches!(method, Method::GET | Method::HEAD) {
            if let Some(asset_name) = route.asset_name {
                let device = authenticate(core, request.headers(), true).await?;
                consume_rate_limit(
                    core,
                    format!("media:{}:read", device.id),
                    600,
                    Duration::from_secs(60),
                )
                .await?;
                let asset = core
                    .media
                    .get_asset(&route.session_id, &device.id, &asset_name)
                    .await
                    .map_err(GatewayHttpError::from_media)?;
                return stream_media(request.headers(), &method, asset).await;
            }
        }
    }
    if matches!(method, Method::GET | Method::HEAD) {
        if let Some(route) = parse_external_media_route(&path) {
            consume_rate_limit(
                core,
                format!("media:external:{}", peer.ip()),
                600,
                Duration::from_secs(60),
            )
            .await?;
            let asset = core
                .media
                .get_external_asset(&route.session_id, &route.access_token, &route.asset_name)
                .await
                .map_err(GatewayHttpError::from_media)?;
            return stream_media(request.headers(), &method, asset).await;
        }
        return serve_renderer(core, &path, method == Method::HEAD).await;
    }
    Err(GatewayHttpError::new(404, "NOT_FOUND", "请求路径不存在"))
}

async fn validate_host_origin(
    core: &GatewayCore,
    host: Option<&str>,
    origin: Option<&str>,
) -> Result<(), GatewayHttpError> {
    let runtime = core.runtime.read().await;
    let mut allowed = vec!["127.0.0.1".to_owned(), "localhost".to_owned()];
    allowed.extend(runtime.addresses.iter().map(ToString::to_string));
    if !is_trusted_host(host, runtime.port, &allowed, &core.trusted_origins) {
        return Err(GatewayHttpError::new(
            403,
            "HOST_FORBIDDEN",
            "请求 Host 不受信任",
        ));
    }
    if !is_trusted_origin(
        origin,
        runtime.protocol,
        runtime.port,
        &allowed,
        &core.trusted_origins,
    ) {
        return Err(GatewayHttpError::new(
            403,
            "ORIGIN_FORBIDDEN",
            "请求 Origin 不受信任",
        ));
    }
    Ok(())
}

/// 返回 HTTP/2 `:authority`，并为 HTTP/1.1 请求回退读取 `Host`。
fn request_authority(request: &Request) -> Option<&str> {
    request
        .uri()
        .authority()
        .map(|authority| authority.as_str())
        .or_else(|| {
            request
                .headers()
                .get(HOST)
                .and_then(|value| value.to_str().ok())
        })
}

async fn authenticate(
    core: &GatewayCore,
    headers: &HeaderMap,
    allow_cookie: bool,
) -> Result<ani_contracts::RemoteDeviceInfo, GatewayHttpError> {
    let token =
        bearer_token(headers).or_else(|| allow_cookie.then(|| media_cookie(headers)).flatten());
    match token {
        Some(token) => core
            .auth
            .authenticate(token)
            .await
            .ok_or_else(|| GatewayHttpError::new(401, "UNAUTHORIZED", "设备未配对或令牌已失效")),
        None => Err(GatewayHttpError::new(
            401,
            "UNAUTHORIZED",
            "设备未配对或令牌已失效",
        )),
    }
}

async fn consume_rate_limit(
    core: &GatewayCore,
    key: String,
    limit: u32,
    window: Duration,
) -> Result<(), GatewayHttpError> {
    let now = Instant::now();
    let mut entries = core.rate_limits.lock().await;
    let entry = entries.entry(key).or_insert(RateLimitEntry {
        started_at: now,
        count: 0,
    });
    if now.duration_since(entry.started_at) >= window {
        entry.started_at = now;
        entry.count = 0;
    }
    entry.count = entry.count.saturating_add(1);
    if entry.count > limit {
        return Err(GatewayHttpError::new(
            429,
            "RATE_LIMITED",
            "请求过于频繁，请稍后重试",
        ));
    }
    Ok(())
}

async fn parse_json_body(request: Request) -> Result<Value, GatewayHttpError> {
    let bytes = to_bytes(request.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|_| GatewayHttpError::new(413, "BODY_TOO_LARGE", "请求体超过 64KB 限制"))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| GatewayHttpError::new(400, "INVALID_JSON", "请求体不是有效 JSON"))
}

async fn parse_body<T: for<'de> Deserialize<'de>>(request: Request) -> Result<T, GatewayHttpError> {
    serde_json::from_value(parse_json_body(request).await?)
        .map_err(|_| GatewayHttpError::new(400, "INVALID_REQUEST", "请求格式无效"))
}

async fn stream_media(
    headers: &HeaderMap,
    method: &Method,
    asset: RemoteMediaAsset,
) -> Result<Response<Body>, GatewayHttpError> {
    let metadata = tokio::fs::metadata(&asset.file_path)
        .await
        .map_err(|_| GatewayHttpError::new(404, "MEDIA_ASSET_NOT_FOUND", "媒体资源不存在"))?;
    let requested_range = headers.get(RANGE).and_then(|value| value.to_str().ok());
    let range = asset
        .direct
        .then(|| parse_byte_range(requested_range, metadata.len()))
        .flatten();
    if asset.direct && requested_range.is_some() && range.is_none() {
        log::warn!(
            "Rust 远程媒体 Range 无效 method={} file_name={} requested_range={} total={}",
            method,
            asset.file_name.as_deref().unwrap_or("unknown"),
            requested_range.unwrap_or("none"),
            metadata.len()
        );
        return response_with_headers(
            StatusCode::RANGE_NOT_SATISFIABLE,
            Body::empty(),
            &[
                (CONTENT_RANGE, &format!("bytes */{}", metadata.len())),
                (ACCEPT_RANGES, "bytes"),
            ],
        );
    }
    if asset.direct {
        let (start, end) = range
            .map(|range| (range.start, range.end))
            .unwrap_or_else(|| (0, metadata.len().saturating_sub(1)));
        let length = if metadata.len() == 0 {
            0
        } else {
            end - start + 1
        };
        log::info!(
            "Rust 远程媒体直传 method={} file_name={} requested_range={} start={} end={} length={} total={}",
            method,
            asset.file_name.as_deref().unwrap_or("unknown"),
            requested_range.unwrap_or("none"),
            start,
            end,
            length,
            metadata.len()
        );
    }
    let content_disposition = asset.file_name.as_deref().map(|file_name| {
        format!(
            "inline; filename*=UTF-8''{}",
            utf8_percent_encode(file_name, NON_ALPHANUMERIC)
        )
    });
    let mut extra_headers = vec![(CACHE_CONTROL, "no-store")];
    if asset.direct {
        extra_headers.push((ACCEPT_RANGES, "bytes"));
    }
    if let Some(content_disposition) = content_disposition.as_deref() {
        extra_headers.push((CONTENT_DISPOSITION, content_disposition));
    }
    stream_file(
        method,
        &asset.file_path,
        &asset.content_type,
        range,
        &extra_headers,
    )
    .await
}

async fn stream_file(
    method: &Method,
    path: &Path,
    content_type: &str,
    range: Option<ByteRange>,
    extra_headers: &[(axum::http::HeaderName, &str)],
) -> Result<Response<Body>, GatewayHttpError> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|_| GatewayHttpError::new(404, "ASSET_NOT_FOUND", "资源不存在"))?;
    let start = range.map_or(0, |range| range.start);
    let end = range.map_or_else(|| metadata.len().saturating_sub(1), |range| range.end);
    let length = if metadata.len() == 0 {
        0
    } else {
        end - start + 1
    };
    let mut response = Response::builder()
        .status(if range.is_some() {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(CONTENT_TYPE, content_type)
        .header(CONTENT_LENGTH, length)
        .header("X-Content-Type-Options", "nosniff");
    if range.is_some() {
        response = response.header(
            CONTENT_RANGE,
            format!("bytes {start}-{end}/{}", metadata.len()),
        );
    }
    for (name, value) in extra_headers {
        response = response.header(name, *value);
    }
    if method == Method::HEAD || length == 0 {
        return response
            .body(Body::empty())
            .map_err(|_| GatewayHttpError::internal());
    }
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| GatewayHttpError::new(404, "ASSET_NOT_FOUND", "资源不存在"))?;
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|_| GatewayHttpError::internal())?;
    let stream = ReaderStream::with_capacity(file.take(length), MEDIA_STREAM_BUFFER_BYTES);
    response
        .body(Body::from_stream(stream))
        .map_err(|_| GatewayHttpError::internal())
}

async fn serve_renderer(
    core: &GatewayCore,
    raw_path: &str,
    head_only: bool,
) -> Result<Response<Body>, GatewayHttpError> {
    let decoded = percent_decode_str(raw_path)
        .decode_utf8()
        .map_err(|_| GatewayHttpError::new(400, "PATH_INVALID", "静态资源路径编码无效"))?;
    if decoded.chars().any(|value| matches!(value, '\0' | '\\'))
        || decoded.split('/').any(|segment| segment == "..")
    {
        return Err(GatewayHttpError::new(
            403,
            "PATH_FORBIDDEN",
            "静态资源路径无效",
        ));
    }
    let root = tokio::fs::canonicalize(&core.renderer_directory)
        .await
        .map_err(|_| GatewayHttpError::new(404, "PWA_NOT_BUILT", "PWA 静态资源尚未构建"))?;
    let relative = decoded.trim_start_matches('/');
    let candidate = root.join(if relative.is_empty() {
        "index.html"
    } else {
        relative
    });
    if !candidate.starts_with(&root) {
        return Err(GatewayHttpError::new(
            403,
            "PATH_FORBIDDEN",
            "静态资源路径无效",
        ));
    }
    let candidate_exists = tokio::fs::metadata(&candidate)
        .await
        .is_ok_and(|metadata| metadata.is_file());
    if !candidate_exists && Path::new(relative).extension().is_some() {
        return Err(GatewayHttpError::new(
            404,
            "ASSET_NOT_FOUND",
            "静态资源不存在",
        ));
    }
    let selected = if candidate_exists {
        candidate
    } else {
        root.join("index.html")
    };
    let selected = tokio::fs::canonicalize(selected)
        .await
        .map_err(|_| GatewayHttpError::new(404, "PWA_NOT_BUILT", "PWA 静态资源尚未构建"))?;
    if !selected.starts_with(&root) {
        return Err(GatewayHttpError::new(
            403,
            "PATH_FORBIDDEN",
            "静态资源路径无效",
        ));
    }
    let bytes = tokio::fs::read(&selected)
        .await
        .map_err(|_| GatewayHttpError::new(404, "ASSET_NOT_FOUND", "静态资源不可用"))?;
    let is_renderer_entry =
        selected.file_name().and_then(|value| value.to_str()) == Some("index.html");
    let is_service_worker = selected.file_name().and_then(|value| value.to_str()) == Some("sw.js");
    let script_nonce = is_renderer_entry.then(|| uuid::Uuid::new_v4().simple().to_string());
    let bytes = if let Some(script_nonce) = script_nonce.as_deref() {
        let html = String::from_utf8(bytes).map_err(|_| {
            GatewayHttpError::new(500, "PWA_ENTRY_INVALID", "PWA 入口文件不是有效 UTF-8")
        })?;
        prepare_renderer_entry_html(&html, script_nonce).into_bytes()
    } else {
        bytes
    };
    let content_type = mime_guess::from_path(&selected)
        .first_or_octet_stream()
        .essence_str()
        .to_owned();
    let csp = create_renderer_content_security_policy(script_nonce.as_deref());
    response_with_headers(
        StatusCode::OK,
        if head_only {
            Body::empty()
        } else {
            Body::from(bytes.clone())
        },
        &[
            (CONTENT_TYPE, &content_type),
            (CONTENT_LENGTH, &bytes.len().to_string()),
            (
                CACHE_CONTROL,
                if is_renderer_entry || is_service_worker {
                    "no-cache"
                } else {
                    "public, max-age=31536000, immutable"
                },
            ),
            (axum::http::header::CONTENT_SECURITY_POLICY, &csp),
            (axum::http::header::REFERRER_POLICY, "no-referrer"),
        ],
    )
}

/// 为远程入口页补齐根路径基准，并授权受控的内联初始化脚本。
fn prepare_renderer_entry_html(html: &str, script_nonce: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let with_base = if lower.contains("<base") {
        html.to_owned()
    } else if let Some(head_end) = lower
        .find("<head")
        .and_then(|start| lower[start..].find('>').map(|end| start + end + 1))
    {
        let mut output = String::with_capacity(html.len() + 24);
        output.push_str(&html[..head_end]);
        output.push_str("\n    <base href=\"/\" />");
        output.push_str(&html[head_end..]);
        output
    } else {
        format!("<base href=\"/\" />{html}")
    };
    add_inline_script_nonce(&with_base, script_nonce)
}

/// 仅为没有 src 和 nonce 的内联脚本追加当前响应随机 nonce。
fn add_inline_script_nonce(html: &str, script_nonce: &str) -> String {
    let mut output = String::with_capacity(html.len() + 48);
    let mut cursor = 0;
    while cursor < html.len() {
        let remaining_lower = html[cursor..].to_ascii_lowercase();
        let Some(relative_start) = remaining_lower.find("<script") else {
            output.push_str(&html[cursor..]);
            break;
        };
        let start = cursor + relative_start;
        output.push_str(&html[cursor..start]);
        let Some(relative_end) = html[start..].find('>') else {
            output.push_str(&html[start..]);
            break;
        };
        let end = start + relative_end;
        let tag = &html[start..=end];
        let tag_lower = tag.to_ascii_lowercase();
        if tag_lower.contains(" src=") || tag_lower.contains(" nonce=") {
            output.push_str(tag);
        } else {
            output.push_str(&tag[..tag.len() - 1]);
            output.push_str(&format!(" nonce=\"{script_nonce}\">"));
        }
        cursor = end + 1;
    }
    output
}

/// 创建远程 PWA CSP，只放行当前入口响应的内联初始化脚本。
fn create_renderer_content_security_policy(script_nonce: Option<&str>) -> String {
    let script_source = script_nonce.map_or_else(
        || "script-src 'self'".to_owned(),
        |nonce| format!("script-src 'self' 'nonce-{nonce}'"),
    );
    format!(
        "default-src 'self'; {script_source}; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; connect-src 'self'; media-src 'self' blob:; worker-src 'self' blob:; object-src 'none'; base-uri 'self'; frame-ancestors 'none'"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

/// 解析单段 HTTP Range，拒绝多段、越界和非法范围。
pub fn parse_byte_range(value: Option<&str>, size: u64) -> Option<ByteRange> {
    let value = value?.strip_prefix("bytes=")?;
    if size == 0 || value.contains(',') {
        return None;
    }
    let (start, end) = value.split_once('-')?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().ok()?;
        if suffix == 0 {
            return None;
        }
        return Some(ByteRange {
            start: size.saturating_sub(suffix),
            end: size - 1,
        });
    }
    let start = start.parse::<u64>().ok()?;
    let end = if end.is_empty() {
        size - 1
    } else {
        end.parse::<u64>().ok()?.min(size - 1)
    };
    (start < size && end >= start).then_some(ByteRange { start, end })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PairBody {
    code: String,
    device_name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageResolveBody {
    url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MediaSessionBody {
    task_id: String,
    mode: String,
    #[serde(default)]
    file_index: Option<i64>,
    #[serde(default)]
    enhancement: RemotePlaybackEnhancement,
}

struct BrowserMediaRoute {
    session_id: String,
    asset_name: Option<String>,
}

struct ExternalMediaRoute {
    access_token: String,
    session_id: String,
    asset_name: String,
}

fn parse_browser_media_route(path: &str) -> Option<BrowserMediaRoute> {
    let rest = path.strip_prefix("/api/media/sessions/")?;
    let mut parts = rest.split('/');
    let session_id = parts.next()?.to_owned();
    if !valid_token(&session_id, 32) {
        return None;
    }
    let remaining = parts.collect::<Vec<_>>();
    let asset_name = match remaining.as_slice() {
        [] => None,
        ["file"] => Some("file".to_owned()),
        ["hls", name] if is_hls_name(name) => Some((*name).to_owned()),
        ["subtitles", name] if is_subtitle_name(name) => Some((*name).to_owned()),
        _ => return None,
    };
    Some(BrowserMediaRoute {
        session_id,
        asset_name,
    })
}

fn parse_external_media_route(path: &str) -> Option<ExternalMediaRoute> {
    let rest = path.strip_prefix("/api/media/external/")?;
    let mut parts = rest.split('/');
    let access_token = parts.next()?.to_owned();
    if !valid_token(&access_token, 43) || parts.next()? != "sessions" {
        return None;
    }
    let session_id = parts.next()?.to_owned();
    if !valid_token(&session_id, 32) {
        return None;
    }
    let remaining = parts.collect::<Vec<_>>();
    let asset_name = match remaining.as_slice() {
        ["file"] => "file".to_owned(),
        [name] => decode_direct_asset_name(name)?,
        ["hls", name] if is_hls_name(name) => (*name).to_owned(),
        ["subtitles", name] if is_subtitle_name(name) => (*name).to_owned(),
        _ => return None,
    };
    Some(ExternalMediaRoute {
        access_token,
        session_id,
        asset_name,
    })
}

/// 解码外部播放器 URL 末段中的真实 UTF-8 文件名。
fn decode_direct_asset_name(value: &str) -> Option<String> {
    let decoded = percent_decode_str(value).decode_utf8().ok()?.into_owned();
    (!decoded.is_empty()
        && decoded.len() <= 1_024
        && decoded != "."
        && decoded != ".."
        && !decoded
            .chars()
            .any(|character| matches!(character, '/' | '\\' | '\0')))
    .then_some(decoded)
}

fn valid_token(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn is_hls_name(value: &str) -> bool {
    value == "index.m3u8"
        || value
            .strip_prefix("segment-")
            .and_then(|value| value.strip_suffix(".ts"))
            .is_some_and(|value| {
                value.len() == 6 && value.bytes().all(|byte| byte.is_ascii_digit())
            })
}

fn is_subtitle_name(value: &str) -> bool {
    value
        .strip_prefix("subtitle-")
        .and_then(|value| {
            value
                .strip_suffix(".ass")
                .or_else(|| value.strip_suffix(".vtt"))
        })
        .is_some_and(|value| value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_digit()))
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let token = headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    ((40..=80).contains(&token.len())
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
    .then_some(token)
}

fn media_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|value| value.strip_prefix("ani_media_token="))
        .filter(|token| {
            (40..=80).contains(&token.len())
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
}

fn json_response(status: StatusCode, payload: Value) -> Response<Body> {
    response_with_headers(
        status,
        Body::from(payload.to_string()),
        &[
            (CONTENT_TYPE, "application/json; charset=utf-8"),
            (CACHE_CONTROL, "no-store"),
        ],
    )
    .unwrap_or_else(error_response)
}

fn empty_response(status: StatusCode) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(Body::empty())
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn response_with_headers(
    status: StatusCode,
    body: Body,
    headers: &[(axum::http::HeaderName, &str)],
) -> Result<Response<Body>, GatewayHttpError> {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(name, *value);
    }
    builder.body(body).map_err(|_| GatewayHttpError::internal())
}

struct GatewayHttpError {
    status: u16,
    code: &'static str,
    message: String,
}

impl GatewayHttpError {
    fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn internal() -> Self {
        Self::new(500, "INTERNAL_ERROR", "远程服务内部错误")
    }

    fn from_auth(error: RemoteDeviceAuthError) -> Self {
        Self::new(
            if matches!(error, RemoteDeviceAuthError::Persistence(_)) {
                500
            } else {
                400
            },
            error.code(),
            "配对请求失败",
        )
    }

    fn from_rpc(error: RemoteRpcError) -> Self {
        Self::new(error.status, error.code, error.message)
    }

    fn from_media(error: RemoteMediaError) -> Self {
        Self::new(error.status, error.code, error.message)
    }

    fn from_image(error: ImageCacheError) -> Self {
        Self::new(error.status(), error.code, error.message)
    }
}

impl From<serde_json::Error> for GatewayHttpError {
    fn from(_: serde_json::Error) -> Self {
        Self::internal()
    }
}

fn error_response(error: GatewayHttpError) -> Response<Body> {
    let status = StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = json!({ "error": error.message, "code": error.code }).to_string();
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json; charset=utf-8")
        .header(CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use ani_domain::{
        AppSettings, DownloadStatus, DownloadTask, MediaFile, PlaybackCheckpoint,
        TorrentEngineKind, TorrentFile,
    };
    use async_trait::async_trait;
    use tempfile::TempDir;

    use crate::{RemoteMediaRepository, RemoteMediaTools, RemoteRpcHandler, RemoteSecretStore};

    use super::*;

    #[derive(Default)]
    struct MemorySecretStore {
        values: StdMutex<HashMap<String, Vec<u8>>>,
    }

    #[async_trait]
    impl RemoteSecretStore for MemorySecretStore {
        async fn read(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(self
                .values
                .lock()
                .map_err(|error| error.to_string())?
                .get(key)
                .cloned())
        }

        async fn write(&self, key: &str, value: &[u8]) -> Result<(), String> {
            self.values
                .lock()
                .map_err(|error| error.to_string())?
                .insert(key.to_owned(), value.to_vec());
            Ok(())
        }

        async fn delete(&self, key: &str) -> Result<(), String> {
            self.values
                .lock()
                .map_err(|error| error.to_string())?
                .remove(key);
            Ok(())
        }
    }

    struct TestRpcHandler;

    #[async_trait]
    impl RemoteRpcHandler for TestRpcHandler {
        async fn call(&self, method: &str, _args: Vec<Value>) -> Result<Value, String> {
            match method {
                "getDashboard" => Ok(json!({ "dailyReminder": null })),
                _ => Err("测试未装配此方法".to_owned()),
            }
        }
    }

    struct TestMediaRepository {
        task: DownloadTask,
    }

    #[async_trait]
    impl RemoteMediaRepository for TestMediaRepository {
        async fn get_download_task(&self, task_id: &str) -> Result<Option<DownloadTask>, String> {
            Ok((self.task.id == task_id).then(|| self.task.clone()))
        }

        async fn list_media_files(&self) -> Result<Vec<MediaFile>, String> {
            Ok(Vec::new())
        }

        async fn get_settings(&self) -> Result<AppSettings, String> {
            Ok(json!({ "media": { "videoExtensions": [".mkv"] } }))
        }

        async fn get_playback_checkpoint(
            &self,
            _task_id: &str,
            _file_index: Option<i64>,
        ) -> Result<Option<PlaybackCheckpoint>, String> {
            Ok(None)
        }
    }

    /// 验证单段、后缀、开放结尾和越界 Range 解析。
    #[test]
    fn parses_byte_ranges() {
        assert_eq!(
            parse_byte_range(Some("bytes=10-19"), 100),
            Some(ByteRange { start: 10, end: 19 })
        );
        assert_eq!(
            parse_byte_range(Some("bytes=-10"), 100),
            Some(ByteRange { start: 90, end: 99 })
        );
        assert_eq!(
            parse_byte_range(Some("bytes=95-"), 100),
            Some(ByteRange { start: 95, end: 99 })
        );
        assert_eq!(parse_byte_range(Some("bytes=100-110"), 100), None);
        assert_eq!(parse_byte_range(Some("bytes=0-1,3-4"), 100), None);
    }

    /// 验证远程媒体路由拒绝额外层级和错误票据长度。
    #[test]
    fn parses_fixed_media_routes() {
        let session = "a".repeat(32);
        assert!(parse_browser_media_route(&format!(
            "/api/media/sessions/{session}/hls/segment-000001.ts"
        ))
        .is_some());
        assert!(
            parse_browser_media_route(&format!("/api/media/sessions/{session}/../../secret"))
                .is_none()
        );
        let token = "b".repeat(43);
        assert!(parse_external_media_route(&format!(
            "/api/media/external/{token}/sessions/{session}/file"
        ))
        .is_some());
        let named = parse_external_media_route(&format!(
            "/api/media/external/{token}/sessions/{session}/%E6%B5%8B%E8%AF%95%20SP01.mkv"
        ))
        .expect("named media route");
        assert_eq!(named.asset_name, "测试 SP01.mkv");
    }

    /// 验证 HTTPS 的 HTTP/2 authority 与 HTTP/1.1 Host 都进入同一白名单校验。
    #[test]
    fn resolves_http2_authority_and_http1_host() {
        let http2_request = Request::builder()
            .uri("https://192.168.60.36:18083/api/health")
            .body(Body::empty())
            .expect("HTTP/2 request");
        assert_eq!(
            request_authority(&http2_request),
            Some("192.168.60.36:18083")
        );

        let http1_request = Request::builder()
            .uri("/api/health")
            .header(HOST, "192.168.60.36:18083")
            .body(Body::empty())
            .expect("HTTP/1.1 request");
        assert_eq!(
            request_authority(&http1_request),
            Some("192.168.60.36:18083")
        );

        let allowed = vec!["192.168.60.36".to_owned()];
        assert!(is_trusted_host(
            request_authority(&http2_request),
            18_083,
            &allowed,
            &[]
        ));
        assert!(is_trusted_host(
            request_authority(&http1_request),
            18_083,
            &allowed,
            &[]
        ));
    }

    /// 验证真实 HTTP 监听器上的 PWA、Host 防护、配对、RPC、媒体会话和 Range 闭环。
    #[tokio::test]
    async fn serves_authenticated_rpc_and_media_range() {
        let temporary = TempDir::new().expect("temporary gateway directory");
        let renderer = temporary.path().join("renderer");
        let download = temporary.path().join("download");
        tokio::fs::create_dir_all(renderer.join("assets"))
            .await
            .expect("create renderer directory");
        tokio::fs::create_dir_all(&download)
            .await
            .expect("create download directory");
        tokio::fs::write(
            renderer.join("index.html"),
            b"<!doctype html><html><head><link rel=\"manifest\" href=\"./manifest.webmanifest\"><link rel=\"stylesheet\" href=\"./assets/app.css\"><script>window.__theme=true</script><script type=\"module\" src=\"./assets/app.js\"></script><title>Ani</title></head><body></body></html>",
        )
        .await
        .expect("write renderer");
        tokio::fs::write(renderer.join("assets/app.js"), b"window.__app=true")
            .await
            .expect("write renderer script");
        tokio::fs::write(renderer.join("assets/app.css"), b"body{color:black}")
            .await
            .expect("write renderer style");
        tokio::fs::write(renderer.join("manifest.webmanifest"), b"{}")
            .await
            .expect("write renderer manifest");
        tokio::fs::write(
            renderer.join("sw.js"),
            b"self.addEventListener('fetch',()=>{})",
        )
        .await
        .expect("write renderer service worker");
        let media_bytes = b"0123456789";
        tokio::fs::write(download.join("episode-01.mkv"), media_bytes)
            .await
            .expect("write media");

        let task = DownloadTask {
            id: "task-1".to_owned(),
            release_id: None,
            anime_id: Some("anime-1".to_owned()),
            episode_id: Some("episode-1".to_owned()),
            anime_title: Some("测试番剧".to_owned()),
            episode_no: Some(1.0),
            fansub_group_id: None,
            fansub_name: None,
            resolution: Some("1080p".to_owned()),
            declared_video_codec: None,
            normalized_video_codec: None,
            bit_depth: None,
            subtitle_languages: Vec::new(),
            subtitle: None,
            correlation_tag: None,
            engine: TorrentEngineKind::Embedded,
            torrent_hash: None,
            name: "episode-01".to_owned(),
            status: DownloadStatus::Completed,
            progress: 1.0,
            download_speed: 0,
            upload_speed: 0,
            eta_seconds: None,
            save_path: download.to_string_lossy().into_owned(),
            files: vec![TorrentFile {
                id: "file-0".to_owned(),
                index: 0,
                name: "episode-01.mkv".to_owned(),
                episode_id: Some("episode-1".to_owned()),
                episode_no: Some(1.0),
                size: media_bytes.len() as i64,
                progress: 1.0,
                priority: 1,
                selected: true,
            }],
            created_at: "2026-07-25T12:00:00.000Z".to_owned(),
            completed_at: Some("2026-07-25T12:10:00.000Z".to_owned()),
        };
        let secret_store: Arc<dyn RemoteSecretStore> = Arc::new(MemorySecretStore::default());
        let auth = Arc::new(RemoteDeviceAuth::new(Arc::clone(&secret_store)));
        let rpc = Arc::new(RemoteRpcService::new(Arc::new(TestRpcHandler)));
        let media = Arc::new(RemoteMediaSessionService::new(
            Arc::new(TestMediaRepository { task }),
            RemoteMediaTools {
                ffprobe_paths: Vec::new(),
                ffmpeg_path: PathBuf::from("ffmpeg"),
                timeout: Duration::from_secs(1),
                rife_sidecar_root: None,
                rife_available_vram_bytes: 0,
            },
            temporary.path().join("sessions"),
        ));
        let image_cache = Arc::new(
            ImageCache::new(temporary.path().join("images"), vec![7_u8; 32]).expect("image cache"),
        );
        let tls_store = Arc::new(RemoteTlsCertificateStore::new(
            temporary.path().join("tls"),
            secret_store,
        ));
        let gateway = RemoteGateway::new(RemoteGatewayDependencies {
            auth,
            rpc,
            media,
            image_cache,
            tls_store,
            renderer_directory: renderer,
            trusted_origins: Vec::new(),
        });
        let port = available_loopback_port();
        gateway
            .apply_config(GatewayConfig::new(false, port).expect("gateway config"))
            .await
            .expect("start gateway");

        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("http client");
        let base_url = format!("http://127.0.0.1:{port}");
        let renderer_response = client
            .get(format!("{base_url}/player/task-1"))
            .send()
            .await
            .expect("renderer request");
        assert_eq!(renderer_response.status(), reqwest::StatusCode::OK);
        let content_security_policy = renderer_response
            .headers()
            .get(reqwest::header::CONTENT_SECURITY_POLICY)
            .and_then(|value| value.to_str().ok())
            .expect("renderer csp")
            .to_owned();
        let renderer_html = renderer_response.text().await.expect("renderer body");
        let script_nonce = renderer_html
            .split("<script nonce=\"")
            .nth(1)
            .and_then(|value| value.split('\"').next())
            .expect("inline script nonce");
        assert!(renderer_html.contains("<base href=\"/\" />"));
        assert!(renderer_html.contains("<title>Ani</title>"));
        assert!(content_security_policy.contains(&format!("'nonce-{script_nonce}'")));
        assert!(!content_security_policy.contains("script-src 'self' 'unsafe-inline'"));

        for asset_path in ["/assets/app.js", "/assets/app.css", "/manifest.webmanifest"] {
            let response = client
                .get(format!("{base_url}{asset_path}"))
                .send()
                .await
                .expect("renderer asset request");
            assert_eq!(response.status(), reqwest::StatusCode::OK, "{asset_path}");
        }
        let service_worker_response = client
            .get(format!("{base_url}/sw.js"))
            .send()
            .await
            .expect("renderer service worker request");
        assert_eq!(service_worker_response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            service_worker_response
                .headers()
                .get(reqwest::header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-cache")
        );
        let missing_nested_asset = client
            .get(format!("{base_url}/player/assets/app.js"))
            .send()
            .await
            .expect("missing nested asset request");
        assert_eq!(
            missing_nested_asset.status(),
            reqwest::StatusCode::NOT_FOUND
        );

        let forbidden = client
            .get(format!("{base_url}/api/health"))
            .header(reqwest::header::HOST, "attacker.invalid")
            .send()
            .await
            .expect("forbidden host request");
        assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);

        let pairing = gateway
            .create_pairing_code()
            .await
            .expect("pairing challenge");
        let paired = client
            .post(format!("{base_url}/api/pair"))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(json!({ "code": pairing.code, "deviceName": "集成测试设备" }).to_string())
            .send()
            .await
            .expect("pair request");
        assert_eq!(paired.status(), reqwest::StatusCode::OK);
        let paired: Value =
            serde_json::from_str(&paired.text().await.expect("pair body")).expect("pair response");
        let token = paired["token"].as_str().expect("access token");

        let rejected_enhancement = client
            .post(format!("{base_url}/api/media/sessions"))
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(
                json!({
                    "taskId": "task-1",
                    "mode": "direct",
                    "fileIndex": 0,
                    "enhancement": {
                        "videoEnhancement": "clear",
                        "frameInterpolation": "off"
                    }
                })
                .to_string(),
            )
            .send()
            .await
            .expect("invalid direct enhancement request");
        assert_eq!(
            rejected_enhancement.status(),
            reqwest::StatusCode::BAD_REQUEST
        );
        let rejected_enhancement: Value = serde_json::from_str(
            &rejected_enhancement
                .text()
                .await
                .expect("invalid direct enhancement body"),
        )
        .expect("invalid direct enhancement response");
        assert_eq!(
            rejected_enhancement["code"],
            "MEDIA_ENHANCEMENT_REQUIRES_TRANSCODE"
        );

        let rpc_response = client
            .post(format!("{base_url}/api/rpc"))
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(json!({ "method": "getDashboard", "args": [] }).to_string())
            .send()
            .await
            .expect("rpc request");
        assert_eq!(rpc_response.status(), reqwest::StatusCode::OK);
        let rpc_result: Value = serde_json::from_str(&rpc_response.text().await.expect("rpc body"))
            .expect("rpc response");
        assert!(rpc_result["result"].is_object());

        let session_response = client
            .post(format!("{base_url}/api/media/sessions"))
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(json!({ "taskId": "task-1", "mode": "direct", "fileIndex": 0 }).to_string())
            .send()
            .await
            .expect("media session request");
        assert_eq!(session_response.status(), reqwest::StatusCode::OK);
        let session: Value =
            serde_json::from_str(&session_response.text().await.expect("media session body"))
                .expect("media session response");
        let stream_url = session["streamUrl"].as_str().expect("stream url");
        let range_response = client
            .get(format!("{base_url}{stream_url}"))
            .bearer_auth(token)
            .header(reqwest::header::RANGE, "bytes=2-5")
            .send()
            .await
            .expect("range request");
        assert_eq!(
            range_response.status(),
            reqwest::StatusCode::PARTIAL_CONTENT
        );
        assert_eq!(
            range_response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok()),
            Some("bytes 2-5/10")
        );
        assert_eq!(
            range_response
                .headers()
                .get(reqwest::header::ACCEPT_RANGES)
                .and_then(|value| value.to_str().ok()),
            Some("bytes")
        );
        assert_eq!(
            range_response
                .headers()
                .get(reqwest::header::CONTENT_DISPOSITION)
                .and_then(|value| value.to_str().ok()),
            Some("inline; filename*=UTF-8''episode%2D01%2Emkv")
        );
        assert_eq!(
            range_response.bytes().await.expect("range body").as_ref(),
            b"2345"
        );

        let external_session_response = client
            .post(format!("{base_url}/api/media/external-sessions"))
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(json!({ "taskId": "task-1", "mode": "direct", "fileIndex": 0 }).to_string())
            .send()
            .await
            .expect("external media session request");
        assert_eq!(external_session_response.status(), reqwest::StatusCode::OK);
        let external_session: Value = serde_json::from_str(
            &external_session_response
                .text()
                .await
                .expect("external media session body"),
        )
        .expect("external media session response");
        let external_stream_url = external_session["streamUrl"]
            .as_str()
            .expect("external stream url");
        assert!(external_stream_url.ends_with("/episode-01.mkv"));
        let external_head = client
            .head(format!("{base_url}{external_stream_url}"))
            .send()
            .await
            .expect("external media head request");
        assert_eq!(external_head.status(), reqwest::StatusCode::OK);
        assert_eq!(
            external_head
                .headers()
                .get(reqwest::header::ACCEPT_RANGES)
                .and_then(|value| value.to_str().ok()),
            Some("bytes")
        );

        gateway.stop().await;
    }

    fn available_loopback_port() -> u16 {
        let listener =
            std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve loopback port");
        listener.local_addr().expect("loopback address").port()
    }
}
