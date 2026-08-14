import Artplayer from "artplayer";
import Hls from "hls.js";
import {
  createInitialPlayerSnapshot,
  PLAYER_SUBTITLE_SCALES,
  rejectUnsupportedPlayerCommand,
  type PlayerAspectRatio,
  type PlayerCapabilities,
  type PlayerCommand,
  type PlayerCommandResult,
  type PlayerError,
  type PlayerMediaSource,
  type PlayerSnapshot,
  type PlayerSubtitleScale,
  type UnifiedPlayerAdapter
} from "@shared/player-contract";

const ART_PLAYER_CAPABILITIES: PlayerCapabilities = {
  backend: "artplayer",
  platform: "remote-web",
  availability: "available",
  canSeek: true,
  canSetVolume: true,
  canMute: true,
  playbackRates: [0.5, 0.75, 1, 1.25, 1.5, 2],
  supportsAudioTracks: false,
  supportsSubtitleTracks: true,
  supportsSubtitleScale: true,
  supportsVideoEnhancement: false,
  supportsFrameInterpolation: false,
  supportsModelEnhancement: false,
  supportsAspectRatio: true,
  supportsFullscreen: true,
  supportsPictureInPicture: true,
  supportsPlaylistNavigation: false,
  supportsDirectPlayback: true,
  supportsTranscodingFallback: true,
  supportsHdr: false
};

// 远程点播列表在转码完成前属于 EVENT 流，必须把同步点固定在列表起点，避免追赶快速增长的直播边缘。
const REMOTE_HLS_LIVE_SYNC_DURATION_COUNT = 1_000_000;

export interface ArtPlayerAdapterOptions {
  container: HTMLDivElement;
  sessionId: string;
  baseUrl?: string;
  subtitleScale?: PlayerSubtitleScale;
}

/** 将 ArtPlayer/HLS 映射为跨平台统一播放器契约。 */
export class ArtPlayerAdapter implements UnifiedPlayerAdapter {
  private readonly listeners = new Set<(snapshot: PlayerSnapshot) => void>();
  private snapshot: PlayerSnapshot;
  private player?: Artplayer;
  private hls?: Hls;
  private source?: PlayerMediaSource;
  private sequence = 0;
  private disposed = false;

  constructor(private readonly options: ArtPlayerAdapterOptions) {
    this.snapshot = {
      ...createInitialPlayerSnapshot({
        sessionId: options.sessionId,
        capabilities: ART_PLAYER_CAPABILITIES
      }),
      subtitleScale: options.subtitleScale ?? 100
    };
  }

  /** 返回远程网页后端稳定支持的能力。 */
  getCapabilities(): PlayerCapabilities {
    return ART_PLAYER_CAPABILITIES;
  }

  /** 返回当前完整状态，调用方无需读取 ArtPlayer 实例。 */
  getSnapshot(): PlayerSnapshot {
    return this.snapshot;
  }

  /** 判断绝对媒体时间是否位于当前浏览器可跳转范围。 */
  canSeekTo(positionSeconds: number): boolean {
    if (!this.player || !this.source || !Number.isFinite(positionSeconds) || positionSeconds < 0) {
      return false;
    }
    if (this.source.mode !== "hls") return positionSeconds <= this.resolveDurationSeconds();
    const localPositionSeconds = positionSeconds - this.timelineOffsetSeconds();
    if (localPositionSeconds < 0) return false;
    return timeRangesContain(this.player.template.$video.seekable, localPositionSeconds);
  }

  /** 订阅完整快照，并立即收到当前状态。 */
  subscribe(listener: (snapshot: PlayerSnapshot) => void): () => void {
    this.listeners.add(listener);
    listener(this.snapshot);
    return () => this.listeners.delete(listener);
  }

