import {
  AlertCircle,
  ChevronDown,
  ChevronUp,
  Download,
  Link2,
  PackageSearch,
  Search
} from "lucide-react";
import { type FocusEvent as ReactFocusEvent, useEffect, useMemo, useState } from "react";
import { toast } from "@/lib/toast";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList
} from "@/components/ui/command";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty";
import { Field, FieldGroup, FieldLabel } from "@/components/ui/field";
import { InputGroup, InputGroupAddon, InputGroupButton } from "@/components/ui/input-group";
import {
  Pagination,
  PaginationContent,
  PaginationEllipsis,
  PaginationItem,
  PaginationLink,
  PaginationNext,
  PaginationPrevious
} from "@/components/ui/pagination";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Page, PageBreadcrumb, PageHeader } from "@/components/page-layout";
import { ReleaseMetadataBadges } from "@/components/release-metadata-badges";
import { appApi } from "@/lib/api";
import { formatBytes, formatDateTime } from "@/lib/format";
import { classifyAnimeRelease, isAnimeSearchTerm, matchesAnimeSearchKeyword } from "@shared/anime-release-search";
import { resolveAnimeTitleDisplay } from "@shared/anime-title";
import { parseReleaseSearchInput } from "@shared/release-search-input";
import {
  compareReleaseEpisodeDescending,
  dedupeReleasesByEpisodeContent
} from "@shared/release-identity";
import type { ReleaseSearchResult } from "@shared/contracts";
import type { MyAnime, Release } from "@shared/domain";

const MAX_ANIME_SUGGESTIONS = 10;
const RESULTS_PER_PAGE = 10;
const AGGREGATE_SOURCE_TAB = "aggregate";

type ReleaseSortKey = "match" | "published" | "seeders";

interface SearchedContext {
  mode: "anime" | "keyword";
  keyword: string;
  episodeNo?: number;
  myAnime?: MyAnime;
}

interface ReleaseSearchPageProps {
  initialIntent?: {
    keyword: string;
    key: number;
  } | null;
}

