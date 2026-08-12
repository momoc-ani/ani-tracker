import {
  AlertCircle,
  ArrowLeft,
  CalendarDays,
  CheckCircle2,
  Clock3,
  Download,
  ExternalLink,
  ImageOff,
  Info,
  ListTodo,
  MoreHorizontal,
  Play,
  Plus,
  RefreshCw,
  Search,
  SlidersHorizontal,
  Star,
  Trash2,
  Users
} from "lucide-react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { toast } from "@/lib/toast";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
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
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger
} from "@/components/ui/dropdown-menu";
import { Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty";
import { Progress } from "@/components/ui/progress";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { CachedImage } from "@/components/cached-image";
import { Page, PageHeader } from "@/components/page-layout";
import { appApi, isLocalClient } from "@/lib/api";
import { cn } from "@/lib/cn";
import { formatBytes } from "@/lib/format";
import type { AnimeDetailResult } from "@shared/contracts";
import type { Anime, MediaContentKind, MediaFile, MyAnime } from "@shared/domain";
import { isSpecialMediaContent } from "@shared/media-content";
import { createDefaultMyAnimePreferences } from "@shared/my-anime-policy";
import {
  formatSubtitleLanguages,
  formatVideoBitDepth,
  resolveSubtitleLanguages
} from "@shared/release-metadata";
import { buildAnimeDetailViewModel } from "./anime-detail-view-model";

export type AnimeDetailLibraryAction = "rules" | "resources" | "tasks";

interface AnimeDetailPageProps {
  allowLibraryManagement?: boolean;
  animeId: string;
  previewAnime?: Anime;
  refreshKey?: number;
  sourceLabel: string;
  onBack: () => void;
  onOpenLibraryAction: (animeId: string, action: AnimeDetailLibraryAction) => void;
  onOpenReleaseSearch: (anime: Anime) => void;
  onPlayMedia?: (filePath: string) => Promise<void>;
}

interface DetailSectionPosition {
  id: string;
  top: number;
}

/** 根据双栏详情区的视觉位置确定当前分区，并在同高分区中保留当前选择。 */
function resolveActiveSectionId(
  positions: DetailSectionPosition[],
  activationLine: number,
  currentSectionId: string,
  atScrollEnd: boolean
): string | undefined {
  if (positions.length === 0) return undefined;

  const reachedPositions = positions.filter((position) => position.top <= activationLine + 1);
  const targetTop = atScrollEnd
    ? Math.max(...positions.map((position) => position.top))
    : reachedPositions.length > 0
      ? Math.max(...reachedPositions.map((position) => position.top))
      : Math.min(...positions.map((position) => position.top));
  const nearestPositions = positions.filter((position) => Math.abs(position.top - targetTop) <= 1);
  return nearestPositions.find((position) => position.id === currentSectionId)?.id ?? nearestPositions[0]?.id;
}

/** 渲染未追番与已追番共用的番剧详情长页。 */
export function AnimeDetailPage({
  allowLibraryManagement,
  animeId,
  previewAnime,
  refreshKey = 0,
  sourceLabel,
  onBack,
  onOpenLibraryAction,
  onOpenReleaseSearch,
  onPlayMedia
}: AnimeDetailPageProps) {
  const [result, setResult] = useState<AnimeDetailResult | null>(null);
  const [mediaFiles, setMediaFiles] = useState<MediaFile[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [tracking, setTracking] = useState(false);
  const [previewingOnline, setPreviewingOnline] = useState(false);
  const [removeDialogOpen, setRemoveDialogOpen] = useState(false);
  const [removing, setRemoving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [summaryExpanded, setSummaryExpanded] = useState(false);
  const [summaryOverflow, setSummaryOverflow] = useState(false);
  const [online, setOnline] = useState(() => navigator.onLine);
  const [activeSectionId, setActiveSectionId] = useState("detail-overview");
  const summaryRef = useRef<HTMLParagraphElement>(null);
  const programmaticSectionIdRef = useRef<string | null>(null);
  const localClient = isLocalClient();
  const libraryManagement = allowLibraryManagement ?? localClient;

  useEffect(() => {
    void loadDetail();
  }, [animeId, previewAnime, refreshKey]);

  useEffect(() => {
    const updateOnline = () => setOnline(navigator.onLine);
    window.addEventListener("online", updateOnline);
    window.addEventListener("offline", updateOnline);
    return () => {
      window.removeEventListener("online", updateOnline);
      window.removeEventListener("offline", updateOnline);
    };
  }, []);

  useLayoutEffect(() => {
    const element = summaryRef.current;
    if (!element || summaryExpanded) {
      setSummaryOverflow(false);
      return;
    }
    const updateOverflow = () => setSummaryOverflow(element.scrollHeight > element.clientHeight + 1);
    updateOverflow();
    const observer = new ResizeObserver(updateOverflow);
    observer.observe(element);
    return () => observer.disconnect();
  }, [result?.anime.summary, summaryExpanded]);

  const viewModel = useMemo(() => result ? buildAnimeDetailViewModel(result) : null, [result]);
  const detail = result?.anime.detail;
  const hasProduction = Boolean(detail?.genres?.length || detail?.studios?.length || detail?.staff?.length);
  const hasBasicInfo = Boolean(
    viewModel?.format || viewModel?.airingStatus || viewModel?.endDate || detail?.episodeCount
      || detail?.durationMinutes || detail?.sourceMaterial || detail?.contentRating || detail?.demographic
  );
  const hasOverview = Boolean(result?.anime.summary || hasBasicInfo);
  const specialMedia = useMemo(
    () => mediaFiles.filter((media) => isSpecialMediaContent(media.contentKind)),
    [mediaFiles]
  );
  const sectionLinks = useMemo(() => [
    hasOverview ? { id: "detail-overview", label: "简介" } : null,
    (viewModel?.nextAiring || viewModel?.broadcast) ? { id: "detail-broadcast", label: "放送" } : null,
    hasProduction ? { id: "detail-production", label: "制作" } : null,
    specialMedia.length ? { id: "detail-specials", label: "特别内容" } : null,
    result?.myAnime ? { id: "detail-tracker", label: "追番" } : null,
    viewModel?.externalLinks.length ? { id: "detail-sources", label: "来源" } : null
  ].filter((item): item is { id: string; label: string } => Boolean(item)), [
    hasOverview,
    hasProduction,
    result?.myAnime,
    specialMedia.length,
    viewModel?.broadcast,
    viewModel?.externalLinks.length,
    viewModel?.nextAiring
  ]);
  const sectionLinkIds = sectionLinks.map((item) => item.id).join("|");
  const defaultFansubName = result?.myAnime?.defaultFansubGroupId
    ? result.fansubGroups.find((group) => group.id === result.myAnime?.defaultFansubGroupId)?.name
    : undefined;

  useEffect(() => {
    if (sectionLinks.length === 0) return;
    if (!sectionLinks.some((item) => item.id === activeSectionId)) {
      setActiveSectionId(sectionLinks[0].id);
    }
  }, [activeSectionId, sectionLinkIds]);

  useEffect(() => {
    if (!sectionLinkIds) return;
    const sections = sectionLinks
      .map((item) => document.getElementById(item.id))
      .filter((element): element is HTMLElement => Boolean(element));
    const scrollRoot = sections[0]?.closest("main");
    if (!(scrollRoot instanceof HTMLElement) || sections.length === 0) return;
    const detailScrollRoot = scrollRoot;

    let animationFrame: number | undefined;
    /** 根据详情滚动位置同步当前分区，不写入浏览器历史。 */
    function updateActiveSection() {
      if (animationFrame !== undefined) return;
      animationFrame = window.requestAnimationFrame(() => {
        animationFrame = undefined;
        const programmaticSectionId = programmaticSectionIdRef.current;
        if (programmaticSectionId) {
          setActiveSectionId((current) => current === programmaticSectionId ? current : programmaticSectionId);
          return;
        }

        const activationLine = detailScrollRoot.getBoundingClientRect().top + (window.innerWidth < 768 ? 128 : 80);
        const positions = sections.map((section) => ({ id: section.id, top: section.getBoundingClientRect().top }));
        const maxScrollTop = detailScrollRoot.scrollHeight - detailScrollRoot.clientHeight;
        const atScrollEnd = maxScrollTop > 1 && maxScrollTop - detailScrollRoot.scrollTop <= 1;
        setActiveSectionId((current) => {
          const nextSectionId = resolveActiveSectionId(positions, activationLine, current, atScrollEnd);
          return nextSectionId && current !== nextSectionId ? nextSectionId : current;
        });
      });
    }

    /** 用户主动滚动时解除标签点击产生的高亮锁定。 */
    function releaseProgrammaticSection() {
      programmaticSectionIdRef.current = null;
    }

    /** 键盘滚动页面时解除锁定，标签自身的键盘选择继续保持高亮。 */
    function releaseProgrammaticSectionOnKeyDown(event: KeyboardEvent) {
      const scrollKeys = ["ArrowDown", "ArrowUp", "End", "Home", "PageDown", "PageUp", " "];
      const target = event.target;
      if (!scrollKeys.includes(event.key) || (target instanceof Element && target.closest('[aria-label="番剧详情分区"]'))) {
        return;
      }
      releaseProgrammaticSection();
    }

    updateActiveSection();
    detailScrollRoot.addEventListener("scroll", updateActiveSection, { passive: true });
    detailScrollRoot.addEventListener("pointerdown", releaseProgrammaticSection, { passive: true });
    detailScrollRoot.addEventListener("touchstart", releaseProgrammaticSection, { passive: true });
    detailScrollRoot.addEventListener("wheel", releaseProgrammaticSection, { passive: true });
    window.addEventListener("keydown", releaseProgrammaticSectionOnKeyDown);
    window.addEventListener("resize", updateActiveSection);
    return () => {
      detailScrollRoot.removeEventListener("scroll", updateActiveSection);
      detailScrollRoot.removeEventListener("pointerdown", releaseProgrammaticSection);
      detailScrollRoot.removeEventListener("touchstart", releaseProgrammaticSection);
      detailScrollRoot.removeEventListener("wheel", releaseProgrammaticSection);
      window.removeEventListener("keydown", releaseProgrammaticSectionOnKeyDown);
      window.removeEventListener("resize", updateActiveSection);
      if (animationFrame !== undefined) window.cancelAnimationFrame(animationFrame);
    };
  }, [sectionLinkIds]);

  /** 从本地聚合接口加载详情首屏。 */
  async function loadDetail() {
    setLoading(true);
    setResult(null);
    setMediaFiles([]);
    setPreviewingOnline(false);
    setError(null);
    console.info("[anime-detail] load requested", { animeId });
    try {
      const [detailResult, registeredMedia] = await Promise.all([
        appApi.getAnimeDetail(animeId),
        localClient
          ? appApi.listMediaFiles().catch((mediaError) => {
              console.warn("[anime-detail] 本地媒体读取失败", { animeId, error: mediaError });
              return [];
            })
          : Promise.resolve([])
      ]);
      setResult(detailResult);
      setMediaFiles(registeredMedia.filter((media) => media.animeId === animeId));
      setPreviewingOnline(false);
      setSummaryExpanded(false);
    } catch (loadError) {
      if (previewAnime?.id === animeId && isMissingAnimeDetailError(loadError)) {
        setResult(createOnlinePreviewResult(previewAnime));
        setPreviewingOnline(true);
        setSummaryExpanded(false);
        console.info("[anime-detail] Bangumi 在线预览已启用", { animeId });
        return;
      }
      const message = loadError instanceof Error ? loadError.message : "加载番剧详情失败";
      console.error("[anime-detail] load failed", { animeId, error: loadError });
      setError(message);
    } finally {
      setLoading(false);
    }
  }

  /** 主动补全外部详情，并保留当前页面内容。 */
  async function refreshDetail() {
    if (!online || !localClient || previewingOnline) return;
    setRefreshing(true);
    try {
      const refreshed = await appApi.refreshAnimeDetail(animeId);
      setResult(refreshed);
      toast.success(refreshed.partialErrors.length ? "详情已部分更新" : "番剧详情已更新");
    } catch (refreshError) {
      const message = refreshError instanceof Error ? refreshError.message : "刷新番剧详情失败";
      toast.error(message);
      console.error("[anime-detail] refresh failed", { animeId, error: refreshError });
    } finally {
      setRefreshing(false);
    }
  }

  /** 使用当前默认规则将目录番剧加入追番。 */
  async function addTracker() {
    if (!result || result.myAnime) return;
    setTracking(true);
    try {
      const now = new Date().toISOString();
      const item = createDefaultMyAnime(result.anime, now);
      if (previewingOnline) {
        await appApi.followBangumiAnime(item);
      } else {
        await appApi.upsertMyAnime(item);
      }
      setResult(await appApi.getAnimeDetail(animeId));
      setPreviewingOnline(false);
      toast.success(`已添加「${viewModel?.title ?? result.anime.title}」到我的追番`);
      console.info("[anime-detail] tracker added", {
        animeId,
        source: previewingOnline ? "bangumi" : "catalog"
      });
    } catch (trackingError) {
      toast.error(trackingError instanceof Error ? trackingError.message : "添加追番失败");
    } finally {
      setTracking(false);
    }
  }

  /** 移除当前追番记录并原地切回未追番详情。 */
  async function removeTracker() {
    if (!result?.myAnime) return;
    setRemoving(true);
    try {
      await appApi.removeMyAnime(result.myAnime.id);
      setResult(await appApi.getAnimeDetail(animeId));
      setRemoveDialogOpen(false);
      toast.success("已移除追番，下载文件保持不变");
      console.info("[anime-detail] tracker removed", { animeId });
    } catch (removeError) {
      toast.error(removeError instanceof Error ? removeError.message : "移除追番失败");
    } finally {
      setRemoving(false);
    }
  }

  /** 在本地应用调用系统浏览器，远程端使用标准新窗口。 */
  async function openExternal(url: string) {
    if (localClient) {
      await appApi.openExternal(url);
      return;
    }
    window.open(url, "_blank", "noopener,noreferrer");
  }

  /** 将详情内容滚动到指定分区，并避免锚点污染返回历史。 */
  function scrollToSection(sectionId: string) {
    const section = document.getElementById(sectionId);
    if (!section) return;
    programmaticSectionIdRef.current = sectionId;
    setActiveSectionId(sectionId);
    section.scrollIntoView({ behavior: "smooth", block: "start" });
    console.info("[anime-detail] section selected", { animeId, sectionId });
  }

  if (loading) {
    return <AnimeDetailSkeleton />;
  }

  if (!result || !viewModel) {
    if (error?.includes("不存在")) {
      return (
        <Empty className="min-h-[60vh]">
          <EmptyHeader>
            <EmptyMedia variant="icon"><Info /></EmptyMedia>
            <EmptyTitle>番剧不存在</EmptyTitle>
            <EmptyDescription>本地目录中没有找到这部番剧，可能已被清理。</EmptyDescription>
          </EmptyHeader>
          <EmptyContent><Button onClick={onBack}>返回{sourceLabel}</Button></EmptyContent>
        </Empty>
      );
    }
    return (
      <Page>
        <Alert variant="destructive">
          <AlertCircle />
          <AlertTitle>番剧详情加载失败</AlertTitle>
          <AlertDescription>{error ?? "请稍后重试"}</AlertDescription>
        </Alert>
        <div className="flex flex-wrap gap-2">
          <Button onClick={() => void loadDetail()}><RefreshCw data-icon="inline-start" />重试</Button>
          <Button onClick={onBack} variant="outline"><ArrowLeft data-icon="inline-start" />返回{sourceLabel}</Button>
        </div>
      </Page>
    );
  }

  return (
    <Page className="gap-5 pb-8">
      <PageHeader
        className="hidden gap-3 border-b pb-3 sm:items-center md:flex"
        data-window-controls-clearance=""
      >
        <div className="flex min-w-0 items-center gap-2">
          <Button aria-label={`返回${sourceLabel}`} className="size-9 px-0" onClick={onBack} variant="ghost">
            <ArrowLeft />
          </Button>
          <div className="min-w-0 text-sm text-muted-foreground">
            <span>{sourceLabel}</span>
            <span aria-hidden="true" className="px-2">/</span>
            <span className="text-foreground">番剧详情</span>
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {localClient && !previewingOnline && (
            <Button
              disabled={refreshing || !online}
              onClick={() => void refreshDetail()}
              title={online ? "刷新元数据" : "离线时不可刷新"}
              variant="outline"
            >
              <RefreshCw className={cn(refreshing && "animate-spin")} data-icon="inline-start" />
              {refreshing ? "刷新中" : "刷新"}
            </Button>
          )}
          <DetailMoreMenu
            externalLinks={viewModel.externalLinks}
            followed={viewModel.followed && libraryManagement}
            onOpenExternal={openExternal}
            onRemove={() => setRemoveDialogOpen(true)}
          />
        </div>
      </PageHeader>

      {result.partialErrors.length > 0 && (
        <Alert>
          <AlertCircle />
          <AlertTitle>部分来源未能更新</AlertTitle>
          <AlertDescription>
            {result.partialErrors[0].source}：{result.partialErrors[0].message}
            {result.partialErrors.length > 1 ? `，另有 ${result.partialErrors.length - 1} 个来源异常。` : ""}
          </AlertDescription>
        </Alert>
      )}

      {!online && (
        <Alert>
          <Info />
          <AlertTitle>当前处于离线状态</AlertTitle>
          <AlertDescription>
            {previewingOnline
              ? "已显示进入详情前加载的 Bangumi 数据，恢复网络后可继续追番。"
              : "已显示本地缓存，恢复网络后可主动刷新详情。"}
          </AlertDescription>
        </Alert>
      )}

      <section className="min-w-0 border-b pb-6">
        {detail?.bannerUrl && (
          <div className="mb-4 h-36 overflow-hidden rounded-md border bg-muted sm:h-44 md:h-52">
            <CachedImage alt="" className="size-full object-cover" sourceUrl={detail.bannerUrl} />
          </div>
        )}

        <div className="grid min-w-0 grid-cols-[104px_minmax(0,1fr)] items-start gap-4 sm:grid-cols-[132px_minmax(0,1fr)] md:grid-cols-[200px_minmax(0,1fr)] md:gap-6 xl:grid-cols-[220px_minmax(0,1fr)_304px]">
          <div className="aspect-[2/3] w-full overflow-hidden rounded-md border bg-muted">
            {result.anime.coverUrl ? (
              <CachedImage alt={viewModel.title} className="size-full object-cover" sourceUrl={result.anime.coverUrl} />
            ) : (
              <div className="flex size-full items-center justify-center text-muted-foreground"><ImageOff /></div>
            )}
          </div>

          <div className="min-w-0 pt-1 md:pt-2">
            <div className="flex min-w-0 flex-wrap gap-2">
              {previewingOnline && <Badge tone="blue">Bangumi 在线</Badge>}
              {viewModel.followed && <Badge tone="green"><CheckCircle2 className="mr-1 size-3" />已追番</Badge>}
              {viewModel.airingStatus && <Badge tone="primary">{viewModel.airingStatus}</Badge>}
              {result.stale && <Badge tone="amber">缓存较旧</Badge>}
            </div>
            <h1 className="mt-3 break-words text-2xl font-bold leading-8 tracking-normal md:text-3xl md:leading-10">
              {viewModel.title}
            </h1>
            {viewModel.subtitle && (
              <p className="mt-1 break-words text-sm leading-5 text-muted-foreground">{viewModel.subtitle}</p>
            )}
            <div className="mt-3 flex min-w-0 flex-wrap gap-x-4 gap-y-2 text-sm text-muted-foreground">
              <span className="inline-flex items-center gap-1.5"><CalendarDays className="size-4" />{viewModel.premiere}</span>
              {viewModel.format && <span>{viewModel.format}</span>}
              {result.anime.rating && (
                <span className="inline-flex items-center gap-1.5 text-foreground">
                  <Star className="size-4 fill-current text-warning" />
                  <strong>{result.anime.rating.score.toFixed(1)}</strong>
                  <span className="text-muted-foreground">{result.anime.rating.source}</span>
                </span>
              )}
              {detail?.ranking && <span>#{detail.ranking.rank} · {detail.ranking.source}</span>}
            </div>
            {result.anime.summary && (
              <p className="mt-5 hidden max-w-3xl whitespace-pre-line text-sm leading-6 text-muted-foreground md:line-clamp-5 md:block">
                {result.anime.summary}
              </p>
            )}
          </div>

          <Card className="col-span-2 min-w-0 bg-primary/5 shadow-none xl:col-span-1 xl:self-start">
            <CardContent className="flex flex-col gap-2 p-4 sm:p-4">
              {previewingOnline ? (
                <Button disabled={tracking || !online} onClick={() => void addTracker()}>
                  <Plus data-icon="inline-start" />{tracking ? "添加中" : "添加追番"}
                </Button>
              ) : !libraryManagement ? (
                result.myAnime ? (
                  <Button onClick={() => onOpenLibraryAction(animeId, "tasks")} variant="outline">
                    <ListTodo data-icon="inline-start" />查看追番
                  </Button>
                ) : null
              ) : result.myAnime ? (
                <>
                  <Button onClick={() => onOpenLibraryAction(animeId, "rules")}>
                    <SlidersHorizontal data-icon="inline-start" />编辑规则
                  </Button>
                  <Button onClick={() => onOpenLibraryAction(animeId, "resources")} variant="outline">
                    <Download data-icon="inline-start" />查看资源
                  </Button>
                </>
              ) : (
                <>
                  <Button disabled={tracking} onClick={() => void addTracker()}>
                    <Plus data-icon="inline-start" />{tracking ? "添加中" : "添加追番"}
                  </Button>
                  <Button onClick={() => onOpenReleaseSearch(result.anime)} variant="outline">
                    <Search data-icon="inline-start" />搜索资源
                  </Button>
                </>
              )}

              {viewModel.externalLinks.length > 0 && (
                <>
                  <Separator className="my-2" />
                  <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">外部来源</div>
                  {viewModel.externalLinks.map((link) => (
                    <Button
                      className="min-w-0 justify-between px-2"
                      key={link.key}
                      onClick={() => void openExternal(link.url)}
                      variant="ghost"
                    >
                      <span className="truncate">{link.label}</span>
                      <ExternalLink data-icon="inline-end" />
                    </Button>
                  ))}
                </>
              )}
            </CardContent>
          </Card>
        </div>
      </section>

      {sectionLinks.length > 1 && (
        <nav className="sticky top-0 z-20 hidden w-fit max-w-full overflow-x-auto bg-background/95 backdrop-blur md:block">
          <Tabs onValueChange={scrollToSection} value={activeSectionId}>
            <TabsList aria-label="番剧详情分区" className="min-w-max gap-8" variant="line">
              {sectionLinks.map((item) => (
                <TabsTrigger
                  aria-current={activeSectionId === item.id ? "location" : undefined}
                  key={item.id}
                  value={item.id}
                >
                  {item.label}
                </TabsTrigger>
              ))}
            </TabsList>
          </Tabs>
        </nav>
      )}

      <div className="grid min-w-0 gap-8 lg:grid-cols-[minmax(0,1fr)_320px]">
        <div className="flex min-w-0 flex-col gap-10">
          {hasOverview && (
            <DetailSection icon={<Info />} id="detail-overview" title="简介">
              {result.anime.summary && (
                <>
                  <p
                    className={cn(
                      "whitespace-pre-line break-words text-sm leading-7 text-muted-foreground",
                      !summaryExpanded && "line-clamp-6 md:line-clamp-none"
                    )}
                    ref={summaryRef}
                  >
                    {result.anime.summary}
                  </p>
                  {summaryOverflow && (
                    <Button className="mt-2 h-auto min-h-0 p-0 text-sm md:hidden" onClick={() => setSummaryExpanded(true)} variant="ghost">
                      展开简介
                    </Button>
                  )}
                  {summaryExpanded && (
                    <Button className="mt-2 h-auto min-h-0 p-0 text-sm md:hidden" onClick={() => setSummaryExpanded(false)} variant="ghost">
                      收起简介
                    </Button>
                  )}
                </>
              )}

              {hasBasicInfo && (
                <Card className={cn("shadow-none", result.anime.summary && "mt-5")}>
                  <CardContent className="grid min-w-0 grid-cols-2 gap-x-5 gap-y-5 p-4 sm:grid-cols-3 sm:p-5">
                    {viewModel.format && <OverviewFact label="形式" value={viewModel.format} />}
                    {viewModel.airingStatus && <OverviewFact label="放送状态" value={viewModel.airingStatus} />}
                    {detail?.episodeCount && <OverviewFact label="总集数" value={`${detail.episodeCount} 集`} />}
                    {detail?.durationMinutes && <OverviewFact label="单集时长" value={`${detail.durationMinutes} 分钟`} />}
                    <OverviewFact label="首播" value={viewModel.premiere} />
                    {viewModel.endDate && <OverviewFact label="完结" value={viewModel.endDate} />}
                    {detail?.sourceMaterial && <OverviewFact label="原作类型" value={formatMetadataValue(detail.sourceMaterial)} />}
                    {detail?.contentRating && <OverviewFact label="内容分级" value={detail.contentRating} />}
                    {detail?.demographic && <OverviewFact label="受众" value={detail.demographic} />}
                  </CardContent>
                </Card>
              )}
            </DetailSection>
          )}

          {hasProduction && (
            <DetailSection icon={<Users />} id="detail-production" title="制作信息">
              {detail?.genres?.length && (
                <div className="mb-4 flex flex-wrap gap-2">
                  {detail.genres.map((genre) => <Badge key={genre}>{genre}</Badge>)}
                </div>
              )}
              {detail?.studios?.length && (
                <div className="mb-4 flex min-w-0 flex-wrap items-center gap-2">
                  <span className="text-xs font-semibold text-muted-foreground">制作公司</span>
                  {detail.studios.map((studio) => <Badge tone="blue" key={studio}>{studio}</Badge>)}
                </div>
              )}
              {detail?.staff?.length && (
                <Card className="overflow-hidden shadow-none">
                  <CardContent className="divide-y p-0 sm:p-0">
                    {detail.staff.map((credit) => (
                      <div className="grid min-w-0 gap-1 bg-primary/5 px-4 py-3 sm:grid-cols-[minmax(10rem,0.9fr)_minmax(0,1.1fr)] sm:gap-6" key={`${credit.name}-${credit.role}`}>
                        <div className="break-words text-xs font-semibold text-muted-foreground">{credit.role}</div>
                        <div className="break-words text-sm font-medium">{credit.name}</div>
                      </div>
                    ))}
                  </CardContent>
                </Card>
              )}
            </DetailSection>
          )}

          {specialMedia.length > 0 && (
            <DetailSection icon={<ListTodo />} id="detail-specials" title="特别内容">
              <div className="flex min-w-0 flex-col">
                {specialMedia.map((media, index) => (
                  <div className="min-w-0" key={media.id}>
                    <div className="flex min-w-0 items-center gap-3 py-3">
                      <Badge>{media.specialNo ?? mediaContentKindLabel(media.contentKind)}</Badge>
                      <div className="min-w-0 flex-1">
                        <div className="truncate text-sm font-medium" title={media.fileName}>{media.fileName}</div>
                        <div className="mt-1 truncate text-xs text-muted-foreground">
                          {formatBytes(media.size)} · {mediaAvailabilityLabel(media)}
                        </div>
                      </div>
                      {onPlayMedia && (
                        <Button
                          aria-label={`播放 ${media.fileName}`}
                          className="size-9 shrink-0 px-0"
                          disabled={media.availability === "missing" || media.availability === "unavailable"}
                          onClick={() => void playRegisteredMedia(media, onPlayMedia)}
                          title="播放"
                          variant="outline"
                        >
                          <Play />
                        </Button>
                      )}
                    </div>
                    {index < specialMedia.length - 1 && <Separator />}
                  </div>
                ))}
              </div>
            </DetailSection>
          )}
        </div>

        <aside className="flex min-w-0 flex-col gap-7">
          {result.myAnime && (
            <TrackerCard
              defaultFansubName={defaultFansubName}
              item={result.myAnime}
              result={result}
              viewModel={viewModel}
              onOpenAction={(action) => onOpenLibraryAction(animeId, action)}
              onRemove={() => setRemoveDialogOpen(true)}
              readOnly={!libraryManagement}
            />
          )}

          {(viewModel.nextAiring || viewModel.broadcast) && (
            <DetailSection icon={<CalendarDays />} id="detail-broadcast" title="放送信息">
              <Card className="bg-primary/5 shadow-none">
                <CardContent className="p-4 sm:p-4">
                  {viewModel.nextAiring && <DetailFact icon={<Clock3 />} label="下一次放送" value={viewModel.nextAiring} />}
                  {viewModel.broadcast && <DetailFact icon={<CalendarDays />} label="固定时段" value={viewModel.broadcast} />}
                </CardContent>
              </Card>
            </DetailSection>
          )}

          {result.anime.aliases.length > 0 && (
            <DetailSection title="别名">
              <div className="flex min-w-0 flex-col gap-3">
                {result.anime.aliases.map((alias) => (
                  <div className="min-w-0" key={alias.id}>
                    <div className="text-xs text-muted-foreground">{formatAliasLanguage(alias.language)}</div>
                    <div className="mt-0.5 break-words text-sm">{alias.alias}</div>
                  </div>
                ))}
              </div>
            </DetailSection>
          )}

          {viewModel.externalLinks.length > 0 && (
            <DetailSection icon={<ExternalLink />} id="detail-sources" title="外部来源">
              <Card className="shadow-none">
                <CardContent className="flex min-w-0 flex-col gap-1 p-3 sm:p-3">
                  {viewModel.externalLinks.map((link) => (
                    <Button className="min-w-0 justify-between px-2" key={link.key} onClick={() => void openExternal(link.url)} variant="ghost">
                      <span className="truncate">{link.label}</span><ExternalLink data-icon="inline-end" />
                    </Button>
                  ))}
                  {viewModel.metadataSources.length > 0 && (
                    <p className="border-t px-2 pt-3 text-xs leading-5 text-muted-foreground">
                      元数据来源：{viewModel.metadataSources.join("、")}
                    </p>
                  )}
                </CardContent>
              </Card>
            </DetailSection>
          )}
        </aside>
      </div>

      <AlertDialog onOpenChange={setRemoveDialogOpen} open={removeDialogOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>确认移除追番？</AlertDialogTitle>
            <AlertDialogDescription>
              「{viewModel.title}」及其追番规则将被移除，已下载文件不会被删除。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={removing}>取消</AlertDialogCancel>
            <AlertDialogAction disabled={removing} onClick={() => void removeTracker()} variant="destructive">
              {removing ? "移除中" : "移除追番"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </Page>
  );
}

/** 渲染详情页中的无卡片分区。 */
function DetailSection({
  id,
  title,
  icon,
  children
}: {
  id?: string;
  title: string;
  icon?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <section className="min-w-0 scroll-mt-32 border-t pt-5 md:scroll-mt-16" id={id}>
      <h2 className="mb-4 flex items-center gap-2 text-base font-semibold">
        {icon && <span className="text-primary [&_svg]:size-5">{icon}</span>}
        {title}
      </h2>
      {children}
    </section>
  );
}

/** 渲染简介信息面板中的紧凑字段。 */
function OverviewFact({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0">
      <div className="text-xs font-medium text-muted-foreground">{label}</div>
      <div className="mt-1 break-words text-sm font-medium">{value}</div>
    </div>
  );
}

/** 渲染标签和值组成的详情事实行。 */
function DetailFact({ label, value, icon }: { label: string; value: string; icon?: React.ReactNode }) {
  return (
    <div className="flex min-w-0 gap-3 border-b py-3 first:pt-0">
      {icon && <div className="mt-0.5 shrink-0 text-muted-foreground [&_svg]:size-4">{icon}</div>}
      <div className="min-w-0 flex-1">
        <div className="text-xs text-muted-foreground">{label}</div>
        <div className="mt-1 break-words text-sm font-medium">{value}</div>
      </div>
    </div>
  );
}

/** 渲染已追番详情中的进度、偏好与快捷操作。 */
function TrackerCard({
  defaultFansubName,
  item,
  result,
  viewModel,
  onOpenAction,
  onRemove,
  readOnly
}: {
  defaultFansubName?: string;
  item: MyAnime;
  result: AnimeDetailResult;
  viewModel: ReturnType<typeof buildAnimeDetailViewModel>;
  onOpenAction: (action: AnimeDetailLibraryAction) => void;
  onRemove: () => void;
  readOnly: boolean;
}) {
  const subtitleLanguages = resolveSubtitleLanguages(item.preferredSubtitleLanguages, item.preferredSubtitle);
  const progressLabel = viewModel.totalEpisodes
    ? `${viewModel.watchedCount} / ${viewModel.totalEpisodes} 集已看`
    : `${viewModel.watchedCount} 集已看`;

  return (
    <Card className="scroll-mt-32 shadow-none md:scroll-mt-16" id="detail-tracker">
      <CardHeader>
        <CardTitle>追番概览</CardTitle>
        <CardDescription>{viewModel.trackerStatus} · {progressLabel}</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        <div>
          <div className="flex items-center justify-between gap-3 text-xs">
            <span className="text-muted-foreground">观看进度</span>
            <span className="font-medium tabular-nums">{Math.round((viewModel.progress ?? 0) * 100)}%</span>
          </div>
          <Progress className="mt-2 h-2" value={viewModel.progress ?? 0} />
          <div className="mt-2 text-xs text-muted-foreground">已下载或看完 {viewModel.downloadedCount} 集</div>
        </div>
        <div className="grid grid-cols-2 gap-x-4 gap-y-3 text-sm">
          <TrackerFact label="字幕组" value={defaultFansubName} />
          <TrackerFact label="自动下载" value={item.autoDownload ? "开启" : "关闭"} />
          <TrackerFact label="清晰度" value={item.preferredResolution} />
          <TrackerFact label="编码" value={item.preferredCodec} />
          <TrackerFact label="位深" value={item.preferredBitDepth ? formatVideoBitDepth(item.preferredBitDepth) : undefined} />
          <TrackerFact label="字幕" value={subtitleLanguages.length ? formatSubtitleLanguages(subtitleLanguages) : undefined} />
        </div>
        {result.episodes.length === 0 && <p className="text-xs text-muted-foreground">尚未建立单集记录。</p>}
      </CardContent>
      {!readOnly && <CardFooter className="grid grid-cols-3 gap-2">
        <Button aria-label="编辑规则" className="px-0" onClick={() => onOpenAction("rules")} title="编辑规则" variant="outline">
          <SlidersHorizontal />
        </Button>
        <Button aria-label="查看资源" className="px-0" onClick={() => onOpenAction("resources")} title="查看资源" variant="outline">
          <Download />
        </Button>
        <Button aria-label="下载任务" className="px-0" onClick={() => onOpenAction("tasks")} title="下载任务" variant="outline">
          <ListTodo />
        </Button>
        <Button className="col-span-3" onClick={onRemove} variant="ghost">
          <Trash2 data-icon="inline-start" />移除追番
        </Button>
      </CardFooter>}
    </Card>
  );
}

function TrackerFact({ label, value }: { label: string; value?: string }) {
  if (!value) return null;
  return <div className="min-w-0"><div className="text-xs text-muted-foreground">{label}</div><div className="mt-1 break-words font-medium">{value}</div></div>;
}

function DetailMoreMenu({
  externalLinks,
  followed,
  onOpenExternal,
  onRemove
}: {
  externalLinks: ReturnType<typeof buildAnimeDetailViewModel>["externalLinks"];
  followed: boolean;
  onOpenExternal: (url: string) => Promise<void>;
  onRemove: () => void;
}) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button aria-label="更多详情操作" className="size-9 px-0" title="更多操作" variant="outline"><MoreHorizontal /></Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-48">
        {externalLinks.length > 0 && (
          <DropdownMenuGroup>
            {externalLinks.map((link) => (
              <DropdownMenuItem key={link.key} onSelect={() => void onOpenExternal(link.url)}>
                <ExternalLink />打开 {link.label}
              </DropdownMenuItem>
            ))}
          </DropdownMenuGroup>
        )}
        {externalLinks.length > 0 && followed && <DropdownMenuSeparator />}
        {followed && (
          <DropdownMenuItem className="text-destructive focus:text-destructive" onSelect={onRemove}>
            <Trash2 />移除追番
          </DropdownMenuItem>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** 创建发现页和详情页共用的默认追番配置。 */
function createDefaultMyAnime(anime: Anime, timestamp: string): MyAnime {
  return {
    id: `my-${anime.id}`,
    anime,
    status: "watching",
    ...createDefaultMyAnimePreferences(),
    addedAt: timestamp,
    updatedAt: timestamp
  };
}

/** 使用 Bangumi 在线快照构造不依赖本地业务数据的详情结果。 */
function createOnlinePreviewResult(anime: Anime): AnimeDetailResult {
  return {
    anime,
    episodes: [],
    fansubGroups: [],
    stale: false,
    partialErrors: []
  };
}

/** 仅将明确的本地记录缺失错误降级为在线预览。 */
function isMissingAnimeDetailError(error: unknown): boolean {
  const message = error instanceof Error ? error.message : String(error);
  return message.includes("番剧不存在") || message.includes("record_not_found");
}

/** 调用统一播放入口打开已登记的特别内容。 */
async function playRegisteredMedia(
  media: MediaFile,
  onPlayMedia: (filePath: string) => Promise<void>
): Promise<void> {
  try {
    await onPlayMedia(media.filePath);
    console.info("[anime-detail] 特别内容播放已请求", {
      mediaId: media.id,
      contentKind: media.contentKind,
      specialNo: media.specialNo
    });
  } catch (error) {
    console.error("[anime-detail] 特别内容播放失败", { mediaId: media.id, error });
    toast.error(error instanceof Error ? error.message : "特别内容播放失败");
  }
}

/** 返回特别内容类型的中文标签。 */
function mediaContentKindLabel(contentKind: MediaContentKind): string {
  const labels: Record<MediaContentKind, string> = {
    episode: "正片",
    special: "SP",
    ova: "OVA",
    oad: "OAD",
    opening: "片头",
    ending: "片尾",
    pv: "PV",
    cm: "CM",
    extra: "特典",
    unknown: "其他"
  };
  return labels[contentKind];
}

/** 返回媒体可用状态和最近变更提示。 */
function mediaAvailabilityLabel(media: MediaFile): string {
  switch (media.availability) {
    case "changed": return "文件已变化";
    case "missing": return "文件缺失";
    case "unavailable": return "目录不可访问";
    default: return media.container?.toUpperCase() ?? "可播放";
  }
}

function formatMetadataValue(value: string): string {
  return value.replaceAll("_", " ").toLocaleLowerCase();
}

/** 将别名语言转换为详情页可读标签。 */
function formatAliasLanguage(language: Anime["aliases"][number]["language"]): string {
  const labels: Record<Anime["aliases"][number]["language"], string> = {
    zh: "中文",
    ja: "日文",
    en: "英文",
    romaji: "罗马字",
    custom: "其他"
  };
  return labels[language];
}

/** 保持海报、标题、动作区和双列内容尺寸稳定的详情骨架。 */
function AnimeDetailSkeleton() {
  return (
    <Page className="gap-5">
      <PageHeader
        className="hidden border-b pb-3 sm:items-center md:flex"
        data-window-controls-clearance=""
      >
        <Skeleton className="h-9 w-48" />
        <Skeleton className="h-9 w-28" />
      </PageHeader>
      <section className="border-b pb-6">
        <Skeleton className="mb-4 h-36 w-full sm:h-44 md:h-52" />
        <div className="grid grid-cols-[112px_minmax(0,1fr)] gap-4 md:grid-cols-[176px_minmax(0,1fr)] md:gap-6 lg:grid-cols-[176px_minmax(0,1fr)_192px]">
          <Skeleton className="aspect-[2/3] w-full" />
          <div className="flex flex-col justify-center gap-3"><Skeleton className="h-6 w-28" /><Skeleton className="h-9 w-4/5" /><Skeleton className="h-5 w-3/5" /><Skeleton className="h-5 w-2/5" /></div>
          <div className="col-span-2 flex flex-col gap-2 lg:col-span-1 lg:justify-end"><Skeleton className="h-10 w-full" /><Skeleton className="h-10 w-full" /></div>
        </div>
      </section>
      <div className="grid gap-8 lg:grid-cols-[minmax(0,1fr)_320px]">
        <div className="flex flex-col gap-8"><Skeleton className="h-48 w-full" /><Skeleton className="h-64 w-full" /></div>
        <div className="flex flex-col gap-6"><Skeleton className="h-40 w-full" /><Skeleton className="h-64 w-full" /></div>
      </div>
    </Page>
  );
}
