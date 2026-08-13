import { LoaderCircle } from "lucide-react";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent
} from "react";
import { toast } from "@/lib/toast";
import { Skeleton } from "@/components/ui/skeleton";
import { PlayerChrome } from "@/features/player/PlayerChrome";
import { PlayerAutoNextPrompt } from "@/features/player/PlayerAutoNextPrompt";
import { PlayerEpisodeList } from "@/features/player/PlayerEpisodeList";
import { PlayerErrorState } from "@/features/player/PlayerErrorState";
import { PlayerMobileDetails } from "@/features/player/PlayerMobileDetails";
import { PlayerPlaylistSheet } from "@/features/player/PlayerPlaylistSheet";
import { usePlaybackBusiness } from "@/features/player/use-playback-business";
import {
  buildPlayerEpisodeItems,
  type PlayerEpisodeUiItem
} from "@/features/player/player-ui-model";
import {
  appApi,
  closeRemotePlaybackSession,
  createRemoteExternalPlaybackSession
} from "@/lib/api";
import { cn } from "@/lib/cn";
import type {
  RemotePlaybackRequestMode,
  RemotePlaybackSession
} from "@shared/contracts";
import type { Anime, DownloadTask, Episode } from "@shared/domain";
import type {
  PlayerAspectRatio,
  PlayerCommand,
  PlayerSnapshot,
  PlayerSubtitleScale
} from "@shared/player-contract";
import { resolvePlayerShortcut } from "@shared/player-shortcuts";
import { readStoredSubtitleScale, storeSubtitleScale } from "@/features/player/subtitle-scale";
import { ArtPlayerAdapter } from "./art-player-adapter";
import {
  buildExternalPlayerProtocolUrl,
  detectExternalPlayer
} from "./external-player-launch";
import type { PlaybackSessionClient } from "./playback-session-client";
import {
  playlistItemLabel,
  resolveAdjacentPlaylistItem,
  type RemotePlaylistItem
} from "@/features/player/playback-list-model";
import {
  readRemotePlaybackMode,
  storeRemotePlaybackMode
} from "@/features/player/remote-playback-preferences";

const TOOLBAR_HIDE_DELAY_MS = 3_000;

type RemoteFullscreenMode = "native" | "web";

interface WebkitFullscreenDocument extends Document {
  webkitExitFullscreen?: () => Promise<void> | void;
  webkitFullscreenElement?: Element | null;
}

interface WebkitFullscreenElement extends HTMLElement {
  webkitRequestFullscreen?: () => Promise<void> | void;
}

interface RemoteVideoPlayerProps {
  activeItem: RemotePlaylistItem | null;
  allowExternalPlayback: boolean;
  anime?: Anime;
  downloadTasks: DownloadTask[];
  environment: "desktop" | "remote";
  episodes: Episode[];
  error: string | null;
  loading: boolean;
  onClose: () => void;
  onSelectItem: (item: RemotePlaylistItem) => void;
  playlist: RemotePlaylistItem[];
  sessionClient: PlaybackSessionClient;
}