  /** 校验会话后把统一命令映射到 ArtPlayer。 */
  async dispatch(command: PlayerCommand): Promise<PlayerCommandResult> {
    if (this.disposed) return reject(command, createPlayerError("unknown", "播放器已经关闭", false, []));
    if (command.sessionId !== this.snapshot.sessionId) {
      return reject(command, createPlayerError("unknown", "播放器会话已切换", false, []));
    }

    try {
      switch (command.type) {
        case "load":
          await this.load(command.source, command.startPositionSeconds);
          break;
        case "play":
          if (!this.player) return rejectNotReady(command);
          await this.player.play();
          break;
        case "pause":
          if (!this.player) return rejectNotReady(command);
          this.player.pause();
          break;
        case "seek":
          if (!this.player || !isFiniteRange(command.positionSeconds, 0, Number.MAX_SAFE_INTEGER)) {
            return reject(command, createPlayerError("unknown", "跳转时间无效", false, []));
          }
          if (this.source?.mode === "hls" && !this.canSeekTo(command.positionSeconds)) {
            return reject(command, createPlayerError("resource-unavailable", "目标时间不在当前转码窗口", true, ["retry"]));
          }
          this.player.currentTime = clamp(
            this.toLocalPosition(command.positionSeconds),
            0,
            this.resolveLocalDurationSeconds()
          );
          break;
        case "set-volume":
          if (!this.player || !isFiniteRange(command.volume, 0, 1)) {
            return reject(command, createPlayerError("unknown", "音量参数无效", false, []));
          }
          this.player.muted = false;
          this.player.volume = command.volume;
          break;
        case "set-muted":
          if (!this.player || typeof command.muted !== "boolean") return rejectNotReady(command);
          this.player.muted = command.muted;
          break;
        case "set-rate":
          if (!this.player || !ART_PLAYER_CAPABILITIES.playbackRates.includes(command.rate)) {
            return reject(command, createPlayerError("unknown", "播放倍速无效", false, []));
          }
          this.player.playbackRate = command.rate;
          break;
        case "select-audio-track":
          return rejectUnsupportedPlayerCommand(command.commandId, "远程网页暂不支持切换音轨");
        case "select-subtitle-track":
          if (!this.player) return rejectNotReady(command);
          await this.selectSubtitle(command.trackId);
          break;
        case "set-subtitle-scale":
          if (!this.player || !PLAYER_SUBTITLE_SCALES.includes(command.subtitleScale)) {
            return reject(command, createPlayerError("unknown", "字幕缩放比例无效", false, []));
          }
          this.setSubtitleScale(command.subtitleScale);
          break;
        case "set-video-enhancement":
          return rejectUnsupportedPlayerCommand(command.commandId, "网页播放器不支持 GPU 画质增强");
        case "set-frame-interpolation":
          return rejectUnsupportedPlayerCommand(command.commandId, "网页播放器不支持模型补帧");
        case "set-hdr":
          return rejectUnsupportedPlayerCommand(command.commandId, "网页播放器不支持 HDR 输出");
        case "set-aspect-ratio":
          if (!this.player || !isValidAspectRatio(command.aspectRatio, command.value)) {
            return reject(command, createPlayerError("unknown", "画面比例无效", false, []));
          }
          this.setAspectRatio(command.aspectRatio, command.value);
          break;
        case "set-fullscreen":
          if (!this.player || typeof command.fullscreen !== "boolean") return rejectNotReady(command);
          this.player.fullscreen = command.fullscreen;
          this.patch({ fullscreen: command.fullscreen });
          break;
        case "set-picture-in-picture":
          if (!this.player || typeof command.enabled !== "boolean") return rejectNotReady(command);
          this.player.pip = command.enabled;
          this.patch({ pictureInPicture: command.enabled });
          break;
        case "previous-item":
        case "next-item":
          return rejectUnsupportedPlayerCommand(command.commandId, "播放列表切换由页面会话管理");
        case "retry":
          if (!this.source) return rejectNotReady(command);
          await this.load(this.source, this.snapshot.positionSeconds);
          break;
        case "close":
          await this.dispose();
          break;
      }
      return { commandId: command.commandId, accepted: true };
    } catch (error) {
      const playerError = createPlayerError(
        "unknown",
        error instanceof Error ? error.message : "播放器命令执行失败",
        true,
        ["retry", "close"]
      );
      this.patch({ status: "error", error: playerError });
      return reject(command, playerError);
    }
  }

