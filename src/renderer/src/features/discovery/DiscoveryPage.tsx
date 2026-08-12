import {
  AlertCircle,
  ArrowLeft,
  CloudDownload,
  CalendarDays,
  CalendarRange,
  CalendarPlus,
  Check,
  CheckCircle2,
  ChevronDown,
  Download,
  ExternalLink,
  ImageOff,
  Info,
  LayoutGrid,
  List,
  LoaderCircle,
  Plus,
  RotateCcw,
  Search,
  SlidersHorizontal,
  Star,
  X
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useAppScrollContainer, useAppScrollToTopHandler } from "@/components/app-shell";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator
} from "@/components/ui/breadcrumb";
import { Button } from "@/components/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty";
import { Field, FieldLabel } from "@/components/ui/field";
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupInput } from "@/components/ui/input-group";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Pagination,
  PaginationContent,
  PaginationEllipsis,
  PaginationItem,
  PaginationLink,
  PaginationNext,
  PaginationPrevious
} from "@/components/ui/pagination";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle
} from "@/components/ui/sheet";
import { Slider } from "@/components/ui/slider";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { CachedImage } from "@/components/cached-image";
import { FilterToolbar, Page, PageActions, PageHeader, PageHeading } from "@/components/page-layout";
import { YearPicker } from "@/components/year-picker";
import { appApi } from "@/lib/api";
import { cn } from "@/lib/cn";
import { toast } from "@/lib/toast";
import { useVirtualizerScrollMargin } from "@/hooks/use-virtualizer-scroll-margin";
import { resolveAnimeTitleDisplay } from "@shared/anime-title";
import {
  countDiscoveryBrowseFilters,
  createEmptyDiscoveryBrowseFilters,
  type DiscoveryBrowseFilters,
  type DiscoveryBrowseSortKey,
  type DiscoveryDemographic,
  type DiscoveryGenre,
  type DiscoveryRegion,
  type DiscoverySourceMaterial
} from "@shared/discovery-filter";
import { createDefaultMyAnimePreferences } from "@shared/my-anime-policy";
import type { Anime, AnimeFormat, MyAnime, Season } from "@shared/domain";
import type { AnimeDiscoverySyncTaskStatus, BangumiBrowseQuery } from "@shared/contracts";

export interface SeasonTarget {
  year: number;
  season: Season;
}

interface SeasonOption {
  value: Season;
  label: string;
  months: readonly [number, number, number];
}

type DiscoverySortKey = "premiereAsc" | "premiereDesc" | "ratingDesc";
const DEFAULT_DISCOVERY_SORT: DiscoverySortKey = "ratingDesc";
type ScheduleView = "grid" | "list";
type DiscoveryWorkspaceTab = "season" | "schedule" | "browse";
interface DiscoveryPageProps {
  allowCollection?: boolean;
  onOpenAnimeDetail?: (animeId: string, previewAnime?: Anime) => void;
  onOpenSchedule?: (target: SeasonTarget) => void;
  workspaceTabs?: boolean;
}

const seasonOptions: readonly SeasonOption[] = [
  { value: "winter", label: "冬季", months: [1, 2, 3] },
  { value: "spring", label: "春季", months: [4, 5, 6] },
  { value: "summer", label: "夏季", months: [7, 8, 9] },
  { value: "fall", label: "秋季", months: [10, 11, 12] }
];

const seasonText: Record<Season, string> = {
  winter: "冬季",
  spring: "春季",
  summer: "夏季",
  fall: "秋季"
};

/** 去除稳定来源前缀，返回可直接展示的 AniList 错误。 */
function normalizeAnilistError(error?: string): string | null {
  return error?.replace(/^anilist:\s*/i, "").trim() || null;
}

