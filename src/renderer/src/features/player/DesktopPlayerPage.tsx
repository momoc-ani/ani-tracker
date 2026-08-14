import { LoaderCircle } from "lucide-react";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent
} from "react";
import { appApi } from "@/lib/api";
import type { Anime, DownloadTask, Episode } from "@shared/domain";
import type {
  RemotePlaybackSession,
  RemotePlaybackSubtitle
} from "@shared/contracts";
import {
  acceptPlayerSnapshot,
  type PlayerAspectRatio,
  type PlayerCapabilities,
  type PlayerCommand,
  type PlayerFrameInterpolation,
  type PlayerHdrMode,
  type PlayerSnapshot,
  type PlayerSubtitleScale,
  type PlayerVideoEnhancement
} from "@shared/player-contract";
import { resolvePlayerShortcut } from "@shared/player-shortcuts";
import {
  buildRemotePlaylist,
  playlistItemLabel,
  resolveAdjacentPlaylistItem,
  resolveInitialPlaylistItem,
  type RemotePlaylistItem
} from "@/features/player/playback-list-model";
import { desktopPlaybackSessionClient } from "./playback-session-client";
import { PlayerChrome } from "./PlayerChrome";
import { PlayerAutoNextPrompt } from "./PlayerAutoNextPrompt";
import { PlayerErrorState } from "./PlayerErrorState";
import { PlayerPlaylistSheet } from "./PlayerPlaylistSheet";
import { buildPlayerEpisodeItems, type PlayerEpisodeUiItem } from "./player-ui-model";
import { useDesktopWindowDrag } from "./use-desktop-window-drag";
import { usePlaybackBusiness } from "./use-playback-business";
import { readStoredSubtitleScale, storeSubtitleScale } from "./subtitle-scale";
import { readStoredVideoEnhancement, storeVideoEnhancement } from "./video-enhancement";

const TOOLBAR_HIDE_DELAY_MS = 3_000;

interface DesktopPlayerPageProps {
  taskId: string;
  initialFileIndex?: number;
  onClose: () => void;
}

/** 加载桌面播放列表，并将专用控制页连接到主进程 libmpv 后端。 */
export function DesktopPlayerPage({ taskId, initialFileIndex, onClose }: DesktopPlayerPageProps) {
  const [playlist, setPlaylist] = useState<RemotePlaylistItem[]>([]);
  const [downloadTasks, setDownloadTasks] = useState<DownloadTask[]>([]);
  const [anime, setAnime] = useState<Anime>();
  const [episodes, setEpisodes] = useState<Episode[]>([]);
  const [activeItemId, setActiveItemId] = useState<string>();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const activeItem = useMemo(
    () => playlist.find((item) => item.id === activeItemId) ?? null,
    [activeItemId, playlist]
  );

  useLayoutEffect(() => {
    document.documentElement.classList.add("desktop-player-overlay-root");
    return () => document.documentElement.classList.remove("desktop-player-overlay-root");
  }, []);

  useEffect(() => {
    let active = true;
    setLoading(true);
    setError(null);
    appApi.listDownloads().then((tasks) => {
      if (!active) return;
      const matchedTask = tasks.find((item) => item.id === taskId);
      if (!matchedTask) {
        setError("播放任务不存在或已被删除");
        return;
      }
      const items = buildRemotePlaylist(tasks, matchedTask);
      const initialItem = resolveInitialPlaylistItem(items, taskId, initialFileIndex);
      if (!initialItem) {
        setError("当前番剧没有已完成的可播放视频");
        return;
      }
      setPlaylist(items);
      setDownloadTasks(matchedTask.animeId
        ? tasks.filter((task) => task.animeId === matchedTask.animeId)
        : [matchedTask]);
      setActiveItemId(initialItem.id);
      document.title = `${initialItem.displayTitle} - Ani Tracker`;
      if (matchedTask.animeId) {
        void appApi.getAnimeDetail(matchedTask.animeId).then((detail) => {
          if (!active) return;
          setAnime(detail.anime);
          setEpisodes(detail.episodes);
        }).catch((caught) => {
          console.warn("[player] 桌面播放器番剧信息读取失败", {
            animeId: matchedTask.animeId,
            error: caught
          });
        });
      }
      console.info("[player] 桌面 libmpv 播放列表读取完成", {
        taskId,
        itemCount: items.length,
        fileIndex: initialItem.fileIndex
      });
    }).catch((caught) => {
      if (!active) return;
      console.error("[player] 桌面播放器任务读取失败", { taskId, error: caught });
      setError(caught instanceof Error ? caught.message : "播放任务读取失败");
    }).finally(() => {
      if (active) setLoading(false);
    });
    return () => { active = false; };
  }, [initialFileIndex, taskId]);

  /** 切集时保留当前独立窗口，仅由新会话触发 libVLC 换源。 */
  const selectItem = useCallback((item: RemotePlaylistItem): void => {
    setActiveItemId(item.id);
    document.title = `${item.displayTitle} - Ani Tracker`;
    console.info("[player] 桌面 libVLC 切换文件", {
      taskId: item.task.id,
      fileIndex: item.fileIndex
    });
  }, []);

  return (
    <DesktopMpvControls
      activeItem={activeItem}
      anime={anime}
      downloadTasks={downloadTasks}
      episodes={episodes}
      error={error}
      loading={loading}
      onClose={onClose}
      onSelectItem={selectItem}
      playlist={playlist}
    />
  );
}

