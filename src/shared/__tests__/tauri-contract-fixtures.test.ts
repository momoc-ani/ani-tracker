import { strict as assert } from "node:assert";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { test } from "node:test";
import type { PlayerCapabilities, PlayerCommand, PlayerCommandResult, PlayerSnapshot } from "../player-contract";
import { acceptPlayerSnapshot } from "../player-contract";
import type {
  AnimeDetailResult,
  AnimeDiscoveryResult,
  AnimeDiscoverySearchResult,
  AnimeDiscoverySeasonResult,
  AnimeSourceBindingState,
  AnimeSourceCandidate,
  AnimeWatchProgress,
  AutomationRunResult,
  AutomationSchedulerStatus,
  ConfirmAnimeSourceBindingInput,
  DownloadServiceStatus,
  DesktopMediaToolsStatus,
  EmbeddedTorrentCoreStatus,
  EpisodeReleasePreview,
  ImageCacheResolveResult,
  MediaScanResult,
  PlaybackCheckpoint,
  PlayerDetectionResult,
  QbittorrentManagedStatus,
  RemoteGatewayStatus,
  RemotePairingChallenge,
  ReleaseSearchResult,
  RemoveAnimeSourceCandidateMismatchInput,
  ReportAnimeSourceCandidateMismatchInput,
  ReportPlaybackProgressInput,
  RssSubscriptionReleaseResult,
  SavePlaybackCheckpointInput,
  SetAnimeSourceExclusionInput,
  SetAnimeWatchProgressInput,
  SourceSyncRunResult,
  SourceSyncSchedulerStatus,
  TorrentConnectionTestResult,
  RemotePlaybackSession
} from "../contracts";
import type {
  AnimeSourceBinding,
  AnimeSourceExclusion,
  DashboardData,
  Episode,
  EpisodePreference,
  MyAnime,
  NotificationRecord,
  ReleaseSourceConfig,
  ReleaseSourceSyncState,
  RequestCircuitState
} from "../domain";

interface ContractFixture<T> {
  schemaVersion: number;
  kind: string;
  payload: T;
}

/** 读取 P6 外部播放器金样，验证 Tauri 探测结构与 TypeScript 契约一致。 */
test("Tauri P6 外部播放器契约金样可被 TypeScript 接受", () => {
  const fixturePath = resolve("fixtures/contracts/p6-external-player.v1.json");
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as ContractFixture<PlayerDetectionResult>;

  assert.equal(fixture.schemaVersion, 1);
  assert.equal(fixture.kind, "p6-external-player");
  assert.equal(fixture.payload.platform, "windows");
  assert.equal(fixture.payload.candidates[0].available, true);
  assert.equal(fixture.payload.candidates[1].resolvedPath, undefined);
});

/** 读取 P6 远程网关金样，验证状态、配对、图片和媒体会话字段。 */
test("Tauri P6 远程网关契约金样可被 TypeScript 接受", () => {
  const fixturePath = resolve("fixtures/contracts/p6-remote-gateway.v1.json");
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as ContractFixture<{
    gatewayStatus: RemoteGatewayStatus;
    pairingChallenge: RemotePairingChallenge;
    imageCache: ImageCacheResolveResult;
    playbackSession: RemotePlaybackSession;
  }>;

  assert.equal(fixture.schemaVersion, 1);
  assert.equal(fixture.kind, "p6-remote-gateway");
  assert.equal(fixture.payload.gatewayStatus.protocol, "https");
  assert.equal(fixture.payload.gatewayStatus.devices[0]?.lastAccessedAt, null);
  assert.equal(fixture.payload.pairingChallenge.code, "123456");
  assert.equal(fixture.payload.playbackSession.mode, "direct");
  assert.equal(fixture.payload.playbackSession.diagnostics?.enhancedFrameInput, false);
});

/** 读取 P6 桌面功能对等金样，验证采集和单集预览字段。 */
test("Tauri P6 桌面功能对等契约金样可被 TypeScript 接受", () => {
  const fixturePath = resolve("fixtures/contracts/p6-desktop-parity.v1.json");
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as ContractFixture<{
    monthResult: AnimeDiscoveryResult;
    seasonResult: AnimeDiscoverySeasonResult;
    episodePreview: EpisodeReleasePreview;
  }>;

  assert.equal(fixture.schemaVersion, 1);
  assert.equal(fixture.kind, "p6-desktop-parity");
  assert.equal(fixture.payload.monthResult.query.forceRefresh, true);
  assert.equal(fixture.payload.seasonResult.query.season, "summer");
  assert.equal(fixture.payload.episodePreview.candidates[0].score, 95);
});

