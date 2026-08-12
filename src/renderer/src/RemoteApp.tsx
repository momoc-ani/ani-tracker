import { Bell, Download, Home, Library, Search, Settings, Sparkles, Subtitles } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { AppShell, type AppShellStatus } from "@/components/app-shell";
import { AnimeDetailPage, type AnimeDetailLibraryAction } from "@/features/anime-detail/AnimeDetailPage";
import {
  DiscoveryPage,
  DiscoverySchedulePage,
  type SeasonTarget
} from "@/features/discovery/DiscoveryPage";
import { HomePage } from "@/features/home/HomePage";
import { MyAnimePage, type MyAnimePageIntent } from "@/features/my-anime/MyAnimePage";
import { NotificationsPage } from "@/features/notifications/NotificationsPage";
import { ReleaseSearchPage } from "@/features/release-search/ReleaseSearchPage";
import { RemoteDownloadsPage } from "@/features/remote/RemoteDownloadsPage";
import { RemotePairingPage } from "@/features/remote/RemotePairingPage";
import { RemotePlayerPage, resolveRemotePlayerTaskId } from "@/features/remote/RemotePlayerPage";
import { SettingsPage } from "@/features/settings/SettingsPage";
import { SourcesPage } from "@/features/sources/SourcesPage";
import {
  appApi,
  getRemotePairingState,
  REMOTE_AUTH_CHANGED_EVENT,
  type RemotePairingState
} from "@/lib/api";
import type { Anime } from "@shared/domain";
import type { MediaPlaybackTarget } from "@shared/player-selection";

type RemotePageId = "home" | "myAnime" | "discovery" | "releaseSearch" | "downloads" | "notifications" | "sources" | "settings";

const remoteNavItems = [
  { id: "home", label: "首页", icon: Home },
  { id: "myAnime", label: "我的追番", icon: Library },
  { id: "discovery", label: "新番发现", icon: Sparkles },
  { id: "releaseSearch", label: "资源搜索", icon: Search },
  { id: "downloads", label: "下载队列", icon: Download },
  { id: "notifications", label: "提醒中心", icon: Bell },
  { id: "sources", label: "下载源", icon: Subtitles },
  { id: "settings", label: "设置", icon: Settings }
] satisfies Array<{ id: RemotePageId; label: string; icon: typeof Home }>;

interface AnimeDetailOrigin {
  pageId: RemotePageId;
  label: string;
  scrollTop: number;
  focusElement: HTMLElement | null;
}

interface AnimeDetailState {
  animeId: string;
  previewAnime?: Anime;
  origin: AnimeDetailOrigin;
}

const connectedStatus: AppShellStatus = {
  state: "online",
  label: "桌面端在线",
  detail: "远程同步已连接"
};

/** 渲染桌面网关单独托管的远程 PWA。 */
export function App() {
  const playerTaskId = resolveRemotePlayerTaskId(window.location.pathname);
  if (playerTaskId) return <RemotePlayerPage taskId={playerTaskId} />;
  return <RemoteApplication />;
}