/** Renders the seasonal anime catalog and its follow actions. */
export function DiscoveryPage({
  allowCollection = true,
  onOpenAnimeDetail,
  onOpenSchedule,
  workspaceTabs = true
}: DiscoveryPageProps = {}) {
  const [activeWorkspaceTab, setActiveWorkspaceTab] = useState<DiscoveryWorkspaceTab>("season");
  const [target, setTarget] = useState<SeasonTarget>(getCurrentSeasonTarget);
  const [selectedMonth, setSelectedMonth] = useState<number | null>(null);
  const [keyword, setKeyword] = useState("");
  const [appliedKeyword, setAppliedKeyword] = useState("");
  const [sortKey, setSortKey] = useState<DiscoverySortKey>(DEFAULT_DISCOVERY_SORT);
  const [items, setItems] = useState<Anime[]>([]);
  const [searchItems, setSearchItems] = useState<Anime[]>([]);
  const [myAnime, setMyAnime] = useState<MyAnime[]>([]);
  const [loading, setLoading] = useState(true);
  const [searching, setSearching] = useState(false);
  const [syncTaskStatus, setSyncTaskStatus] = useState<AnimeDiscoverySyncTaskStatus | null>(null);
  const [startingSync, setStartingSync] = useState(false);
  const [addingAnimeId, setAddingAnimeId] = useState<string | null>(null);
  const [message, setMessage] = useState<{ tone: "success" | "error"; text: string } | null>(null);
  const [anilistSyncError, setAnilistSyncError] = useState<string | null>(null);
  const loadRequestId = useRef(0);
  const searchRequestId = useRef(0);
  const manualCollectRef = useRef(false);
  const manualTaskStartedAtRef = useRef<string | null>(null);
  const workspaceScrollPositionsRef = useRef<Record<DiscoveryWorkspaceTab, number>>({
    season: 0,
    schedule: 0,
    browse: 0
  });
  const workspaceScrollContainerRef = useAppScrollContainer();

  const activeSeason = getSeasonOption(target.season);
  const followedIds = useMemo(() => new Set(myAnime.map((item) => item.anime.id)), [myAnime]);
  const visibleItems = useMemo(
    () => sortAnimeItems(
      appliedKeyword ? searchItems : filterAnimeItems(items, selectedMonth, ""),
      sortKey
    ),
    [appliedKeyword, items, searchItems, selectedMonth, sortKey]
  );
  const visibleLoading = appliedKeyword ? searching : loading;
  const collecting = startingSync || Boolean(syncTaskStatus?.inFlight);

  /** 切换工作区页签，并恢复该页签上次的滚动位置。 */
  function changeWorkspaceTab(nextTab: DiscoveryWorkspaceTab) {
    const scrollContainer = workspaceScrollContainerRef.current;
    if (scrollContainer) workspaceScrollPositionsRef.current[activeWorkspaceTab] = scrollContainer.scrollTop;
    setActiveWorkspaceTab(nextTab);
    console.info("[discovery] 工作区页签已切换", { from: activeWorkspaceTab, to: nextTab });
  }

  useEffect(() => {
    const frame = window.requestAnimationFrame(() => {
      workspaceScrollContainerRef.current?.scrollTo({
        top: workspaceScrollPositionsRef.current[activeWorkspaceTab],
        behavior: "auto"
      });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [activeWorkspaceTab, workspaceScrollContainerRef]);

  useEffect(() => {
    void loadSeasonCatalog(target.year, target.season);
  }, [target.year, target.season]);

  useEffect(() => {
    if (!allowCollection) return;
    let active = true;

    /** 定期读取本地同步状态，使后台 AniList 失败能及时显示。 */
    async function refreshSyncState() {
      try {
        const syncState = await appApi.getAnimeSeasonSyncState(target.year, target.season);
        if (active) setAnilistSyncError(normalizeAnilistError(syncState?.lastAnilistError));
      } catch (error) {
        console.warn("[discovery] failed to refresh season sync state", error);
      }
    }

    const timer = window.setInterval(() => void refreshSyncState(), 15_000);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, [allowCollection, target.year, target.season]);

  useEffect(() => {
    if (!allowCollection) return;
    let active = true;
    let initialized = false;
    let wasInFlight = false;
    let refreshing = false;
    let lastCatalogFinishedAt: string | undefined;

    /** 轮询宿主任务状态，使切页返回后恢复采集进度。 */
    async function refreshTaskStatus() {
      if (refreshing) return;
      refreshing = true;
      try {
        const status = await appApi.getAnimeSeasonSyncTaskStatus();
        if (!active) return;
        setSyncTaskStatus(status);
        const catalogJustFinished = Boolean(
          status.catalogFinishedAt && status.catalogFinishedAt !== lastCatalogFinishedAt
        );
        if (catalogJustFinished) {
          const query = status.activeQuery ?? status.lastResult?.query;
          if (query?.year === target.year && query.season === target.season) {
            await loadSeasonCatalog(target.year, target.season);
          }
        }
        lastCatalogFinishedAt = status.catalogFinishedAt;
        const trackedTaskFinished = manualCollectRef.current
          && !status.inFlight
          && Boolean(status.finishedAt)
          && status.startedAt === manualTaskStartedAtRef.current;
        if ((initialized && wasInFlight && !status.inFlight) || trackedTaskFinished) {
          const result = status.lastResult;
          if (result?.query.year === target.year && result.query.season === target.season) {
            await loadSeasonCatalog(target.year, target.season);
          }
          if (manualCollectRef.current) {
            if (status.lastError) {
              toast.error(status.lastError);
            } else if (result) {
              const summary = `新增 ${result.addedCount}，更新 ${result.existingCount}，共 ${result.itemCount} 部`;
              if (result.errorCount > 0) {
                toast.warning(`季度采集已完成，${result.errorCount} 个来源异常；${summary}`);
              } else {
                toast.success(`季度采集完成：${summary}`);
              }
            }
            manualCollectRef.current = false;
            manualTaskStartedAtRef.current = null;
          }
        }
        initialized = true;
        wasInFlight = status.inFlight;
      } catch (error) {
        console.warn("[discovery] failed to refresh background task status", error);
      } finally {
        refreshing = false;
      }
    }

    void refreshTaskStatus();
    const timer = window.setInterval(() => void refreshTaskStatus(), 1_500);
    window.addEventListener("focus", refreshTaskStatus);
    return () => {
      active = false;
      window.clearInterval(timer);
      window.removeEventListener("focus", refreshTaskStatus);
    };
  }, [allowCollection, target.year, target.season]);

  /** Loads and merges the three local month catalogs in the selected season. */
  async function loadSeasonCatalog(year: number, season: Season) {
    const requestId = ++loadRequestId.current;
    const months = getSeasonOption(season).months;
    setLoading(true);

    try {
      const [catalogs, followed, syncState] = await Promise.all([
        Promise.all(months.map((month) => appApi.listAnimeCatalog(year, month))),
        appApi.listMyAnime(),
        allowCollection
          ? appApi.getAnimeSeasonSyncState(year, season)
          : Promise.resolve(undefined)
      ]);

      if (requestId !== loadRequestId.current) {
        return;
      }

      setItems(mergeAnimeItems(catalogs.flat()));
      setMyAnime(followed);
      setAnilistSyncError(normalizeAnilistError(syncState?.lastAnilistError));
      if (!appliedKeyword) {
        setMessage(null);
      }
    } catch (error) {
      if (requestId !== loadRequestId.current) {
        return;
      }

      console.error("[discovery] failed to load season catalog", { year, season, error });
      setMessage({
        tone: "error",
        text: error instanceof Error ? error.message : "加载新番目录失败"
      });
    } finally {
      if (requestId === loadRequestId.current) {
        setLoading(false);
      }
    }
  }

  /** 将当前季度采集交给宿主后台执行。 */
  async function collectSeason(forceRefresh = false) {
    if (collecting) return;
    setStartingSync(true);
    setMessage(null);
    console.info("[discovery] starting background season sync", { ...target, forceRefresh });

    try {
      manualCollectRef.current = true;
      const status = await appApi.startAnimeSeasonSync({ ...target, forceRefresh });
      manualTaskStartedAtRef.current = status.startedAt ?? null;
      setSyncTaskStatus(status);
      toast.info("已发起后台同步任务。");
      console.info("[discovery] background season sync accepted", { ...target });
    } catch (error) {
      manualCollectRef.current = false;
      manualTaskStartedAtRef.current = null;
      const errorMessage = error instanceof Error ? error.message : "采集新番失败";
      toast.error(errorMessage);
      console.error("[discovery] failed to start background season sync", { ...target, error });
    } finally {
      setStartingSync(false);
    }
  }

  /** 搜索本地全量缓存与在线元数据来源。 */
  async function searchCatalog() {
    const normalizedKeyword = keyword.trim();
    if (!normalizedKeyword) {
      searchRequestId.current += 1;
      setAppliedKeyword("");
      setSearchItems([]);
      setSearching(false);
      return;
    }

    const requestId = ++searchRequestId.current;
    setAppliedKeyword(normalizedKeyword);
    setSearchItems([]);
    setSearching(true);
    setMessage(null);
    console.info("[discovery] searching local and online catalog", { keyword: normalizedKeyword });

    try {
      const result = await appApi.searchAnimeCatalog(normalizedKeyword);
      if (requestId !== searchRequestId.current) return;
      setSearchItems(result.items);
      setMessage(result.errors.length ? {
        tone: "error",
        text: `部分来源搜索失败，已展示可用结果：${result.errors[0]}`
      } : null);
      console.info("[discovery] catalog search completed", {
        keyword: normalizedKeyword,
        source: result.source,
        itemCount: result.items.length,
        errorCount: result.errors.length
      });
    } catch (error) {
      if (requestId !== searchRequestId.current) return;
      console.error("[discovery] catalog search failed", { keyword: normalizedKeyword, error });
      setMessage({
        tone: "error",
        text: error instanceof Error ? error.message : "搜索新番失败"
      });
    } finally {
      if (requestId === searchRequestId.current) {
        setSearching(false);
      }
    }
  }

  /** 将目录条目添加到追番，并按来源选择后续处理流程。 */
  async function addToMyAnime(anime: Anime, source: "catalog" | "bangumi" = "catalog") {
    setAddingAnimeId(anime.id);
    try {
      const now = new Date().toISOString();
      const input: MyAnime = {
        id: `my-${anime.id}`,
        anime,
        status: "watching",
        ...createDefaultMyAnimePreferences(),
        addedAt: now,
        updatedAt: now
      };
      const updated = source === "bangumi"
        ? await appApi.followBangumiAnime(input)
        : await appApi.upsertMyAnime(input);
      setMyAnime(updated);
      setMessage({ tone: "success", text: `已添加「${resolveAnimeTitleDisplay(anime).title}」到我的追番` });
      toast.success(`已添加「${resolveAnimeTitleDisplay(anime).title}」到我的追番`);
      console.info("[discovery] anime added to library", { animeId: anime.id });
    } catch (error) {
      console.error("[discovery] failed to add anime", { animeId: anime.id, error });
      setMessage({
        tone: "error",
        text: error instanceof Error ? error.message : "添加追番失败"
      });
      toast.error(error instanceof Error ? error.message : "添加追番失败");
    } finally {
      setAddingAnimeId(null);
    }
  }

  /** 通过本地宿主打开外部元数据页面。 */
  async function openExternalId(externalId: ExternalIdBadge) {
    if (!externalId.url) {
      return;
    }

    try {
      await appApi.openExternal(externalId.url);
    } catch (error) {
      console.error("[discovery] failed to open external page", { url: externalId.url, error });
      setMessage({
        tone: "error",
        text: error instanceof Error ? error.message : "打开外部页面失败"
      });
    }
  }

  /** 清空目录筛选并恢复默认评分排序。 */
  function resetFilters() {
    setSelectedMonth(null);
    setKeyword("");
    setAppliedKeyword("");
    setSearchItems([]);
    searchRequestId.current += 1;
    setSearching(false);
    setSortKey(DEFAULT_DISCOVERY_SORT);
  }

  const collectingLabel = collecting
    ? syncTaskStatus?.phase === "details" ? "详情补全中" : "采集中"
    : "采集当前季度";
  const resultLabel = visibleLoading
    ? appliedKeyword ? "正在搜索" : "正在加载"
    : `共 ${visibleItems.length} 部`;
  const emptyCatalog = !appliedKeyword && items.length === 0;

  const seasonCatalogPanel = (
    <>
      {message && (
        <Alert variant={message.tone === "error" ? "destructive" : "default"}>
          {message.tone === "error" ? <AlertCircle /> : <CheckCircle2 />}
          <AlertTitle>{message.tone === "error" ? "操作未完成" : "操作完成"}</AlertTitle>
          <AlertDescription>{message.text}</AlertDescription>
        </Alert>
      )}

      <FilterToolbar className="items-stretch py-2 sm:flex-col sm:items-stretch">
        <div className="flex min-w-0 flex-wrap items-center gap-2 sm:flex-nowrap">
          <SeasonTargetPicker
            id="discovery-season"
            value={target}
            onValueChange={(nextTarget) => {
              if (nextTarget.season !== target.season) setSelectedMonth(null);
              setTarget(nextTarget);
            }}
          />

          <Field className="order-last min-w-0 basis-full sm:order-none sm:w-auto sm:basis-auto sm:shrink-0">
            <FieldLabel className="sr-only">选择月份</FieldLabel>
            <Tabs
              value={selectedMonth === null ? "all" : String(selectedMonth)}
              onValueChange={(value) => setSelectedMonth(value === "all" ? null : Number(value))}
            >
              <TabsList className="grid h-9 w-full grid-cols-4 sm:w-52" aria-label="选择月份">
                <TabsTrigger value="all">全部</TabsTrigger>
                {activeSeason.months.map((month) => (
                  <TabsTrigger key={month} value={String(month)}>{month}月</TabsTrigger>
                ))}
              </TabsList>
            </Tabs>
          </Field>

          <div aria-live="polite" className="shrink-0 text-sm tabular-nums text-muted-foreground">
            {resultLabel}
          </div>

          {workspaceTabs && allowCollection && (
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  aria-label="采集本季新番"
                  className="ml-auto size-9"
                  disabled={collecting}
                  onClick={() => void collectSeason(false)}
                  size="icon"
                >
                  {collecting
                    ? <LoaderCircle className="animate-spin" />
                    : <CloudDownload />}
                </Button>
              </TooltipTrigger>
              <TooltipContent align="end" side="bottom" sideOffset={8}>
                {collecting ? collectingLabel : "采集本季新番"}
              </TooltipContent>
            </Tooltip>
          )}
        </div>

        <div className="grid min-w-0 gap-2 sm:grid-cols-[minmax(0,1fr)_auto]">
          <form
            className="min-w-0"
            onSubmit={(event) => {
              event.preventDefault();
              searchCatalog();
            }}
          >
            <Field className="min-w-0">
              <FieldLabel className="sr-only" htmlFor="discovery-keyword">搜索番剧</FieldLabel>
              <InputGroup>
                <InputGroupAddon className="pl-3 pr-0 text-muted-foreground">
                  <Search aria-hidden="true" />
                </InputGroupAddon>
                <InputGroupInput
                  id="discovery-keyword"
                  placeholder="搜索中文名、日文名、罗马音或英文名"
                  value={keyword}
                  onChange={(event) => {
                    const value = event.target.value;
                    setKeyword(value);
                    if (!value) {
                      searchRequestId.current += 1;
                      setAppliedKeyword("");
                      setSearchItems([]);
                      setSearching(false);
                    }
                  }}
                />
                <InputGroupAddon>
                  <InputGroupButton aria-label="搜索新番" disabled={searching} title="搜索" type="submit">
                    <Search />
                  </InputGroupButton>
                </InputGroupAddon>
              </InputGroup>
            </Field>
          </form>

          <div className="flex min-w-0 gap-2 sm:w-auto">
            <Field className="min-w-0 flex-1 sm:w-[9.375rem] sm:flex-none">
              <FieldLabel className="sr-only" htmlFor="discovery-sort">排序方式</FieldLabel>
              <Select value={sortKey} onValueChange={(value) => setSortKey(value as DiscoverySortKey)}>
                <SelectTrigger id="discovery-sort">
                  <SelectValue placeholder="排序方式" />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    <SelectItem value="premiereAsc">发布时间升序</SelectItem>
                    <SelectItem value="premiereDesc">发布时间降序</SelectItem>
                    <SelectItem value="ratingDesc">评分降序</SelectItem>
                  </SelectGroup>
                </SelectContent>
              </Select>
            </Field>

            {(selectedMonth !== null || appliedKeyword || sortKey !== DEFAULT_DISCOVERY_SORT) && (
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button aria-label="重置所有筛选" onClick={resetFilters} size="icon" variant="ghost">
                    <RotateCcw />
                  </Button>
                </TooltipTrigger>
                <TooltipContent side="bottom">重置所有筛选</TooltipContent>
              </Tooltip>
            )}
          </div>
        </div>
      </FilterToolbar>

      {visibleLoading ? (
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5" aria-label="正在加载季度新番目录">
          {Array.from({ length: 10 }, (_, index) => (
            <div className="flex min-w-0 flex-col gap-3" key={index}>
              <Skeleton className="aspect-[2/3] w-full rounded-lg" />
              <Skeleton className="h-5 w-4/5" />
              <Skeleton className="h-4 w-3/5" />
            </div>
          ))}
        </div>
      ) : (
        visibleItems.length > 0 ? (
          <VirtualDiscoveryGrid
            addingAnimeId={addingAnimeId}
            followedIds={followedIds}
            items={visibleItems}
            onAdd={addToMyAnime}
            onOpenDetail={onOpenAnimeDetail}
            onOpenExternal={openExternalId}
          />
        ) : (
          <Empty>
            <EmptyHeader>
              <EmptyMedia variant="icon">
                {emptyCatalog ? <CalendarPlus /> : <Search />}
              </EmptyMedia>
              <EmptyTitle>{emptyCatalog ? "当前季度暂无目录" : "没有匹配的新番"}</EmptyTitle>
              <EmptyDescription>
                {emptyCatalog
                  ? allowCollection
                    ? "采集当前季度后即可浏览新番数据。"
                    : "桌面端完成目录采集后会自动显示在这里。"
                  : appliedKeyword ? "请更换关键词后重试。" : "请调整月份后重试。"}
              </EmptyDescription>
            </EmptyHeader>
            <EmptyContent>
              {emptyCatalog && allowCollection ? (
                <Button onClick={() => void collectSeason(false)} disabled={collecting}>
                  <CalendarPlus data-icon="inline-start" />
                  {collectingLabel}
                </Button>
              ) : !emptyCatalog ? (
                <Button
                  variant="outline"
                  onClick={() => {
                    setKeyword("");
                    setAppliedKeyword("");
                    setSearchItems([]);
                    searchRequestId.current += 1;
                    setSearching(false);
                    if (!appliedKeyword) setSelectedMonth(null);
                  }}
                >
                  <RotateCcw data-icon="inline-start" />
                  清除筛选
                </Button>
              ) : null}
            </EmptyContent>
          </Empty>
        )
      )}

      {anilistSyncError && (
        <Alert variant="destructive">
          <AlertCircle />
          <AlertTitle>AniList 同步失败</AlertTitle>
          <AlertDescription>{anilistSyncError}</AlertDescription>
        </Alert>
      )}
    </>
  );

  if (workspaceTabs) {
    return (
      <Page className="gap-0">
        <header>
          <Breadcrumb className="pt-1">
            <BreadcrumbList className="gap-1.5 text-sm">
              <BreadcrumbItem><span>发现</span></BreadcrumbItem>
              <BreadcrumbSeparator />
              <BreadcrumbItem><BreadcrumbPage className="font-medium">新番发现</BreadcrumbPage></BreadcrumbItem>
            </BreadcrumbList>
          </Breadcrumb>
          <h1 className="sr-only">新番发现</h1>
          <Tabs value={activeWorkspaceTab} onValueChange={(value) => changeWorkspaceTab(value as DiscoveryWorkspaceTab)}>
            <TabsList aria-label="新番发现视图" className="mt-2 w-full" variant="line">
              <TabsTrigger value="season">季度新番</TabsTrigger>
              <TabsTrigger value="schedule">新番时间表</TabsTrigger>
              <TabsTrigger value="browse">Bangumi</TabsTrigger>
            </TabsList>
          </Tabs>
        </header>

        <section
          aria-labelledby="discovery-tab-season"
          className={cn(
            "mt-4 min-w-0 flex-col gap-6",
            activeWorkspaceTab === "season" ? "flex" : "hidden"
          )}
          hidden={activeWorkspaceTab !== "season"}
        >
          <span className="sr-only" id="discovery-tab-season">季度新番</span>
          {seasonCatalogPanel}
        </section>

        <DiscoveryScheduleWorkspacePanel
          addingAnimeId={addingAnimeId}
          followedIds={followedIds}
          hidden={activeWorkspaceTab !== "schedule"}
          initialTarget={target}
          onAdd={addToMyAnime}
          onOpenAnimeDetail={onOpenAnimeDetail}
        />

        <DiscoveryBrowsePanel
          addingAnimeId={addingAnimeId}
          followedIds={followedIds}
          hidden={activeWorkspaceTab !== "browse"}
          onAdd={(anime) => addToMyAnime(anime, "bangumi")}
          onOpenAnimeDetail={onOpenAnimeDetail}
        />
      </Page>
    );
  }

  return (
    <Page>
      <PageHeader>
        <PageHeading description="按季度浏览新番目录，并查看作品信息。" title="新番发现" />
        <PageActions className={cn("grid grid-cols-1", allowCollection && "sm:grid-cols-2")}>
          <Button className="w-full" variant="outline" onClick={() => onOpenSchedule?.(target)}>
            <CalendarRange data-icon="inline-start" />
            新番时间表
          </Button>
          {allowCollection && (
            <Button className="w-full" onClick={() => void collectSeason(false)} disabled={collecting}>
              {collecting
                ? <LoaderCircle className="animate-spin" data-icon="inline-start" />
                : <CalendarPlus data-icon="inline-start" />}
              {collectingLabel}
            </Button>
          )}
        </PageActions>
      </PageHeader>
      {seasonCatalogPanel}
    </Page>
  );
}

interface VirtualDiscoveryGridProps {
  addingAnimeId: string | null;
  followedIds: Set<string>;
  items: Anime[];
  onAdd: (anime: Anime) => Promise<void>;
  onOpenDetail?: (animeId: string) => void;
  onOpenExternal: (externalId: ExternalIdBadge) => Promise<void>;
}

/** 返回与新番卡片 Tailwind 断点一致的当前列数。 */
function getDiscoveryColumnCount(viewportWidth: number): number {
  if (viewportWidth >= 1280) return 5;
  if (viewportWidth >= 1024) return 4;
  if (viewportWidth >= 640) return 3;
  return 2;
}

/** 跟踪视口断点，确保虚拟行与响应式网格列数一致。 */
function useDiscoveryColumnCount(): number {
  const [columnCount, setColumnCount] = useState(() => getDiscoveryColumnCount(window.innerWidth));
  useEffect(() => {
    const updateColumnCount = () => setColumnCount(getDiscoveryColumnCount(window.innerWidth));
    window.addEventListener("resize", updateColumnCount);
    return () => window.removeEventListener("resize", updateColumnCount);
  }, []);
  return columnCount;
}

/** 按响应式行虚拟化新番网格，避免长目录一次挂载全部卡片。 */
function VirtualDiscoveryGrid({
  addingAnimeId,
  followedIds,
  items,
  onAdd,
  onOpenDetail,
  onOpenExternal
}: VirtualDiscoveryGridProps) {
  const columnCount = useDiscoveryColumnCount();
  const rows = useMemo(
    () => Array.from(
      { length: Math.ceil(items.length / columnCount) },
      (_, rowIndex) => items.slice(rowIndex * columnCount, (rowIndex + 1) * columnCount)
    ),
    [columnCount, items]
  );
  const scrollContainerRef = useAppScrollContainer();
  const gridRef = useRef<HTMLDivElement | null>(null);
  const scrollMargin = useVirtualizerScrollMargin(scrollContainerRef, gridRef);
  const virtualizer = useVirtualizer({
    count: rows.length,
    estimateSize: () => 410,
    getItemKey: (index) => rows[index]?.[0]?.id ?? index,
    getScrollElement: () => scrollContainerRef.current,
    overscan: 3,
    scrollMargin
  });
  const scrollToTop = useCallback(() => {
    virtualizer.scrollToOffset(0, { align: "start", behavior: "auto" });
  }, [virtualizer]);
  useAppScrollToTopHandler(scrollToTop);

  return (
    <div
      ref={gridRef}
      className="relative min-w-0"
      style={{ height: virtualizer.getTotalSize() }}
    >
      {virtualizer.getVirtualItems().map((virtualRow) => {
        const row = rows[virtualRow.index];
        if (!row) return null;
        return (
          <div
            className="absolute left-0 top-0 grid w-full grid-cols-2 gap-x-3 pb-6 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5 xl:gap-x-4"
            data-index={virtualRow.index}
            key={row[0]?.id ?? virtualRow.index}
            ref={virtualizer.measureElement}
            style={{ transform: `translateY(${virtualRow.start - scrollMargin}px)` }}
          >
            {row.map((anime) => (
              <DiscoveryAnimeCard
                adding={addingAnimeId === anime.id}
                anime={anime}
                followed={followedIds.has(anime.id)}
                key={anime.id}
                onAdd={onAdd}
                onOpenDetail={onOpenDetail}
                onOpenExternal={onOpenExternal}
              />
            ))}
          </div>
        );
      })}
    </div>
  );
}

/** 在新番发现工作区内渲染保持独立季度状态的时间表。 */
function DiscoveryScheduleWorkspacePanel({
  addingAnimeId,
  followedIds,
  hidden,
  initialTarget,
  onAdd,
  onOpenAnimeDetail
}: {
  addingAnimeId: string | null;
  followedIds: Set<string>;
  hidden: boolean;
  initialTarget: SeasonTarget;
  onAdd: (anime: Anime) => Promise<void>;
  onOpenAnimeDetail?: (animeId: string) => void;
}) {
  const [target, setTarget] = useState(initialTarget);
  const [view, setView] = useState<ScheduleView>("grid");
  const [items, setItems] = useState<Anime[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const loadRequestId = useRef(0);
  const activeSeason = getSeasonOption(target.season);
  const visibleItems = useMemo(() => sortAnimeItems(items, "premiereAsc"), [items]);
  const today = new Date();
  const todayItems = visibleItems.filter((anime) => getAnimeWeekday(anime) === today.getDay());

  useEffect(() => {
    const requestId = ++loadRequestId.current;
    setLoading(true);
    Promise.all(activeSeason.months.map((month) => appApi.listAnimeCatalog(target.year, month)))
      .then((catalogs) => {
        if (requestId !== loadRequestId.current) return;
        setItems(mergeAnimeItems(catalogs.flat()));
        setError(null);
      })
      .catch((caught) => {
        if (requestId !== loadRequestId.current) return;
        console.error("[discovery-schedule] 工作区时间表加载失败", { ...target, error: caught });
        setError(caught instanceof Error ? caught.message : "加载新番时间表失败");
      })
      .finally(() => {
        if (requestId === loadRequestId.current) setLoading(false);
      });
  }, [activeSeason.months, target.season, target.year]);

  return (
    <section className={cn("mt-4 min-w-0 flex-col gap-6", hidden ? "hidden" : "flex")} hidden={hidden}>
      <div className="flex min-w-0 items-center justify-between gap-3 text-xs font-medium">
        <span className="sr-only">新番时间表</span>
        <span className="text-muted-foreground">按季度查看每周首播放送安排</span>
        <span className="flex shrink-0 items-center gap-2">
          <span className="size-1.5 rounded-full bg-primary" aria-hidden="true" />
          今日放送 · {formatTodayLabel(today)}
        </span>
      </div>

      {error && (
        <Alert variant="destructive">
          <AlertCircle />
          <AlertTitle>时间表加载失败</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}

      <FilterToolbar className="py-2">
        <SeasonTargetPicker id="workspace-schedule-season" value={target} onValueChange={setTarget} />
        <ToggleGroup
          aria-label="选择时间表视图"
          className="grid grid-cols-2"
          type="single"
          value={view}
          onValueChange={(value) => value && setView(value as ScheduleView)}
        >
          <ToggleGroupItem value="grid"><LayoutGrid data-icon="inline-start" />按天</ToggleGroupItem>
          <ToggleGroupItem value="list"><List data-icon="inline-start" />列表</ToggleGroupItem>
        </ToggleGroup>
      </FilterToolbar>

      {loading ? (
        <div className="min-w-0 overflow-x-auto" aria-label="正在加载新番时间表">
          <div className="grid min-w-[70rem] grid-cols-7 gap-2 overflow-hidden">
            {Array.from({ length: 7 }, (_, index) => <Skeleton className="h-48 w-full" key={index} />)}
          </div>
        </div>
      ) : view === "grid" ? (
        <DiscoverySchedule
          addingAnimeId={addingAnimeId}
          followedIds={followedIds}
          items={visibleItems}
          onAdd={onAdd}
          onOpenDetail={onOpenAnimeDetail}
        />
      ) : (
        <DiscoveryScheduleList
          addingAnimeId={addingAnimeId}
          followedIds={followedIds}
          items={todayItems}
          onAdd={onAdd}
          onOpenDetail={onOpenAnimeDetail}
        />
      )}
    </section>
  );
}

interface DiscoverySchedulePageProps {
  initialTarget: SeasonTarget;
  onBack: () => void;
  onOpenAnimeDetail?: (animeId: string) => void;
}

/** 渲染独立的新番时间表二级页面。 */
export function DiscoverySchedulePage({ initialTarget, onBack, onOpenAnimeDetail }: DiscoverySchedulePageProps) {
  const [target, setTarget] = useState(initialTarget);
  const [view, setView] = useState<ScheduleView>("grid");
  const [items, setItems] = useState<Anime[]>([]);
  const [myAnime, setMyAnime] = useState<MyAnime[]>([]);
  const [loading, setLoading] = useState(true);
  const [addingAnimeId, setAddingAnimeId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const loadRequestId = useRef(0);
  const activeSeason = getSeasonOption(target.season);
  const followedIds = useMemo(() => new Set(myAnime.map((item) => item.anime.id)), [myAnime]);
  const visibleItems = useMemo(
    () => sortAnimeItems(items, "premiereAsc"),
    [items]
  );
  const today = new Date();
  const todayItems = visibleItems.filter((anime) => getAnimeWeekday(anime) === today.getDay());

  useEffect(() => {
    const requestId = ++loadRequestId.current;
    setLoading(true);
    Promise.all([
      Promise.all(activeSeason.months.map((month) => appApi.listAnimeCatalog(target.year, month))),
      appApi.listMyAnime()
    ])
      .then(([catalogs, followed]) => {
        if (requestId !== loadRequestId.current) return;
        setItems(mergeAnimeItems(catalogs.flat()));
        setMyAnime(followed);
        setError(null);
      })
      .catch((caught) => {
        if (requestId !== loadRequestId.current) return;
        console.error("[discovery-schedule] 时间表加载失败", { ...target, error: caught });
        setError(caught instanceof Error ? caught.message : "加载新番时间表失败");
      })
      .finally(() => {
        if (requestId === loadRequestId.current) setLoading(false);
      });
  }, [activeSeason.months, target.season, target.year]);

  /** 添加时间表中的番剧到我的追番。 */
  async function addToMyAnime(anime: Anime) {
    setAddingAnimeId(anime.id);
    try {
      const now = new Date().toISOString();
      const updated = await appApi.upsertMyAnime({
        id: `my-${anime.id}`,
        anime,
        status: "watching",
        ...createDefaultMyAnimePreferences(),
        addedAt: now,
        updatedAt: now
      });
      setMyAnime(updated);
      setError(null);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : "添加追番失败");
    } finally {
      setAddingAnimeId(null);
    }
  }

  return (
    <Page>
      <PageHeader className="items-center sm:items-center" data-window-controls-clearance="">
        <Button className="h-auto w-fit min-h-0 justify-start px-0 text-xs" onClick={onBack} variant="ghost">
          <ArrowLeft data-icon="inline-start" />
          新番发现 / 新番时间表
        </Button>
        <div className="flex items-center gap-2 text-xs font-medium">
          <span className="uppercase text-muted-foreground">今日放送</span>
          <span className="size-1.5 rounded-full bg-primary" aria-hidden="true" />
          <span>{formatTodayLabel(today)}</span>
        </div>
      </PageHeader>

      {error && (
        <Alert variant="destructive">
          <AlertCircle />
          <AlertTitle>时间表加载失败</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}

      <FilterToolbar className="items-stretch sm:items-center">
        <div className="grid min-w-0 flex-1 gap-3 sm:grid-cols-[9.25rem_auto] sm:items-center sm:justify-between">
          <SeasonTargetPicker
            id="schedule-season"
            value={target}
            onValueChange={(nextTarget) => setTarget(nextTarget)}
          />
          <ToggleGroup
            aria-label="选择时间表视图"
            className="grid grid-cols-2 sm:w-fit"
            type="single"
            value={view}
            onValueChange={(value) => value && setView(value as ScheduleView)}
          >
            <ToggleGroupItem value="grid"><LayoutGrid data-icon="inline-start" />网格视图</ToggleGroupItem>
            <ToggleGroupItem value="list"><List data-icon="inline-start" />列表视图</ToggleGroupItem>
          </ToggleGroup>
        </div>
      </FilterToolbar>

      {loading ? (
        <div className="min-w-0 overflow-x-auto" aria-label="正在加载新番时间表">
          <div className="grid min-w-[70rem] grid-cols-7 gap-2 overflow-hidden">
            {Array.from({ length: 7 }, (_, index) => <Skeleton className="h-48 w-full" key={index} />)}
          </div>
        </div>
      ) : view === "grid" ? (
        <DiscoverySchedule
          addingAnimeId={addingAnimeId}
          followedIds={followedIds}
          items={visibleItems}
          onAdd={addToMyAnime}
          onOpenDetail={onOpenAnimeDetail}
        />
      ) : (
        <DiscoveryScheduleList
          addingAnimeId={addingAnimeId}
          followedIds={followedIds}
          items={todayItems}
          onAdd={addToMyAnime}
          onOpenDetail={onOpenAnimeDetail}
        />
      )}
    </Page>
  );
}

/** 渲染新番页面共用的年份与季度选择器。 */
function SeasonTargetPicker({
  id,
  value,
  onValueChange
}: {
  id: string;
  value: SeasonTarget;
  onValueChange: (target: SeasonTarget) => void;
}) {
  const [open, setOpen] = useState(false);
  const activeSeason = getSeasonOption(value.season);

  /** 选择季度，并保留当前年份。 */
  function selectSeason(season: string) {
    if (!season) return;
    console.info("[season-target-picker] 季度已选择", { year: value.year, season });
    onValueChange({ ...value, season: season as Season });
    setOpen(false);
  }

  return (
    <Field className="w-auto min-w-0 shrink-0">
      <FieldLabel className="sr-only" htmlFor={`${id}-trigger`}>选择年份和季度</FieldLabel>
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <Button
            aria-expanded={open}
            aria-haspopup="dialog"
            className="h-9 min-h-9 w-[9.25rem] justify-start gap-2 px-2.5 tabular-nums"
            id={`${id}-trigger`}
            type="button"
            variant="outline"
          >
            <CalendarDays aria-hidden="true" />
            <span>{value.year}</span>
            <Separator className="h-4" orientation="vertical" />
            <span className="text-primary">{activeSeason.label}</span>
            <ChevronDown aria-hidden="true" className="ml-auto" />
          </Button>
        </PopoverTrigger>
        <PopoverContent align="start" className="w-72 p-3">
          <div className="flex items-center justify-between gap-3">
            <div className="text-sm font-semibold">选择季度</div>
            <div className="w-28">
              <YearPicker
                id={`${id}-year`}
                triggerLabel={value.year}
                value={value.year}
                onValueChange={(year) => onValueChange({ ...value, year })}
              />
            </div>
          </div>
          <ToggleGroup
            aria-label="在选择器中选择季度"
            className="mt-3 grid grid-cols-2 gap-2"
            type="single"
            value={value.season}
            variant="outline"
            onValueChange={selectSeason}
          >
            {seasonOptions.map((season) => (
              <ToggleGroupItem
                aria-label={`选择${season.label}`}
                className="h-auto min-h-14 justify-between px-3 py-2 text-left data-[state=on]:border-primary data-[state=on]:bg-primary/10 data-[state=on]:text-primary"
                key={season.value}
                value={season.value}
              >
                <span className="flex min-w-0 flex-col items-start gap-0.5">
                  <span>{season.label}</span>
                  <span className="text-xs font-normal text-muted-foreground">
                    {season.months[0]}–{season.months[2]}月
                  </span>
                </span>
                {season.value === value.season && <Check aria-hidden="true" />}
              </ToggleGroupItem>
            ))}
          </ToggleGroup>
        </PopoverContent>
      </Popover>
    </Field>
  );
}

/** 渲染无外层装饰卡片的 2:3 新番海报项。 */
function DiscoveryAnimeCard({
  adding,
  anime,
  followed,
  onAdd,
  onOpenDetail,
  onOpenExternal
}: {
  adding: boolean;
  anime: Anime;
  followed: boolean;
  onAdd: (anime: Anime) => Promise<void>;
  onOpenDetail?: (animeId: string) => void;
  onOpenExternal: (externalId: ExternalIdBadge) => Promise<void>;
}) {
  const titleDisplay = resolveAnimeTitleDisplay(anime);
  const externalIds = buildExternalIdBadges(anime).filter((item) => item.url).slice(0, 2);
  const aliasTitle = titleDisplay.aliases.map((alias) => alias.alias).join("\n");

  return (
    <article className="flex min-w-0 flex-col" title={aliasTitle || undefined}>
      <button
        aria-label={`查看${titleDisplay.title}详情`}
        className="relative aspect-[2/3] overflow-hidden rounded-lg border bg-muted text-left outline-none ring-offset-background focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"
        onClick={() => onOpenDetail?.(anime.id)}
        type="button"
      >
        {anime.coverUrl ? (
          <CachedImage
            alt={titleDisplay.title}
            className="size-full object-cover"
            loading="lazy"
            sourceUrl={anime.coverUrl}
          />
        ) : (
          <div className="flex size-full items-center justify-center text-muted-foreground">
            <ImageOff className="size-7" />
          </div>
        )}
        <Badge className="absolute right-2 top-2" tone="primary">
          {anime.rating ? anime.rating.score.toFixed(1) : `${anime.premiereMonth}月`}
        </Badge>
        {followed && <Badge className="absolute left-2 top-2" tone="green">已追番</Badge>}
      </button>

      <div className="mt-3 min-w-0">
        <button
          className="block w-full truncate text-left text-sm font-semibold hover:text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          onClick={() => onOpenDetail?.(anime.id)}
          title={titleDisplay.title}
          type="button"
        >
          {titleDisplay.title}
        </button>
        <p className="mt-1 truncate text-xs text-muted-foreground" title={titleDisplay.subtitle}>
          {titleDisplay.subtitle ?? formatPremiere(anime)}
        </p>
        <div className="mt-2 flex items-center gap-2 text-xs text-muted-foreground">
          <CalendarDays className="size-4 shrink-0" />
          <span className="truncate">{formatPremiere(anime)}</span>
        </div>
      </div>

      {externalIds.length > 0 && (
        <div className="mt-2 grid grid-cols-2 gap-2">
          {externalIds.map((externalId) => (
            <Button
              className="min-w-0 px-2"
              key={externalId.key}
              onClick={() => void onOpenExternal(externalId)}
              title={`${externalId.label}: ${externalId.value}`}
              type="button"
              variant="ghost"
            >
              <span className="truncate">{externalId.label}</span>
              <ExternalLink data-icon="inline-end" />
            </Button>
          ))}
        </div>
      )}

      <Button
        className="mt-2 w-full"
        disabled={followed || adding}
        onClick={() => void onAdd(anime)}
        variant={followed ? "secondary" : "primary"}
      >
        <Plus data-icon="inline-start" />
        {followed ? "已在追番" : adding ? "添加中" : "添加追番"}
      </Button>
    </article>
  );
}

interface BrowseFilterOption<T extends string | number> {
  value: T;
  label: string;
  badgeLabel?: string;
}

const browseFormatOptions: readonly BrowseFilterOption<AnimeFormat>[] = [
  { value: "tv", label: "TV动画", badgeLabel: "TV" },
  { value: "movie", label: "剧场版" },
  { value: "ova", label: "OVA/OAD", badgeLabel: "OVA" },
  { value: "ona", label: "Web动画", badgeLabel: "WEB" }
];
const browseSourceOptions: readonly BrowseFilterOption<DiscoverySourceMaterial>[] = [
  { value: "original", label: "原创" },
  { value: "manga", label: "漫画改" },
  { value: "lightNovel", label: "轻小说改" },
  { value: "game", label: "游戏改" },
  { value: "other", label: "其他" }
];
const browseGenreOptions: readonly BrowseFilterOption<DiscoveryGenre>[] = [
  { value: "reasoning", label: "推理" },
  { value: "harem", label: "后宫" },
  { value: "sciFi", label: "科幻" },
  { value: "girlsLove", label: "百合" },
  { value: "horror", label: "恐怖" },
  { value: "romance", label: "恋爱" },
  { value: "music", label: "音乐" },
  { value: "school", label: "校园" },
  { value: "timeTravel", label: "穿越" },
  { value: "action", label: "战斗" },
  { value: "sports", label: "运动" },
  { value: "martialArts", label: "武侠" },
  { value: "fantasy", label: "奇幻" },
  { value: "thriller", label: "惊悚" },
  { value: "comedy", label: "搞笑" },
  { value: "sliceOfLife", label: "日常" },
  { value: "mystery", label: "悬疑" },
  { value: "adventure", label: "冒险" },
  { value: "history", label: "历史" },
  { value: "otome", label: "乙女" },
  { value: "food", label: "美食" },
  { value: "workplace", label: "职场" },
  { value: "xuanhuan", label: "玄幻" },
  { value: "mecha", label: "机战" }
];
const browseDemographicOptions: readonly BrowseFilterOption<DiscoveryDemographic>[] = [
  { value: "shounen", label: "少年" },
  { value: "shoujo", label: "少女" },
  { value: "seinen", label: "青年" },
  { value: "josei", label: "女性" },
  { value: "kids", label: "儿童" }
];
const browseRegionOptions: readonly BrowseFilterOption<DiscoveryRegion>[] = [
  { value: "japan", label: "日本" },
  { value: "china", label: "中国" },
  { value: "korea", label: "韩国" },
  { value: "western", label: "欧美" },
  { value: "other", label: "其他" }
];
const BROWSE_PAGE_SIZE = 20;

/** 仅传递 Bangumi API 支持的筛选字段。 */
function toBangumiBrowseFilters(filters: DiscoveryBrowseFilters): BangumiBrowseQuery["filters"] {
  return {
    formats: filters.formats,
    sourceMaterials: filters.sourceMaterials,
    genres: filters.genres,
    demographics: filters.demographics,
    regions: filters.regions,
    years: filters.years,
    yearRange: filters.yearRange,
    minRating: filters.minRating
  };
}

/** 渲染不依赖本地季度缓存的 Bangumi 在线浏览页签。 */
function DiscoveryBrowsePanel({
  addingAnimeId,
  followedIds,
  hidden,
  onAdd,
  onOpenAnimeDetail
}: {
  addingAnimeId: string | null;
  followedIds: Set<string>;
  hidden: boolean;
  onAdd: (anime: Anime) => Promise<void>;
  onOpenAnimeDetail?: (animeId: string, previewAnime?: Anime) => void;
}) {
  const [items, setItems] = useState<Anime[]>([]);
  const [total, setTotal] = useState(0);
  const [keyword, setKeyword] = useState("");
  const [appliedKeyword, setAppliedKeyword] = useState("");
  const [sortKey, setSortKey] = useState<DiscoveryBrowseSortKey>("bangumiRank");
  const [filters, setFilters] = useState<DiscoveryBrowseFilters>(createEmptyDiscoveryBrowseFilters);
  const [draftFilters, setDraftFilters] = useState<DiscoveryBrowseFilters>(createEmptyDiscoveryBrowseFilters);
  const [page, setPage] = useState(1);
  const [loading, setLoading] = useState(true);
  const [requestVersion, setRequestVersion] = useState(0);
  const [sheetOpen, setSheetOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const loadRequestId = useRef(0);

  useEffect(() => {
    if (hidden) return;
    const requestId = ++loadRequestId.current;
    setLoading(true);
    setError(null);
    const query: BangumiBrowseQuery = {
      keyword: appliedKeyword,
      sort: sortKey,
      filters: toBangumiBrowseFilters(filters),
      page,
      pageSize: BROWSE_PAGE_SIZE
    };
    appApi.browseBangumiAnime(query)
      .then((result) => {
        if (requestId !== loadRequestId.current) return;
        setItems(result.items);
        setTotal(result.total);
        console.info("[discovery-browse] Bangumi 在线结果已加载", {
          keyword: appliedKeyword,
          page,
          itemCount: result.items.length,
          total: result.total
        });
      })
      .catch((caught) => {
        if (requestId !== loadRequestId.current) return;
        setItems([]);
        setTotal(0);
        console.error("[discovery-browse] Bangumi 在线浏览失败", { query, error: caught });
        setError(caught instanceof Error ? caught.message : "Bangumi 在线浏览失败");
      })
      .finally(() => {
        if (requestId === loadRequestId.current) setLoading(false);
      });
  }, [appliedKeyword, filters, hidden, page, requestVersion, sortKey]);

  const totalPages = Math.max(1, Math.ceil(total / BROWSE_PAGE_SIZE));
  const selectedCount = countDiscoveryBrowseFilters(filters);
  const draftSelectedCount = countDiscoveryBrowseFilters(draftFilters);
  const currentYear = useMemo(() => new Date().getFullYear(), []);
  const yearOptions = useMemo(
    () => Array.from({ length: 10 }, (_, index) => currentYear - index),
    [currentYear]
  );
  const timeOptions = useMemo<readonly BrowseFilterOption<BrowseTimeValue>[]>(() => [
    { value: "future", label: "未来年份" },
    ...yearOptions.map((year) => ({ value: `year:${year}` as const, label: `${year}年` })),
    { value: "earlier", label: "更早年份" }
  ], [yearOptions]);
  const selectedDraftTime = getSelectedBrowseTimeValue(draftFilters);

  useEffect(() => {
    if (page <= totalPages) return;
    setPage(totalPages);
  }, [page, totalPages]);

  /** 提交 Bangumi 在线关键词并重新读取第一页。 */
  function searchBrowseCatalog() {
    const normalizedKeyword = keyword.trim();
    setAppliedKeyword(normalizedKeyword);
    setPage(1);
    setRequestVersion((current) => current + 1);
  }

  /** 打开筛选 Sheet，并从当前已应用条件创建独立草稿。 */
  function openFilterSheet() {
    setDraftFilters(cloneBrowseFilters(filters));
    setSheetOpen(true);
  }

  /** 移除紧凑工具栏中的一个已应用条件。 */
  function removeAppliedFilter(field: BrowseFilterKey, value: string | number) {
    setFilters((current) => {
      if (field === "minRating") return { ...current, minRating: 0 };
      if (field === "yearRange") return { ...current, yearRange: null };
      return ({
        ...current,
        [field]: (current[field] as Array<string | number>).filter((item) => item !== value)
      }) as DiscoveryBrowseFilters;
    });
    setPage(1);
  }

  const appliedBadges = buildBrowseFilterBadges(filters);

  return (
    <section className={cn("min-w-0 flex-col", hidden ? "hidden" : "flex")} hidden={hidden}>
      <div
        className="mt-4 flex h-auto min-w-0 flex-col gap-2 border-y py-2 sm:h-[88px] sm:justify-between sm:gap-0"
        data-testid="discovery-browse-toolbar"
      >
        <div className="grid min-w-0 grid-cols-[minmax(0,1fr)_auto] items-center gap-2 sm:flex sm:h-9 sm:justify-between sm:gap-3">
          <form
            className="col-span-2 min-w-0 sm:col-span-1 sm:max-w-md sm:flex-1"
            onSubmit={(event) => {
              event.preventDefault();
              searchBrowseCatalog();
            }}
          >
            <InputGroup className="h-9 min-h-9">
              <InputGroupAddon className="pl-3 pr-0 text-muted-foreground"><Search aria-hidden="true" /></InputGroupAddon>
              <InputGroupInput
                aria-label="搜索 Bangumi 番剧"
                placeholder="搜索 Bangumi 番剧..."
                value={keyword}
                onChange={(event) => {
                  const value = event.target.value;
                  setKeyword(value);
                  if (!value) {
                    setAppliedKeyword("");
                    setPage(1);
                  }
                }}
              />
              <InputGroupAddon>
                <InputGroupButton aria-label="搜索 Bangumi 番剧" disabled={loading} title="搜索" type="submit">
                  {loading ? <LoaderCircle className="animate-spin" /> : <Search />}
                </InputGroupButton>
              </InputGroupAddon>
            </InputGroup>
          </form>
          <div className="col-span-2 flex shrink-0 items-center justify-end gap-2 sm:col-span-1">
            <Select
              value={sortKey}
              onValueChange={(value) => {
                setSortKey(value as DiscoveryBrowseSortKey);
                setPage(1);
              }}
            >
              <SelectTrigger aria-label="Bangumi 结果排序" className="h-9 min-h-9 w-36">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectGroup>
                  <SelectItem value="bangumiRank">Bangumi 排名</SelectItem>
                <SelectItem value="recent">Bangumi 热度</SelectItem>
                  <SelectItem value="rating">最高评分</SelectItem>
                </SelectGroup>
              </SelectContent>
            </Select>
            <Button
              aria-label={`筛选番剧，已选择 ${selectedCount} 项`}
              className="relative"
              onClick={openFilterSheet}
              size="icon"
              title="筛选番剧"
              variant="secondary"
            >
              <SlidersHorizontal />
              {selectedCount > 0 && (
                <span className="absolute -right-1 -top-1 grid size-4 place-content-center rounded-full bg-primary text-[10px] font-bold leading-none text-primary-foreground">
                  {selectedCount}
                </span>
              )}
            </Button>
          </div>
        </div>

        <div className="flex min-h-7 min-w-0 items-center justify-between gap-3">
          <div className="flex min-w-0 flex-1 items-center gap-1.5 overflow-hidden whitespace-nowrap">
            {appliedBadges.map((badge) => (
              <span className="inline-flex h-6 shrink-0 items-center gap-1 rounded bg-muted px-2 text-[11px] font-medium" key={badge.key}>
                {badge.label}
                <button
                  aria-label={`移除${badge.label}筛选`}
                  className="grid size-4 place-content-center rounded-sm text-muted-foreground hover:text-primary focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                  onClick={() => removeAppliedFilter(badge.field, badge.value)}
                  title={`移除${badge.label}`}
                  type="button"
                >
                  <X className="size-3" />
                </button>
              </span>
            ))}
            {selectedCount > 0 && (
              <Button
                className="h-6 min-h-0 shrink-0 px-2 text-[11px]"
                onClick={() => {
                  setFilters(createEmptyDiscoveryBrowseFilters());
                  setPage(1);
                }}
                variant="ghost"
              >
                清空全部
              </Button>
            )}
          </div>
          <div className="flex shrink-0 items-center gap-4 text-[11px] text-muted-foreground">
            <span>找到 {total} 部</span>
            <span>Bangumi 在线</span>
          </div>
        </div>
      </div>

      {error && (
        <Alert className="mt-4" variant="destructive">
          <AlertCircle />
          <AlertTitle>Bangumi 暂时不可用</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}

      <div className="mt-6 min-w-0">
        {loading ? (
          <div className="grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5" aria-label="正在加载 Bangumi 番剧">
            {Array.from({ length: 10 }, (_, index) => (
              <div className="overflow-hidden rounded-lg border" key={index}>
                <Skeleton className="aspect-[2/3] w-full rounded-none" />
                <div className="flex flex-col gap-2 p-3"><Skeleton className="h-4 w-4/5" /><Skeleton className="h-3 w-3/5" /></div>
              </div>
            ))}
          </div>
        ) : items.length > 0 ? (
          <>
            <div className="grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5">
              {items.map((anime) => (
                <DiscoveryBrowseAnimeCard
                  adding={addingAnimeId === anime.id}
                  anime={anime}
                  followed={followedIds.has(anime.id)}
                  key={anime.id}
                  onAdd={onAdd}
                  onOpenDetail={onOpenAnimeDetail}
                />
              ))}
            </div>
            {totalPages > 1 && <DiscoveryBrowsePagination page={page} totalPages={totalPages} onPageChange={setPage} />}
          </>
        ) : (
          <Empty>
            <EmptyHeader>
              <EmptyMedia variant="icon"><Search /></EmptyMedia>
              <EmptyTitle>没有匹配的番剧</EmptyTitle>
              <EmptyDescription>请调整搜索词或筛选条件后重试。</EmptyDescription>
            </EmptyHeader>
            <EmptyContent>
              <Button
                onClick={() => {
                  setKeyword("");
                  setAppliedKeyword("");
                  setFilters(createEmptyDiscoveryBrowseFilters());
                  setPage(1);
                }}
                variant="outline"
              >
                <RotateCcw data-icon="inline-start" />
                清除筛选
              </Button>
            </EmptyContent>
          </Empty>
        )}
      </div>

      <Sheet open={sheetOpen} onOpenChange={setSheetOpen}>
        <SheetContent className="flex w-full max-w-[400px] flex-col gap-0 p-0 sm:max-w-[400px]" side="right">
          <SheetHeader className="shrink-0 border-b px-6 py-5 pr-12 text-left">
            <SheetTitle>筛选番剧</SheetTitle>
            <SheetDescription>已选择 {draftSelectedCount} 项</SheetDescription>
          </SheetHeader>
          <ScrollArea className="min-h-0 flex-1">
            <div className="px-6">
              <BrowseChoiceFilterSection
                defaultOpen
                options={browseFormatOptions}
                selected={draftFilters.formats}
                title="作品类型"
                onToggle={(value) => setDraftFilters((current) => ({ ...current, formats: toggleBrowseSingleValue(current.formats, value) }))}
              />
              <BrowseChoiceFilterSection
                defaultOpen
                options={browseSourceOptions}
                selected={draftFilters.sourceMaterials}
                title="来源"
                onToggle={(value) => setDraftFilters((current) => ({ ...current, sourceMaterials: toggleBrowseSingleValue(current.sourceMaterials, value) }))}
              />
              <BrowseChoiceFilterSection
                defaultOpen
                options={browseGenreOptions}
                selected={draftFilters.genres}
                title="题材"
                onToggle={(value) => setDraftFilters((current) => ({ ...current, genres: toggleBrowseValue(current.genres, value) }))}
              />
              <BrowseChoiceFilterSection
                options={browseDemographicOptions}
                selected={draftFilters.demographics}
                title="受众"
                onToggle={(value) => setDraftFilters((current) => ({ ...current, demographics: toggleBrowseSingleValue(current.demographics, value) }))}
              />
              <BrowseChoiceFilterSection
                options={browseRegionOptions}
                selected={draftFilters.regions}
                title="地区"
                onToggle={(value) => setDraftFilters((current) => ({ ...current, regions: toggleBrowseSingleValue(current.regions, value) }))}
              />
              <BrowseChoiceFilterSection
                options={timeOptions}
                selected={selectedDraftTime ? [selectedDraftTime] : []}
                title="时间"
                onToggle={(value) => setDraftFilters((current) => ({
                  ...current,
                  ...toggleBrowseTime(current, value, currentYear)
                }))}
              />
              <BrowseFilterSection title="最低评分">
                <div className="mb-3 flex items-center justify-between text-xs text-muted-foreground">
                  <span>不限</span>
                  <span className="font-semibold tabular-nums text-foreground">
                    {draftFilters.minRating > 0 ? `${draftFilters.minRating.toFixed(1)} 分及以上` : "不限"}
                  </span>
                </div>
                <Slider
                  aria-label="最低评分"
                  aria-valuetext={draftFilters.minRating > 0 ? `${draftFilters.minRating.toFixed(1)} 分及以上` : "不限"}
                  max={10}
                  min={0}
                  step={0.5}
                  value={[draftFilters.minRating]}
                  onValueChange={([value]) => setDraftFilters((current) => ({ ...current, minRating: value ?? 0 }))}
                />
              </BrowseFilterSection>
            </div>
          </ScrollArea>
          <SheetFooter className="grid shrink-0 grid-cols-[7rem_1fr] border-t bg-background px-6 py-4">
            <Button onClick={() => setDraftFilters(createEmptyDiscoveryBrowseFilters())} variant="outline">重置</Button>
            <Button
              onClick={() => {
                setFilters(cloneBrowseFilters(draftFilters));
                setPage(1);
                setSheetOpen(false);
                console.info("[discovery-browse] Bangumi 筛选已应用", { selectedCount: draftSelectedCount });
              }}
            >
              应用筛选
            </Button>
          </SheetFooter>
        </SheetContent>
      </Sheet>
    </section>
  );
}

/** 渲染分类浏览中的紧凑海报卡片和图标式追番操作。 */
function DiscoveryBrowseAnimeCard({
  adding,
  anime,
  followed,
  onAdd,
  onOpenDetail
}: {
  adding: boolean;
  anime: Anime;
  followed: boolean;
  onAdd: (anime: Anime) => Promise<void>;
  onOpenDetail?: (animeId: string, previewAnime?: Anime) => void;
}) {
  const titleDisplay = resolveAnimeTitleDisplay(anime);
  const genres = anime.detail?.genres?.slice(0, 2) ?? [];
  const episodeStatus = anime.detail?.airingStatus === "airing"
    ? "连载中"
    : anime.detail?.episodeCount
      ? `${anime.detail.episodeCount} 集全`
      : "集数待定";
  const studio = anime.detail?.studios?.[0] ?? titleDisplay.subtitle ?? "制作信息待补全";

  return (
    <article className="group min-w-0 overflow-hidden rounded-lg border bg-card transition-colors hover:border-primary/40">
      <div className="relative aspect-[2/3] overflow-hidden bg-muted">
        <button
          aria-label={`查看${titleDisplay.title}详情`}
          className="block size-full text-left outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
          onClick={() => onOpenDetail?.(anime.id, anime)}
          type="button"
        >
          {anime.coverUrl ? (
            <CachedImage
              alt={titleDisplay.title}
              className="size-full object-cover transition-transform duration-300 group-hover:scale-105"
              loading="lazy"
              sourceUrl={anime.coverUrl}
            />
          ) : (
            <div className="grid size-full place-content-center text-muted-foreground"><ImageOff className="size-7" /></div>
          )}
          <div className="absolute inset-x-0 bottom-0 bg-foreground/75 p-3 text-background opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100">
            <div className="mb-2 flex flex-wrap gap-1">
              {genres.map((genre) => <span className="rounded-sm border border-background/20 bg-background/10 px-1.5 py-0.5 text-[10px]" key={genre}>{genre}</span>)}
            </div>
            <div className="flex items-center justify-between text-[11px]">
              <span>{episodeStatus}</span>
              {anime.rating && <span className="flex items-center gap-1"><Star className="size-3 fill-current" />{anime.rating.score.toFixed(1)}</span>}
            </div>
          </div>
        </button>
        {followed && <Badge className="absolute left-2 top-2" tone="green">追番中</Badge>}
        <Button
          aria-label={followed ? `${titleDisplay.title}已追番` : `添加${titleDisplay.title}到追番`}
          className="absolute right-2 top-2 rounded-full bg-background/90 shadow-sm"
          disabled={followed || adding}
          onClick={() => void onAdd(anime)}
          size="icon"
          title={followed ? "已追番" : adding ? "添加中" : "添加追番"}
          variant="outline"
        >
          {adding ? <LoaderCircle className="animate-spin" /> : followed ? <Check className="text-emerald-600" /> : <Plus />}
        </Button>
      </div>
      <button className="block w-full min-w-0 p-3 text-left" onClick={() => onOpenDetail?.(anime.id, anime)} type="button">
        <h3 className="truncate text-sm font-semibold" title={titleDisplay.title}>{titleDisplay.title}</h3>
        <p className="mt-1 truncate text-[11px] text-muted-foreground" title={`${studio} · ${anime.premiereYear}`}>
          {studio} · {anime.premiereYear}
        </p>
      </button>
    </article>
  );
}

/** 渲染紧凑分页，并在页数较多时保留当前页与末页。 */
function DiscoveryBrowsePagination({
  page,
  totalPages,
  onPageChange
}: {
  page: number;
  totalPages: number;
  onPageChange: (page: number) => void;
}) {
  const pages = buildBrowsePaginationPages(page, totalPages);
  return (
    <Pagination className="mt-8">
      <PaginationContent>
        <PaginationItem><PaginationPrevious disabled={page <= 1} onClick={() => onPageChange(Math.max(1, page - 1))} /></PaginationItem>
        {pages.map((item, index) => item === "ellipsis" ? (
          <PaginationItem key={`ellipsis-${index}`}><PaginationEllipsis /></PaginationItem>
        ) : (
          <PaginationItem key={item}>
            <PaginationLink isActive={item === page} onClick={() => onPageChange(item)}>{item}</PaginationLink>
          </PaginationItem>
        ))}
        <PaginationItem><PaginationNext disabled={page >= totalPages} onClick={() => onPageChange(Math.min(totalPages, page + 1))} /></PaginationItem>
      </PaginationContent>
    </Pagination>
  );
}

function BrowseFilterSection({
  children,
  defaultOpen = false,
  title
}: {
  children: ReactNode;
  defaultOpen?: boolean;
  title: string;
}) {
  return (
    <Collapsible className="border-b" defaultOpen={defaultOpen}>
      <CollapsibleTrigger asChild>
        <button className="group flex w-full items-center justify-between py-4 text-left text-sm font-semibold" type="button">
          {title}
          <ChevronDown className="size-4 text-muted-foreground transition-transform group-data-[state=open]:rotate-180" />
        </button>
      </CollapsibleTrigger>
      <CollapsibleContent className="pb-4">{children}</CollapsibleContent>
    </Collapsible>
  );
}

/** 渲染 Sheet 中一组可多选的筛选按钮。 */
function BrowseChoiceFilterSection<T extends string | number>({
  defaultOpen = false,
  options,
  selected,
  title,
  onToggle
}: {
  defaultOpen?: boolean;
  options: readonly BrowseFilterOption<T>[];
  selected: T[];
  title: string;
  onToggle: (value: T) => void;
}) {
  return (
    <BrowseFilterSection defaultOpen={defaultOpen} title={title}>
      <div className="flex flex-wrap gap-2">
        {options.map((option) => {
          const active = selected.includes(option.value);
          return (
            <Button
              aria-pressed={active}
              className="h-9 min-h-9"
              key={option.value}
              onClick={() => onToggle(option.value)}
              variant={active ? "secondary" : "outline"}
            >
              {option.label}
            </Button>
          );
        })}
      </div>
    </BrowseFilterSection>
  );
}

type BrowseArrayFilterKey = Exclude<keyof DiscoveryBrowseFilters, "minRating" | "yearRange">;
type BrowseFilterKey = keyof DiscoveryBrowseFilters;

interface BrowseAppliedBadge {
  field: BrowseFilterKey;
  key: string;
  label: string;
  value: string | number;
}

function buildBrowseFilterBadges(filters: DiscoveryBrowseFilters): BrowseAppliedBadge[] {
  const badges: BrowseAppliedBadge[] = [];
  appendBrowseBadges(badges, "formats", filters.formats, browseFormatOptions);
  appendBrowseBadges(badges, "sourceMaterials", filters.sourceMaterials, browseSourceOptions);
  appendBrowseBadges(badges, "genres", filters.genres, browseGenreOptions);
  appendBrowseBadges(badges, "demographics", filters.demographics, browseDemographicOptions);
  appendBrowseBadges(badges, "regions", filters.regions, browseRegionOptions);
  for (const year of filters.years) badges.push({ field: "years", key: `year-${year}`, label: `${year}年`, value: year });
  if (filters.yearRange) {
    badges.push({
      field: "yearRange",
      key: `year-range-${filters.yearRange.kind}`,
      label: filters.yearRange.kind === "future" ? "未来年份" : "更早年份",
      value: filters.yearRange.kind
    });
  }
  if (filters.minRating > 0) badges.push({ field: "minRating", key: "rating", label: `评分 ≥ ${filters.minRating.toFixed(1)}`, value: filters.minRating });
  return badges;
}

function appendBrowseBadges<T extends string | number>(
  target: BrowseAppliedBadge[],
  field: BrowseArrayFilterKey,
  values: T[],
  options: readonly BrowseFilterOption<T>[]
) {
  for (const value of values) {
    const option = options.find((item) => item.value === value);
    target.push({ field, key: `${field}-${value}`, label: option?.badgeLabel ?? option?.label ?? String(value), value });
  }
}

/** 复制筛选数组，避免 Sheet 草稿修改已应用状态。 */
function cloneBrowseFilters(filters: DiscoveryBrowseFilters): DiscoveryBrowseFilters {
  return {
    formats: [...filters.formats],
    sourceMaterials: [...filters.sourceMaterials],
    genres: [...filters.genres],
    demographics: [...filters.demographics],
    regions: [...filters.regions],
    airingStatuses: [...filters.airingStatuses],
    years: [...filters.years],
    yearRange: filters.yearRange ? { ...filters.yearRange } : null,
    minRating: filters.minRating
  };
}

type BrowseTimeValue = "future" | "earlier" | `year:${number}`;

/** 将已保存的年份条件转换为筛选按钮使用的稳定值。 */
function getSelectedBrowseTimeValue(filters: DiscoveryBrowseFilters): BrowseTimeValue | undefined {
  const year = filters.years[0];
  if (year !== undefined) return `year:${year}`;
  return filters.yearRange?.kind;
}

/** 切换单一时间条件，并保证具体年份与范围不会同时生效。 */
function toggleBrowseTime(
  filters: DiscoveryBrowseFilters,
  value: BrowseTimeValue,
  currentYear: number
): Pick<DiscoveryBrowseFilters, "years" | "yearRange"> {
  if (getSelectedBrowseTimeValue(filters) === value) return { years: [], yearRange: null };
  if (value === "future") {
    return { years: [], yearRange: { kind: "future", startYear: currentYear + 1 } };
  }
  if (value === "earlier") {
    return { years: [], yearRange: { kind: "earlier", endYear: currentYear - 9 } };
  }
  return { years: [Number(value.slice("year:".length))], yearRange: null };
}

function toggleBrowseValue<T extends string | number>(values: T[], value: T): T[] {
  return values.includes(value) ? values.filter((item) => item !== value) : [...values, value];
}

/** 单选筛选再次点击时清空当前值。 */
function toggleBrowseSingleValue<T extends string | number>(values: T[], value: T): T[] {
  return values.includes(value) ? [] : [value];
}

function buildBrowsePaginationPages(page: number, totalPages: number): Array<number | "ellipsis"> {
  if (totalPages <= 5) return Array.from({ length: totalPages }, (_, index) => index + 1);
  const pages = new Set([1, totalPages, page - 1, page, page + 1]);
  const sorted = [...pages].filter((item) => item >= 1 && item <= totalPages).sort((left, right) => left - right);
  const result: Array<number | "ellipsis"> = [];
  sorted.forEach((item, index) => {
    if (index > 0 && item - sorted[index - 1] > 1) result.push("ellipsis");
    result.push(item);
  });
  return result;
}

const weekdayOptions = [
  { day: 1, label: "周一" },
  { day: 2, label: "周二" },
  { day: 3, label: "周三" },
  { day: 4, label: "周四" },
  { day: 5, label: "周五" },
  { day: 6, label: "周六" },
  { day: 0, label: "周日" }
] as const;

/** 按首播日期将季度目录组织为 Stitch 周视图。 */
function DiscoverySchedule({
  addingAnimeId,
  followedIds,
  items,
  onAdd,
  onOpenDetail
}: {
  addingAnimeId: string | null;
  followedIds: Set<string>;
  items: Anime[];
  onAdd: (anime: Anime) => Promise<void>;
  onOpenDetail?: (animeId: string) => void;
}) {
  const schedule = weekdayOptions.map((weekday) => ({
    ...weekday,
    items: items.filter((anime) => getAnimeWeekday(anime) === weekday.day)
  }));
  const undatedItems = items.filter((anime) => getAnimeWeekday(anime) === null);
  const todayWeekday = new Date().getDay();

  return (
    <div className="min-w-0">
      <div className="overflow-x-auto pb-2">
        <div className="grid min-w-[1040px] grid-cols-7 divide-x border-y">
          {schedule.map((weekday) => (
            <section
              className={cn("min-w-0 px-2 pb-4", weekday.day === todayWeekday && "bg-primary/5")}
              key={weekday.day}
            >
              <div className="sticky top-0 border-b bg-inherit py-3 text-xs font-semibold uppercase">
                {weekday.label}
                {weekday.day === todayWeekday && <span className="ml-1 text-primary">（今天）</span>}
              </div>
              <div className="mt-2 flex flex-col gap-2">
                {weekday.items.map((anime) => (
                  <DiscoveryScheduleItem
                    adding={addingAnimeId === anime.id}
                    anime={anime}
                    followed={followedIds.has(anime.id)}
                    key={anime.id}
                    onAdd={onAdd}
                    onOpenDetail={onOpenDetail}
                  />
                ))}
                {weekday.items.length === 0 && <p className="py-4 text-center text-xs text-muted-foreground">暂无首播</p>}
              </div>
            </section>
          ))}
        </div>
      </div>

      {undatedItems.length > 0 && (
        <section className="mt-5 border-t pt-4">
          <h2 className="text-sm font-semibold">首播日期待定 · {undatedItems.length}</h2>
          <div className="mt-3 grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
            {undatedItems.map((anime) => (
              <DiscoveryScheduleItem
                adding={addingAnimeId === anime.id}
                anime={anime}
                followed={followedIds.has(anime.id)}
                key={anime.id}
                onAdd={onAdd}
                onOpenDetail={onOpenDetail}
              />
            ))}
          </div>
        </section>
      )}
    </div>
  );
}

/** 按 Stitch 列表视图展示今天星期对应的番剧。 */
function DiscoveryScheduleList({
  addingAnimeId,
  followedIds,
  items,
  onAdd,
  onOpenDetail
}: {
  addingAnimeId: string | null;
  followedIds: Set<string>;
  items: Anime[];
  onAdd: (anime: Anime) => Promise<void>;
  onOpenDetail?: (animeId: string) => void;
}) {
  if (items.length === 0) {
    return (
      <Empty>
        <EmptyHeader>
          <EmptyMedia variant="icon"><CalendarDays /></EmptyMedia>
          <EmptyTitle>今天暂无番剧放送</EmptyTitle>
          <EmptyDescription>当前季度没有安排在今天首播的番剧。</EmptyDescription>
        </EmptyHeader>
      </Empty>
    );
  }

  return (
    <div className="min-w-0 overflow-x-auto">
      <div className="min-w-[760px] border-y">
        <div className="grid grid-cols-[4.5rem_minmax(17rem,1.7fr)_7rem_7rem_minmax(11rem,1fr)_6rem] gap-4 border-b bg-muted/40 px-3 py-2 text-xs font-semibold uppercase text-muted-foreground">
          <span>海报</span><span>标题与信息</span><span>放送</span><span>状态</span><span>资源</span><span className="text-right">操作</span>
        </div>
        <div className="divide-y">
          {items.map((anime) => {
            const titleDisplay = resolveAnimeTitleDisplay(anime);
            const followed = followedIds.has(anime.id);
            const metadata = anime.detail;
            const detail = [metadata?.genres?.slice(0, 2).join("、"), metadata?.studios?.[0]].filter(Boolean).join(" · ");
            return (
              <article
                className="grid min-h-28 grid-cols-[4.5rem_minmax(17rem,1.7fr)_7rem_7rem_minmax(11rem,1fr)_6rem] items-center gap-4 px-3 py-3"
                key={anime.id}
              >
                <button className="aspect-[2/3] overflow-hidden border bg-muted" onClick={() => onOpenDetail?.(anime.id)} type="button">
                  {anime.coverUrl ? <CachedImage alt={titleDisplay.title} className="size-full object-cover" sourceUrl={anime.coverUrl} /> : <ImageOff className="m-auto" />}
                </button>
                <button className="min-w-0 text-left" onClick={() => onOpenDetail?.(anime.id)} type="button">
                  <h3 className="line-clamp-2 text-sm font-semibold">{titleDisplay.title}{titleDisplay.subtitle ? `（${titleDisplay.subtitle}）` : ""}</h3>
                  <p className="mt-1 truncate text-xs text-muted-foreground">{detail || `${anime.premiereYear} ${seasonText[anime.season ?? "winter"]}`}</p>
                </button>
                <span className="text-xs font-medium text-primary">{formatBroadcastTime(anime)}</span>
                <Badge className="w-fit" tone={followed ? "primary-soft" : "neutral"}>{followed ? "追番中" : "待关注"}</Badge>
                <div className="min-w-0 text-xs">
                  <div className="truncate font-medium">{followed ? "可搜索最新资源" : "等待追番后匹配"}</div>
                  <div className="mt-1 truncate text-muted-foreground">{formatScheduleDate(anime)}</div>
                </div>
                <div className="flex justify-end gap-1">
                  <Button aria-label={`搜索${titleDisplay.title}资源`} className="size-9 p-0" onClick={() => onOpenDetail?.(anime.id)} title="查看资源" variant="ghost"><Download /></Button>
                  <Button
                    aria-label={followed ? `${titleDisplay.title}已追番` : `添加${titleDisplay.title}到追番`}
                    className="size-9 p-0"
                    disabled={followed || addingAnimeId === anime.id}
                    onClick={() => void onAdd(anime)}
                    title={followed ? "已追番" : "添加追番"}
                    variant="ghost"
                  >
                    {followed ? <Info /> : <Plus />}
                  </Button>
                </div>
              </article>
            );
          })}
        </div>
      </div>
    </div>
  );
}

/** 渲染时间表中的紧凑新番条目。 */
function DiscoveryScheduleItem({
  adding,
  anime,
  followed,
  onAdd,
  onOpenDetail
}: {
  adding: boolean;
  anime: Anime;
  followed: boolean;
  onAdd: (anime: Anime) => Promise<void>;
  onOpenDetail?: (animeId: string) => void;
}) {
  const titleDisplay = resolveAnimeTitleDisplay(anime);
  return (
    <article className={cn("flex min-w-0 items-start gap-2 bg-background p-3", followed && "border-l-2 border-primary")}>
      <button
        className="min-w-0 flex-1 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
        onClick={() => onOpenDetail?.(anime.id)}
        type="button"
      >
        <div className="text-xs font-medium text-primary">{formatBroadcastTime(anime)}</div>
        <h3 className="mt-1 line-clamp-2 text-sm font-semibold">{titleDisplay.title}</h3>
        <p className="mt-1 truncate text-xs text-muted-foreground">{followed ? "追番中" : titleDisplay.subtitle ?? formatScheduleDate(anime)}</p>
      </button>
      <Button
        aria-label={followed ? `${titleDisplay.title}已追番` : `添加${titleDisplay.title}到追番`}
        className="size-11 shrink-0 p-0 md:size-9"
        disabled={followed || adding}
        onClick={() => void onAdd(anime)}
        title={followed ? "已追番" : "添加追番"}
        variant={followed ? "secondary" : "outline"}
      >
        {followed ? <Star /> : <Search />}
      </Button>
    </article>
  );
}

/** 返回新番首播日期对应的星期索引。 */
function getAnimeWeekday(anime: Anime): number | null {
  if (anime.detail?.broadcast?.weekday !== undefined) return anime.detail.broadcast.weekday;
  if (!anime.premiereDate) return null;
  const date = new Date(`${anime.premiereDate.slice(0, 10)}T00:00:00`);
  return Number.isNaN(date.getTime()) ? null : date.getDay();
}

/** 格式化番剧的常规放送时间。 */
function formatBroadcastTime(anime: Anime): string {
  const time = anime.detail?.broadcast?.time;
  const timezone = anime.detail?.broadcast?.timezone;
  return time ? `${time}${timezone ? ` ${timezone}` : ""}` : formatScheduleDate(anime);
}

/** 格式化时间表右上角的今天日期。 */
function formatTodayLabel(date: Date): string {
  return new Intl.DateTimeFormat("zh-CN", {
    month: "long",
    day: "numeric",
    weekday: "long"
  }).format(date);
}

/** 格式化时间表条目的首播日期。 */
function formatScheduleDate(anime: Anime): string {
  const parts = anime.premiereDate?.match(/^\d{4}-(\d{2})-(\d{2})/);
  return parts ? `${Number(parts[1])} 月 ${Number(parts[2])} 日` : "日期待定";
}

const externalIdText: Record<string, string> = {
  bangumi: "Bangumi",
  anilist: "AniList",
  mikan: "Mikan",
  mal: "MAL"
};

const externalIdOrder = ["bangumi", "anilist", "mikan", "mal"];

interface ExternalIdBadge {
  key: string;
  label: string;
  value: string;
  url?: string;
}

/** Resolves the current date to its calendar anime season. */
function getCurrentSeasonTarget(): SeasonTarget {
  const date = new Date();
  return {
    year: date.getFullYear(),
    season: getSeasonByMonth(date.getMonth() + 1)
  };
}

/** Finds the season containing the supplied month. */
function getSeasonByMonth(month: number): Season {
  if (month <= 3) return "winter";
  if (month <= 6) return "spring";
  if (month <= 9) return "summer";
  return "fall";
}

/** Returns display metadata for a season identifier. */
function getSeasonOption(season: Season): SeasonOption {
  return seasonOptions.find((option) => option.value === season) ?? seasonOptions[0];
}

/** Merges monthly catalogs and removes duplicates. */
function mergeAnimeItems(items: Anime[]): Anime[] {
  const uniqueItems = new Map(items.map((anime) => [anime.id, anime]));
  return Array.from(uniqueItems.values());
}

/** Filters a seasonal catalog by month and normalized title text. */
function filterAnimeItems(items: Anime[], month: number | null, keyword: string): Anime[] {
  const normalizedKeyword = keyword.trim().toLocaleLowerCase();
  return items.filter((anime) => {
    if (month !== null && anime.premiereMonth !== month) {
      return false;
    }

    if (!normalizedKeyword) {
      return true;
    }

    const searchableTitles = [anime.title, anime.originalTitle, ...anime.aliases.map((alias) => alias.alias)];
    return searchableTitles.some((title) => title?.toLocaleLowerCase().includes(normalizedKeyword));
  });
}

/** Applies the selected catalog sort order after filtering. */
function sortAnimeItems(items: Anime[], sortKey: DiscoverySortKey): Anime[] {
  return [...items].sort((left, right) => {
    if (sortKey === "ratingDesc") {
      const leftScore = left.rating?.score;
      const rightScore = right.rating?.score;
      if (leftScore !== undefined || rightScore !== undefined) {
        if (leftScore === undefined) return 1;
        if (rightScore === undefined) return -1;
        if (leftScore !== rightScore) return rightScore - leftScore;
        if ((left.rating?.count ?? 0) !== (right.rating?.count ?? 0)) {
          return (right.rating?.count ?? 0) - (left.rating?.count ?? 0);
        }
      }
    }

    const direction = sortKey === "premiereDesc" ? -1 : 1;
    return direction * compareAnimePremiere(left, right) || left.title.localeCompare(right.title, "zh-CN");
  });
}

/** Compares two anime entries by the most precise premiere date available. */
function compareAnimePremiere(left: Anime, right: Anime): number {
  return getPremiereSortValue(left).localeCompare(getPremiereSortValue(right));
}

function getPremiereSortValue(anime: Anime): string {
  return anime.premiereDate ?? `${anime.premiereYear}-${String(anime.premiereMonth).padStart(2, "0")}-01`;
}

/** Formats the most precise available premiere date for a card. */
function formatPremiere(anime: Anime): string {
  const dateParts = anime.premiereDate?.match(/^\d{4}-(\d{2})-(\d{2})/);
  if (dateParts) {
    return `${Number(dateParts[1])} 月 ${Number(dateParts[2])} 日首播`;
  }
  return `${anime.premiereYear} 年 ${anime.premiereMonth} 月首播`;
}

/** Builds ordered metadata-source badges for an anime entry. */
function buildExternalIdBadges(anime: Anime): ExternalIdBadge[] {
  return Object.entries(anime.externalIds)
    .filter(([, value]) => Boolean(value))
    .sort(([left], [right]) => getExternalIdRank(left) - getExternalIdRank(right))
    .map(([key, value]) => ({
      key,
      label: externalIdText[key] ?? key,
      value,
      url: buildExternalIdUrl(key, value)
    }));
}

/** Returns the configured display order for one metadata source. */
function getExternalIdRank(key: string): number {
  const index = externalIdOrder.indexOf(key);
  return index >= 0 ? index : externalIdOrder.length;
}

/** Maps a metadata source identifier to its public detail page. */
function buildExternalIdUrl(key: string, value: string): string | undefined {
  if (key === "bangumi") {
    return `https://bgm.tv/subject/${encodeURIComponent(value)}`;
  }

  if (key === "anilist") {
    return `https://anilist.co/anime/${encodeURIComponent(value)}`;
  }

  if (key === "mikan") {
    return `https://mikanani.me/Home/Bangumi/${encodeURIComponent(value)}`;
  }

  if (key === "mal") {
    return `https://myanimelist.net/anime/${encodeURIComponent(value)}`;
  }

  return undefined;
}