/** 读取版本化播放器快照金样，验证 TypeScript 与 Rust 共用契约。 */
test("Tauri 播放器快照契约金样可被 TypeScript 接受", () => {
  const fixturePath = resolve("fixtures/contracts/player-snapshot.v1.json");
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as ContractFixture<PlayerSnapshot>;

  assert.equal(fixture.schemaVersion, 1);
  assert.equal(fixture.kind, "player-snapshot");
  assert.equal(fixture.payload.platform, "tauri-desktop");
  assert.equal(fixture.payload.status, "playing");
  assert.equal(fixture.payload.audioTracks.length, 2);
  assert.equal(fixture.payload.subtitleTracks.length, 1);
  assert.equal(fixture.payload.subtitleScale, 150);
  assert.equal(
    acceptPlayerSnapshot(fixture.payload.sessionId, undefined, fixture.payload),
    fixture.payload
  );
});

/** 读取 P2 数据金样，验证 Rust 只读模型与现有 TypeScript 领域契约一致。 */
test("Tauri P2 只读数据契约金样可被 TypeScript 接受", () => {
  const fixturePath = resolve("fixtures/contracts/p2-read-model.v1.json");
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as ContractFixture<{
    notification: NotificationRecord;
    myAnime: MyAnime;
    dashboard: DashboardData;
  }>;

  assert.equal(fixture.schemaVersion, 1);
  assert.equal(fixture.kind, "p2-read-model");
  assert.equal(fixture.payload.notification.kind, "download");
  assert.equal(fixture.payload.myAnime.anime.externalIds.bangumi, "1");
  assert.deepEqual(fixture.payload.myAnime.preferredSubtitleLanguages, ["chs", "cht"]);
  assert.equal(fixture.payload.dashboard.dailyReminder.total, 0);
});

/** 读取 P3 追番写模型金样，验证 Tauri 命令输入输出与前端契约一致。 */
test("Tauri P3 追番写模型契约金样可被 TypeScript 接受", () => {
  const fixturePath = resolve("fixtures/contracts/p3-following-write-model.v1.json");
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as ContractFixture<{
    myAnime: MyAnime;
    episode: Episode;
    preference: EpisodePreference;
    watchProgressInput: SetAnimeWatchProgressInput;
    reportPlaybackProgressInput: ReportPlaybackProgressInput;
    savePlaybackCheckpointInput: SavePlaybackCheckpointInput;
    checkpoint: PlaybackCheckpoint;
  }>;
  const progress: AnimeWatchProgress = {
    animeId: fixture.payload.myAnime.anime.id,
    watchedEpisodeCount: fixture.payload.watchProgressInput.watchedEpisodeCount,
    totalEpisodeCount: fixture.payload.myAnime.anime.detail?.episodeCount ?? 0
  };

  assert.equal(fixture.schemaVersion, 1);
  assert.equal(fixture.kind, "p3-following-write-model");
  assert.equal(fixture.payload.episode.animeId, fixture.payload.myAnime.anime.id);
  assert.equal(fixture.payload.preference.episodeId, fixture.payload.episode.id);
  assert.equal(fixture.payload.reportPlaybackProgressInput.percent, 92);
  assert.equal(fixture.payload.savePlaybackCheckpointInput.fileIndex, 0);
  assert.equal(fixture.payload.savePlaybackCheckpointInput.completed, true);
  assert.equal(fixture.payload.checkpoint.completed, true);
  assert.equal(fixture.payload.checkpoint.watchedReported, true);
  assert.equal(progress.totalEpisodeCount, 12);
});

/** 读取 P3 目录与详情金样，验证 Rust 聚合结果和前端契约一致。 */
test("Tauri P3 番剧目录契约金样可被 TypeScript 接受", () => {
  const fixturePath = resolve("fixtures/contracts/p3-catalog-read-model.v1.json");
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as ContractFixture<{
    searchResult: AnimeDiscoverySearchResult;
    detailResult: AnimeDetailResult;
  }>;

  assert.equal(fixture.schemaVersion, 1);
  assert.equal(fixture.kind, "p3-catalog-read-model");
  assert.equal(fixture.payload.searchResult.source, "local");
  assert.equal(fixture.payload.searchResult.items[0].externalIds.bangumi, "catalog-contract-1");
  assert.equal(fixture.payload.detailResult.anime.id, fixture.payload.searchResult.items[0].id);
  assert.equal(fixture.payload.detailResult.stale, false);
});

/** 读取 P3 来源网络金样，验证配置、游标与熔断状态字段一致。 */
test("Tauri P3 来源网络契约金样可被 TypeScript 接受", () => {
  const fixturePath = resolve("fixtures/contracts/p3-source-network-model.v1.json");
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as ContractFixture<{
    source: ReleaseSourceConfig;
    syncState: ReleaseSourceSyncState;
    circuitState: RequestCircuitState;
  }>;

  assert.equal(fixture.schemaVersion, 1);
  assert.equal(fixture.kind, "p3-source-network-model");
  assert.equal(fixture.payload.source.kind, "torznab");
  assert.equal(fixture.payload.source.requestIntervalMs, 1_750);
  assert.equal(fixture.payload.syncState.requestFailureCount, 2);
  assert.equal(fixture.payload.circuitState.key, `release-source:${fixture.payload.source.id}`);
  assert.equal(fixture.payload.circuitState.networkContext, "fixture-network");
});