interface DesktopMpvControlsProps {
  activeItem: RemotePlaylistItem | null;
  anime?: Anime;
  downloadTasks: DownloadTask[];
  episodes: Episode[];
  error: string | null;
  loading: boolean;
  onClose: () => void;
  onSelectItem: (item: RemotePlaylistItem) => void;
  playlist: RemotePlaylistItem[];
}

/** 将播放器控制层交互映射为 preload 暴露的统一命令。 */
function DesktopMpvControls({
  activeItem,
  anime,
  downloadTasks,
  episodes,
  error: loadError,
  loading,
  onClose,
  onSelectItem,
  playlist
}: DesktopMpvControlsProps) {
  const toolbarTimerRef = useRef<number>();
  const activeSessionIdRef = useRef<string>();
  const commandSequenceRef = useRef(0);
  const [subtitleScale, setSubtitleScale] = useState<PlayerSubtitleScale>(readStoredSubtitleScale);
  const initialSubtitleScaleRef = useRef(subtitleScale);
  const [videoEnhancement, setVideoEnhancement] = useState<PlayerVideoEnhancement>(readStoredVideoEnhancement);
  const initialVideoEnhancementRef = useRef(videoEnhancement);
  const [capabilities, setCapabilities] = useState<PlayerCapabilities>();
  const [session, setSession] = useState<RemotePlaybackSession | null>(null);
  const [snapshot, setSnapshot] = useState<PlayerSnapshot>();
  const [playbackError, setPlaybackError] = useState<string | null>(null);
  const [retryNonce, setRetryNonce] = useState(0);
  const [toolbarVisible, setToolbarVisible] = useState(true);
  const [playlistOpen, setPlaylistOpen] = useState(false);
  const [panelOpen, setPanelOpen] = useState(false);
  const windowDrag = useDesktopWindowDrag();

  const previousItem = useMemo(
    () => resolveAdjacentPlaylistItem(playlist, activeItem, "previous"),
    [activeItem, playlist]
  );
  const nextItem = useMemo(
    () => resolveAdjacentPlaylistItem(playlist, activeItem, "next"),
    [activeItem, playlist]
  );
  const playing = snapshot?.status === "playing";
  const buffering = !snapshot || snapshot.status === "loading" || snapshot.status === "buffering";
  const animeTitle = anime?.title ?? activeItem?.task.animeTitle ?? "Ani Tracker";
  const episodeLabel = activeItem ? playlistItemLabel(activeItem) : "当前视频";
  const runtimeError = capabilities?.availability === "unavailable"
    ? capabilities.unavailableReason ?? "原生播放器运行时不可用"
    : null;
  const currentError = loadError ?? playbackError ?? snapshot?.error?.message ?? runtimeError;
  const episodeItems = useMemo(() => buildPlayerEpisodeItems({
    activeItem,
    currentTimeSeconds: snapshot?.positionSeconds ?? 0,
    downloadTasks,
    durationSeconds: snapshot?.durationSeconds ?? session?.durationSeconds ?? 0,
    episodes,
    playlist,
    session
  }), [activeItem, downloadTasks, episodes, playlist, session, snapshot?.durationSeconds, snapshot?.positionSeconds]);
  const subtitleOptions = useMemo<RemotePlaybackSubtitle[]>(() => (
    snapshot?.subtitleTracks.map((track) => ({
      id: track.id,
      label: track.label,
      language: track.language,
      type: "vtt",
      url: "",
      default: track.selected
    })) ?? []
  ), [snapshot?.subtitleTracks]);
  const selectedSubtitleId = snapshot?.subtitleTracks.find((track) => track.selected)?.id;
  const {
    autoNextSeconds,
    cancelAutoNext,
    closeAfterFlush,
    selectItemAfterFlush
  } = usePlaybackBusiness({
    activeItem,
    nextItem,
    onSelectItem,
    snapshot
  });

  useEffect(() => {
    let active = true;
    void appApi.getDesktopPlayerCapabilities().then((result) => {
      if (active) setCapabilities(result);
    }).catch((caught) => {
      if (!active) return;
      console.error("[player] libmpv 能力读取失败", caught);
      setPlaybackError(caught instanceof Error ? caught.message : "libmpv 能力读取失败");
    });
    return () => { active = false; };
  }, []);

  useEffect(() => appApi.onDesktopPlayerSnapshot((incoming) => {
    const activeSessionId = activeSessionIdRef.current;
    if (!activeSessionId) return;
    setCapabilities(incoming.capabilities);
    setSnapshot((current) => acceptPlayerSnapshot(activeSessionId, current, incoming));
  }), []);

  useEffect(() => {
    if (!activeItem || capabilities?.availability !== "available") return;
    let active = true;
    let createdSession: RemotePlaybackSession | undefined;
    activeSessionIdRef.current = undefined;
    setSession(null);
    setSnapshot(undefined);
    setPlaybackError(null);
    queueMicrotask(() => {
      if (!active) return;
      console.info("[player] 正在创建桌面 libmpv 会话", {
        taskId: activeItem.task.id,
        fileIndex: activeItem.fileIndex
      });
      void desktopPlaybackSessionClient.create(
        activeItem.task.id,
        "direct",
        activeItem.fileIndex,
        { videoEnhancement: "off", frameInterpolation: "off" }
      ).then(async (result) => {
        createdSession = result;
        if (!active) {
          await desktopPlaybackSessionClient.close(result.id);
          return;
        }
        activeSessionIdRef.current = result.id;
        setSession(result);
        const command: PlayerCommand = {
          type: "load",
          commandId: createCommandId(commandSequenceRef),
          sessionId: result.id,
          startPositionSeconds: result.startPositionSeconds,
          source: {
            taskId: activeItem.task.id,
            fileIndex: result.fileIndex,
            title: result.fileName,
            uri: result.streamUrl,
            mode: result.mode,
            durationSeconds: result.durationSeconds,
            subtitles: result.subtitles.map((subtitle) => ({
              id: subtitle.id,
              label: subtitle.label,
              language: subtitle.language,
              type: subtitle.type,
              uri: subtitle.url,
              default: subtitle.default
            }))
          }
        };
        const dispatchResult = await appApi.dispatchDesktopPlayerCommand(command);
        if (!dispatchResult.accepted) throw new Error(dispatchResult.error.message);
        const subtitleScaleCommand: Extract<PlayerCommand, { type: "set-subtitle-scale" }> = {
          type: "set-subtitle-scale",
          commandId: createCommandId(commandSequenceRef),
          sessionId: result.id,
          subtitleScale: initialSubtitleScaleRef.current
        };
        const subtitleScaleResult = await appApi.dispatchDesktopPlayerCommand(subtitleScaleCommand);
        if (!subtitleScaleResult.accepted) {
          console.warn("[player] 桌面字幕大小恢复失败", {
            subtitleScale: initialSubtitleScaleRef.current,
            error: subtitleScaleResult.error.message
          });
        }
        if (capabilities.supportsVideoEnhancement) {
          const enhancementCommand: Extract<PlayerCommand, { type: "set-video-enhancement" }> = {
            type: "set-video-enhancement",
            commandId: createCommandId(commandSequenceRef),
            sessionId: result.id,
            videoEnhancement: initialVideoEnhancementRef.current
          };
          const enhancementResult = await appApi.dispatchDesktopPlayerCommand(enhancementCommand);
          if (!enhancementResult.accepted) {
            console.warn("[player] 桌面画质增强恢复失败", {
              videoEnhancement: initialVideoEnhancementRef.current,
              error: enhancementResult.error.message
            });
          }
        }
      }).catch((caught) => {
        if (!active) return;
        console.error("[player] 桌面 libmpv 会话加载失败", {
          taskId: activeItem.task.id,
          fileIndex: activeItem.fileIndex,
          error: caught
        });
        setPlaybackError(caught instanceof Error ? caught.message : "播放器会话加载失败");
      });
    });
    return () => {
      active = false;
      if (createdSession && activeSessionIdRef.current === createdSession.id) {
        activeSessionIdRef.current = undefined;
      }
      if (createdSession) void desktopPlaybackSessionClient.close(createdSession.id);
    };
  }, [activeItem, capabilities?.availability, retryNonce]);

  /** 发送带当前媒体会话标识的播放器命令，并统一展示拒绝原因。 */
  const dispatchCommand = useCallback(async (command: PlayerCommand): Promise<boolean> => {
    const result = await appApi.dispatchDesktopPlayerCommand(command);
    if (result.accepted) return true;
    setPlaybackError(result.error.message);
    return false;
  }, []);

  const createCommand = useCallback(<T extends PlayerCommand>(command: Omit<T, "commandId" | "sessionId">): T | undefined => {
    const sessionId = activeSessionIdRef.current;
    if (!sessionId) return undefined;
    return {
      ...command,
      commandId: createCommandId(commandSequenceRef),
      sessionId
    } as T;
  }, []);

  const sendSimpleCommand = useCallback((type: "play" | "pause" | "retry"): void => {
    const command = createCommand<PlayerCommand>({ type } as Omit<PlayerCommand, "commandId" | "sessionId">);
    if (command) void dispatchCommand(command);
  }, [createCommand, dispatchCommand]);

  const scheduleToolbarHide = useCallback((): void => {
    window.clearTimeout(toolbarTimerRef.current);
    if (!session || playlistOpen || panelOpen || currentError || !playing || buffering) return;
    toolbarTimerRef.current = window.setTimeout(() => setToolbarVisible(false), TOOLBAR_HIDE_DELAY_MS);
  }, [buffering, currentError, panelOpen, playing, playlistOpen, session]);

  const revealToolbar = useCallback((): void => {
    setToolbarVisible(true);
    scheduleToolbarHide();
  }, [scheduleToolbarHide]);

  useEffect(() => {
    if (playlistOpen || panelOpen || currentError || !session || !playing || buffering) {
      window.clearTimeout(toolbarTimerRef.current);
      setToolbarVisible(true);
      return;
    }
    scheduleToolbarHide();
    return () => window.clearTimeout(toolbarTimerRef.current);
  }, [buffering, currentError, panelOpen, playing, playlistOpen, scheduleToolbarHide, session]);

  const togglePlayback = (): void => sendSimpleCommand(playing ? "pause" : "play");
  const seekTo = (positionSeconds: number): void => {
    const command = createCommand<Extract<PlayerCommand, { type: "seek" }>>({
      type: "seek",
      positionSeconds: Math.max(0, Math.min(snapshot?.durationSeconds ?? 0, positionSeconds))
    });
    if (command) void dispatchCommand(command);
  };
  const setVolume = (volume: number): void => {
    const command = createCommand<Extract<PlayerCommand, { type: "set-volume" }>>({ type: "set-volume", volume });
    if (command) void dispatchCommand(command);
  };
  const toggleMute = (): void => {
    const command = createCommand<Extract<PlayerCommand, { type: "set-muted" }>>({
      type: "set-muted",
      muted: !(snapshot?.muted ?? false)
    });
    if (command) void dispatchCommand(command);
  };
  const setRate = (rate: number): void => {
    const command = createCommand<Extract<PlayerCommand, { type: "set-rate" }>>({ type: "set-rate", rate });
    if (command) void dispatchCommand(command);
  };
  const setAspectRatio = (aspectRatio: PlayerAspectRatio): void => {
    const command = createCommand<Extract<PlayerCommand, { type: "set-aspect-ratio" }>>({
      type: "set-aspect-ratio",
      aspectRatio
    });
    if (command) void dispatchCommand(command);
  };
  const changeSubtitle = (trackId?: string): void => {
    const command = createCommand<Extract<PlayerCommand, { type: "select-subtitle-track" }>>({
      type: "select-subtitle-track",
      trackId
    });
    if (command) void dispatchCommand(command);
  };
  const changeSubtitleScale = (value: PlayerSubtitleScale): void => {
    const command = createCommand<Extract<PlayerCommand, { type: "set-subtitle-scale" }>>({
      type: "set-subtitle-scale",
      subtitleScale: value
    });
    if (!command) return;
    void dispatchCommand(command).then((accepted) => {
      if (!accepted) return;
      initialSubtitleScaleRef.current = value;
      setSubtitleScale(value);
      storeSubtitleScale(value);
      console.info("[player] 桌面字幕大小已保存", { subtitleScale: value });
    });
  };
  const changeVideoEnhancement = (value: PlayerVideoEnhancement): void => {
    const command = createCommand<Extract<PlayerCommand, { type: "set-video-enhancement" }>>({
      type: "set-video-enhancement",
      videoEnhancement: value
    });
    if (!command) return;
    void dispatchCommand(command).then((accepted) => {
      if (!accepted) return;
      initialVideoEnhancementRef.current = value;
      setVideoEnhancement(value);
      storeVideoEnhancement(value);
      console.info("[player] 桌面画质增强预设已保存", { videoEnhancement: value });
    });
  };
  const changeFrameInterpolation = (value: PlayerFrameInterpolation): void => {
    const command = createCommand<Extract<PlayerCommand, { type: "set-frame-interpolation" }>>({
      type: "set-frame-interpolation",
      frameInterpolation: value
    });
    if (command) void dispatchCommand(command);
  };
  const changeHdr = (value: PlayerHdrMode): void => {
    const command = createCommand<Extract<PlayerCommand, { type: "set-hdr" }>>({
      type: "set-hdr",
      hdr: value
    });
    if (command) void dispatchCommand(command);
  };
  const toggleFullscreen = (): void => {
    const command = createCommand<Extract<PlayerCommand, { type: "set-fullscreen" }>>({
      type: "set-fullscreen",
      fullscreen: !(snapshot?.fullscreen ?? false)
    });
    if (command) void dispatchCommand(command);
  };

  /** 在播放器非编辑态处理空格和既有播放快捷键。 */
  const handleKeyDown = (event: ReactKeyboardEvent<HTMLElement>): void => {
    const key = resolvePlayerShortcut(event);
    if (!key) return;
    if (["space", "arrowleft", "arrowright", "arrowup", "arrowdown"].includes(key)) event.preventDefault();
    if (key === "space") togglePlayback();
    if (key === "arrowleft") seekTo((snapshot?.positionSeconds ?? 0) - 10);
    if (key === "arrowright") seekTo((snapshot?.positionSeconds ?? 0) + 10);
    if (key === "arrowup") setVolume(Math.min(1, (snapshot?.volume ?? 0.7) + 0.05));
    if (key === "arrowdown") setVolume(Math.max(0, (snapshot?.volume ?? 0.7) - 0.05));
    if (key === "m") toggleMute();
    if (key === "f") toggleFullscreen();
    if (key === "l") setPlaylistOpen(true);
    if (key === "p" && previousItem) selectItemAfterFlush(previousItem);
    if (key === "n" && nextItem) selectItemAfterFlush(nextItem);
    if (key === "c") changeSubtitle(selectedSubtitleId ? undefined : snapshot?.subtitleTracks[0]?.id);
    revealToolbar();
  };

  const handleSurfaceClick = (event: ReactMouseEvent<HTMLElement>): void => {
    if ((event.target as Element).closest("[data-player-controls], [role='dialog']")) return;
    setToolbarVisible((visible) => !visible);
  };
  const handleSurfaceDoubleClick = (event: ReactMouseEvent<HTMLElement>): void => {
    if ((event.target as Element).closest("[data-player-controls], [role='dialog']")) return;
    const bounds = event.currentTarget.getBoundingClientRect();
    const relativeX = (event.clientX - bounds.left) / Math.max(bounds.width, 1);
    if (relativeX < 1 / 3) seekTo((snapshot?.positionSeconds ?? 0) - 10);
    else if (relativeX > 2 / 3) seekTo((snapshot?.positionSeconds ?? 0) + 10);
    else togglePlayback();
    revealToolbar();
  };

  const selectEpisode = (item: PlayerEpisodeUiItem): void => {
    if (item.playlistItem && item.playlistItem.id !== activeItem?.id) selectItemAfterFlush(item.playlistItem);
    setPlaylistOpen(false);
  };
  const retry = (): void => {
    if (snapshot?.error) sendSimpleCommand("retry");
    else setRetryNonce((value) => value + 1);
  };
  const statusBadges = [
    snapshot?.backend === "mpv" ? "libmpv" : "libVLC",
    snapshot?.enhancementDiagnostics.pipeline !== "none"
      ? snapshot?.enhancementDiagnostics.pipeline
      : undefined,
    snapshot?.enhancementDiagnostics.degradationReason ? "增强已降级" : undefined,
    session ? "原文件直放" : undefined,
    snapshot ? `${snapshot.subtitleTracks.length} 条字幕` : undefined,
    activeItem?.task.resolution?.toUpperCase()
  ].filter((value): value is string => Boolean(value));

  return (
    <main
      autoFocus
      className="player-page player-page-desktop"
      data-player-environment="desktop"
      onClick={handleSurfaceClick}
      onDoubleClick={handleSurfaceDoubleClick}
      onKeyDown={handleKeyDown}
      onPointerDown={(event) => {
        event.currentTarget.focus({ preventScroll: true });
        revealToolbar();
      }}
      onPointerMove={revealToolbar}
      tabIndex={0}
    >
      <section className="player-video-stage" aria-label={`${animeTitle} ${episodeLabel} 视频播放器`}>
        {(loading || (activeItem && !session && !currentError)) && (
          <div className="absolute inset-0 z-10 flex items-center justify-center text-white">
            <div className="flex flex-col items-center gap-2 text-sm">
              <LoaderCircle className="animate-spin" aria-hidden="true" />
              <span>正在准备原生播放器</span>
            </div>
          </div>
        )}
        {currentError && (
          <PlayerErrorState
            message={currentError}
            onClose={() => closeAfterFlush(onClose)}
            onRetry={capabilities?.availability === "available" ? retry : undefined}
            title={runtimeError ? "原生播放器无法启动" : loadError ? "播放器无法打开" : "播放失败"}
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
          bufferedSeconds={snapshot?.bufferedSeconds ?? 0}
          buffering={buffering}
          canGoNext={Boolean(nextItem)}
          canGoPrevious={Boolean(previousItem)}
          currentTimeSeconds={snapshot?.positionSeconds ?? 0}
          durationSeconds={snapshot?.durationSeconds ?? session?.durationSeconds ?? 0}
          episodeLabel={episodeLabel}
          fullscreen={snapshot?.fullscreen ?? false}
          muted={snapshot?.muted ?? false}
          nativeWindowDrag={windowDrag.nativeWindowDrag}
          onActivity={revealToolbar}
          onChangeRate={setRate}
          onChangeSubtitle={changeSubtitle}
          onChangeSubtitleScale={changeSubtitleScale}
          onChangeVideoEnhancement={changeVideoEnhancement}
          onChangeFrameInterpolation={changeFrameInterpolation}
          onChangeHdr={changeHdr}
          onClose={() => closeAfterFlush(onClose)}
          onGoNext={() => nextItem && selectItemAfterFlush(nextItem)}
          onGoPrevious={() => previousItem && selectItemAfterFlush(previousItem)}
          onOpenPlaylist={() => setPlaylistOpen(true)}
          onPanelOpenChange={setPanelOpen}
          onSeek={seekTo}
          onSetAspectRatio={setAspectRatio}
          onSetVolume={setVolume}
          onToggleFullscreen={toggleFullscreen}
          onToggleMute={toggleMute}
          onTogglePictureInPicture={() => undefined}
          onTogglePlay={togglePlayback}
          pictureInPicture={false}
          pictureInPictureAvailable={false}
          playbackRate={snapshot?.playbackRate ?? 1}
          playing={playing}
          selectedSubtitleId={selectedSubtitleId}
          statusBadges={statusBadges}
          subtitleScale={subtitleScale}
          subtitleScaleAvailable={capabilities?.supportsSubtitleScale ?? false}
          videoEnhancement={snapshot?.videoEnhancement ?? videoEnhancement}
          videoEnhancementAvailable={capabilities?.supportsVideoEnhancement ?? false}
          videoEnhancementDegraded={snapshot?.videoEnhancementDegraded ?? false}
          frameInterpolation={snapshot?.frameInterpolation ?? "off"}
          frameInterpolationAvailable={capabilities?.supportsFrameInterpolation ?? false}
          hdr={snapshot?.hdr ?? "off"}
          hdrAvailable={capabilities?.supportsHdr ?? false}
          subtitles={subtitleOptions}
          visible={toolbarVisible}
          volume={snapshot?.volume ?? 0.7}
          windowDragHandlers={windowDrag.handlers}
        />
      </section>
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

function createCommandId(sequenceRef: { current: number }): string {
  sequenceRef.current += 1;
  return `desktop-${Date.now()}-${sequenceRef.current}`;
}
