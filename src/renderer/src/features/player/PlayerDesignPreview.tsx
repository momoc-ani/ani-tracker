import { useEffect, useMemo, useRef, useState } from "react";
import { useTheme } from "@/components/theme-provider";
import { PlayerChrome } from "./PlayerChrome";
import { PlayerEpisodeList } from "./PlayerEpisodeList";
import { PlayerErrorState } from "./PlayerErrorState";
import { PlayerMobileDetails } from "./PlayerMobileDetails";
import { PlayerPlaylistSheet } from "./PlayerPlaylistSheet";
import type { PlayerEpisodeUiItem } from "./player-ui-model";
import type { RemotePlaybackRequestMode, RemotePlaybackSession } from "@shared/contracts";
import type { Anime, DownloadTask, Episode } from "@shared/domain";
import type { RemotePlaylistItem } from "@/features/player/playback-list-model";
import type { PlayerSubtitleScale } from "@shared/player-contract";
import type { PlayerVideoEnhancement } from "@shared/player-contract";

const PREVIEW_DESKTOP_FRAME_URL = "https://lh3.googleusercontent.com/aida-public/AB6AXuAgL2mdTyi1Xs9Yb1id3AmPBIeCFYvY3nrylRUwWUo4ZN0w9MExoBEQiABCj5Up9Wo9PyCbZ2YSrLVvfVANbuBTxvKjqOKEKtzcfpaquZdgnIlHdH9-FLekoQyXY0UgHLYciZh92dSS2hw9zOWX7ocE6pEGW6_ZOxFfPSaBbZDgs9Oa5QrWK8URPx2SazTvrW-Kg-1MDJPsJlc9jSKldT9YMKsgaqCiIXrYQYjOYbZinQxIHWfvR9YPnQn8o4N1znZbRfjKMNlavdoh";
const PREVIEW_LANDSCAPE_FRAME_URL = "https://lh3.googleusercontent.com/aida-public/AB6AXuD8BzePUI2MmVa32TfyST_TnZ4O2188fprpF8JpHDSGtzjI7mbh2KQm1hYcktxVwwTqkkedkdCobDBczFz4Mhlm1qEIWruz02t2lGRka9ZqdEwjFU_KJHKloj5sR37Ndev9kwU8qE1tcPE9WjI0Ydx7WkPPF-iLuGBNSXZxSB6x-2PLSX85MHIsvsQ9pti4BH989tUvLFPbL269TQKRrmesVoJSNoDxTo8AeVeHyTrtYdJd5JBhfvlKcq053xClbSJSgsUlA5nQW4pk";
const PREVIEW_POSTER_URL = "https://lh3.googleusercontent.com/aida-public/AB6AXuBmZqrJ5KhyvDDZeq3whiPtscqY-PgxgEx_LqPX36bqDEUSlZdMW4dZKlSf39CtDVDhj8-GyTP0YitWUPKTQqWMuq8dJ1bNndXb3aeJCcMtIWelTg1tFpAyXObLVeBb7j2nG9oWhHd64n4Nz6vMktvXhOgOO4ISvPoL-1cjiSwKwaG4zCiN_mlbjRfwzBmdKXlHMe5D83MoeNEB3VHp2GcOfFcG-3DwKA0A6PWdOwOx8SVP99BRlDVaSY_oWxN327ATo6dbrxhkBAu1";