/** 读取 P3 来源同步金样，验证调度器和执行结果字段一致。 */
test("Tauri P3 来源同步契约金样可被 TypeScript 接受", () => {
  const fixturePath = resolve("fixtures/contracts/p3-source-sync-model.v1.json");
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as ContractFixture<{
    syncState: ReleaseSourceSyncState;
    runResult: SourceSyncRunResult;
    schedulerStatus: SourceSyncSchedulerStatus;
  }>;

  assert.equal(fixture.schemaVersion, 1);
  assert.equal(fixture.kind, "p3-source-sync-model");
  assert.equal(fixture.payload.syncState.etag, "\"release-v1\"");
  assert.equal(fixture.payload.runResult.addedReleaseCount, 2);
  assert.deepEqual(fixture.payload.schedulerStatus.lastResult, fixture.payload.runResult);
});

/** 读取 P3 自动扫描金样，验证下载、跳过和调度状态字段一致。 */
test("Tauri P3 自动扫描契约金样可被 TypeScript 接受", () => {
  const fixturePath = resolve("fixtures/contracts/p3-automation-model.v1.json");
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as ContractFixture<{
    runResult: AutomationRunResult;
    schedulerStatus: AutomationSchedulerStatus;
  }>;

  assert.equal(fixture.schemaVersion, 1);
  assert.equal(fixture.kind, "p3-automation-model");
  assert.equal(fixture.payload.runResult.downloaded[0].downloadTaskId, "download-automation-1");
  assert.equal(fixture.payload.runResult.skipped[0].reason, "已有下载任务");
  assert.deepEqual(fixture.payload.schedulerStatus.lastResult, fixture.payload.runResult);
});

/** 读取 P3 来源绑定金样，验证绑定、候选、排除和命令输入字段一致。 */
test("Tauri P3 来源绑定契约金样可被 TypeScript 接受", () => {
  const fixturePath = resolve("fixtures/contracts/p3-source-binding-model.v1.json");
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as ContractFixture<{
    binding: AnimeSourceBinding;
    exclusion: AnimeSourceExclusion;
    candidate: AnimeSourceCandidate;
    state: AnimeSourceBindingState;
    confirmInput: ConfirmAnimeSourceBindingInput;
    mismatchInput: ReportAnimeSourceCandidateMismatchInput;
    setExclusionInput: SetAnimeSourceExclusionInput;
    removeMismatchInput: RemoveAnimeSourceCandidateMismatchInput;
  }>;

  assert.equal(fixture.schemaVersion, 1);
  assert.equal(fixture.kind, "p3-source-binding-model");
  assert.equal(fixture.payload.binding.matchMethod, "external_id");
  assert.equal(fixture.payload.exclusion.scope, "candidate");
  assert.equal(fixture.payload.candidate.score, 94);
  assert.equal(fixture.payload.state.excludedSources[0].sourceId, "mikan-contract");
  assert.equal(fixture.payload.confirmInput.confidence, 0.94);
  assert.equal(fixture.payload.mismatchInput.sourceAnimeId, "999999");
  assert.equal(fixture.payload.setExclusionInput.excluded, true);
  assert.equal(fixture.payload.removeMismatchInput.sourceAnimeId, "999999");
});

/** 读取 P3 资源搜索金样，验证聚合结果、单源错误和 RSS 字段一致。 */
test("Tauri P3 资源搜索契约金样可被 TypeScript 接受", () => {
  const fixturePath = resolve("fixtures/contracts/p3-release-search-model.v1.json");
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as ContractFixture<{
    searchResult: ReleaseSearchResult;
    rssResult: RssSubscriptionReleaseResult;
  }>;

  assert.equal(fixture.schemaVersion, 1);
  assert.equal(fixture.kind, "p3-release-search-model");
  assert.equal(fixture.payload.searchResult.releases[0].episodeNo, 3);
  assert.equal(fixture.payload.searchResult.errors[0].sourceId, "broken-contract");
  assert.equal(fixture.payload.rssResult.query.subscriptionId, "rss-subscription-contract");
});