/** 使用 ArtPlayer 处理远程网页视频，并由统一控制层承载全部交互。 */
export function RemoteVideoPlayer({
  activeItem,
  allowExternalPlayback,
  anime,
  downloadTasks,
  environment,
  episodes,
  error: loadError,
  loading,
  onClose,
  onSelectItem,
  playlist,
  sessionClient
}: RemoteVideoPlayerProps) {
  const playerStageRef = useRef<HTMLElement>(null);
  const playerContainerRef = useRef<HTMLDivElement>(null);
  const playerAdapterRef = useRef<ArtPlayerAdapter | null>(null);
  const toolbarTimerRef = useRef<number>();
  const automaticFallbackStartedRef = useRef(false);
  const commandSequenceRef = useRef(0);
  const [subtitleScale, setSubtitleScale] = useState<PlayerSubtitleScale>(readStoredSubtitleScale);
  const subtitleScaleRef = useRef(subtitleScale);
  const [requestedMode, setRequestedMode] = useState<RemotePlaybackRequestMode>(readRemotePlaybackMode);
  const [session, setSession] = useState<RemotePlaybackSession | null>(null);
  const [playbackError, setPlaybackError] = useState<string | null>(null);
  const [retryNonce, setRetryNonce] = useState(0);
  const [toolbarVisible, setToolbarVisible] = useState(true);
  const [remoteFullscreenMode, setRemoteFullscreenMode] = useState<RemoteFullscreenMode | null>(null);
  const [playlistOpen, setPlaylistOpen] = useState(false);
  const [panelOpen, setPanelOpen] = useState(false);
  const [externalPlayerOpening, setExternalPlayerOpening] = useState(false);
  const [playerSnapshot, setPlayerSnapshot] = useState<PlayerSnapshot>();
  const externalPlayer = useMemo(
    () => allowExternalPlayback
      ? detectExternalPlayer(navigator.userAgent, navigator.platform)
      : undefined,
    [allowExternalPlayback]
  );
  const previousItem = useMemo(
    () => resolveAdjacentPlaylistItem(playlist, activeItem, "previous"),
    [activeItem, playlist]
  );
  const nextItem = useMemo(
    () => resolveAdjacentPlaylistItem(playlist, activeItem, "next"),
    [activeItem, playlist]
  );
  const playing = playerSnapshot?.status === "playing";
  const buffering = !playerSnapshot
    || playerSnapshot.status === "loading"
    || playerSnapshot.status === "buffering";
  const currentTimeSeconds = playerSnapshot?.positionSeconds ?? 0;
  const durationSeconds = playerSnapshot?.durationSeconds ?? session?.durationSeconds ?? 0;
  const bufferedSeconds = playerSnapshot?.bufferedSeconds ?? 0;
  const volume = playerSnapshot?.volume ?? 0.7;
  const muted = playerSnapshot?.muted ?? false;
  const playbackRate = playerSnapshot?.playbackRate ?? 1;
  const selectedSubtitleId = playerSnapshot?.subtitleTracks.find((track) => track.selected)?.id;
  const fullscreen = environment === "remote"
    ? remoteFullscreenMode !== null
    : playerSnapshot?.fullscreen ?? false;
  const pictureInPicture = playerSnapshot?.pictureInPicture ?? false;
  const animeTitle = anime?.title ?? activeItem?.task.animeTitle ?? "Ani Tracker";
  const episodeLabel = activeItem ? playlistItemLabel(activeItem) : "当前视频";
  const episodeItems = useMemo(() => buildPlayerEpisodeItems({
    activeItem,
    currentTimeSeconds,
    downloadTasks,
    durationSeconds,
    episodes,
    playlist,
    session
  }), [activeItem, currentTimeSeconds, downloadTasks, durationSeconds, episodes, playlist, session]);
  const {
    autoNextSeconds,
    cancelAutoNext,
    closeAfterFlush,
    selectItemAfterFlush
  } = usePlaybackBusiness({
    activeItem,
    nextItem,
    onSelectItem,
    snapshot: playerSnapshot
  });

  /** 清理并重新安排控制层的自动隐藏计时。 */
  const scheduleToolbarHide = useCallback((): void => {
    window.clearTimeout(toolbarTimerRef.current);
    if (!session || playlistOpen || panelOpen || playbackError || loadError || !playing || buffering) {
      return;
    }
    toolbarTimerRef.current = window.setTimeout(() => {
      setToolbarVisible(false);
    }, TOOLBAR_HIDE_DELAY_MS);
  }, [buffering, loadError, panelOpen, playbackError, playing, playlistOpen, session]);

  /** 响应指针或键盘活动，重新呼出控制层并重置计时。 */
  const revealToolbar = useCallback((): void => {
    setToolbarVisible(true);
    scheduleToolbarHide();
  }, [scheduleToolbarHide]);

  useEffect(() => {
    if (playlistOpen || panelOpen || playbackError || loadError || !session || !playing || buffering) {
      window.clearTimeout(toolbarTimerRef.current);
      setToolbarVisible(true);
      return;
    }
    scheduleToolbarHide();
    return () => window.clearTimeout(toolbarTimerRef.current);
  }, [buffering, loadError, panelOpen, playbackError, playing, playlistOpen, scheduleToolbarHide, session]);

  useEffect(() => {
    if (environment !== "remote") return;
    const handleFullscreenChange = (): void => {
      const nativeFullscreen = isRemoteNativeFullscreen(playerStageRef.current);
      setRemoteFullscreenMode((current) => nativeFullscreen ? "native" : current === "web" ? current : null);
      revealToolbar();
      console.info("[remote] 网页播放器全屏状态变化", { nativeFullscreen });
    };
    document.addEventListener("fullscreenchange", handleFullscreenChange);
    document.addEventListener("webkitfullscreenchange", handleFullscreenChange);
    return () => {
      document.removeEventListener("fullscreenchange", handleFullscreenChange);
      document.removeEventListener("webkitfullscreenchange", handleFullscreenChange);
    };
  }, [environment, revealToolbar]);

  useEffect(() => {
    if (!remoteFullscreenMode) return;
    const previousHtmlOverflow = document.documentElement.style.overflow;
    const previousBodyOverflow = document.body.style.overflow;
    document.documentElement.style.overflow = "hidden";
    document.body.style.overflow = "hidden";
    return () => {
      document.documentElement.style.overflow = previousHtmlOverflow;
      document.body.style.overflow = previousBodyOverflow;
    };
  }, [remoteFullscreenMode]);

  useEffect(() => {
    if (!activeItem) {
      return;
    }
    let active = true;
    let createdSession: RemotePlaybackSession | undefined;
    setSession(null);
    setPlayerSnapshot(undefined);
    setPlaybackError(null);

    // 延迟到微任务阶段，避免 React 严格模式的探测挂载重复创建媒体会话。
    queueMicrotask(() => {
      if (!active) return;
      console.info("[remote] 正在创建播放会话", {
        taskId: activeItem.task.id,
        fileIndex: activeItem.fileIndex,
        requestedMode
      });
      void sessionClient.create(activeItem.task.id, requestedMode, activeItem.fileIndex)
        .then((result) => {
          createdSession = result;
          if (!active) return sessionClient.close(result.id);
          setSession(result);
          console.info("[remote] 播放会话已创建", {
            taskId: activeItem.task.id,
            fileIndex: result.fileIndex,
            mode: result.mode
          });
        })
        .catch((caught) => {
          if (!active) return;
          console.error("[remote] 播放会话创建失败", {
            taskId: activeItem.task.id,
            fileIndex: activeItem.fileIndex,
            requestedMode,
            error: caught
          });
          setPlaybackError(caught instanceof Error ? caught.message : "播放会话创建失败");
        });
    });

    return () => {
      active = false;
      if (createdSession) void sessionClient.close(createdSession.id);
    };
  }, [activeItem, requestedMode, retryNonce, sessionClient]);

  /** 原文件发生媒体错误时仅自动升级一次实时转码。 */
  const startAutomaticTranscode = useCallback((): void => {
    if (!activeItem || requestedMode !== "direct" || automaticFallbackStartedRef.current) return;
    automaticFallbackStartedRef.current = true;
    setPlaybackError(null);
    setRequestedMode("transcode");
    toast.info("原文件无法播放，正在切换实时转码");
    console.warn("[remote] 原文件播放失败，自动切换实时转码", {
      taskId: activeItem.task.id,
      fileIndex: activeItem.fileIndex
    });
  }, [activeItem, requestedMode]);

  useEffect(() => {
    const container = playerContainerRef.current;
    if (!container || !session || !activeItem) return;
    const adapter = new ArtPlayerAdapter({
      container,
      sessionId: session.id,
      subtitleScale: subtitleScaleRef.current
    });
    playerAdapterRef.current = adapter;
    const unsubscribe = adapter.subscribe((nextSnapshot) => {
      setPlayerSnapshot(nextSnapshot);
      if (nextSnapshot.status !== "error" || !nextSnapshot.error) return;
      if (session.mode === "direct" && !automaticFallbackStartedRef.current) {
        startAutomaticTranscode();
        return;
      }
      console.error("[remote] ArtPlayer 适配器播放失败", {
        taskId: activeItem.task.id,
        fileIndex: activeItem.fileIndex,
        mode: session.mode,
        errorCode: nextSnapshot.error.code
      });
      setPlaybackError(nextSnapshot.error.message);
    });
    const loadCommand: PlayerCommand = {
      type: "load",
      commandId: createRemoteCommandId(commandSequenceRef),
      sessionId: session.id,
      startPositionSeconds: session.startPositionSeconds,
      source: {
        taskId: activeItem.task.id,
        fileIndex: session.fileIndex,
        title: session.fileName,
        uri: session.streamUrl,
        mode: session.mode,
        durationSeconds: session.durationSeconds,
        subtitles: session.subtitles.map((subtitle) => ({
          id: subtitle.id,
          label: subtitle.label,
          language: subtitle.language,
          type: subtitle.type,
          uri: subtitle.url,
          default: subtitle.default
        }))
      }
    };
    void adapter.dispatch(loadCommand).then((result) => {
      if (!result.accepted) setPlaybackError(result.error.message);
      else console.info("[remote] ArtPlayer 适配器已加载媒体", {
        taskId: activeItem.task.id,
        fileIndex: activeItem.fileIndex,
        mode: session.mode
      });
    });
    return () => {
      unsubscribe();
      if (playerAdapterRef.current === adapter) playerAdapterRef.current = null;
      void adapter.dispose();
    };
  }, [activeItem, session, startAutomaticTranscode]);

  /** 手动切换播放模式，并允许下次直传失败时再次自动升级。 */
  const handleModeChange = (value: RemotePlaybackRequestMode): void => {
    if (value === requestedMode) return;
    automaticFallbackStartedRef.current = value === "transcode";
    setPlaybackError(null);
    setRequestedMode(value);
    storeRemotePlaybackMode(value);
    console.info("[remote] 手动切换播放模式", {
      taskId: activeItem?.task.id,
      requestedMode: value
    });
  };

  /** 为当前远程媒体会话构造并发送统一播放器命令。 */
  const dispatchPlayerCommand = useCallback(async (command: PlayerCommand): Promise<boolean> => {
    const adapter = playerAdapterRef.current;
    if (!adapter) return false;
    const result = await adapter.dispatch(command);
    if (result.accepted) return true;
    setPlaybackError(result.error.message);
    return false;
  }, []);

  const createPlayerCommand = useCallback(<T extends PlayerCommand>(
    command: Omit<T, "commandId" | "sessionId">
  ): T | undefined => {
    if (!session) return undefined;
    return {
      ...command,
      commandId: createRemoteCommandId(commandSequenceRef),
      sessionId: session.id
    } as T;
  }, [session]);

  const sendSimpleCommand = useCallback((type: "play" | "pause" | "retry"): void => {
    const command = createPlayerCommand<PlayerCommand>({ type } as Omit<PlayerCommand, "commandId" | "sessionId">);
    if (command) void dispatchPlayerCommand(command);
  }, [createPlayerCommand, dispatchPlayerCommand]);

  /** 创建外部拉流会话并调用远程设备本机播放器。 */
  const handleExternalPlayback = async (): Promise<void> => {
    if (!activeItem || !externalPlayer || externalPlayerOpening) return;
    const wasPlaying = playing;
    sendSimpleCommand("pause");
    setExternalPlayerOpening(true);
    const toastId = toast.loading(`正在准备 ${externalPlayer.label} 播放地址`);
    let externalSession: RemotePlaybackSession | undefined;
    try {
      externalSession = await createRemoteExternalPlaybackSession(
        activeItem.task.id,
        requestedMode,
        activeItem.fileIndex
      );
      const mediaUrl = new URL(externalSession.streamUrl, window.location.origin).toString();
      window.location.assign(buildExternalPlayerProtocolUrl(externalPlayer.kind, mediaUrl));
      toast.info(`已请求打开 ${externalPlayer.label}`, {
        id: toastId,
        description: "若播放器未启动，请确认已安装并允许浏览器打开外部应用。"
      });
      console.info("[remote] 已下发本地播放器拉流请求", {
        player: externalPlayer.kind,
        taskId: activeItem.task.id,
        fileIndex: activeItem.fileIndex,
        mode: requestedMode
      });
    } catch (caught) {
      if (externalSession) void closeRemotePlaybackSession(externalSession.id);
      if (wasPlaying) sendSimpleCommand("play");
      console.error("[remote] 本地播放器调起失败", {
        player: externalPlayer.kind,
        taskId: activeItem.task.id,
        error: caught
      });
      toast.error(`无法打开 ${externalPlayer.label}`, {
        id: toastId,
        description: caught instanceof Error ? caught.message : "本地播放器调用失败"
      });
    } finally {
      setExternalPlayerOpening(false);
    }
  };

  /** 切换当前播放项并关闭播放列表。 */
  const selectEpisode = (item: PlayerEpisodeUiItem): void => {
    if (item.playlistItem && item.playlistItem.id !== activeItem?.id) selectItemAfterFlush(item.playlistItem);
    setPlaylistOpen(false);
  };

  /** 在竖屏滚动到页面列表，其余布局打开右侧 Sheet。 */
  const openPlaylist = (): void => {
    const usesInlinePlaylist = environment === "remote"
      && window.matchMedia("(max-width: 767px) and (orientation: portrait)").matches;
    if (usesInlinePlaylist) {
      document.getElementById("player-inline-playlist")?.scrollIntoView({ behavior: "smooth", block: "start" });
      return;
    }
    setPlaylistOpen(true);
  };

  /** 切换 ArtPlayer 播放状态。 */
  const togglePlayback = (): void => sendSimpleCommand(playing ? "pause" : "play");

  /** 跳转到合法媒体时间。 */
  const seekTo = (seconds: number): void => {
    const command = createPlayerCommand<Extract<PlayerCommand, { type: "seek" }>>({
      type: "seek",
      positionSeconds: Math.max(0, Math.min(durationSeconds, seconds))
    });
    if (command) void dispatchPlayerCommand(command);
  };

  /** 切换 ArtPlayer 字幕轨道或关闭字幕。 */
  const changeSubtitle = (subtitleId?: string): void => {
    const command = createPlayerCommand<Extract<PlayerCommand, { type: "select-subtitle-track" }>>({
      type: "select-subtitle-track",
      trackId: subtitleId
    });
    if (command) void dispatchPlayerCommand(command);
  };

  /** 即时调整并保存远程网页字幕大小。 */
  const changeSubtitleScale = (value: PlayerSubtitleScale): void => {
    const command = createPlayerCommand<Extract<PlayerCommand, { type: "set-subtitle-scale" }>>({
      type: "set-subtitle-scale",
      subtitleScale: value
    });
    if (!command) return;
    void dispatchPlayerCommand(command).then((accepted) => {
      if (!accepted) return;
      subtitleScaleRef.current = value;
      setSubtitleScale(value);
      storeSubtitleScale(value);
    });
  };

  /** 设置视频比例并保持默认模式不裁切。 */
  const setAspectRatio = (aspectRatio: PlayerAspectRatio): void => {
    const command = createPlayerCommand<Extract<PlayerCommand, { type: "set-aspect-ratio" }>>({
      type: "set-aspect-ratio",
      aspectRatio
    });
    if (command) void dispatchPlayerCommand(command);
  };

  const setPlayerVolume = (nextVolume: number): void => {
    const command = createPlayerCommand<Extract<PlayerCommand, { type: "set-volume" }>>({
      type: "set-volume",
      volume: nextVolume
    });
    if (command) void dispatchPlayerCommand(command);
  };

  const setPlayerRate = (rate: number): void => {
    const command = createPlayerCommand<Extract<PlayerCommand, { type: "set-rate" }>>({ type: "set-rate", rate });
    if (command) void dispatchPlayerCommand(command);
  };

  const togglePlayerMute = (): void => {
    const command = createPlayerCommand<Extract<PlayerCommand, { type: "set-muted" }>>({
      type: "set-muted",
      muted: !muted
    });
    if (command) void dispatchPlayerCommand(command);
  };

  /** 远程网页让完整视频舞台进入全屏，保留自定义控制层。 */
  const toggleRemoteFullscreen = async (): Promise<void> => {
    const stage = playerStageRef.current;
    if (!stage) return;
    if (remoteFullscreenMode === "web") {
      setRemoteFullscreenMode(null);
      revealToolbar();
      console.info("[remote] 已退出网页全屏");
      return;
    }
    if (isRemoteNativeFullscreen(stage)) {
      try {
        await exitDocumentFullscreen();
      } catch (error) {
        console.warn("[remote] 退出原生全屏失败", { error });
      }
      return;
    }

    try {
      const enteredNativeFullscreen = await requestElementFullscreen(document.documentElement);
      setRemoteFullscreenMode(enteredNativeFullscreen ? "native" : "web");
      revealToolbar();
      console.info("[remote] 已进入网页播放器全屏", {
        mode: enteredNativeFullscreen ? "native" : "web"
      });
    } catch (error) {
      setRemoteFullscreenMode("web");
      revealToolbar();
      console.warn("[remote] 原生全屏不可用，已切换网页全屏", { error });
    }
  };

  /** 根据运行环境切换网页舞台全屏或 ArtPlayer 全屏。 */
  const togglePlayerFullscreen = (): void => {
    if (environment === "remote") {
      void toggleRemoteFullscreen();
      return;
    }
    const command = createPlayerCommand<Extract<PlayerCommand, { type: "set-fullscreen" }>>({
      type: "set-fullscreen",
      fullscreen: !fullscreen
    });
    if (command) void dispatchPlayerCommand(command);
  };

  const togglePictureInPicture = (): void => {
    const command = createPlayerCommand<Extract<PlayerCommand, { type: "set-picture-in-picture" }>>({
      type: "set-picture-in-picture",
      enabled: !pictureInPicture
    });
    if (command) void dispatchPlayerCommand(command);
  };

  /** 在播放器非编辑态处理空格和既有播放快捷键。 */
  const handleKeyDown = (event: ReactKeyboardEvent<HTMLElement>): void => {
    const key = resolvePlayerShortcut(event);
    if (!key) return;
    if (["space", "arrowleft", "arrowright", "arrowup", "arrowdown"].includes(key)) event.preventDefault();
    if (key === "space") togglePlayback();
    if (key === "arrowleft") seekTo(currentTimeSeconds - 10);
    if (key === "arrowright") seekTo(currentTimeSeconds + 10);
    if (key === "arrowup") setPlayerVolume(Math.min(1, volume + 0.05));
    if (key === "arrowdown") setPlayerVolume(Math.max(0, volume - 0.05));
    if (key === "m") togglePlayerMute();
    if (key === "f") togglePlayerFullscreen();
    if (key === "l") openPlaylist();
    if (key === "p" && previousItem) selectItemAfterFlush(previousItem);
    if (key === "n" && nextItem) selectItemAfterFlush(nextItem);
    if (key === "c") changeSubtitle(selectedSubtitleId ? undefined : session?.subtitles[0]?.id);
    revealToolbar();
  };

  /** 点击空白视频区域切换控制层可见性。 */
  const handleSurfaceClick = (event: ReactMouseEvent<HTMLElement>): void => {
    if ((event.target as Element).closest("[data-player-controls], [role='dialog']")) return;
    if (environment === "remote" && !(event.target as Element).closest(".player-video-stage")) return;
    setToolbarVisible((visible) => !visible);
  };

  /** 双击视频左右区域快退或快进，中部切换播放状态。 */
  const handleSurfaceDoubleClick = (event: ReactMouseEvent<HTMLElement>): void => {
    if ((event.target as Element).closest("[data-player-controls], [role='dialog']")) return;
    const stage = event.currentTarget.querySelector<HTMLElement>(".player-video-stage");
    if (!stage) return;
    const bounds = stage.getBoundingClientRect();
    const relativeX = (event.clientX - bounds.left) / Math.max(bounds.width, 1);
    if (relativeX < 1 / 3) {
      seekTo(currentTimeSeconds - 10);
    } else if (relativeX > 2 / 3) {
      seekTo(currentTimeSeconds + 10);
    } else {
      togglePlayback();
    }
    revealToolbar();
  };

  const statusBadges = [
    session?.mode === "hls" ? "实时转码" : session ? "原文件直传" : undefined,
    session?.diagnostics?.encoder ? `编码 ${session.diagnostics.encoder}` : undefined,
    session?.diagnostics?.encoderDegraded ? "编码器已降级" : undefined,
    session ? `${session.subtitles.length} 条字幕` : undefined,
    activeItem?.task.resolution?.toUpperCase()
  ].filter((value): value is string => Boolean(value));

  return (
    <main
      autoFocus
      className={cn("player-page", environment === "desktop" ? "player-page-desktop" : "player-page-remote")}
      data-player-environment={environment}
      data-remote-fullscreen={environment === "remote" ? remoteFullscreenMode ?? undefined : undefined}
      onClick={handleSurfaceClick}
      onDoubleClick={handleSurfaceDoubleClick}
      onKeyDown={handleKeyDown}
      onPointerDown={(event) => {
        event.currentTarget.focus({ preventScroll: true });
        if (environment === "desktop" || (event.target as Element).closest("[data-player-controls]")) {
          revealToolbar();
        }
      }}
      onPointerMove={(event) => {
        if (environment === "desktop" || event.pointerType === "mouse") revealToolbar();
      }}
      tabIndex={0}
    >
      <section
        ref={playerStageRef}
        className="player-video-stage"
        aria-label={`${animeTitle} ${episodeLabel} 视频播放器`}
        data-remote-fullscreen={environment === "remote" ? remoteFullscreenMode ?? undefined : undefined}
      >
        <div ref={playerContainerRef} className="absolute inset-0" data-artplayer-surface />
        {(loading || (activeItem && !session && !playbackError)) && !loadError && (
          <div className="absolute inset-0 z-10 flex items-center justify-center bg-black text-white">
            <Skeleton className="absolute inset-0 size-full rounded-none bg-black" />
            <div className="relative flex flex-col items-center gap-2 text-sm">
              <LoaderCircle className="animate-spin" aria-hidden="true" />
              <span>正在准备视频</span>
            </div>
          </div>
        )}
        {(loadError || playbackError) && (
          <PlayerErrorState
            message={loadError ?? playbackError ?? "未知播放错误"}
            onClose={() => closeAfterFlush(onClose)}
            onRetry={playbackError ? () => setRetryNonce((value) => value + 1) : undefined}
            onTranscode={playbackError && requestedMode === "direct" ? startAutomaticTranscode : undefined}
            title={loadError ? "播放器无法打开" : "播放失败"}
          />
        )}
        {autoNextSeconds !== undefined && nextItem && (
          <PlayerAutoNextPrompt
            episodeLabel={playlistItemLabel(nextItem)}
            onCancel={cancelAutoNext}
            onPlayNow={() => selectItemAfterFlush(nextItem)}
            seconds={autoNextSeconds}
          />
        )}
        <PlayerChrome
          animeTitle={animeTitle}
          bufferedSeconds={bufferedSeconds}
          buffering={buffering}
          canGoNext={Boolean(nextItem)}
          canGoPrevious={Boolean(previousItem)}
          currentTimeSeconds={currentTimeSeconds}
          durationSeconds={durationSeconds}
          episodeLabel={episodeLabel}
          externalPlayerLabel={externalPlayer?.label}
          externalPlayerOpening={externalPlayerOpening}
          fullscreen={fullscreen}
          mode={requestedMode}
          muted={muted}
          onActivity={revealToolbar}
          onChangeMode={handleModeChange}
          onChangeRate={setPlayerRate}
          onChangeSubtitle={changeSubtitle}
          onChangeSubtitleScale={changeSubtitleScale}
          onClose={() => closeAfterFlush(onClose)}
          onGoNext={() => nextItem && selectItemAfterFlush(nextItem)}
          onGoPrevious={() => previousItem && selectItemAfterFlush(previousItem)}
          onOpenExternalPlayer={externalPlayer ? () => void handleExternalPlayback() : undefined}
          onOpenPlaylist={openPlaylist}
          onPanelOpenChange={setPanelOpen}
          onSeek={seekTo}
          onSetAspectRatio={setAspectRatio}
          onSetVolume={setPlayerVolume}
          onToggleFullscreen={togglePlayerFullscreen}
          onToggleMute={togglePlayerMute}
          onTogglePictureInPicture={togglePictureInPicture}
          onTogglePlay={togglePlayback}
          pictureInPicture={pictureInPicture}
          playbackRate={playbackRate}
          playing={playing}
          selectedSubtitleId={selectedSubtitleId}
          statusBadges={statusBadges}
          subtitleScale={subtitleScale}
          subtitleScaleAvailable={playerSnapshot?.capabilities.supportsSubtitleScale ?? true}
          subtitles={session?.subtitles ?? []}
          visible={toolbarVisible}
          volume={volume}
        />
      </section>

      {environment === "remote" && (
        <div className="player-mobile-content">
          <PlayerMobileDetails
            activeItem={activeItem}
            anime={anime}
            currentTimeSeconds={currentTimeSeconds}
            episodes={episodes}
            session={session}
          />
          <div id="player-inline-playlist" className="h-80 min-h-0 scroll-mt-[calc(56.25vw+0.5rem)] pb-[max(1rem,var(--safe-area-bottom))] md:h-[calc(100svh-56.25vw)]">
            <PlayerEpisodeList animeTitle={animeTitle} items={episodeItems} onSelect={selectEpisode} scrollable />
          </div>
        </div>
      )}

      <PlayerPlaylistSheet
        animeTitle={animeTitle}
        items={episodeItems}
        onOpenChange={setPlaylistOpen}
        onSelect={selectEpisode}
        open={playlistOpen}
      />
    </main>
  );
}