/** 管理远程 PWA 的配对、导航和二级视图。 */
function RemoteApplication() {
  const [activePage, setActivePage] = useState<RemotePageId>("home");
  const [detailView, setDetailView] = useState<AnimeDetailState | null>(null);
  const [discoverySchedule, setDiscoverySchedule] = useState<SeasonTarget | null>(null);
  const [myAnimeIntent, setMyAnimeIntent] = useState<MyAnimePageIntent | null>(null);
  const [releaseSearchIntent, setReleaseSearchIntent] = useState<{ keyword: string; key: number } | null>(null);
  const [pairingState, setPairingState] = useState<RemotePairingState>(getRemotePairingState);
  const [unreadCount, setUnreadCount] = useState(0);
  const contentRef = useRef<HTMLElement | null>(null);
  const detailViewRef = useRef<AnimeDetailState | null>(null);
  const discoveryScheduleRef = useRef<SeasonTarget | null>(null);
  detailViewRef.current = detailView;
  discoveryScheduleRef.current = discoverySchedule;

  /** 记录来源上下文并进入番剧详情。 */
  function openAnimeDetail(animeId: string, previewAnime?: Anime): void {
    const originItem = remoteNavItems.find((item) => item.id === activePage);
    window.history.pushState({ aniView: "animeDetail", animeId }, "");
    setDetailView({
      animeId,
      previewAnime,
      origin: {
        pageId: activePage,
        label: discoverySchedule ? "新番时间表" : originItem?.label ?? "上一页",
        scrollTop: contentRef.current?.scrollTop ?? 0,
        focusElement: document.activeElement instanceof HTMLElement ? document.activeElement : null
      }
    });
    window.requestAnimationFrame(() => contentRef.current?.scrollTo({ top: 0, behavior: "auto" }));
  }

  /** 从详情返回来源页并恢复滚动和焦点。 */
  function restoreDetailView(): void {
    const origin = detailViewRef.current?.origin;
    setDetailView(null);
    if (!origin) return;
    window.requestAnimationFrame(() => {
      contentRef.current?.scrollTo({ top: origin.scrollTop, behavior: "auto" });
      origin.focusElement?.focus({ preventScroll: true });
    });
  }

  /** 切换远程主导航并退出二级视图。 */
  function navigatePage(pageId: RemotePageId): void {
    if (detailViewRef.current || discoveryScheduleRef.current) {
      window.history.replaceState({ aniView: "page", pageId }, "");
      setDetailView(null);
      setDiscoverySchedule(null);
    }
    setActivePage(pageId);
  }

  /** 关闭详情并进入指定远程业务页。 */
  function leaveDetailToPage(pageId: RemotePageId): void {
    window.history.replaceState({ aniView: "page", pageId }, "");
    setDetailView(null);
    setDiscoverySchedule(null);
    setActivePage(pageId);
    window.requestAnimationFrame(() => contentRef.current?.scrollTo({ top: 0, behavior: "auto" }));
  }

  /** 打开复用的新番时间表二级页。 */
  function openDiscoverySchedule(target: SeasonTarget): void {
    window.history.pushState({ aniView: "discoverySchedule" }, "");
    setDiscoverySchedule(target);
    window.requestAnimationFrame(() => contentRef.current?.scrollTo({ top: 0, behavior: "auto" }));
  }

  /** 在独立标签页打开远程播放器。 */
  async function playRemoteMedia(target: MediaPlaybackTarget): Promise<void> {
    if (!target.taskId) throw new Error("当前媒体缺少下载任务关联，无法远程播放");
    const playerUrl = new URL(`/player/${encodeURIComponent(target.taskId)}`, window.location.origin);
    if (target.fileIndex !== undefined) playerUrl.searchParams.set("file", String(target.fileIndex));
    window.open(playerUrl, "_blank", "noopener,noreferrer");
    console.info("[remote] 已打开独立播放器标签页", {
      taskId: target.taskId,
      fileIndex: target.fileIndex
    });
  }

  /** 将详情中的追番操作转到复用的追番工作台。 */
  function openLibraryAction(animeId: string, action: AnimeDetailLibraryAction): void {
    setMyAnimeIntent({ animeId, action, key: Date.now() });
    leaveDetailToPage("myAnime");
  }

  /** 将详情中的资源搜索请求带入共享搜索页。 */
  function openReleaseSearch(anime: Anime): void {
    setReleaseSearchIntent({ keyword: anime.title, key: Date.now() });
    leaveDetailToPage("releaseSearch");
  }

  useEffect(() => {
    if (!window.history.state?.aniView) {
      window.history.replaceState({ aniView: "page", pageId: activePage }, "");
    }
    const handlePopState = () => {
      if (detailViewRef.current) restoreDetailView();
      else if (discoveryScheduleRef.current) setDiscoverySchedule(null);
    };
    window.addEventListener("popstate", handlePopState);
    return () => window.removeEventListener("popstate", handlePopState);
  }, []);

  useEffect(() => {
    /** 同步当前标签页与其他远程页面的鉴权状态。 */
    function refreshRemoteAuth(): void {
      setPairingState(getRemotePairingState());
      setActivePage("home");
    }
    window.addEventListener(REMOTE_AUTH_CHANGED_EVENT, refreshRemoteAuth);
    window.addEventListener("storage", refreshRemoteAuth);
    return () => {
      window.removeEventListener(REMOTE_AUTH_CHANGED_EVENT, refreshRemoteAuth);
      window.removeEventListener("storage", refreshRemoteAuth);
    };
  }, []);

  useEffect(() => {
    if (pairingState.needsPairing) return;
    let active = true;

    /** 刷新远程应用壳的未读提醒数量。 */
    async function refreshUnreadCount(): Promise<void> {
      try {
        const count = await appApi.getUnreadNotificationCount();
        if (active) setUnreadCount(count);
      } catch (error) {
        console.warn("[remote] 未读提醒数量刷新失败", error);
      }
    }
    void refreshUnreadCount();
    const timer = window.setInterval(() => void refreshUnreadCount(), 30_000);
    window.addEventListener("focus", refreshUnreadCount);
    return () => {
      active = false;
      window.clearInterval(timer);
      window.removeEventListener("focus", refreshUnreadCount);
    };
  }, [pairingState.needsPairing]);

  if (pairingState.needsPairing) {
    return <RemotePairingPage onPaired={() => setPairingState(getRemotePairingState())} />;
  }

  return (
    <AppShell
      activePageId={activePage}
      contentRef={contentRef}
      items={remoteNavItems}
      onNavigate={(pageId) => navigatePage(pageId as RemotePageId)}
      secondaryView={detailView
        ? { key: `anime-detail:${detailView.animeId}`, title: "番剧详情", onBack: () => window.history.back() }
        : discoverySchedule
          ? { key: "discovery-schedule", title: "新番时间表", onBack: () => window.history.back() }
          : undefined}
      status={connectedStatus}
      unreadCount={unreadCount}
    >
      <div className={detailView || discoverySchedule ? "hidden" : undefined}>
        {renderRemotePage(activePage, {
          myAnimeIntent,
          onDiscoverySchedule: openDiscoverySchedule,
          onMyAnimeIntentHandled: () => setMyAnimeIntent(null),
          onOpenAnimeDetail: openAnimeDetail,
          onOpenDownloads: () => navigatePage("downloads"),
          onPlayMedia: playRemoteMedia,
          releaseSearchIntent
        })}
      </div>
      {discoverySchedule && !detailView && (
        <DiscoverySchedulePage
          initialTarget={discoverySchedule}
          onBack={() => window.history.back()}
          onOpenAnimeDetail={openAnimeDetail}
        />
      )}
      {detailView && (
        <AnimeDetailPage
          allowLibraryManagement
          animeId={detailView.animeId}
          onBack={() => window.history.back()}
          onOpenLibraryAction={openLibraryAction}
          onOpenReleaseSearch={openReleaseSearch}
          previewAnime={detailView.previewAnime}
          sourceLabel={detailView.origin.label}
        />
      )}
    </AppShell>
  );
}

