import {
  Bell,
  Download,
  Home,
  Library,
  Search,
  Settings,
  Sparkles,
  Subtitles
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { AppShell, type AppShellStatus } from "@/components/app-shell";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle
} from "@/components/ui/alert-dialog";
import { WindowControls } from "@/components/window-controls";
import { AnimeDetailPage, type AnimeDetailLibraryAction } from "@/features/anime-detail/AnimeDetailPage";
import {
  DiscoveryPage,
  DiscoverySchedulePage,
  type SeasonTarget
} from "@/features/discovery/DiscoveryPage";
import { DownloadsPage } from "@/features/downloads/DownloadsPage";
import { HomePage } from "@/features/home/HomePage";
import { MyAnimePage } from "@/features/my-anime/MyAnimePage";
import { NotificationsPage } from "@/features/notifications/NotificationsPage";
import { ReleaseSearchPage } from "@/features/release-search/ReleaseSearchPage";
import { SettingsPage } from "@/features/settings/SettingsPage";
import { PlayerDesignPreview } from "@/features/player/PlayerDesignPreview";
import { DesktopPlayerPage } from "@/features/player/DesktopPlayerPage";
import { DesktopVlcHostPage } from "@/features/player/DesktopVlcHostPage";
import { SourcesPage } from "@/features/sources/SourcesPage";
import { appApi, isTauriClient } from "@/lib/api";
import {
  claimMobileDownloadNotificationPrompt,
  onManualDownloadAdded
} from "@/lib/mobile-download-notification";
import { getAppCapabilities, getAppRuntime } from "@/lib/runtime";
import { toast } from "@/lib/toast";
import type { DownloadServiceStatus, MobilePlatformStatus } from "@shared/contracts";
import type { Anime } from "@shared/domain";
import type { MyAnimePageIntent } from "@/features/my-anime/MyAnimePage";
import {
  isDesktopVlcHostView,
  resolveDesktopPlayerWindowInput
} from "@shared/desktop-player-route";
import {
  resolvePlaybackFileIndex,
  usesBuiltinPlayer,
  type MediaPlaybackTarget
} from "@shared/player-selection";

type PageId = "home" | "myAnime" | "discovery" | "releaseSearch" | "downloads" | "notifications" | "sources" | "settings";

const navItems = [
  { id: "home", label: "首页", icon: Home },
  { id: "myAnime", label: "我的追番", icon: Library },
  { id: "discovery", label: "新番发现", icon: Sparkles },
  { id: "releaseSearch", label: "资源搜索", icon: Search },
  { id: "downloads", label: "下载队列", icon: Download },
  { id: "notifications", label: "提醒中心", icon: Bell },
  { id: "sources", label: "下载源", icon: Subtitles },
  { id: "settings", label: "设置", icon: Settings }
] satisfies Array<{ id: PageId; label: string; icon: typeof Home }>;

interface AnimeDetailOrigin {
  pageId: PageId;
  label: string;
  scrollTop: number;
  focusElement: HTMLElement | null;
}

interface AnimeDetailState {
  animeId: string;
  previewAnime?: Anime;
  origin: AnimeDetailOrigin;
}

interface DiscoveryScheduleState {
  target: SeasonTarget;
  scrollTop: number;
  focusElement: HTMLElement | null;
}

interface ReleaseSearchIntent {
  keyword: string;
  key: number;
}

interface RenderPageOptions {
  discoveryWorkspaceTabs: boolean;
  onOpenAnimeDetail: (animeId: string, previewAnime?: Anime) => void;
  onOpenDownloads: () => void;
  onOpenLibraryAction: (animeId: string, action: AnimeDetailLibraryAction) => void;
  onOpenReleaseSearch: (anime: Anime) => void;
  onOpenDiscoverySchedule: (target: SeasonTarget) => void;
  onPlayMedia: (target: MediaPlaybackTarget) => Promise<void>;
  myAnimeIntent: MyAnimePageIntent | null;
  onMyAnimeIntentHandled: () => void;
  releaseSearchIntent: ReleaseSearchIntent | null;
}