/** 仅在开发环境提供固定播放器状态，供设计稿截图验收。 */
export function PlayerDesignPreview({ mode }: { mode: string }) {
  const desktop = mode === "desktop" || mode === "playlist" || mode === "error";
  const error = mode === "error";
  const previewFrameUrl = desktop ? PREVIEW_DESKTOP_FRAME_URL : PREVIEW_LANDSCAPE_FRAME_URL;
  const { appearance, clearPreview, previewAppearance } = useTheme();
  const initialAppearance = useRef(appearance);
  const [playing, setPlaying] = useState(false);
  const [playlistOpen, setPlaylistOpen] = useState(mode === "playlist");
  const [currentTime, setCurrentTime] = useState(18 * 60 + 42);
  const [volume, setVolume] = useState(0.7);
  const [muted, setMuted] = useState(false);
  const [rate, setRate] = useState(1);
  const [playbackMode, setPlaybackMode] = useState<RemotePlaybackRequestMode>("direct");
  const [subtitleId, setSubtitleId] = useState<string | undefined>("chs-ass");
  const [subtitleScale, setSubtitleScale] = useState<PlayerSubtitleScale>(100);
  const [videoEnhancement, setVideoEnhancement] = useState<PlayerVideoEnhancement>("balanced");
  const episodeItems = useMemo(createPreviewEpisodeItems, []);

  useEffect(() => {
    previewAppearance({ ...initialAppearance.current, themeMode: "light" });
    return clearPreview;
  }, [clearPreview, previewAppearance]);

  return (
    <main className={desktop ? "player-page player-page-desktop" : "player-page player-page-remote"}>
      <section className="player-video-stage" aria-label="星海回声 第 08 集视频播放器">
        <div
          aria-hidden="true"
          className="absolute inset-0 bg-contain bg-center bg-no-repeat"
          style={{ backgroundImage: `url(${previewFrameUrl})` }}
        />
        <PlayerChrome
          animeTitle="星海回声"
          bufferedSeconds={21 * 60}
          buffering={false}
          canGoNext
          canGoPrevious
          currentTimeSeconds={currentTime}
          durationSeconds={24 * 60 + 18}
          episodeLabel="第 08 集"
          externalPlayerLabel="本机播放器"
          fullscreen={false}
          mode={playbackMode}
          muted={muted}
          onActivity={() => undefined}
          onChangeMode={setPlaybackMode}
          onChangeRate={setRate}
          onChangeSubtitle={setSubtitleId}
          onChangeSubtitleScale={setSubtitleScale}
          onChangeVideoEnhancement={setVideoEnhancement}
          onClose={() => undefined}
          onGoNext={() => undefined}
          onGoPrevious={() => undefined}
          onOpenExternalPlayer={() => undefined}
          onOpenPlaylist={() => setPlaylistOpen(true)}
          onPanelOpenChange={() => undefined}
          onSeek={setCurrentTime}
          onSetAspectRatio={() => undefined}
          onSetVolume={setVolume}
          onToggleFullscreen={() => undefined}
          onToggleMute={() => setMuted((value) => !value)}
          onTogglePictureInPicture={() => undefined}
          onTogglePlay={() => setPlaying((value) => !value)}
          pictureInPicture={false}
          playbackRate={rate}
          playing={playing}
          selectedSubtitleId={subtitleId}
          statusBadges={["原文件直传", "3 条字幕", "1080P"]}
          subtitleScale={subtitleScale}
          subtitleScaleAvailable
          videoEnhancement={videoEnhancement}
          videoEnhancementAvailable
          subtitles={previewSession.subtitles}
          visible
          volume={volume}
        />
        {error && (
          <PlayerErrorState
            message="无法解码当前文件，或当前设备不支持该视频格式。"
            onClose={() => undefined}
            onRetry={() => undefined}
            onTranscode={() => undefined}
          />
        )}
      </section>

      {!desktop && (
        <div className="player-mobile-content">
          <PlayerMobileDetails
            activeItem={previewActiveItem}
            anime={previewAnime}
            coverImageUrl={PREVIEW_POSTER_URL}
            currentTimeSeconds={currentTime}
            episodes={previewEpisodes}
            session={previewSession}
          />
          <div id="player-inline-playlist" className="h-80 min-h-0 pb-4 md:h-[calc(100svh-56.25vw)]">
            <PlayerEpisodeList animeTitle="星海回声" items={episodeItems} onSelect={() => undefined} scrollable />
          </div>
        </div>
      )}

      <PlayerPlaylistSheet
        animeTitle="星海回声"
        items={episodeItems}
        onOpenChange={setPlaylistOpen}
        onSelect={() => undefined}
        open={playlistOpen}
      />
    </main>
  );
}

const previewTask: DownloadTask = {
  id: "preview-task-08",
  animeId: "preview-anime",
  episodeId: "preview-episode-08",
  animeTitle: "星海回声",
  episodeNo: 8,
  fansubName: "北极星字幕组",
  resolution: "1080p",
  normalizedVideoCodec: "H.265/HEVC",
  engine: "embedded",
  name: "[Polaris] Stellar Echo - 08 [1080p][HEVC][AAC].mkv",
  status: "completed",
  progress: 1,
  downloadSpeed: 0,
  uploadSpeed: 0,
  savePath: "preview",
  files: [],
  createdAt: "2026-07-01T00:00:00.000Z",
  completedAt: "2026-07-22T00:00:00.000Z"
};

const previewActiveItem: RemotePlaylistItem = {
  id: "preview-task-08:file:0",
  task: previewTask,
  fileIndex: 0,
  episodeNo: 8,
  fileName: "越过静默轨道",
  displayTitle: "星海回声 · E8",
  contentKind: "episode",
  size: 1_460_288_307
};