function createRemoteCommandId(sequenceRef: { current: number }): string {
  sequenceRef.current += 1;
  return `remote-${Date.now()}-${sequenceRef.current}`;
}

/** 读取标准或 WebKit 的当前全屏元素。 */
function getDocumentFullscreenElement(): Element | null {
  const webkitDocument = document as WebkitFullscreenDocument;
  return document.fullscreenElement ?? webkitDocument.webkitFullscreenElement ?? null;
}

/** 判断远程播放器是否占用当前原生全屏树。 */
function isRemoteNativeFullscreen(stage: HTMLElement | null): boolean {
  const fullscreenElement = getDocumentFullscreenElement();
  return fullscreenElement === document.documentElement || fullscreenElement === stage;
}

/** 尝试让完整播放器舞台进入原生全屏，不支持时返回网页全屏标记。 */
async function requestElementFullscreen(element: HTMLElement): Promise<boolean> {
  if (element.requestFullscreen) {
    await element.requestFullscreen();
    return true;
  }
  const webkitRequestFullscreen = (element as WebkitFullscreenElement).webkitRequestFullscreen;
  if (!webkitRequestFullscreen) return false;
  await webkitRequestFullscreen.call(element);
  return true;
}

/** 退出标准或 WebKit 原生全屏。 */
async function exitDocumentFullscreen(): Promise<void> {
  if (document.exitFullscreen) {
    await document.exitFullscreen();
    return;
  }
  const webkitExitFullscreen = (document as WebkitFullscreenDocument).webkitExitFullscreen;
  if (webkitExitFullscreen) await webkitExitFullscreen.call(document);
}