/** 读取 P4 下载服务金样，验证统一状态、托管进程与内置核心字段一致。 */
test("Tauri P4 下载服务契约金样可被 TypeScript 接受", () => {
  const fixturePath = resolve("fixtures/contracts/p4-download-service-model.v1.json");
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as ContractFixture<{
    serviceStatus: DownloadServiceStatus;
    connectionTest: TorrentConnectionTestResult;
    managedStatus: QbittorrentManagedStatus;
    embeddedStatus: EmbeddedTorrentCoreStatus;
  }>;

  assert.equal(fixture.schemaVersion, 1);
  assert.equal(fixture.kind, "p4-download-service-model");
  assert.equal(fixture.payload.serviceStatus.mode, "managed");
  assert.equal(fixture.payload.connectionTest.taskCount, 2);
  assert.equal(fixture.payload.managedStatus.webUiUrl, "http://127.0.0.1:18080/");
  assert.equal(fixture.payload.embeddedStatus.listenPort, 6881);
});

/** 读取 P4 媒体金样，验证工具状态、媒体记录和扫描结果字段一致。 */
test("Tauri P4 媒体扫描契约金样可被 TypeScript 接受", () => {
  const fixturePath = resolve("fixtures/contracts/p4-media-model.v1.json");
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as ContractFixture<{
    mediaToolsStatus: DesktopMediaToolsStatus;
    scanResult: MediaScanResult;
  }>;

  assert.equal(fixture.schemaVersion, 1);
  assert.equal(fixture.kind, "p4-media-model");
  assert.equal(fixture.payload.mediaToolsStatus.ffprobe.available, true);
  assert.equal(fixture.payload.scanResult.mediaFiles[0].normalizedVideoCodec, "H.265/HEVC");
  assert.equal(fixture.payload.scanResult.skippedFiles[0].reason, "非视频文件");
});

/** 验证移动 torrent-core 生命周期和桌面使用同一状态契约。 */
test("Tauri P4 移动 torrent-core 生命周期金样可被 TypeScript 接受", () => {
  const fixturePath = resolve("fixtures/contracts/p4-mobile-torrent-lifecycle.v1.json");
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as ContractFixture<{
    androidStatus: EmbeddedTorrentCoreStatus;
    iosStatus: EmbeddedTorrentCoreStatus;
    executeRequest: { id: string; method: string; params: Record<string, never> };
    executeResponse: { id: string; ok: string; result: { tasks: string } };
  }>;

  assert.equal(fixture.schemaVersion, 1);
  assert.equal(fixture.kind, "p4-mobile-torrent-lifecycle");
  assert.equal(fixture.payload.androidStatus.foregroundService, true);
  assert.equal(fixture.payload.iosStatus.foregroundService, false);
  assert.equal(fixture.payload.executeRequest.method, "listTasks");
  assert.equal(fixture.payload.executeResponse.ok, "true");
});

/** 验证 P5 播放命令、结构化错误和受控会话的跨语言字段。 */
test("Tauri P5 播放器命令契约金样可被 TypeScript 接受", () => {
  const fixturePath = resolve("fixtures/contracts/p5-player-command.v1.json");
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as ContractFixture<{
    loadCommand: PlayerCommand;
    rejectedResult: PlayerCommandResult;
    subtitleScaleCommand: PlayerCommand;
    frameInterpolationCommand: PlayerCommand;
    hdrCommand: PlayerCommand;
    androidCapabilities: PlayerCapabilities;
    iosCapabilities: PlayerCapabilities;
    playbackSession: RemotePlaybackSession;
  }>;

  assert.equal(fixture.schemaVersion, 1);
  assert.equal(fixture.kind, "p5-player-command");
  assert.equal(fixture.payload.loadCommand.type, "load");
  if (fixture.payload.loadCommand.type !== "load") {
    assert.fail("播放器金样必须包含 load 命令");
  }
  assert.equal(fixture.payload.loadCommand.source.subtitles[0].type, "ass");
  assert.equal(fixture.payload.rejectedResult.accepted, false);
  assert.equal(fixture.payload.subtitleScaleCommand.type, "set-subtitle-scale");
  assert.equal(fixture.payload.frameInterpolationCommand.type, "set-frame-interpolation");
  if (fixture.payload.frameInterpolationCommand.type === "set-frame-interpolation") {
    assert.equal(fixture.payload.frameInterpolationCommand.frameInterpolation, "rife-realtime");
  }
  assert.equal(fixture.payload.hdrCommand.type, "set-hdr");
  if (fixture.payload.hdrCommand.type === "set-hdr") {
    assert.equal(fixture.payload.hdrCommand.hdr, "auto");
  }
  assert.equal(fixture.payload.androidCapabilities.platform, "android");
  assert.equal(fixture.payload.iosCapabilities.platform, "ios");
  assert.equal(fixture.payload.iosCapabilities.supportsTranscodingFallback, false);
  assert.equal(fixture.payload.iosCapabilities.supportsHdr, false);
  assert.equal(fixture.payload.playbackSession.mode, "direct");
  assert.equal(fixture.payload.playbackSession.diagnostics?.enhancedFrameInput, false);
});