/** 根据远程导航标识渲染共享或受限业务页面。 */
function renderRemotePage(page: RemotePageId, options: {
  myAnimeIntent: MyAnimePageIntent | null;
  onDiscoverySchedule: (target: SeasonTarget) => void;
  onMyAnimeIntentHandled: () => void;
  onOpenAnimeDetail: (animeId: string, previewAnime?: Anime) => void;
  onOpenDownloads: () => void;
  onPlayMedia: (target: MediaPlaybackTarget) => Promise<void>;
  releaseSearchIntent: { keyword: string; key: number } | null;
}) {
  switch (page) {
    case "home":
      return (
        <HomePage
          onOpenAnimeDetail={options.onOpenAnimeDetail}
          onOpenDownloads={options.onOpenDownloads}
          onPlayMedia={options.onPlayMedia}
        />
      );
    case "myAnime":
      return (
        <MyAnimePage
          allowLocalPathRules={false}
          intent={options.myAnimeIntent}
          onIntentHandled={options.onMyAnimeIntentHandled}
          onOpenAnimeDetail={options.onOpenAnimeDetail}
          onPlayMedia={options.onPlayMedia}
        />
      );
    case "discovery":
      return (
        <DiscoveryPage
          allowCollection={false}
          onOpenAnimeDetail={options.onOpenAnimeDetail}
          onOpenSchedule={options.onDiscoverySchedule}
        />
      );
    case "releaseSearch":
      return <ReleaseSearchPage initialIntent={options.releaseSearchIntent} />;
    case "downloads":
      return <RemoteDownloadsPage />;
    case "notifications":
      return <NotificationsPage />;
    case "sources":
      return <SourcesPage allowImmediateSync={false} />;
    case "settings":
      return <SettingsPage />;
  }
}