/** 将主进程统一下载状态转换为应用壳展示状态。 */
function toDownloadShellStatus(status: DownloadServiceStatus): AppShellStatus {
  const detail = status.taskCount === undefined
    ? status.message
    : `${status.message} · ${status.taskCount} 个任务`;
  if (status.state === "online") {
    return { state: "online", label: "下载服务正常", detail };
  }
  if (status.state === "idle") {
    return { state: "idle", label: "下载服务待机", detail };
  }
  return { state: "error", label: "下载服务异常", detail };
}

/** 移动端资源约束优先覆盖下载服务状态，避免继续触发不可恢复操作。 */
function applyMobileShellStatus(
  status: AppShellStatus,
  mobileStatus: MobilePlatformStatus | null
): AppShellStatus {
  if (!mobileStatus) return status;
  if (mobileStatus.storage === "critical") {
    return {
      state: "error",
      label: "存储空间不足",
      detail: "可用空间低于 256 MiB，新增和恢复下载已暂停"
    };
  }
  if (mobileStatus.network === "offline") {
    return {
      state: "idle",
      label: "当前离线",
      detail: "本地数据可用，下载任务等待网络恢复"
    };
  }
  if (mobileStatus.network === "limited") {
    return {
      state: "idle",
      label: "网络待验证",
      detail: "联网操作将等待系统确认网络可用"
    };
  }
  if (mobileStatus.storage === "low") {
    return {
      state: "idle",
      label: "存储空间偏低",
      detail: "建议清理空间后再添加大型下载"
    };
  }
  return status;
}

/** 根据导航标识渲染对应业务页面。 */
function renderPage(page: PageId, options: RenderPageOptions) {
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
          intent={options.myAnimeIntent}
          onIntentHandled={options.onMyAnimeIntentHandled}
          onOpenAnimeDetail={options.onOpenAnimeDetail}
          onPlayMedia={options.onPlayMedia}
        />
      );
    case "discovery":
      return (
        <DiscoveryPage
          onOpenAnimeDetail={options.onOpenAnimeDetail}
          onOpenSchedule={options.onOpenDiscoverySchedule}
          workspaceTabs={options.discoveryWorkspaceTabs}
        />
      );
    case "releaseSearch":
      return <ReleaseSearchPage initialIntent={options.releaseSearchIntent} />;
    case "downloads":
      return <DownloadsPage />;
    case "notifications":
      return <NotificationsPage />;
    case "sources":
      return <SourcesPage />;
    case "settings":
      return <SettingsPage />;
  }
}

/** 按当前窗口用途渲染主界面或独立播放器。 */
export function App() {
  if (isDesktopVlcHostView(window.location.search)) {
    return <DesktopVlcHostPage />;
  }
  const playerPreview = import.meta.env.DEV
    ? new URLSearchParams(window.location.search).get("playerPreview")
    : null;
  if (playerPreview) {
    return <PlayerDesignPreview mode={playerPreview} />;
  }
  const desktopPlayerTarget = getAppRuntime() === "desktop"
    ? resolveDesktopPlayerWindowInput(window.location.search)
    : null;
  if (desktopPlayerTarget) {
    return (
      <DesktopPlayerPage
        initialFileIndex={desktopPlayerTarget.fileIndex}
        onClose={() => {
          try {
            appApi.closeDesktopPlayerWindow();
          } catch (error) {
            console.error("[player] 独立播放器窗口关闭失败", error);
          }
        }}
        taskId={desktopPlayerTarget.taskId}
      />
    );
  }
  return <MainApplication />;
}