  /** 幂等销毁 HLS、ArtPlayer 与订阅者。 */
  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    this.disposePlayer();
    this.patch({ status: "closed" });
    this.listeners.clear();
  }

  /** 创建 ArtPlayer，并把媒体事件转换为统一快照。 */
  private async load(source: PlayerMediaSource, startPositionSeconds?: number): Promise<void> {
    if (!isValidSource(source)) throw new Error("远程媒体资源参数无效");
    this.disposePlayer();
    this.source = source;
    const streamUrl = new URL(source.uri, this.options.baseUrl ?? window.location.origin).toString();
    this.sequence += 1;
    this.snapshot = {
      ...createInitialPlayerSnapshot({
        sessionId: this.options.sessionId,
        capabilities: ART_PLAYER_CAPABILITIES
      }),
      sequence: this.sequence,
      status: "loading",
      source,
      positionSeconds: startPositionSeconds ?? source.streamStartPositionSeconds ?? 0,
      durationSeconds: source.durationSeconds ?? 0,
      volume: this.snapshot.volume,
      muted: this.snapshot.muted,
      subtitleScale: this.snapshot.subtitleScale,
      subtitleTracks: source.subtitles.map((subtitle) => ({
        id: subtitle.id,
        kind: "subtitle",
        label: subtitle.label,
        language: subtitle.language,
        selected: false
      }))
    };
    this.publish();

    let player: Artplayer;
    player = new Artplayer({
      container: this.options.container,
      url: streamUrl,
      ...(source.mode === "hls" ? { type: "m3u8" as const } : {}),
      lang: "zh-cn",
      autoplay: true,
      volume: this.snapshot.volume,
      muted: this.snapshot.muted,
      setting: false,
      subtitleOffset: false,
      playbackRate: false,
      aspectRatio: false,
      pip: false,
      airplay: false,
      fullscreen: false,
      fullscreenWeb: false,
      hotkey: false,
      mutex: true,
      playsInline: true,
      lock: false,
      fastForward: false,
      customType: {
        m3u8: (video, url, art) => this.attachHls(video, url, art, startPositionSeconds)
      }
    }, () => {
      if (this.player !== player || this.disposed) return;
      if (startPositionSeconds !== undefined && Number.isFinite(startPositionSeconds)) {
        player.currentTime = clamp(
          this.toLocalPosition(startPositionSeconds),
          0,
          this.resolveLocalDurationSeconds()
        );
      }
      this.patch({
        status: "ready",
        positionSeconds: this.toAbsolutePosition(player.currentTime),
        durationSeconds: this.resolveDurationSeconds(),
        error: undefined
      });
    });
    this.player = player;
    this.applySubtitleScale(player, this.snapshot.subtitleScale);
    this.bindPlayerEvents(player);

    const defaultSubtitle = source.subtitles.find((subtitle) => subtitle.default) ?? source.subtitles[0];
    if (defaultSubtitle) {
      try {
        await this.switchSubtitle(player, defaultSubtitle);
        this.markSelectedSubtitle(defaultSubtitle.id);
      } catch (error) {
        console.warn("[remote] ArtPlayer 默认字幕加载失败", {
          subtitleId: defaultSubtitle.id,
          error
        });
      }
    }
  }

  /** 优先挂接 hls.js，并在浏览器不支持 MSE 时回退原生 HLS。 */
  private attachHls(
    video: HTMLVideoElement,
    url: string,
    art: Artplayer,
    startPositionSeconds?: number
  ): void {
    if (!Hls.isSupported()) {
      if (video.canPlayType("application/vnd.apple.mpegurl")) {
        video.src = url;
        return;
      }
      this.fail("unsupported", "当前浏览器不支持 HLS 实时转码播放");
      return;
    }
    this.hls?.destroy();
    const startPosition = startPositionSeconds !== undefined && Number.isFinite(startPositionSeconds)
      ? this.toLocalPosition(startPositionSeconds)
      : 0;
    const hls = new Hls({
      enableWorker: false,
      startPosition,
      liveSyncDurationCount: REMOTE_HLS_LIVE_SYNC_DURATION_COUNT,
      maxLiveSyncPlaybackRate: 1
    });
    this.hls = hls;
    art.hls = hls;
    hls.on(Hls.Events.ERROR, (_event, data) => {
      if (!data.fatal) return;
      console.error("[remote] HLS 播放发生致命错误", {
        type: data.type,
        details: data.details,
        reason: data.reason ?? data.error.message,
        responseCode: data.response?.code,
        url: data.url
      });
      this.fail("network", "实时转码视频流中断，请重试");
    });
    hls.attachMedia(video);
    hls.loadSource(url);
  }

  /** 监听 ArtPlayer 的 video/fullscreen/pip 事件。 */
  private bindPlayerEvents(player: Artplayer): void {
    const active = () => this.player === player && !this.disposed;
    player.on("video:error", () => {
      if (!active()) return;
      const video = player.template.$video;
      console.error("[remote] 浏览器媒体元素播放失败", {
        code: video.error?.code,
        message: video.error?.message,
        currentSrc: video.currentSrc,
        readyState: video.readyState,
        networkState: video.networkState,
        mode: this.source?.mode
      });
      this.fail(
        this.source?.mode === "direct" ? "decoder" : "network",
        this.source?.mode === "direct"
          ? "浏览器无法解码当前原文件"
          : "浏览器无法播放当前转码视频流，请重试"
      );
    });
    player.on("video:play", () => active() && this.patch({ status: "playing" }));
    player.on("video:pause", () => active() && this.patch({ status: "paused" }));
    player.on("video:playing", () => active() && this.patch({ status: "playing", error: undefined }));
    player.on("video:waiting", () => active() && this.patch({ status: "buffering" }));
    player.on("video:stalled", () => active() && this.patch({ status: "buffering" }));
    player.on("video:loadedmetadata", () => active() && this.patch({
      positionSeconds: this.toAbsolutePosition(player.currentTime),
      durationSeconds: this.resolveDurationSeconds()
    }));
    player.on("video:progress", () => active() && this.patch({
      bufferedSeconds: this.toAbsolutePosition(player.loadedTime || 0)
    }));
    player.on("video:volumechange", () => active() && this.patch({
      volume: player.volume,
      muted: player.muted
    }));
    player.on("video:ratechange", () => active() && this.patch({ playbackRate: player.playbackRate }));
    player.on("fullscreen", (fullscreen: boolean) => active() && this.patch({ fullscreen }));
    player.on("fullscreenWeb", (fullscreen: boolean) => active() && this.patch({ fullscreen }));
    player.on("pip", (pictureInPicture: boolean) => active() && this.patch({ pictureInPicture }));
    player.on("video:timeupdate", () => active() && this.patch({
      positionSeconds: this.toAbsolutePosition(player.currentTime || 0),
      durationSeconds: this.resolveDurationSeconds(),
      bufferedSeconds: this.toAbsolutePosition(player.loadedTime || 0)
    }));
    player.on("video:ended", () => active() && this.patch({
      status: "ended",
      positionSeconds: this.resolveDurationSeconds()
    }));
    player.on("subtitleLoad", (cues: unknown[]) => {
      if (active()) console.info("[remote] ArtPlayer 字幕加载完成", { cueCount: cues.length });
    });
  }

  private async selectSubtitle(subtitleId?: string): Promise<void> {
    const player = this.player!;
    const subtitle = this.source?.subtitles.find((item) => item.id === subtitleId);
    if (!subtitle) {
      player.subtitle.show = false;
      this.markSelectedSubtitle(undefined);
      return;
    }
    await this.switchSubtitle(player, subtitle);
    this.markSelectedSubtitle(subtitle.id);
  }

  private async switchSubtitle(player: Artplayer, subtitle: PlayerMediaSource["subtitles"][number]): Promise<void> {
    const subtitleUrl = new URL(subtitle.uri, this.options.baseUrl ?? window.location.origin).toString();
    await player.subtitle.switch(subtitleUrl, {
      name: subtitle.label,
      type: subtitle.type,
      encoding: "utf-8"
    });
    player.subtitle.show = true;
  }

  private markSelectedSubtitle(subtitleId?: string): void {
    this.patch({
      subtitleTracks: this.snapshot.subtitleTracks.map((track) => ({
        ...track,
        selected: track.id === subtitleId
      }))
    });
  }

  /** 即时调整 ArtPlayer 字幕 CSS 变量并更新统一快照。 */
  private setSubtitleScale(subtitleScale: PlayerSubtitleScale): void {
    this.applySubtitleScale(this.player!, subtitleScale);
    this.patch({ subtitleScale });
    console.info("[remote] ArtPlayer 字幕大小已更新", { subtitleScale });
  }

  /** 将百分比映射到 ArtPlayer 默认 20px 字幕基准。 */
  private applySubtitleScale(player: Artplayer, subtitleScale: PlayerSubtitleScale): void {
    player.template.$player.style.setProperty(
      "--art-subtitle-font-size",
      `${20 * subtitleScale / 100}px`
    );
  }

  /** 将 HLS 窗口内时间转换为整部媒体的绝对时间。 */
  private toAbsolutePosition(localPositionSeconds: number): number {
    const position = localPositionSeconds + this.timelineOffsetSeconds();
    const duration = this.resolveDurationSeconds();
    return clamp(position, 0, duration > 0 ? duration : Number.MAX_SAFE_INTEGER);
  }

  /** 将整部媒体的绝对时间转换为当前 HLS 窗口内时间。 */
  private toLocalPosition(absolutePositionSeconds: number): number {
    return Math.max(0, absolutePositionSeconds - this.timelineOffsetSeconds());
  }

  /** 返回当前流相对整部媒体的时间偏移。 */
  private timelineOffsetSeconds(): number {
    const offset = this.source?.mode === "hls" ? this.source.streamStartPositionSeconds ?? 0 : 0;
    return Number.isFinite(offset) && offset > 0 ? offset : 0;
  }

  /** 总时长优先使用 FFprobe 会话值，避免 HLS EVENT 列表增长改写进度。 */
  private resolveDurationSeconds(): number {
    const duration = this.source?.durationSeconds;
    if (duration !== undefined && Number.isFinite(duration) && duration > 0) return duration;
    const playerDuration = this.player?.duration ?? 0;
    return Number.isFinite(playerDuration) && playerDuration > 0 ? playerDuration : 0;
  }

  /** 返回当前媒体元素使用的局部时长上限。 */
  private resolveLocalDurationSeconds(): number {
    const duration = this.resolveDurationSeconds();
    return this.source?.mode === "hls"
      ? Math.max(0, duration - this.timelineOffsetSeconds())
      : duration;
  }

  private setAspectRatio(aspectRatio: PlayerAspectRatio, customValue?: string): void {
    const player = this.player!;
    player.template.$video.style.objectFit = aspectRatio === "fill" ? "cover" : "contain";
    player.aspectRatio = (aspectRatio === "16:9" || aspectRatio === "4:3"
      ? aspectRatio
      : aspectRatio === "custom"
        ? customValue ?? "default"
        : "default") as typeof player.aspectRatio;
    this.patch({ aspectRatio });
  }

  private fail(code: PlayerError["code"], message: string): void {
    this.patch({
      status: "error",
      error: createPlayerError(code, message, true, ["retry", "transcode", "close"])
    });
  }

  private patch(patch: Partial<PlayerSnapshot>): void {
    this.sequence += 1;
    this.snapshot = {
      ...this.snapshot,
      ...patch,
      sessionId: this.options.sessionId,
      sequence: this.sequence
    };
    this.publish();
  }

  private publish(): void {
    for (const listener of this.listeners) listener(this.snapshot);
  }

  private disposePlayer(): void {
    this.hls?.destroy();
    this.hls = undefined;
    const player = this.player;
    this.player = undefined;
    player?.destroy(true);
  }
}