export function ReleaseSearchPage({ initialIntent }: ReleaseSearchPageProps = {}) {
  const [keyword, setKeyword] = useState("");
  const [myAnime, setMyAnime] = useState<MyAnime[]>([]);
  const [selectedAnimeId, setSelectedAnimeId] = useState("");
  const [suggestionsOpen, setSuggestionsOpen] = useState(false);
  const [loading, setLoading] = useState(false);
  const [result, setResult] = useState<ReleaseSearchResult | null>(null);
  const [searchedContext, setSearchedContext] = useState<SearchedContext | null>(null);
  const [addingId, setAddingId] = useState<string | null>(null);
  const [addedReleaseIds, setAddedReleaseIds] = useState<Set<string>>(new Set());
  const [message, setMessage] = useState<string | null>(null);
  const [sortKey, setSortKey] = useState<ReleaseSortKey>("match");
  const [page, setPage] = useState(1);
  const [errorsExpanded, setErrorsExpanded] = useState(false);
  const [activeSourceTab, setActiveSourceTab] = useState(AGGREGATE_SOURCE_TAB);

  const selectedAnime = useMemo(
    () => myAnime.find((item) => item.id === selectedAnimeId) ?? null,
    [myAnime, selectedAnimeId]
  );
  const parsedInput = useMemo(() => parseReleaseSearchInput(keyword), [keyword]);
  const suggestions = useMemo(
    () => parsedInput.keyword
      ? myAnime
          .filter((item) => matchesAnimeSearchKeyword(item.anime, parsedInput.keyword))
          .slice(0, MAX_ANIME_SUGGESTIONS)
      : [],
    [myAnime, parsedInput.keyword]
  );
  const sourceResults = useMemo(() => normalizeSourceResults(result), [result]);
  const sourceTabs = useMemo(
    () => result
      ? [
          {
            value: AGGREGATE_SOURCE_TAB,
            label: "聚合结果",
            count: result.releases.length,
            hasError: false
          },
          ...sourceResults.map((sourceResult) => ({
            value: createSourceTabValue(sourceResult.sourceId),
            label: sourceResult.sourceName,
            count: sourceResult.releases.length,
            hasError: result.errors.some((error) => error.sourceId === sourceResult.sourceId)
          }))
        ]
      : [],
    [result, sourceResults]
  );
  const activeSourceResult = sourceResults.find(
    (sourceResult) => createSourceTabValue(sourceResult.sourceId) === activeSourceTab
  );
  const activeReleases = activeSourceResult?.releases ?? result?.releases ?? [];
  const activeErrors = result
    ? activeSourceResult
      ? result.errors.filter((error) => error.sourceId === activeSourceResult.sourceId)
      : result.errors
    : [];
  const sortedReleases = useMemo(
    () => sortReleases(activeReleases, sortKey),
    [activeReleases, sortKey]
  );
  const pageCount = Math.max(1, Math.ceil(sortedReleases.length / RESULTS_PER_PAGE));
  const currentPage = Math.min(page, pageCount);
  const visibleReleases = sortedReleases.slice(
    (currentPage - 1) * RESULTS_PER_PAGE,
    currentPage * RESULTS_PER_PAGE
  );
  const pageItems = createPaginationItems(currentPage, pageCount);

  useEffect(() => {
    let active = true;

    appApi
      .listMyAnime()
      .then((items) => {
        if (active) setMyAnime(items);
      })
      .catch((error) => {
        if (active) setMessage(error instanceof Error ? error.message : "加载追番列表失败");
      });

    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    if (!initialIntent) return;
    setKeyword(initialIntent.keyword);
    setSelectedAnimeId("");
    setResult(null);
    setSearchedContext(null);
    void search(initialIntent.keyword, true);
  }, [initialIntent?.key]);

  /** 更新关键词，并在输入不再对应原番剧时取消关联。 */
  function updateKeyword(value: string) {
    const nextInput = parseReleaseSearchInput(value);
    setKeyword(value);
    if (selectedAnime && !isAnimeSearchTerm(selectedAnime.anime, nextInput.keyword)) {
      setSelectedAnimeId("");
    }
    setSuggestionsOpen(Boolean(nextInput.keyword));
  }

  /** 选择联想番剧，并保留输入中已识别的集数。 */
  function selectAnime(item: MyAnime) {
    const title = resolveAnimeTitleDisplay(item.anime).title;
    setSelectedAnimeId(item.id);
    setKeyword(parsedInput.episodeNo === undefined ? title : `${title} 第 ${parsedInput.episodeNo} 集`);
    setSuggestionsOpen(false);
  }

  /** 焦点离开输入框和候选列表后关闭联想内容。 */
  function closeSuggestionsOnBlur(event: ReactFocusEvent<HTMLDivElement>) {
    if (event.relatedTarget && event.currentTarget.contains(event.relatedTarget as Node)) return;
    window.setTimeout(() => setSuggestionsOpen(false), 0);
  }

  /** 根据是否关联追番选择番剧级搜索或普通关键词搜索。 */
  async function search(keywordOverride?: string, forceKeywordMode = false) {
    const input = parseReleaseSearchInput(keywordOverride ?? keyword);
    if (!input.keyword) {
      setMessage("请输入搜索关键词");
      return;
    }

    setLoading(true);
    setMessage(null);
    setSuggestionsOpen(false);

    try {
      const activeSelectedAnime = forceKeywordMode ? null : selectedAnime;
      const searchResult = activeSelectedAnime
        ? await appApi.searchAnimeReleases({
            animeId: activeSelectedAnime.anime.id,
            episodeNo: input.episodeNo,
            fansubGroupId: activeSelectedAnime.defaultFansubGroupId,
            preferredResolution: activeSelectedAnime.preferredResolution,
            limit: 80
          })
        : await appApi.searchReleases({
            keyword: input.keyword,
            episodeNo: input.episodeNo,
            limit: 80
          });

      setResult(searchResult);
      setSearchedContext({
        mode: activeSelectedAnime ? "anime" : "keyword",
        keyword: input.keyword,
        episodeNo: input.episodeNo,
        myAnime: activeSelectedAnime ?? undefined
      });
      setPage(1);
      setActiveSourceTab(AGGREGATE_SOURCE_TAB);
      setErrorsExpanded(searchResult.errors.length > 0 && searchResult.releases.length === 0);
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "资源搜索失败");
    } finally {
      setLoading(false);
    }
  }

  /** 切换聚合或来源结果，并重置该结果集的分页和异常展开状态。 */
  function changeSourceTab(value: string) {
    const nextSourceResult = sourceResults.find(
      (sourceResult) => createSourceTabValue(sourceResult.sourceId) === value
    );
    const nextErrors = nextSourceResult
      ? result?.errors.filter((error) => error.sourceId === nextSourceResult.sourceId) ?? []
      : result?.errors ?? [];
    const nextReleaseCount = nextSourceResult?.releases.length ?? result?.releases.length ?? 0;
    setActiveSourceTab(value);
    setPage(1);
    setErrorsExpanded(nextErrors.length > 0 && nextReleaseCount === 0);
  }

  /** 将当前搜索结果加入下载队列，并沿用执行搜索时的番剧和集数关联。 */
  async function addDownload(releaseId: string) {
    const release = findSearchRelease(result, releaseId);
    if (!release) return;

    const searchedAnime = searchedContext?.myAnime;
    const releaseForDownload: Release = {
      ...release,
      animeId: searchedAnime?.anime.id ?? release.animeId,
      episodeNo: searchedContext?.episodeNo ?? release.episodeNo,
      fansubGroupId: release.fansubGroupId ?? searchedAnime?.defaultFansubGroupId
    };

    setAddingId(releaseId);
    setMessage(null);
    try {
      await appApi.addReleaseDownload({
        release: releaseForDownload,
        animeId: releaseForDownload.animeId,
        episodeNo: releaseForDownload.episodeNo,
        fansubGroupId: releaseForDownload.fansubGroupId
      });
      setAddedReleaseIds((current) => new Set(current).add(releaseId));
      toast.success("已添加到下载队列");
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "添加下载失败");
    } finally {
      setAddingId(null);
    }
  }

  return (
    <Page aria-busy={loading}>
      <PageHeader className="border-b pb-3 sm:items-center">
        <h1 className="sr-only">资源搜索</h1>
        <PageBreadcrumb current="资源搜索" />
      </PageHeader>

      {message && (
        <Alert variant="destructive">
          <AlertCircle />
          <AlertTitle>操作未完成</AlertTitle>
          <AlertDescription>{message}</AlertDescription>
        </Alert>
      )}

      <section className="relative min-w-0">
        <FieldGroup className="gap-3">
          <Field className="min-w-0">
            <FieldLabel className="sr-only" htmlFor="release-keyword">资源搜索关键词</FieldLabel>
            <Command
              className="relative overflow-visible bg-transparent"
              shouldFilter={false}
              onBlur={closeSuggestionsOnBlur}
            >
              <InputGroup className="h-12 md:h-12">
                <InputGroupAddon className="pl-3 pr-0 text-muted-foreground">
                  <Search />
                </InputGroupAddon>
                <CommandInput
                  id="release-keyword"
                  aria-label="资源搜索关键词"
                  autoComplete="off"
                  placeholder="输入番剧名、关键词或集数，如：芙莉莲 EP12"
                  value={keyword}
                  onValueChange={updateKeyword}
                  onFocus={() => setSuggestionsOpen(Boolean(parsedInput.keyword))}
                  onKeyDown={(event) => {
                    if (event.key === "Escape") setSuggestionsOpen(false);
                    if (event.key === "Enter" && (!suggestionsOpen || suggestions.length === 0)) void search();
                  }}
                />
                <InputGroupAddon>
                  <InputGroupButton
                    className="min-h-10 bg-primary px-5 text-primary-foreground hover:bg-primary/90"
                    onClick={() => void search()}
                    disabled={loading || !keyword.trim()}
                  >
                    {loading ? "搜索中" : "搜索"}
                  </InputGroupButton>
                </InputGroupAddon>
              </InputGroup>

              {suggestionsOpen && !selectedAnime && Boolean(parsedInput.keyword) && (
                <CommandList
                  variant="suggestions"
                  className="absolute left-0 right-0 top-full mt-2 max-h-[min(26rem,50vh)]"
                >
                  <CommandEmpty>我的追番中没有匹配项</CommandEmpty>
                  {suggestions.length > 0 && (
                    <CommandGroup heading={`我的追番（${suggestions.length}）`}>
                      {suggestions.map((item) => {
                        const titleDisplay = resolveAnimeTitleDisplay(item.anime);
                        return (
                          <CommandItem key={item.id} value={item.id} onSelect={() => selectAnime(item)}>
                            <div className="min-w-0">
                              <div className="truncate font-medium">{titleDisplay.title}</div>
                              <div className="truncate text-xs text-muted-foreground">
                                {titleDisplay.subtitle ?? `默认字幕组：${item.defaultFansubGroupId ?? "未设置"}`}
                              </div>
                            </div>
                          </CommandItem>
                        );
                      })}
                    </CommandGroup>
                  )}
                </CommandList>
              )}
            </Command>
          </Field>

          {(selectedAnime || parsedInput.episodeNo !== undefined) && (
            <div className="flex flex-wrap gap-2">
              {selectedAnime && (
                <Badge tone="primary">
                  <Link2 className="mr-1 size-3" />
                  已关联：《{resolveAnimeTitleDisplay(selectedAnime.anime).title}》
                </Badge>
              )}
              {parsedInput.episodeNo !== undefined && <Badge>第 {parsedInput.episodeNo} 集</Badge>}
            </div>
          )}
        </FieldGroup>
      </section>

      {loading && !result && <ReleaseSearchSkeleton />}

      {!loading && !result && (
        <Empty className="min-h-72">
          <EmptyHeader>
            <EmptyMedia variant="icon"><PackageSearch /></EmptyMedia>
            <EmptyTitle>等待搜索资源</EmptyTitle>
            <EmptyDescription>选择追番或输入关键词后，即可搜索已启用的下载源。</EmptyDescription>
          </EmptyHeader>
        </Empty>
      )}

      {result && (
        <section className="flex min-w-0 flex-col gap-4">
          <div className="flex min-w-0 flex-col gap-3 border-b-2 border-foreground pb-3 sm:flex-row sm:items-end sm:justify-between">
            <div className="min-w-0">
              {searchedContext && (
                <div className="break-words text-sm font-semibold">
                  {searchedContext.mode === "anime"
                    ? `按《${resolveAnimeTitleDisplay(searchedContext.myAnime!.anime).title}》搜索`
                    : `关键词搜索：${searchedContext.keyword}`}
                  {searchedContext.episodeNo !== undefined ? ` · 第 ${searchedContext.episodeNo} 集` : ""}
                </div>
              )}
              <div className="mt-1 flex flex-wrap gap-x-3 gap-y-1 text-xs text-muted-foreground">
                <span>已搜索 {result.searchedSourceIds.length} 个下载源</span>
                <span>找到 {result.releases.length} 条资源</span>
                {result.errors.length > 0 && <span className="text-destructive">{result.errors.length} 个源异常</span>}
              </div>
            </div>
            <Field className="w-full sm:w-40">
              <FieldLabel className="sr-only" htmlFor="release-sort">排序方式</FieldLabel>
              <Select
                value={sortKey}
                onValueChange={(value) => {
                  setSortKey(value as ReleaseSortKey);
                  setPage(1);
                }}
              >
                <SelectTrigger id="release-sort" variant="ghost">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    <SelectItem value="match">匹配优先</SelectItem>
                    <SelectItem value="published">最新发布</SelectItem>
                    <SelectItem value="seeders">做种最多</SelectItem>
                  </SelectGroup>
                </SelectContent>
              </Select>
            </Field>
          </div>

          <Tabs className="min-w-0 gap-0" value={activeSourceTab} onValueChange={changeSourceTab}>
            <div className="min-w-0 overflow-x-auto">
              <TabsList className="w-max min-w-full" variant="line">
                {sourceTabs.map((tab) => (
                  <TabsTrigger className="shrink-0 gap-2" key={tab.value} value={tab.value}>
                    <span>{tab.label}</span>
                    {tab.hasError && <Badge tone="red" aria-label="来源异常">!</Badge>}
                    <Badge>{tab.count}</Badge>
                  </TabsTrigger>
                ))}
              </TabsList>
            </div>

            {sourceTabs.map((tab) => (
              <TabsContent className="mt-0 flex min-w-0 flex-col gap-4 pt-4" key={tab.value} value={tab.value}>
                {activeSourceTab === tab.value && (
                  <>
                    {activeErrors.length > 0 && (
                      <Alert variant={sortedReleases.length === 0 ? "destructive" : "default"}>
                        <AlertCircle />
                        <AlertTitle className="flex min-w-0 items-center justify-between gap-3">
                          <span>
                            {activeSourceResult
                              ? `${activeSourceResult.sourceName}${sortedReleases.length === 0 ? "搜索失败" : "搜索异常"}`
                              : sortedReleases.length === 0 ? "下载源搜索失败" : "部分下载源异常"}
                          </span>
                          <Button
                            className="size-9 p-0"
                            variant="ghost"
                            aria-expanded={errorsExpanded}
                            aria-label={errorsExpanded ? "收起来源异常" : "展开来源异常"}
                            onClick={() => setErrorsExpanded((current) => !current)}
                          >
                            {errorsExpanded ? <ChevronUp /> : <ChevronDown />}
                          </Button>
                        </AlertTitle>
                        <AlertDescription>
                          {errorsExpanded ? (
                            <ul className="mt-2 flex flex-col gap-2">
                              {activeErrors.map((error, index) => (
                                <li className="break-words" key={`${error.sourceId}-${index}`}>
                                  <span className="font-medium">
                                    {resolveSourceName(sourceResults, error.sourceId)}：
                                  </span>
                                  {error.message}
                                </li>
                              ))}
                            </ul>
                          ) : activeSourceResult
                            ? "该来源的已有结果仍然可用。"
                            : "其余下载源的搜索结果仍然可用。"}
                        </AlertDescription>
                      </Alert>
                    )}

                    {visibleReleases.length > 0 ? (
                      <div className="min-w-0 divide-y bg-card">
                        {visibleReleases.map((release) => {
                          const added = addedReleaseIds.has(release.id);
                          const seasonCompatible = !searchedContext?.myAnime
                            || classifyAnimeRelease(release, searchedContext.myAnime.anime) === "current";
                          return (
                            <article
                              className="flex min-w-0 flex-col gap-3 p-4 transition-colors hover:bg-muted/50 sm:flex-row sm:items-center sm:justify-between"
                              key={release.id}
                            >
                              <div className="min-w-0 flex-1">
                                <h2 className="break-words text-sm font-semibold leading-5" title={release.title}>
                                  {release.title}
                                </h2>
                                <div className="mt-2 flex flex-wrap gap-1.5">
                                  <Badge className="max-w-full truncate" tone="blue">{release.sourceName}</Badge>
                                  {(release.fansubName ?? release.fansubGroupId) && (
                                    <Badge className="max-w-full truncate">
                                      {release.fansubName ?? release.fansubGroupId}
                                    </Badge>
                                  )}
                                  {release.episodeNo !== undefined && <Badge>第 {release.episodeNo} 集</Badge>}
                                  {!seasonCompatible && <Badge tone="amber">季度待确认</Badge>}
                                  <ReleaseMetadataBadges metadata={release} />
                                  {release.size && <Badge>{formatBytes(release.size)}</Badge>}
                                  {typeof release.seeders === "number" && (
                                    <Badge tone={release.seeders > 0 ? "green" : "neutral"}>
                                      {release.seeders} 做种
                                    </Badge>
                                  )}
                                </div>
                                <div className="mt-2 flex flex-wrap gap-x-3 gap-y-1 text-xs text-muted-foreground">
                                  <span>发布于 {formatDateTime(release.publishedAt)}</span>
                                  {release.infoHash && (
                                    <span title={release.infoHash}>Hash {release.infoHash.slice(0, 12)}</span>
                                  )}
                                  {(release.magnetUrl || release.torrentUrl) && (
                                    <span>{release.magnetUrl ? "磁力链接" : "Torrent 文件"}</span>
                                  )}
                                </div>
                              </div>
                              <Button
                                className="w-full shrink-0 sm:w-auto"
                                variant={added || !seasonCompatible ? "secondary" : "primary"}
                                onClick={() => void addDownload(release.id)}
                                disabled={addingId === release.id || added || !seasonCompatible}
                              >
                                <Download data-icon="inline-start" />
                                {!seasonCompatible ? "季度待确认" : addingId === release.id ? "添加中" : added ? "已加入" : "添加下载"}
                              </Button>
                            </article>
                          );
                        })}
                      </div>
                    ) : (
                      <Empty className="min-h-64">
                        <EmptyHeader>
                          <EmptyMedia variant="icon"><PackageSearch /></EmptyMedia>
                          <EmptyTitle>
                            {activeSourceResult
                              ? activeErrors.length > 0
                                ? `${activeSourceResult.sourceName} 未返回资源`
                                : `${activeSourceResult.sourceName} 没有结果`
                              : activeErrors.length > 0 ? "所有来源均未返回资源" : "没有找到资源"}
                          </EmptyTitle>
                          <EmptyDescription>
                            {activeErrors.length > 0
                              ? "请查看来源异常原因，或稍后重新搜索。"
                              : "请检查下载源是否启用，或换用日文名、罗马音再次搜索。"}
                          </EmptyDescription>
                        </EmptyHeader>
                      </Empty>
                    )}

                    {sortedReleases.length > RESULTS_PER_PAGE && (
                      <div className="flex flex-col gap-3 text-xs text-muted-foreground sm:flex-row sm:items-center sm:justify-between">
                        <span>
                          显示 {(currentPage - 1) * RESULTS_PER_PAGE + 1}-
                          {Math.min(currentPage * RESULTS_PER_PAGE, sortedReleases.length)} / 共 {sortedReleases.length} 条
                        </span>
                        <Pagination className="w-auto justify-start sm:justify-end">
                          <PaginationContent>
                            <PaginationItem>
                              <PaginationPrevious
                                disabled={currentPage === 1}
                                onClick={() => setPage((value) => Math.max(1, value - 1))}
                              />
                            </PaginationItem>
                            {pageItems.map((item) => (
                              <PaginationItem key={String(item)}>
                                {typeof item === "number" ? (
                                  <PaginationLink isActive={item === currentPage} onClick={() => setPage(item)}>
                                    {item}
                                  </PaginationLink>
                                ) : (
                                  <PaginationEllipsis />
                                )}
                              </PaginationItem>
                            ))}
                            <PaginationItem>
                              <PaginationNext
                                disabled={currentPage === pageCount}
                                onClick={() => setPage((value) => Math.min(pageCount, value + 1))}
                              />
                            </PaginationItem>
                          </PaginationContent>
                        </Pagination>
                      </div>
                    )}
                  </>
                )}
              </TabsContent>
            ))}
          </Tabs>
        </section>
      )}
    </Page>
  );
}