/** 渲染适配桌面、平板和移动端的应用主界面。 */
function MainApplication() {
  const [activePage, setActivePage] = useState<PageId>("home");
  const [detailView, setDetailView] = useState<AnimeDetailState | null>(null);
  const [discoverySchedule, setDiscoverySchedule] = useState<DiscoveryScheduleState | null>(null);
  const [detailActionHostActive, setDetailActionHostActive] = useState(false);
  const [detailRevision, setDetailRevision] = useState(0);
  const [myAnimeIntent, setMyAnimeIntent] = useState<MyAnimePageIntent | null>(null);
  const [releaseSearchIntent, setReleaseSearchIntent] = useState<ReleaseSearchIntent | null>(null);
  const [unreadCount, setUnreadCount] = useState(0);
  const [shellStatus, setShellStatus] = useState<AppShellStatus>({
    state: "unknown",
    label: "状态读取中",
    detail: "正在连接服务"
  });
  const [mobileStatus, setMobileStatus] = useState<MobilePlatformStatus | null>(null);
  const [notificationPromptOpen, setNotificationPromptOpen] = useState(false);
  const [requestingNotificationPermission, setRequestingNotificationPermission] = useState(false);
  const contentRef = useRef<HTMLElement | null>(null);
  const detailViewRef = useRef<AnimeDetailState | null>(null);
  const discoveryScheduleRef = useRef<DiscoveryScheduleState | null>(null);
  detailViewRef.current = detailView;
  discoveryScheduleRef.current = discoverySchedule;
  const runtime = getAppRuntime();
  const capabilities = getAppCapabilities();
  const desktopClient = runtime === "desktop";
  const framelessWindow = capabilities.windowControls
    && desktopClient
    && isTauriClient();

  /** 记录来源页面上下文并进入详情二级视图。 */
  function openAnimeDetail(animeId: string, previewAnime?: Anime) {
    const originItem = navItems.find((item) => item.id === activePage);
    const origin: AnimeDetailOrigin = {
      pageId: activePage,
      label: originItem?.label ?? "上一页",
      scrollTop: contentRef.current?.scrollTop ?? 0,
      focusElement: document.activeElement instanceof HTMLElement ? document.activeElement : null
    };
    const nextState = { aniView: "animeDetail", animeId };
    window.history.pushState(nextState, "");
    setDetailActionHostActive(false);
    setDetailRevision(0);
    setMyAnimeIntent(null);
    setDetailView({ animeId, previewAnime, origin });
    window.requestAnimationFrame(() => contentRef.current?.scrollTo({ top: 0, behavior: "auto" }));
  }

  /** 从详情返回来源页，并恢复滚动位置和触发元素焦点。 */
  function restoreDetailView() {
    const origin = detailViewRef.current?.origin;
    setDetailActionHostActive(false);
    setMyAnimeIntent(null);
    setDetailView(null);
    if (!origin) return;
    window.requestAnimationFrame(() => {
      contentRef.current?.scrollTo({ top: origin.scrollTop, behavior: "auto" });
      origin.focusElement?.focus({ preventScroll: true });
      console.info("[anime-detail] navigation restored", {
        pageId: origin.pageId,
        scrollTop: origin.scrollTop
      });
    });
  }

  /** 进入独立时间表并保留发现页的滚动与焦点上下文。 */
  function openDiscoverySchedule(target: SeasonTarget) {
    const nextState: DiscoveryScheduleState = {
      target,
      scrollTop: contentRef.current?.scrollTop ?? 0,
      focusElement: document.activeElement instanceof HTMLElement ? document.activeElement : null
    };
    window.history.pushState({ aniView: "discoverySchedule", target }, "");
    setDiscoverySchedule(nextState);
    window.requestAnimationFrame(() => contentRef.current?.scrollTo({ top: 0, behavior: "auto" }));
    console.info("[discovery] 已打开新番时间表", target);
  }

  /** 返回新番发现并恢复进入时间表前的滚动与焦点。 */
  function restoreDiscoverySchedule() {
    const origin = discoveryScheduleRef.current;
    setDiscoverySchedule(null);
    if (!origin) return;
    window.requestAnimationFrame(() => {
      contentRef.current?.scrollTo({ top: origin.scrollTop, behavior: "auto" });
      origin.focusElement?.focus({ preventScroll: true });
    });
  }

  /** 关闭详情并切换到指定业务页，供详情快捷操作使用。 */
  function leaveDetailToPage(pageId: PageId) {
    if (detailViewRef.current) {
      window.history.replaceState({ aniView: "page", pageId }, "");
      setDetailActionHostActive(false);
      setMyAnimeIntent(null);
      setDetailView(null);
    }
    setDiscoverySchedule(null);
    setActivePage(pageId);
    window.requestAnimationFrame(() => contentRef.current?.scrollTo({ top: 0, behavior: "auto" }));
  }

  /** 从详情页打开追番规则、资源或任务面板。 */
  function openLibraryAction(animeId: string, action: AnimeDetailLibraryAction) {
    setDetailActionHostActive(true);
    setMyAnimeIntent({ animeId, action, key: Date.now() });
    console.info("[anime-detail] library action opened in place", { animeId, action });
  }

  /** 将未追番资源搜索请求带入资源搜索页。 */
  function openReleaseSearch(anime: Anime) {
    setReleaseSearchIntent({
      keyword: anime.title,
      key: Date.now()
    });
    leaveDetailToPage("releaseSearch");
  }

  /** 主导航切换时退出详情并回到页面顶部。 */
  function navigatePage(pageId: PageId) {
    if (detailViewRef.current || discoveryScheduleRef.current) {
      window.history.replaceState({ aniView: "page", pageId }, "");
      setDetailActionHostActive(false);
      setMyAnimeIntent(null);
      setDetailView(null);
      setDiscoverySchedule(null);
    }
    setActivePage(pageId);
  }

  /** 按默认播放器配置打开独立内置窗口或调用外部播放器。 */
  async function playMedia(target: MediaPlaybackTarget): Promise<void> {
    if (runtime === "android" || runtime === "ios") {
      if (!target.taskId) {
        throw new Error("当前媒体缺少下载任务关联，无法使用移动内置播放器");
      }
      const mobileTarget = {
        taskId: target.taskId,
        ...(target.fileIndex === undefined ? {} : { fileIndex: target.fileIndex })
      };
      await appApi.openDesktopPlayerWindow(mobileTarget);
      console.info("[player] 已打开移动原生播放器", mobileTarget);
      return;
    }
    const settings = await appApi.getSettings();
    if (!usesBuiltinPlayer(settings)) {
      await appApi.playMedia(target.filePath);
      return;
    }
    if (!target.taskId) {
      await appApi.playMedia(target.filePath);
      console.info("[player] 原地导入媒体使用系统播放器", { filePath: target.filePath });
      return;
    }
    let fileIndex = target.fileIndex;
    if (fileIndex === undefined) {
      const task = (await appApi.listDownloads()).find((item) => item.id === target.taskId);
      if (task) {
        fileIndex = resolvePlaybackFileIndex(target, task);
      }
    }
    const playerTarget = {
      taskId: target.taskId,
      ...(fileIndex === undefined ? {} : { fileIndex })
    };
    await appApi.openDesktopPlayerWindow(playerTarget);
    console.info("[player] 已打开独立内置播放器窗口", playerTarget);
  }

  useEffect(() => {
    if (!window.history.state?.aniView) {
      window.history.replaceState({ aniView: "page", pageId: activePage }, "");
    }
    const handlePopState = () => {
      if (detailViewRef.current) {
        restoreDetailView();
      } else if (discoveryScheduleRef.current) {
        restoreDiscoverySchedule();
      }
    };
    window.addEventListener("popstate", handlePopState);
    return () => window.removeEventListener("popstate", handlePopState);
  }, []);

  useEffect(() => {
    if (!detailView) return;
    function handleEscape(event: KeyboardEvent) {
      const target = event.target as HTMLElement | null;
      const editable = target?.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(target?.tagName ?? "");
      const dialogOpen = Boolean(document.querySelector('[role="dialog"][data-state="open"]'));
      if (event.key === "Escape" && window.matchMedia("(min-width: 768px)").matches && !editable && !dialogOpen) {
        event.preventDefault();
        window.history.back();
      }
    }
    window.addEventListener("keydown", handleEscape);
    return () => window.removeEventListener("keydown", handleEscape);
  }, [detailView]);

  useEffect(() => {
    let active = true;
    let refreshSequence = 0;

    /** 刷新应用壳所需的未读数与下载服务状态。 */
    async function refreshShellState() {
      const sequence = ++refreshSequence;
      const [unreadResult, serviceResult] = await Promise.allSettled([
        appApi.getUnreadNotificationCount(),
        appApi.getDownloadServiceStatus()
      ]);
      if (!active || sequence !== refreshSequence) {
        return;
      }
      if (unreadResult.status === "fulfilled") {
        setUnreadCount(unreadResult.value);
      } else {
        console.warn("[app-shell] 未读提醒数量刷新失败", unreadResult.reason);
      }

      if (serviceResult.status === "fulfilled" && serviceResult.value) {
        setShellStatus(toDownloadShellStatus(serviceResult.value));
      } else {
        setShellStatus({ state: "unknown", label: "服务状态未知", detail: "稍后自动重试" });
        if (serviceResult.status === "rejected") {
          console.warn("[app-shell] 下载服务状态刷新失败", serviceResult.reason);
        }
      }
    }

    void refreshShellState();
    const refreshTimer = window.setInterval(() => void refreshShellState(), 30_000);
    const unsubscribeDownloadStatus = appApi.onDownloadServiceStatusChanged(() => void refreshShellState());
    window.addEventListener("focus", refreshShellState);
    return () => {
      active = false;
      window.clearInterval(refreshTimer);
      unsubscribeDownloadStatus?.();
      window.removeEventListener("focus", refreshShellState);
    };
  }, []);

  useEffect(() => {
    if (runtime !== "android" && runtime !== "ios") return;
    let active = true;

    /** 刷新移动运行约束，并消费通知要求打开的页面。 */
    async function refreshMobileRuntime() {
      try {
        const [status, intent, backgroundRefreshDue] = await Promise.all([
          appApi.getMobilePlatformStatus?.(),
          appApi.consumeMobileNavigation?.(),
          appApi.consumeMobileBackgroundRefresh?.()
        ]);
        if (active && status) setMobileStatus(status);
        if (active && intent) navigatePage(intent.pageId);
        if (active && backgroundRefreshDue) {
          void appApi.syncSourcesNow()
            .then(() => appApi.runAutomationOnce())
            .then(() => console.info("[mobile-runtime] iOS 后台补跑完成"))
            .catch((error) => console.warn("[mobile-runtime] iOS 后台补跑失败", error));
        }
      } catch (error) {
        console.warn("[mobile-runtime] 移动平台状态刷新失败", error);
      }
    }

    void refreshMobileRuntime();
    const timer = window.setInterval(() => void refreshMobileRuntime(), 30_000);
    const handleVisibility = () => {
      if (document.visibilityState === "visible") void refreshMobileRuntime();
    };
    window.addEventListener("focus", refreshMobileRuntime);
    window.addEventListener("online", refreshMobileRuntime);
    window.addEventListener("offline", refreshMobileRuntime);
    window.addEventListener("orientationchange", refreshMobileRuntime);
    document.addEventListener("visibilitychange", handleVisibility);
    return () => {
      active = false;
      window.clearInterval(timer);
      window.removeEventListener("focus", refreshMobileRuntime);
      window.removeEventListener("online", refreshMobileRuntime);
      window.removeEventListener("offline", refreshMobileRuntime);
      window.removeEventListener("orientationchange", refreshMobileRuntime);
      document.removeEventListener("visibilitychange", handleVisibility);
    };
  }, [runtime]);

  useEffect(() => {
    if ((runtime !== "android" && runtime !== "ios") || !isTauriClient()) return;
    let disposed = false;
    let unregister: (() => Promise<void>) | undefined;

    /** 将系统通知点击携带的白名单页面映射到本地主导航。 */
    void import("@tauri-apps/plugin-notification")
      .then(({ onAction }) => onAction((notification) => {
        const pageId = notification.extra?.aniPageId;
        if (pageId === "home" || pageId === "downloads" || pageId === "notifications") {
          navigatePage(pageId);
        }
      }))
      .then((listener) => {
        if (disposed) {
          void listener.unregister();
        } else {
          unregister = () => listener.unregister();
        }
      })
      .catch((error) => console.warn("[mobile-runtime] 系统通知导航监听失败", error));

    return () => {
      disposed = true;
      void unregister?.();
    };
  }, [runtime]);

  useEffect(() => {
    if (runtime !== "android" && runtime !== "ios") return;

    /** 首次手动下载成功后，按当前系统权限决定是否显示一次性引导。 */
    return onManualDownloadAdded(() => {
      if (!claimMobileDownloadNotificationPrompt()) return;
      void appApi.getMobilePlatformStatus?.()
        .then((status) => {
          const permission = status?.notificationPermission;
          if (permission === "granted" || permission === "denied" || permission === "not-required") {
            console.info("[mobile-notification] 无需显示首次下载授权引导", { permission });
            return;
          }
          setNotificationPromptOpen(true);
        })
        .catch((error) => {
          console.warn("[mobile-notification] 权限状态读取失败，继续显示授权引导", error);
          setNotificationPromptOpen(true);
        });
    });
  }, [runtime]);

  /** 由首次下载引导触发系统通知权限申请。 */
  async function requestDownloadNotificationPermission() {
    setRequestingNotificationPermission(true);
    try {
      const result = await appApi.requestMobileNotificationPermission?.();
      if (result === "granted" || result === "not-required") {
        toast.success("下载通知已开启");
      } else {
        toast.warning("下载通知未开启，可稍后在设置中授权");
      }
      setNotificationPromptOpen(false);
    } catch (error) {
      console.warn("[mobile-notification] 系统通知权限申请失败", error);
      toast.error(error instanceof Error ? error.message : "系统通知权限申请失败");
    } finally {
      setRequestingNotificationPermission(false);
    }
  }

  return (
    <>
      <AppShell
        activePageId={activePage}
        items={navItems}
        onNavigate={(pageId) => navigatePage(pageId as PageId)}
        contentRef={contentRef}
        secondaryView={detailView
          ? { key: `anime-detail:${detailView.animeId}`, title: "番剧详情", onBack: () => window.history.back() }
          : discoverySchedule
            ? {
                key: `discovery-schedule:${discoverySchedule.target.year}:${discoverySchedule.target.season}`,
                title: "新番时间表",
                onBack: () => window.history.back()
              }
            : undefined}
        status={applyMobileShellStatus(shellStatus, mobileStatus)}
        unreadCount={unreadCount}
        framelessWindow={framelessWindow}
        windowControls={framelessWindow ? <WindowControls /> : undefined}
      >
        <div className={detailView || discoverySchedule ? "hidden" : undefined}>
          {renderPage(activePage, {
            discoveryWorkspaceTabs: runtime !== "android" && runtime !== "ios",
            onOpenAnimeDetail: openAnimeDetail,
            onOpenDownloads: () => navigatePage("downloads"),
            onOpenLibraryAction: openLibraryAction,
            onOpenReleaseSearch: openReleaseSearch,
            onOpenDiscoverySchedule: openDiscoverySchedule,
            onPlayMedia: playMedia,
            myAnimeIntent: detailView ? null : myAnimeIntent,
            onMyAnimeIntentHandled: () => setMyAnimeIntent(null),
            releaseSearchIntent
          })}
        </div>
        {discoverySchedule && !detailView && (
          <DiscoverySchedulePage
            initialTarget={discoverySchedule.target}
            onBack={() => window.history.back()}
            onOpenAnimeDetail={openAnimeDetail}
          />
        )}
        {detailView && detailActionHostActive && (
          <MyAnimePage
            actionOnly
            intent={myAnimeIntent}
            onDataChanged={() => setDetailRevision((revision) => revision + 1)}
            onIntentHandled={() => setMyAnimeIntent(null)}
            onPlayMedia={playMedia}
          />
        )}
        {detailView && (
          <AnimeDetailPage
            animeId={detailView.animeId}
            onBack={() => window.history.back()}
            onOpenLibraryAction={openLibraryAction}
            onOpenReleaseSearch={openReleaseSearch}
            onPlayMedia={(filePath) => playMedia({ filePath })}
            previewAnime={detailView.previewAnime}
            refreshKey={detailRevision}
            sourceLabel={detailView.origin.label}
          />
        )}
      </AppShell>
      <AlertDialog
        onOpenChange={(open) => !requestingNotificationPermission && setNotificationPromptOpen(open)}
        open={notificationPromptOpen}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>开启下载通知</AlertDialogTitle>
            <AlertDialogDescription>
              开启系统通知后，可在后台查看全局下载速度、上传速度和下载进度。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={requestingNotificationPermission}>暂不开启</AlertDialogCancel>
            <AlertDialogAction
              disabled={requestingNotificationPermission}
              onClick={(event) => {
                event.preventDefault();
                void requestDownloadNotificationPermission();
              }}
            >
              {requestingNotificationPermission ? "请求中" : "开启通知"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}