const previewAnime: Anime = {
  id: "preview-anime",
  title: "星海回声",
  originalTitle: "星のこだま",
  aliases: [],
  premiereYear: 2026,
  premiereMonth: 7,
  summary: "在人类开始跨越恒星迁徙的时代，一名负责修复旧式通信阵列的少女，意外收到了一段来自失踪殖民舰的循环信号。她与临时组成的调查小队沿着被废弃的航线前进，并逐渐发现这段信号并非来自过去。",
  rating: { score: 8.4, source: "preview" },
  externalIds: {},
  detail: { format: "tv", episodeCount: 12, airingStatus: "airing" }
};

const previewEpisodes: Episode[] = Array.from({ length: 12 }, (_, index) => ({
  id: `preview-episode-${index + 1}`,
  animeId: "preview-anime",
  episodeNo: index + 1,
  title: ["启航", "失落信标", "微光坐标", "旧日航线", "陌生回波", "重启阵列", "记忆的残片", "越过静默轨道", "深空的回音", "追光", "星海边缘", "归航"][index],
  status: index < 7 ? "watched" : index === 7 || index === 8 ? "downloaded" : index === 9 ? "downloading" : "upcoming"
}));

const previewSession: RemotePlaybackSession = {
  id: "preview-session",
  taskId: previewTask.id,
  fileIndex: 0,
  fileName: previewTask.name,
  mode: "direct",
  streamUrl: "preview://video",
  expiresAt: "2026-07-24T00:00:00.000Z",
  durationSeconds: 24 * 60 + 18,
  subtitles: [
    { id: "chs-ass", label: "简体中文", language: "zh-CN", type: "ass", url: "preview://chs", default: true },
    { id: "cht-ass", label: "繁体中文", language: "zh-TW", type: "ass", url: "preview://cht", default: false },
    { id: "jpn-vtt", label: "日文", language: "ja", type: "vtt", url: "preview://jpn", default: false }
  ],
  diagnostics: {
    encoder: "libx264",
    encoderDegraded: true,
    subtitleMode: "soft",
    enhancedFrameInput: false
  }
};

/** 构造与 Stitch 稿一致的十二集预览状态。 */
function createPreviewEpisodeItems(): PlayerEpisodeUiItem[] {
  const episodes = previewEpisodes.map<PlayerEpisodeUiItem>((episode) => {
    const playing = episode.episodeNo === 8;
    const watched = episode.episodeNo <= 7;
    const ready = episode.episodeNo === 9;
    const downloading = episode.episodeNo === 10;
    const playable = playing || watched || ready;
    const originalFileName = `[Polaris] Stellar Echo - ${String(episode.episodeNo).padStart(2, "0")} [1080p].mkv`;
    return {
      id: episode.id,
      episodeNo: episode.episodeNo,
      numberLabel: String(episode.episodeNo).padStart(2, "0"),
      title: playable ? `星海回声 · E${episode.episodeNo}` : episode.title ?? `第 ${episode.episodeNo} 集`,
      meta: playable
        ? `${originalFileName} · ${playing ? "24:18" : "24:00"} · 1080P${ready ? " · 1.36 GB" : ""}`
        : "24:00 · 1080P",
      status: playing ? "playing" : watched ? "watched" : ready ? "ready" : downloading ? "downloading" : "unavailable",
      statusLabel: playing ? "正在播放" : watched ? "已看" : ready ? "已下载" : downloading ? "下载中 45%" : "未下载",
      progress: playing ? 0.77 : watched ? 1 : downloading ? 0.45 : 0,
      section: "episodes",
      playlistItem: playable ? {
        ...previewActiveItem,
        id: `preview-${episode.episodeNo}`,
        episodeNo: episode.episodeNo,
        fileName: originalFileName,
        displayTitle: `星海回声 · E${episode.episodeNo}`
      } : undefined
    };
  });
  const specialPlaylistItem: RemotePlaylistItem = {
    ...previewActiveItem,
    id: "preview-special-01",
    episodeNo: undefined,
    fileName: "[Polaris] Stellar Echo SP01 [1080p].mkv",
    displayTitle: "星海回声 · SP01",
    contentKind: "special",
    specialNo: "SP01"
  };
  return [...episodes, {
    id: specialPlaylistItem.id,
    numberLabel: "SP01",
    title: specialPlaylistItem.displayTitle,
    meta: `${specialPlaylistItem.fileName} · 05:12 · 1080P`,
    status: "ready",
    statusLabel: "已下载",
    progress: 0,
    section: "specials",
    playlistItem: specialPlaylistItem
  }];
}