/** 兼容旧查询缓存，并为所有已搜索来源生成可切换结果集。 */
function normalizeSourceResults(result: ReleaseSearchResult | null): ReleaseSearchResult["sourceResults"] {
  if (!result) return [];
  if (Array.isArray(result.sourceResults)) return result.sourceResults;

  return result.searchedSourceIds.map((sourceId) => ({
    sourceId,
    sourceName: result.releases.find((release) => release.sourceId === sourceId)?.sourceName ?? sourceId,
    releases: result.releases.filter((release) => release.sourceId === sourceId)
  }));
}

/** 为来源 Tab 生成不会与聚合 Tab 冲突的稳定值。 */
function createSourceTabValue(sourceId: string): string {
  return `source:${sourceId}`;
}

/** 在聚合与来源结果中定位资源，支持下载聚合去重后未保留的来源条目。 */
function findSearchRelease(result: ReleaseSearchResult | null, releaseId: string): Release | undefined {
  if (!result) return undefined;
  return result.releases.find((release) => release.id === releaseId) ??
    normalizeSourceResults(result)
      .flatMap((sourceResult) => sourceResult.releases)
      .find((release) => release.id === releaseId);
}

/** 将来源错误中的标识转换为用户配置的来源名称。 */
function resolveSourceName(
  sourceResults: ReleaseSearchResult["sourceResults"],
  sourceId: string
): string {
  return sourceResults.find((sourceResult) => sourceResult.sourceId === sourceId)?.sourceName ?? sourceId;
}