function isValidSource(source: PlayerMediaSource | undefined): source is PlayerMediaSource {
  return Boolean(
    source
    && typeof source.uri === "string"
    && source.uri.trim()
    && (source.mode === "direct" || source.mode === "hls")
    && Array.isArray(source.subtitles)
  );
}

function isValidAspectRatio(aspectRatio: PlayerAspectRatio, customValue?: string): boolean {
  return ["default", "16:9", "4:3", "fill", "fit"].includes(aspectRatio)
    || (aspectRatio === "custom" && Boolean(customValue?.trim()));
}

function rejectNotReady(command: PlayerCommand): PlayerCommandResult {
  return reject(command, createPlayerError("resource-unavailable", "播放器尚未加载媒体", true, ["retry"]));
}

function reject(command: PlayerCommand, error: PlayerError): PlayerCommandResult {
  return { commandId: command.commandId, accepted: false, error };
}

function createPlayerError(
  code: PlayerError["code"],
  message: string,
  recoverable: boolean,
  recoveryActions: PlayerError["recoveryActions"]
): PlayerError {
  return { code, message, recoverable, recoveryActions };
}

function isFiniteRange(value: unknown, min: number, max: number): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= min && value <= max;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

/** 判断浏览器 TimeRanges 是否包含指定时间，容忍分片边界的小量浮点误差。 */
function timeRangesContain(ranges: TimeRanges, positionSeconds: number): boolean {
  const toleranceSeconds = 0.25;
  for (let index = 0; index < ranges.length; index += 1) {
    if (
      positionSeconds >= ranges.start(index) - toleranceSeconds
      && positionSeconds <= ranges.end(index) + toleranceSeconds
    ) return true;
  }
  return false;
}