/** 渲染搜索进行中的稳定列表骨架。 */
function ReleaseSearchSkeleton() {
  return (
    <div className="divide-y bg-card" aria-label="正在搜索资源">
      {Array.from({ length: 4 }, (_, index) => (
        <div className="flex flex-col gap-3 p-4" key={index}>
          <Skeleton className="h-5 w-4/5" />
          <Skeleton className="h-6 w-2/3" />
        </div>
      ))}
    </div>
  );
}

/** 先按集数倒序，再按用户选择的规则排列同集资源。 */
function sortReleases(releases: Release[], sortKey: ReleaseSortKey): Release[] {
  return dedupeReleasesByEpisodeContent(releases).sort((left, right) => {
    const episodeDelta = compareReleaseEpisodeDescending(left, right);
    if (episodeDelta) return episodeDelta;
    if (sortKey === "match") return 0;
    if (sortKey === "seeders") return (right.seeders ?? -1) - (left.seeders ?? -1);
    return comparePublishedAt(right.publishedAt, left.publishedAt);
  });
}

/** 比较两个来源发布时间，并在日期无效时退回字符串排序。 */
function comparePublishedAt(left: string, right: string): number {
  const leftTime = new Date(left).getTime();
  const rightTime = new Date(right).getTime();
  if (Number.isNaN(leftTime) && Number.isNaN(rightTime)) return left.localeCompare(right);
  if (Number.isNaN(leftTime)) return 1;
  if (Number.isNaN(rightTime)) return -1;
  return leftTime - rightTime;
}

/** 为长结果集生成紧凑且稳定的页码序列。 */
function createPaginationItems(currentPage: number, pageCount: number): Array<number | string> {
  if (pageCount <= 7) return Array.from({ length: pageCount }, (_, index) => index + 1);

  const items: Array<number | string> = [1];
  const start = Math.max(2, currentPage - 1);
  const end = Math.min(pageCount - 1, currentPage + 1);
  if (start > 2) items.push("ellipsis-start");
  for (let value = start; value <= end; value += 1) items.push(value);
  if (end < pageCount - 1) items.push("ellipsis-end");
  items.push(pageCount);
  return items;
}
