import { AlertTriangle, CalendarDays, Check, CheckCircle2, ChevronDown, ChevronRight, CircleOff, Download, Plus, RefreshCw, Rss, Save, Search, SlidersHorizontal, Trash2, Unlink } from "lucide-react";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { toast } from "@/lib/toast";
import { useAppScrollContainer } from "@/components/app-shell";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty";
import { Field, FieldDescription, FieldGroup, FieldLabel, FieldLegend, FieldSet } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Progress } from "@/components/ui/progress";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Textarea } from "@/components/ui/textarea";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { ConfirmActionDialog } from "@/components/confirm-action-dialog";
import { FilterToolbar, Page, PageBreadcrumb, PageHeader } from "@/components/page-layout";
import { ReleaseMetadataBadges } from "@/components/release-metadata-badges";
import { WorkbenchSheet } from "@/components/workbench-sheet";
import { appApi } from "@/lib/api";
import { cn } from "@/lib/cn";
import { formatBytes, formatPercent } from "@/lib/format";
import { useVirtualizerScrollMargin } from "@/hooks/use-virtualizer-scroll-margin";
import {
  AnimeDownloadTaskSheet,
  isCompletedDownload,
  type AnimeDownloadDetailFilter,
  type AnimeDownloadDetailState
} from "@/features/my-anime/download-task-sheet";
import { MyAnimeRow } from "@/features/my-anime/my-anime-list";
import {
  countReleaseFamilyEpisodes,
  getReleaseVersionLabel,
  groupReleaseFamilyEpisodes,
  groupReleaseVersions,
  isCollectionRelease,
  isReleaseSelectable,
  releaseKey,
  type ReleaseEpisodeFamilyGroup,
  type ReleaseVersionFamily
} from "@/features/my-anime/release-groups";
import { buildAnimeReleaseSearchTerms, classifyAnimeRelease } from "@shared/anime-release-search";
import { resolveAnimeTitleDisplay } from "@shared/anime-title";
import {
  canAnimeStatusAutoDownload,
  createDefaultMyAnimePreferences,
  normalizeMyAnimeAutoDownload
} from "@shared/my-anime-policy";
import type { AddReleaseDownloadInput, AnimeSourceBindingState, AnimeSourceCandidate, AnimeWatchProgress, EpisodeReleasePreview, ReleaseSearchResult, RssSubscriptionReleaseResult } from "@shared/contracts";
import type {
  AnimeRssSubscription,
  AnimeStatus,
  DownloadTask,
  Episode,
  EpisodePreference,
  EpisodeStatus,
  FansubGroup,
  MyAnime,
  NormalizedVideoCodec,
  Release,
  SubtitleLanguage,
  VideoBitDepth
} from "@shared/domain";
import { findEpisodeDownloadLink, summarizeAnimeDownloads } from "@shared/download-episode-links";
import { compareReleaseEpisodeDescending, findReleaseDownloadTask } from "@shared/release-identity";
import type { MediaPlaybackTarget } from "@shared/player-selection";
import { formatSubtitleLanguages, formatVideoBitDepth, resolveSubtitleLanguages, subtitleLanguageText } from "@shared/release-metadata";

const statusText: Record<AnimeStatus, string> = {
  watching: "在追",
  planned: "想看",
  completed: "已完成",
  paused: "暂停",
  dropped: "已弃"
};

const statusOptions = Object.entries(statusText) as Array<[AnimeStatus, string]>;
const episodeStatusText: Record<EpisodeStatus, string> = {
  upcoming: "未开播",
  aired: "已开播",
  matched: "已匹配",
  downloading: "下载中",
  downloaded: "已下载",
  watched: "已观看"
};
const downloadStatusText: Record<DownloadTask["status"], string> = {
  queued: "排队中",
  fetching_metadata: "获取元数据",
  downloading: "下载中",
  stalled: "等待连接",
  waiting_network: "等待 Wi-Fi",
  paused: "已暂停",
  checking: "校验中",
  moving: "移动文件",
  completed: "已完成",
  seeding: "做种中",
  error: "错误",
  missing_files: "文件缺失"
};

const episodeStatusOptions = Object.entries(episodeStatusText) as Array<[EpisodeStatus, string]>;
const resolutionOptions = ["", "720p", "1080p", "2160p"];
const codecOptions: Array<"" | NormalizedVideoCodec> = ["", "H.264/AVC", "H.265/HEVC", "AV1", "VP9", "Unknown"];
const subtitleOptions: SubtitleLanguage[] = ["chs", "cht", "jpn", "eng"];
const bitDepthOptions: Array<"" | VideoBitDepth> = ["", 8, 10, 12];
const unknownFansubFilter = "__unknown__";
const batchAddingReleaseId = "__batch__";
const emptySelectValue = "__empty__";
type DownloadResourceTab = "rss" | "search";
type MyAnimeFilter = "all" | AnimeStatus;
type RulesTab = "basic" | "download" | "rss" | "episodes";
const defaultRssRefreshIntervalMinutes = 20;

interface RssReleaseGroupState {
  subscription: AnimeRssSubscription;
  releases: Release[];
  errors: RssSubscriptionReleaseResult["errors"];
}

interface RssSubscriptionDraft {
  name: string;
  url: string;
  preferredSubtitleLanguages?: SubtitleLanguage[];
}

const myAnimeFilters: Array<{ value: MyAnimeFilter; label: string }> = [
  { value: "all", label: "全部" },
  ...statusOptions.map(([value, label]) => ({ value, label }))
];
const releaseSearchCacheTtlMs = 24 * 60 * 60 * 1000;

export interface MyAnimePageIntent {
  animeId: string;
  action: "rules" | "resources" | "tasks";
  key: number;
}

interface MyAnimePageProps {
  actionOnly?: boolean;
  allowLocalPathRules?: boolean;
  intent?: MyAnimePageIntent | null;
  onDataChanged?: () => void;
  onIntentHandled?: () => void;
  onOpenAnimeDetail?: (animeId: string) => void;
  onPlayMedia?: (target: MediaPlaybackTarget) => Promise<void>;
}

/** 渲染追番列表并协调规则、资源下载和任务明细抽屉。 */
export function MyAnimePage({
  actionOnly = false,
  allowLocalPathRules = true,
  intent,
  onDataChanged,
  onIntentHandled,
  onOpenAnimeDetail,
  onPlayMedia
}: MyAnimePageProps = {}) {
  const [items, setItems] = useState<MyAnime[]>([]);
  const [watchProgress, setWatchProgress] = useState<Record<string, AnimeWatchProgress>>({});
  const [removeTarget, setRemoveTarget] = useState<MyAnime | null>(null);
  const [statusFilter, setStatusFilter] = useState<MyAnimeFilter>("watching");
  const [fansubs, setFansubs] = useState<FansubGroup[]>([]);
  const [animeFansubs, setAnimeFansubs] = useState<FansubGroup[]>([]);
  const [draft, setDraft] = useState<MyAnime | null>(null);
  const [draftBaseline, setDraftBaseline] = useState<string | null>(null);
  const [discardRulesDialogOpen, setDiscardRulesDialogOpen] = useState(false);
  const [downloadTarget, setDownloadTarget] = useState<MyAnime | null>(null);
  const [downloadDetail, setDownloadDetail] = useState<AnimeDownloadDetailState | null>(null);
  const [episodes, setEpisodes] = useState<Episode[]>([]);
  const [episodePreferences, setEpisodePreferences] = useState<EpisodePreference[]>([]);
  const [downloadTasks, setDownloadTasks] = useState<DownloadTask[]>([]);
  const [releasePreviews, setReleasePreviews] = useState<Record<string, EpisodeReleasePreview>>({});
  const [animeReleases, setAnimeReleases] = useState<Release[]>([]);
  const [animeReleaseErrors, setAnimeReleaseErrors] = useState<ReleaseSearchResult["errors"]>([]);
  const [animeRssReleaseGroups, setAnimeRssReleaseGroups] = useState<RssReleaseGroupState[]>([]);
  const [animeRssReleaseLoading, setAnimeRssReleaseLoading] = useState(false);
  const [animeRssReleaseResolved, setAnimeRssReleaseResolved] = useState(false);
  const [downloadResourceTab, setDownloadResourceTab] = useState<DownloadResourceTab>("rss");
  const [animeReleaseFansubId, setAnimeReleaseFansubId] = useState("");
  const [animeReleaseLoading, setAnimeReleaseLoading] = useState(false);
  const [animeReleaseResolved, setAnimeReleaseResolved] = useState(false);
  const [sourceBindingState, setSourceBindingState] = useState<AnimeSourceBindingState | null>(null);
  const [sourceBindingLoading, setSourceBindingLoading] = useState(false);
  const [sourceBindingActionKey, setSourceBindingActionKey] = useState<string | null>(null);
  const [unknownSeasonDownloadTarget, setUnknownSeasonDownloadTarget] = useState<Release | null>(null);
  const [loading, setLoading] = useState(true);
  const [episodeLoading, setEpisodeLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [previewingEpisodeId, setPreviewingEpisodeId] = useState<string | null>(null);
  const [addingReleaseId, setAddingReleaseId] = useState<string | null>(null);
  const [message, setMessage] = useState<{ tone: "success" | "error"; text: string } | null>(null);

  useEffect(() => {
    let active = true;

    Promise.all([
      appApi.listMyAnime(),
      appApi.listFansubs(),
      appApi.listDownloads(),
      appApi.listMyAnimeWatchProgress()
    ])
      .then(([animeItems, groups, downloads, progressItems]) => {
        if (!active) {
          return;
        }

        setItems(animeItems);
        setFansubs(groups);
        setDownloadTasks(downloads);
        setWatchProgress(Object.fromEntries(progressItems.map((progress) => [progress.animeId, progress])));
      })
      .catch((error) => {
        if (active) {
          setMessage({
            tone: "error",
            text: error instanceof Error ? error.message : "加载追番数据失败"
          });
        }
      })
      .finally(() => {
        if (active) {
          setLoading(false);
        }
      });

    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    let active = true;
    /** 返回追番页时读取播放器刚写入的观看进度。 */
    const refreshWatchProgress = (): void => {
      if (document.visibilityState !== "visible") {
        return;
      }
      void appApi.listMyAnimeWatchProgress()
        .then((progressItems) => {
          if (active) {
            setWatchProgress(Object.fromEntries(progressItems.map((progress) => [progress.animeId, progress])));
          }
        })
        .catch((error) => {
          console.warn("[my-anime] 自动刷新观看进度失败", { error });
        });
    };

    window.addEventListener("focus", refreshWatchProgress);
    document.addEventListener("visibilitychange", refreshWatchProgress);
    return () => {
      active = false;
      window.removeEventListener("focus", refreshWatchProgress);
      document.removeEventListener("visibilitychange", refreshWatchProgress);
    };
  }, []);

  const fansubNames = useMemo(
    () => new Map(mergeFansubGroups(fansubs, animeFansubs).map((group) => [group.id, group.name])),
    [fansubs, animeFansubs]
  );
  const visibleItems = useMemo(
    () => items.filter((item) => statusFilter === "all" || item.status === statusFilter),
    [items, statusFilter]
  );
  const draftPersisted = Boolean(draft && items.some((item) => item.id === draft.id));
  const activeFansubAnimeId = draft && draftPersisted ? draft.anime.id : downloadTarget?.anime.id;

  useEffect(() => {
    if (!actionOnly || !message) return;
    if (message.tone === "error") {
      toast.error(message.text);
    } else {
      toast.success(message.text);
    }
    setMessage(null);
  }, [actionOnly, message]);

  useEffect(() => {
    if (!intent || loading) return;
    const target = items.find((item) => item.anime.id === intent.animeId);
    if (target) {
      if (intent.action === "rules") openRulesDrawer(target);
      if (intent.action === "resources") void openAnimeDownloads(target);
      if (intent.action === "tasks") openDownloadDetail(target, "all");
    }
    onIntentHandled?.();
  }, [intent?.key, items, loading]);

  useEffect(() => {
    let active = true;
    if (!activeFansubAnimeId) {
      setAnimeFansubs([]);
      return;
    }

    appApi.listFansubs(activeFansubAnimeId)
      .then((groups) => {
        if (!active) return;
        setAnimeFansubs(groups);
        setFansubs((current) => mergeFansubGroups(current, groups));
      })
      .catch((error) => {
        if (active) {
          setMessage({ tone: "error", text: error instanceof Error ? error.message : "加载番剧字幕组失败" });
        }
      });
    return () => {
      active = false;
    };
  }, [activeFansubAnimeId]);

  useEffect(() => {
    if (!downloadTarget) {
      return;
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        closeAnimeDownloads();
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [downloadTarget]);

  useEffect(() => {
    if (!downloadDetail) {
      return;
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        closeDownloadDetail();
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [downloadDetail]);

  useEffect(() => {
    let active = true;

    if (!draft?.anime.id || !draftPersisted) {
      setEpisodes([]);
      setEpisodePreferences([]);
      return;
    }

    setEpisodeLoading(true);
    Promise.all([appApi.listEpisodes(draft.anime.id), appApi.listEpisodePreferences(draft.anime.id), appApi.listDownloads()])
      .then(([loadedEpisodes, loadedPreferences, downloads]) => {
        if (!active) {
          return;
        }

        setEpisodes(loadedEpisodes);
        setEpisodePreferences(loadedPreferences);
        setDownloadTasks(downloads);
      })
      .catch((error) => {
        if (active) {
          setMessage({
            tone: "error",
            text: error instanceof Error ? error.message : "加载单集规则失败"
          });
        }
      })
      .finally(() => {
        if (active) {
          setEpisodeLoading(false);
        }
      });

    return () => {
      active = false;
    };
  }, [draft?.anime.id, draftPersisted]);

  async function saveDraft() {
    if (!draft) {
      return;
    }

    if (!draft.anime.title.trim()) {
      setMessage({ tone: "error", text: "番剧名称不能为空" });
      return;
    }

    setSaving(true);
    try {
      const now = new Date().toISOString();
      const updated = await appApi.upsertMyAnime({
        ...draft,
        rssSubscriptions: normalizeRssSubscriptions(draft, now),
        anime: {
          ...draft.anime,
          title: draft.anime.title.trim(),
          originalTitle: draft.anime.originalTitle?.trim() || undefined,
          premiereYear: Number(draft.anime.premiereYear),
          premiereMonth: clampMonth(Number(draft.anime.premiereMonth))
        },
        addedAt: draft.addedAt || now,
        updatedAt: now
      });

      setItems(updated);
      setDraft(null);
      setDraftBaseline(null);
      setMessage({ tone: "success", text: "追番规则已保存" });
      onDataChanged?.();
    } catch (error) {
      setMessage({
        tone: "error",
        text: error instanceof Error ? error.message : "保存追番规则失败"
      });
    } finally {
      setSaving(false);
    }
  }

  /** 刷新某部番剧已从真实资源发现的字幕组。 */
  async function refreshAnimeFansubs(animeId: string) {
    const groups = await appApi.listFansubs(animeId);
    setAnimeFansubs(groups);
    setFansubs((current) => mergeFansubGroups(current, groups));
  }

  async function removeItem(item: MyAnime) {
    try {
      const updated = await appApi.removeMyAnime(item.id);
      setItems(updated);
      if (draft?.id === item.id) {
        setDraft(null);
        setDraftBaseline(null);
      }
      if (downloadTarget?.id === item.id) {
        closeAnimeDownloads();
      }
      if (downloadDetail?.item.id === item.id) {
        closeDownloadDetail();
      }
      setMessage({ tone: "success", text: "已移除追番" });
    } catch (error) {
      setMessage({
        tone: "error",
        text: error instanceof Error ? error.message : "移除追番失败"
      });
      throw error;
    }
  }

  async function addNextEpisode() {
    if (!draft || !draftPersisted) {
      setMessage({ tone: "error", text: "请先保存追番，再添加单集" });
      return;
    }

    const nextEpisodeNo = Math.max(0, ...episodes.map((episode) => episode.episodeNo)) + 1;
    const now = new Date().toISOString();
    const episode: Episode = {
      id: createId("episode"),
      animeId: draft.anime.id,
      episodeNo: nextEpisodeNo,
      status: "upcoming",
      airTime: now
    };

    try {
      setEpisodes(await appApi.upsertEpisode(episode));
      setMessage({ tone: "success", text: `已添加第 ${nextEpisodeNo} 集` });
    } catch (error) {
      setMessage({
        tone: "error",
        text: error instanceof Error ? error.message : "添加单集失败"
      });
    }
  }

  async function updateEpisodeStatus(episode: Episode, status: EpisodeStatus) {
    try {
      setEpisodes(
        await appApi.upsertEpisode({
          ...episode,
          status
        })
      );
    } catch (error) {
      setMessage({
        tone: "error",
        text: error instanceof Error ? error.message : "更新单集状态失败"
      });
    }
  }

  async function updateEpisodeFansub(episode: Episode, fansubGroupId: string) {
    try {
      if (!fansubGroupId) {
        setEpisodePreferences(await appApi.removeEpisodePreference(episode.id));
        clearEpisodePreview(episode.id);
        setMessage({ tone: "success", text: "已恢复跟随默认字幕组" });
        return;
      }

      const existing = episodePreferences.find((preference) => preference.episodeId === episode.id);
      setEpisodePreferences(
        await appApi.upsertEpisodePreference({
          id: existing?.id ?? createId("episode-pref"),
          animeId: episode.animeId,
          episodeId: episode.id,
          fansubGroupId,
          releaseId: existing?.releaseId,
          isManualOverride: true
        })
      );
      clearEpisodePreview(episode.id);
      setMessage({ tone: "success", text: "已切换单集字幕组，重新查看发布后会按新字幕组匹配" });
    } catch (error) {
      setMessage({
        tone: "error",
        text: error instanceof Error ? error.message : "更新单集字幕组失败"
      });
    }
  }

  async function previewEpisodeReleases(episode: Episode) {
    setPreviewingEpisodeId(episode.id);
    try {
      const preview = await appApi.previewEpisodeReleases(episode.animeId, episode.id);
      await refreshAnimeFansubs(episode.animeId);
      setReleasePreviews((current) => ({
        ...current,
        [episode.id]: preview
      }));
      setMessage({ tone: "success", text: `已找到 ${preview.candidates.length} 个候选资源` });
    } catch (error) {
      setMessage({
        tone: "error",
        text: error instanceof Error ? error.message : "匹配单集资源失败"
      });
    } finally {
      setPreviewingEpisodeId(null);
    }
  }

  function clearEpisodePreview(episodeId: string) {
    setReleasePreviews((current) => {
      const next = { ...current };
      delete next[episodeId];
      return next;
    });
  }

  async function openAnimeDownloads(item: MyAnime) {
    const target = cloneMyAnime(item);
    const nextTab: DownloadResourceTab = getEnabledRssSubscriptions(target).length > 0 ? "rss" : "search";
    setDraft(null);
    setDraftBaseline(null);
    setDownloadTarget(target);
    setDownloadResourceTab(nextTab);
    setAnimeReleaseFansubId(target.defaultFansubGroupId ?? "");
    setAnimeReleases([]);
    setAnimeReleaseErrors([]);
    setAnimeRssReleaseGroups([]);
    setAnimeReleaseResolved(false);
    setAnimeRssReleaseResolved(false);
    if (nextTab === "rss") {
      void refreshAnimeSourceBindings(target.anime.id);
      await searchAnimeRssReleases(target);
    } else {
      await refreshAnimeSourceBindings(target.anime.id);
      await searchAnimeReleases(target);
    }
  }

  function closeAnimeDownloads() {
    setDownloadTarget(null);
    setAnimeReleases([]);
    setAnimeReleaseErrors([]);
    setAnimeRssReleaseGroups([]);
    setAnimeReleaseFansubId("");
    setAnimeReleaseResolved(false);
    setAnimeRssReleaseResolved(false);
    setSourceBindingState(null);
    setSourceBindingActionKey(null);
    setUnknownSeasonDownloadTarget(null);
  }

  /** 打开某部追番的下载任务明细抽屉。 */
  function openDownloadDetail(item: MyAnime, filter: AnimeDownloadDetailFilter) {
    setDownloadDetail({
      item: cloneMyAnime(item),
      filter
    });
  }

  function closeDownloadDetail() {
    setDownloadDetail(null);
  }

  /** 移除单个已完成下载任务，并同步本地任务快照。 */
  async function removeAnimeDownloadTask(taskId: string, deleteFiles: boolean): Promise<void> {
    try {
      const latestDownloads = await appApi.removeDownload(taskId, deleteFiles);
      setDownloadTasks(latestDownloads);
      console.info("[my-anime] 已移除下载任务", { taskId, deleteFiles });
      setMessage({
        tone: "success",
        text: deleteFiles ? "已删除任务及其文件" : "已移除任务，文件已保留"
      });
    } catch (error) {
      setMessage({
        tone: "error",
        text: error instanceof Error ? error.message : "删除下载资源失败"
      });
      throw error;
    }
  }

  /** 打开追番规则抽屉，并保留已采集的番剧元数据快照。 */
  function openRulesDrawer(item: MyAnime) {
    closeAnimeDownloads();
    closeDownloadDetail();
    const nextDraft = normalizeMyAnimeAutoDownload(cloneMyAnime(item));
    setDraft(nextDraft);
    setDraftBaseline(serializeMyAnimeDraft(nextDraft));
  }

  /** 打开新增追番规则侧栏，并记录初始草稿用于退出确认。 */
  function openNewAnimeDrawer() {
    closeAnimeDownloads();
    closeDownloadDetail();
    const nextDraft = createEmptyDraft();
    setDraft(nextDraft);
    setDraftBaseline(serializeMyAnimeDraft(nextDraft));
  }

  /** 请求关闭规则侧栏；草稿发生变化时先要求确认。 */
  function requestCloseRules() {
    if (draft && draftBaseline !== serializeMyAnimeDraft(draft)) {
      setDiscardRulesDialogOpen(true);
      return;
    }
    setDraft(null);
    setDraftBaseline(null);
  }

  /** 放弃当前规则草稿并关闭侧栏。 */
  function discardRulesDraft() {
    setDraft(null);
    setDraftBaseline(null);
    setDiscardRulesDialogOpen(false);
  }

  /** 查询某部追番的下载资源，默认使用 1 天缓存，强制刷新时绕过缓存。 */
  async function searchAnimeReleases(target = downloadTarget, options: { forceRefresh?: boolean } = {}) {
    if (!target) {
      return;
    }

    setAnimeReleaseLoading(true);
    try {
      const result = await appApi.searchAnimeReleases({
        animeId: target.anime.id,
        preferredResolution: target.preferredResolution,
        limit: 200,
        cacheTtlMs: releaseSearchCacheTtlMs,
        forceRefresh: options.forceRefresh
      });
      const releases = sortReleases(dedupeReleases(result.releases)).map((release) => ({
        ...release,
        animeId: target.anime.id
      }));
      const errors = dedupeReleaseErrors(result.errors);
      setAnimeReleases(releases);
      setAnimeReleaseErrors(errors);
      await refreshAnimeFansubs(target.anime.id);
      setMessage({
        tone: releases.length === 0 && errors.length > 0 ? "error" : "success",
        text:
          releases.length === 0 && errors.length > 0
            ? "下载源请求失败，未获取到可用资源"
            : `已找到 ${releases.length} 个资源`
      });
    } catch (error) {
      setAnimeReleases([]);
      setAnimeReleaseErrors([]);
      setMessage({
        tone: "error",
        text: error instanceof Error ? error.message : "查询发布资源失败"
      });
    } finally {
      setAnimeReleaseLoading(false);
      setAnimeReleaseResolved(true);
    }
  }

  /** 读取精确来源绑定和待确认候选。 */
  async function refreshAnimeSourceBindings(animeId: string) {
    setSourceBindingLoading(true);
    try {
      setSourceBindingState(await appApi.getAnimeSourceBindingState(animeId, true));
    } catch (error) {
      setMessage({ tone: "error", text: error instanceof Error ? error.message : "读取来源匹配失败" });
    } finally {
      setSourceBindingLoading(false);
    }
  }

  /** 确认来源候选并重新读取精确资源。 */
  async function confirmAnimeSourceCandidate(candidate: AnimeSourceCandidate) {
    if (!downloadTarget) return;
    setSourceBindingActionKey(`${candidate.sourceId}:${candidate.sourceAnimeId}`);
    try {
      const state = await appApi.confirmAnimeSourceBinding({
        animeId: downloadTarget.anime.id,
        sourceId: candidate.sourceId,
        sourceAnimeId: candidate.sourceAnimeId,
        sourceAnimeTitle: candidate.title,
        sourceUrl: candidate.sourceUrl,
        confidence: candidate.score / 100
      });
      setSourceBindingState(state);
      await searchAnimeReleases(downloadTarget, { forceRefresh: true });
      setMessage({ tone: "success", text: `已绑定 ${candidate.sourceName}：${candidate.title}` });
    } catch (error) {
      setMessage({ tone: "error", text: error instanceof Error ? error.message : "确认来源匹配失败" });
    } finally {
      setSourceBindingActionKey(null);
    }
  }

  /** 持久化人工确认的不匹配候选，并提供一次撤销入口。 */
  async function reportAnimeSourceCandidateMismatch(candidate: AnimeSourceCandidate) {
    if (!downloadTarget) return;
    const animeId = downloadTarget.anime.id;
    const candidateKey = `${candidate.sourceId}:${candidate.sourceAnimeId}`;
    setSourceBindingActionKey(candidateKey);
    try {
      await appApi.reportAnimeSourceCandidateMismatch({
        animeId,
        sourceId: candidate.sourceId,
        sourceAnimeId: candidate.sourceAnimeId,
        sourceAnimeTitle: candidate.title,
        score: candidate.score,
        reasons: candidate.reasons
      });
      setSourceBindingState((current) => current ? {
        ...current,
        candidates: current.candidates.filter(
          (item) => item.sourceId !== candidate.sourceId || item.sourceAnimeId !== candidate.sourceAnimeId
        )
      } : current);
      toast.success(`已记录不匹配：${candidate.title}`, {
        action: {
          label: "撤销",
          onClick: () => {
            void appApi.removeAnimeSourceCandidateMismatch({
              animeId,
              sourceId: candidate.sourceId,
              sourceAnimeId: candidate.sourceAnimeId
            }).then((state) => {
              setSourceBindingState(state);
              toast.success(`已恢复候选：${candidate.title}`);
            }).catch((error) => {
              toast.error(error instanceof Error ? error.message : "恢复来源候选失败");
            });
          }
        }
      });
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "记录来源不匹配失败");
    } finally {
      setSourceBindingActionKey(null);
    }
  }

  /** 持久化或取消当前番剧对整个来源的候选排除。 */
  async function setAnimeSourceExcluded(sourceId: string, excluded: boolean) {
    if (!downloadTarget) return;
    const actionKey = `source-exclusion:${sourceId}`;
    setSourceBindingActionKey(actionKey);
    try {
      const state = await appApi.setAnimeSourceExcluded({
        animeId: downloadTarget.anime.id,
        sourceId,
        excluded
      });
      setSourceBindingState(state);
      toast.success(excluded ? "已排除该来源的全部候选" : "已恢复该来源的候选匹配");
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "更新来源排除状态失败");
    } finally {
      setSourceBindingActionKey(null);
    }
  }

  /** 移除来源绑定并重新发现候选。 */
  async function removeAnimeSourceBinding(sourceId: string) {
    if (!downloadTarget) return;
    setSourceBindingActionKey(sourceId);
    try {
      setSourceBindingState(await appApi.removeAnimeSourceBinding(downloadTarget.anime.id, sourceId));
      setAnimeReleases((current) => current.filter((release) => release.sourceId !== sourceId));
      setMessage({ tone: "success", text: "已移除来源绑定，请重新确认候选" });
    } catch (error) {
      setMessage({ tone: "error", text: error instanceof Error ? error.message : "移除来源绑定失败" });
    } finally {
      setSourceBindingActionKey(null);
    }
  }

  /** 查询某部追番已配置的 RSS 订阅资源。 */
  async function searchAnimeRssReleases(target = downloadTarget) {
    if (!target) {
      return;
    }

    const subscriptions = getEnabledRssSubscriptions(target);
    if (subscriptions.length === 0) {
      setAnimeRssReleaseGroups([]);
      setMessage({ tone: "error", text: "请先在规则中配置启用的 RSS 订阅" });
      return;
    }

    setAnimeRssReleaseLoading(true);
    try {
      const results = await Promise.all(
        subscriptions.map((subscription) =>
          appApi.searchRssSubscriptionReleases({
            animeId: target.anime.id,
            subscriptionId: subscription.id,
            preferredResolution: target.preferredResolution,
            limit: 200
          })
        )
      );
      const groups = results.map((result, index) => ({
        subscription: subscriptions.find((subscription) => subscription.id === result.query.subscriptionId) ?? subscriptions[index],
        releases: sortReleases(
          dedupeReleases(result.releases).map((release) => ({
            ...release,
            animeId: target.anime.id
          }))
        ),
        errors: result.errors
      }));
      const releaseCount = groups.reduce((sum, group) => sum + group.releases.length, 0);
      const errorCount = groups.reduce((sum, group) => sum + group.errors.length, 0);
      setAnimeRssReleaseGroups(groups);
      await refreshAnimeFansubs(target.anime.id);
      setMessage({
        tone: releaseCount === 0 && errorCount > 0 ? "error" : "success",
        text:
          releaseCount === 0 && errorCount > 0
            ? "RSS 订阅请求失败，未获取到可用资源"
            : `RSS 已找到 ${releaseCount} 个资源`
      });
    } catch (error) {
      setAnimeRssReleaseGroups([]);
      setMessage({
        tone: "error",
        text: error instanceof Error ? error.message : "查询 RSS 订阅资源失败"
      });
    } finally {
      setAnimeRssReleaseLoading(false);
      setAnimeRssReleaseResolved(true);
    }
  }

  /** 将资源分组推导出的 RSS 地址保存到当前追番。 */
  async function addAnimeRssSubscription(subscriptionDraft: RssSubscriptionDraft) {
    if (!downloadTarget) {
      return;
    }

    const url = subscriptionDraft.url.trim();
    if (!url) {
      setMessage({ tone: "error", text: "RSS 地址为空，无法订阅" });
      return;
    }

    const existingSubscriptions = downloadTarget.rssSubscriptions ?? [];
    if (existingSubscriptions.some((subscription) => subscription.url.trim() === url)) {
      setMessage({ tone: "success", text: "该 RSS 已在当前追番订阅中" });
      return;
    }

    const now = new Date().toISOString();
    const nextTarget: MyAnime = {
      ...downloadTarget,
      rssSubscriptions: normalizeRssSubscriptions(
        {
          ...downloadTarget,
          rssSubscriptions: [
            ...existingSubscriptions,
            {
              id: createId("rss"),
              myAnimeId: downloadTarget.id,
              name: subscriptionDraft.name.trim() || "RSS订阅",
              url,
              enabled: true,
              preferredSubtitleLanguages: subscriptionDraft.preferredSubtitleLanguages,
              refreshIntervalMinutes: defaultRssRefreshIntervalMinutes,
              createdAt: now,
              updatedAt: now
            }
          ]
        },
        now
      ),
      updatedAt: now
    };

    try {
      const updatedItems = await appApi.upsertMyAnime(nextTarget);
      const savedTarget = updatedItems.find((item) => item.id === downloadTarget.id) ?? nextTarget;
      setItems(updatedItems);
      setDownloadTarget(cloneMyAnime(savedTarget));
      setMessage({ tone: "success", text: `已添加 RSS 订阅：${subscriptionDraft.name}` });
      if (downloadResourceTab === "rss") {
        void searchAnimeRssReleases(savedTarget);
      }
    } catch (error) {
      setMessage({
        tone: "error",
        text: error instanceof Error ? error.message : "添加 RSS 订阅失败"
      });
    }
  }

  async function addEpisodeReleaseDownload(episode: Episode, release: Release) {
    setAddingReleaseId(release.id);
    try {
      const updatedDownloads = await appApi.addReleaseDownload({
        release,
        animeId: episode.animeId,
        episodeId: episode.id,
        episodeNo: episode.episodeNo,
        fansubGroupId: release.fansubGroupId
      });
      const [updatedEpisodes, updatedPreferences] = await Promise.all([
        appApi.listEpisodes(episode.animeId),
        appApi.listEpisodePreferences(episode.animeId)
      ]);
      setDownloadTasks(updatedDownloads);
      setEpisodes(updatedEpisodes);
      setEpisodePreferences(updatedPreferences);
      setMessage({ tone: "success", text: "已添加到下载队列" });
    } catch (error) {
      setMessage({
        tone: "error",
        text: error instanceof Error ? error.message : "添加下载失败"
      });
    } finally {
      setAddingReleaseId(null);
    }
  }

  /** 未知季度资源先要求单条确认，明确不匹配资源始终拒绝。 */
  function requestAnimeReleaseDownload(release: Release) {
    if (!downloadTarget) {
      return;
    }
    const compatibility = classifyAnimeRelease(release, downloadTarget.anime);
    if (compatibility === "other") {
      setUnknownSeasonDownloadTarget(release);
      return;
    }
    if (compatibility === "mismatch") {
      setMessage({ tone: "error", text: "该资源季度与当前追番不一致，无法添加下载" });
      return;
    }
    void addAnimeReleaseDownload(release);
  }

  async function addAnimeReleaseDownload(release: Release, confirmUnknownSeason = false) {
    if (!downloadTarget) {
      return;
    }

    setAddingReleaseId(release.id);
    try {
      const updatedDownloads = await appApi.addReleaseDownload(
        buildAnimeReleaseDownloadInput(release, downloadTarget, confirmUnknownSeason)
      );
      setDownloadTasks(updatedDownloads);
      setMessage({ tone: "success", text: "已添加到下载队列" });
    } catch (error) {
      setMessage({
        tone: "error",
        text: error instanceof Error ? error.message : "添加下载失败"
      });
    } finally {
      setAddingReleaseId(null);
    }
  }

  /** 批量添加当前追番的多个资源下载。 */
  async function addAnimeReleaseDownloads(releases: Release[]) {
    if (!downloadTarget || releases.length === 0) {
      return;
    }

    const linkedTasks = downloadTasks.filter((task) => task.animeId === downloadTarget.anime.id);
    const candidates = dedupeReleases(releases).filter((release) => {
      const canDownload = Boolean(release.magnetUrl ?? release.torrentUrl);
      const compatible = classifyAnimeRelease(release, downloadTarget.anime) === "current";
      return compatible && canDownload && !findReleaseDownloadTask(linkedTasks, release);
    });
    if (candidates.length === 0) {
      setMessage({ tone: "error", text: "选中的资源都已加入或没有可下载地址" });
      return;
    }

    setAddingReleaseId(batchAddingReleaseId);
    let latestDownloads = downloadTasks;
    let successCount = 0;
    const failed: string[] = [];
    for (const release of candidates) {
      try {
        latestDownloads = await appApi.addReleaseDownload(
          buildAnimeReleaseDownloadInput(release, downloadTarget)
        );
        successCount += 1;
      } catch (error) {
        const reason = error instanceof Error ? error.message : "添加下载失败";
        failed.push(`${release.title}: ${reason}`);
      }
    }

    setDownloadTasks(latestDownloads);
    setAddingReleaseId(null);
    setMessage({
      tone: failed.length > 0 ? "error" : "success",
      text: failed.length > 0
        ? `批量下载完成：成功 ${successCount} 个，失败 ${failed.length} 个`
        : `已批量添加 ${successCount} 个下载任务`
    });
  }

  if (loading) {
    return actionOnly ? null : <MyAnimePageSkeleton />;
  }

  return (
    <>
      {!actionOnly && (
        <Page className="gap-4 pb-24">
          <PageHeader className="pb-3 sm:items-center">
            <h1 className="sr-only">我的追番</h1>
            <PageBreadcrumb current="我的追番" />
          </PageHeader>

          {message && (
            <Alert variant={message.tone === "error" ? "destructive" : "default"}>
              {message.tone === "error" && <AlertTriangle />}
              <AlertTitle>{message.tone === "error" ? "操作失败" : "操作完成"}</AlertTitle>
              <AlertDescription>{message.text}</AlertDescription>
            </Alert>
          )}

          <FilterToolbar className="border-0 bg-transparent py-0 sm:items-end">
            <Tabs
              className="min-w-0 flex-1 overflow-x-auto"
              value={statusFilter}
              onValueChange={(value) => setStatusFilter(value as MyAnimeFilter)}
            >
              <TabsList
                aria-label="筛选追番状态"
                className="min-w-max gap-5 sm:w-full sm:justify-start"
                variant="line"
              >
                {myAnimeFilters.map((filter) => (
                  <TabsTrigger className="min-w-0" key={filter.value} value={filter.value}>
                    {filter.label}
                    <Badge className="ml-1 h-5 min-w-5 justify-center px-1 text-[10px]" tone="neutral">
                      {filter.value === "all" ? items.length : items.filter((item) => item.status === filter.value).length}
                    </Badge>
                  </TabsTrigger>
                ))}
              </TabsList>
            </Tabs>
            <span className="shrink-0 pb-3 text-xs text-muted-foreground">显示 {visibleItems.length} 部</span>
          </FilterToolbar>

          {visibleItems.length > 0 ? (
            <VirtualMyAnimeList
              downloadTasks={downloadTasks}
              fansubNames={fansubNames}
              items={visibleItems}
              watchProgress={watchProgress}
              onOpenActive={(item) => openDownloadDetail(item, "active")}
              onOpenCompleted={(item) => openDownloadDetail(item, "completed")}
              onOpenDetail={(item) => onOpenAnimeDetail?.(item.anime.id)}
              onOpenDownloads={(item) => void openAnimeDownloads(item)}
              onOpenRules={openRulesDrawer}
              onRemove={setRemoveTarget}
            />
          ) : (
            <Empty className="min-h-72">
              <EmptyHeader>
                <EmptyMedia variant="icon">
                  <CalendarDays />
                </EmptyMedia>
                <EmptyTitle>{items.length ? "没有匹配的追番" : "暂无追番"}</EmptyTitle>
                <EmptyDescription>{items.length ? "请选择其他状态筛选。" : "当前还没有追番。"}</EmptyDescription>
              </EmptyHeader>
            </Empty>
          )}

          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                aria-label="添加追番"
                className="fixed bottom-[max(1.5rem,var(--safe-area-bottom))] right-[max(1.5rem,var(--safe-area-right))] z-30 size-12 rounded-full p-0 shadow-lg md:size-11"
                onClick={openNewAnimeDrawer}
                type="button"
              >
                <Plus aria-hidden="true" data-icon="inline-start" />
              </Button>
            </TooltipTrigger>
            <TooltipContent side="left">添加追番</TooltipContent>
          </Tooltip>
        </Page>
      )}

      {draft && (
        <RulesDrawer
          allowLocalPathRules={allowLocalPathRules}
          addingReleaseId={addingReleaseId}
          draft={draft}
          downloadTasks={downloadTasks}
          draftPersisted={draftPersisted}
          episodeLoading={episodeLoading}
          episodePreferences={episodePreferences}
          episodes={episodes}
          fansubNames={fansubNames}
          fansubs={animeFansubs}
          previewingEpisodeId={previewingEpisodeId}
          releasePreviews={releasePreviews}
          saving={saving}
          onAddEpisode={() => void addNextEpisode()}
          onAddRelease={(episode, release) => void addEpisodeReleaseDownload(episode, release)}
          onCancel={requestCloseRules}
          onChange={setDraft}
          onFansubChange={(episode, fansubGroupId) => void updateEpisodeFansub(episode, fansubGroupId)}
          onPreviewReleases={(episode) => void previewEpisodeReleases(episode)}
          onSave={() => void saveDraft()}
          onStatusChange={(episode, status) => void updateEpisodeStatus(episode, status)}
        />
      )}

      {downloadTarget && (
        <WorkbenchSheet
          bodyClassName="flex flex-col overflow-hidden"
          className="sm:max-w-[800px]"
          description={
            <span className="flex flex-wrap items-center gap-x-3 gap-y-1">
              <strong className="text-sm text-foreground">{resolveAnimeTitleDisplay(downloadTarget.anime).title}</strong>
              <span>
                关联集 {countReleaseFamilyEpisodes(groupReleaseVersions(animeReleases, downloadTarget, {}))}
                {" | "}已完成 {downloadTasks.filter((task) => task.animeId === downloadTarget.anime.id && isCompletedDownload(task)).length}
                {" | "}下载任务 {downloadTasks.filter((task) => task.animeId === downloadTarget.anime.id).length}
              </span>
            </span>
          }
          onClose={closeAnimeDownloads}
          title={<span className="flex items-center gap-2">下载资源 <Badge className="h-5" tone="primary-soft">WORKBENCH</Badge></span>}
        >
          <AnimeDownloadPanel
            addingReleaseId={addingReleaseId}
            activeTab={downloadResourceTab}
            batchAdding={addingReleaseId === batchAddingReleaseId}
            downloadTasks={downloadTasks}
            errors={animeReleaseErrors}
            fansubNames={fansubNames}
            fansubs={animeFansubs}
            loading={animeReleaseLoading}
            resolved={animeReleaseResolved}
            releases={animeReleases}
            rssGroups={animeRssReleaseGroups}
            rssLoading={animeRssReleaseLoading}
            rssResolved={animeRssReleaseResolved}
            selectedFansubId={animeReleaseFansubId}
            sourceBindingActionKey={sourceBindingActionKey}
            sourceBindingLoading={sourceBindingLoading}
            sourceBindingState={sourceBindingState}
            target={downloadTarget}
            onCancel={closeAnimeDownloads}
            onAddRelease={requestAnimeReleaseDownload}
            onAddRssSubscription={(subscription) => void addAnimeRssSubscription(subscription)}
            onAddSelected={(releases) => void addAnimeReleaseDownloads(releases)}
            onFansubChange={setAnimeReleaseFansubId}
            onConfirmSourceCandidate={(candidate) => void confirmAnimeSourceCandidate(candidate)}
            onRejectSourceCandidate={(candidate) => void reportAnimeSourceCandidateMismatch(candidate)}
            onSetSourceExcluded={(sourceId, excluded) => void setAnimeSourceExcluded(sourceId, excluded)}
            onRemoveSourceBinding={(sourceId) => void removeAnimeSourceBinding(sourceId)}
            onRefreshSourceBindings={() => void refreshAnimeSourceBindings(downloadTarget.anime.id)}
            onRefreshRss={() => void searchAnimeRssReleases(downloadTarget)}
            onRefresh={() => void searchAnimeReleases()}
            onForceRefresh={() => void searchAnimeReleases(downloadTarget, { forceRefresh: true })}
            onTabChange={(tab) => {
              setDownloadResourceTab(tab);
              if (tab === "rss" && animeRssReleaseGroups.length === 0 && !animeRssReleaseLoading) {
                void searchAnimeRssReleases(downloadTarget);
              }
              if (tab === "search" && animeReleases.length === 0 && !animeReleaseLoading) {
                void searchAnimeReleases(downloadTarget);
              }
            }}
          />
        </WorkbenchSheet>
      )}

      {downloadDetail && (
        <AnimeDownloadTaskSheet
          detail={downloadDetail}
          downloadTasks={downloadTasks}
          fansubNames={fansubNames}
          onClose={closeDownloadDetail}
          onFilterChange={(filter) =>
            setDownloadDetail((current) => (current ? { ...current, filter } : current))
          }
          onPlayMedia={onPlayMedia}
          onRemoveTask={removeAnimeDownloadTask}
        />
      )}

      <ConfirmActionDialog
        confirmLabel="仅下载此条"
        content={unknownSeasonDownloadTarget ? (
          <div className="border-l-2 border-amber-500/70 pl-3 text-sm text-foreground">
            {unknownSeasonDownloadTarget.title}
          </div>
        ) : undefined}
        description="该资源没有足够的季度标记，系统无法确认它属于当前追番。确认后仅添加这一条资源，不会影响自动下载或批量下载规则。"
        onConfirm={() => unknownSeasonDownloadTarget
          ? addAnimeReleaseDownload(unknownSeasonDownloadTarget, true)
          : undefined}
        onOpenChange={(open) => !open && setUnknownSeasonDownloadTarget(null)}
        open={Boolean(unknownSeasonDownloadTarget)}
        title="确认下载季度待确认资源？"
      />

      <ConfirmActionDialog
        confirmLabel="移除追番"
        description={removeTarget
          ? `「${resolveAnimeTitleDisplay(removeTarget.anime).title}」及其追番规则将被移除，已下载文件不会被删除。`
          : "该追番及其规则将被移除。"}
        onConfirm={() => removeTarget ? removeItem(removeTarget) : undefined}
        onOpenChange={(open) => !open && setRemoveTarget(null)}
        open={Boolean(removeTarget)}
        title="确认移除追番？"
      />

      <ConfirmActionDialog
        confirmLabel="放弃修改"
        description="当前规则尚未保存，关闭后本次修改将丢失。"
        onConfirm={discardRulesDraft}
        onOpenChange={setDiscardRulesDialogOpen}
        open={discardRulesDialogOpen}
        title="放弃未保存的规则？"
      />

    </>
  );
}

interface VirtualMyAnimeListProps {
  downloadTasks: DownloadTask[];
  fansubNames: Map<string, string>;
  items: MyAnime[];
  watchProgress: Record<string, AnimeWatchProgress>;
  onOpenActive: (item: MyAnime) => void;
  onOpenCompleted: (item: MyAnime) => void;
  onOpenDetail: (item: MyAnime) => void;
  onOpenDownloads: (item: MyAnime) => void;
  onOpenRules: (item: MyAnime) => void;
  onRemove: (item: MyAnime) => void;
}

/** 仅挂载视口附近的追番条目，并按真实行高修正滚动位置。 */
function VirtualMyAnimeList({
  downloadTasks,
  fansubNames,
  items,
  watchProgress,
  onOpenActive,
  onOpenCompleted,
  onOpenDetail,
  onOpenDownloads,
  onOpenRules,
  onRemove
}: VirtualMyAnimeListProps) {
  const scrollContainerRef = useAppScrollContainer();
  const listRef = useRef<HTMLDivElement | null>(null);
  const scrollMargin = useVirtualizerScrollMargin(scrollContainerRef, listRef);
  const virtualizer = useVirtualizer({
    count: items.length,
    estimateSize: () => 152,
    getItemKey: (index) => items[index]?.id ?? index,
    getScrollElement: () => scrollContainerRef.current,
    overscan: 5,
    scrollMargin
  });

  return (
    <div className="min-w-0 border-y">
      <div
        ref={listRef}
        className="relative min-w-0"
        style={{ height: virtualizer.getTotalSize() }}
      >
        {virtualizer.getVirtualItems().map((virtualItem) => {
          const item = items[virtualItem.index];
          if (!item) return null;
          return (
            <div
              className={cn(
                "absolute left-0 top-0 w-full border-b",
                virtualItem.index === items.length - 1 && "border-b-0"
              )}
              data-index={virtualItem.index}
              key={item.id}
              ref={virtualizer.measureElement}
              style={{ transform: `translateY(${virtualItem.start - scrollMargin}px)` }}
            >
              <MyAnimeRow
                item={item}
                watchProgress={watchProgress[item.anime.id] ?? {
                  animeId: item.anime.id,
                  watchedEpisodeCount: 0,
                  totalEpisodeCount: item.anime.detail?.episodeCount ?? 0
                }}
                defaultFansubName={fansubNames.get(item.defaultFansubGroupId ?? "") ?? "未设置"}
                downloadSummary={summarizeAnimeDownloads(downloadTasks, item.anime.id)}
                onOpenActive={() => onOpenActive(item)}
                onOpenCompleted={() => onOpenCompleted(item)}
                onOpenDetail={() => onOpenDetail(item)}
                onOpenDownloads={() => onOpenDownloads(item)}
                onOpenRules={() => onOpenRules(item)}
                onRemove={() => onRemove(item)}
              />
            </div>
          );
        })}
      </div>
    </div>
  );
}

/** 渲染追番列表加载中的结构化占位状态。 */
function MyAnimePageSkeleton() {
  return (
    <Page aria-busy="true" aria-label="正在加载追番列表">
      <div className="flex flex-col gap-2">
        <Skeleton className="h-7 w-32" />
        <Skeleton className="h-4 w-72 max-w-full" />
      </div>
      <div className="flex min-w-0 flex-col gap-2">
        {["anime-1", "anime-2", "anime-3", "anime-4"].map((item) => (
          <div className="flex gap-4 border p-3" key={item}>
            <Skeleton className="aspect-[2/3] w-16 shrink-0 rounded-md" />
            <div className="flex min-w-0 flex-1 flex-col gap-3 py-1">
              <Skeleton className="h-5 w-2/3" />
              <Skeleton className="h-4 w-1/2" />
              <Skeleton className="h-2 w-full" />
            </div>
          </div>
        ))}
      </div>
    </Page>
  );
}

/** 渲染追番规则和单集规则的右侧抽屉。 */
function RulesDrawer({
  allowLocalPathRules,
  draft,
  fansubs,
  saving,
  draftPersisted,
  episodes,
  episodePreferences,
  downloadTasks,
  releasePreviews,
  fansubNames,
  episodeLoading,
  previewingEpisodeId,
  addingReleaseId,
  onChange,
  onCancel,
  onSave,
  onAddEpisode,
  onStatusChange,
  onFansubChange,
  onPreviewReleases,
  onAddRelease
}: {
  allowLocalPathRules: boolean;
  draft: MyAnime;
  fansubs: FansubGroup[];
  saving: boolean;
  draftPersisted: boolean;
  episodes: Episode[];
  episodePreferences: EpisodePreference[];
  downloadTasks: DownloadTask[];
  releasePreviews: Record<string, EpisodeReleasePreview>;
  fansubNames: Map<string, string>;
  episodeLoading: boolean;
  previewingEpisodeId: string | null;
  addingReleaseId: string | null;
  onChange: (item: MyAnime | null) => void;
  onCancel: () => void;
  onSave: () => void;
  onAddEpisode: () => void;
  onStatusChange: (episode: Episode, status: EpisodeStatus) => void;
  onFansubChange: (episode: Episode, fansubGroupId: string) => void;
  onPreviewReleases: (episode: Episode) => void;
  onAddRelease: (episode: Episode, release: Release) => void;
}) {
  const [activeTab, setActiveTab] = useState<RulesTab>("basic");
  const titleDisplay = resolveAnimeTitleDisplay(draft.anime);

  return (
    <WorkbenchSheet
      description={titleDisplay.subtitle ?? (draftPersisted ? "编辑追番规则" : "创建新的追番")}
      footer={
        <div className="flex flex-col-reverse gap-2 sm:flex-row sm:justify-end">
          <Button onClick={onCancel} variant="outline">取消</Button>
          <Button onClick={onSave} disabled={saving}>
            <Save data-icon="inline-start" />
            {saving ? "保存中" : "保存规则"}
          </Button>
        </div>
      }
      headerContent={
        <Tabs value={activeTab} onValueChange={(value) => setActiveTab(value as RulesTab)}>
          <TabsList className="grid h-auto w-full grid-cols-2 sm:grid-cols-4">
            <TabsTrigger value="basic">基础信息</TabsTrigger>
            <TabsTrigger value="download">下载偏好</TabsTrigger>
            <TabsTrigger value="rss">RSS 订阅</TabsTrigger>
            <TabsTrigger value="episodes">单集规则</TabsTrigger>
          </TabsList>
        </Tabs>
      }
      onClose={onCancel}
      title={draftPersisted ? `追番规则 · ${titleDisplay.title}` : "添加追番"}
    >
      <div className="flex min-w-0 flex-col gap-4">
        {activeTab !== "episodes" && (
        <RulesPanel
          activeTab={activeTab}
          allowLocalPathRules={allowLocalPathRules}
          draft={draft}
          fansubs={fansubs}
          onChange={onChange}
        />
        )}
        {activeTab === "episodes" && (
        <EpisodeRulesPanel
          draft={draft}
          persisted={draftPersisted}
          episodes={episodes}
          episodePreferences={episodePreferences}
          downloadTasks={downloadTasks}
          releasePreviews={releasePreviews}
          fansubs={fansubs}
          fansubNames={fansubNames}
          loading={episodeLoading}
          previewingEpisodeId={previewingEpisodeId}
          addingReleaseId={addingReleaseId}
          onAddEpisode={onAddEpisode}
          onStatusChange={onStatusChange}
          onFansubChange={onFansubChange}
          onPreviewReleases={onPreviewReleases}
          onAddRelease={onAddRelease}
        />
        )}
      </div>
    </WorkbenchSheet>
  );
}

function RulesPanel({
  activeTab,
  allowLocalPathRules,
  draft,
  fansubs,
  onChange
}: {
  activeTab: Exclude<RulesTab, "episodes">;
  allowLocalPathRules: boolean;
  draft: MyAnime | null;
  fansubs: FansubGroup[];
  onChange: (item: MyAnime | null) => void;
}) {
  if (!draft) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>追番规则</CardTitle>
        </CardHeader>
        <CardContent>
          <Empty className="min-h-40 p-4 md:p-6">
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <SlidersHorizontal />
              </EmptyMedia>
              <EmptyTitle>未选择追番</EmptyTitle>
              <EmptyDescription>选择一部番剧编辑规则，或添加新的追番。</EmptyDescription>
            </EmptyHeader>
          </Empty>
        </CardContent>
      </Card>
    );
  }

  if (activeTab === "rss") {
    return <RssSubscriptionsEditor draft={draft} onChange={onChange} />;
  }

  return (
    <Card className="border-0 shadow-none">
      <CardHeader className="px-0 pt-0">
        <CardTitle>{activeTab === "basic" ? "基础信息" : "下载偏好"}</CardTitle>
        <CardDescription>
          {activeTab === "basic" ? "维护标题、首播时间和追番状态。" : "设置自动下载、字幕组与技术规格偏好。"}
        </CardDescription>
      </CardHeader>
      <CardContent>
        <FieldGroup className="gap-4">
          {activeTab === "basic" ? (
            <>
          <TextField
            label="番剧名称"
            value={draft.anime.title}
            onChange={(value) =>
              onChange({
                ...draft,
                anime: {
                  ...draft.anime,
                  title: value
                }
              })
            }
          />
          <TextField
            label="原语言标题"
            value={draft.anime.originalTitle ?? ""}
            onChange={(value) =>
              onChange({
                ...draft,
                anime: {
                  ...draft.anime,
                  originalTitle: value
                }
              })
            }
          />
          <div className="grid gap-3 sm:grid-cols-2">
            <NumberField
              label="首播年份"
              value={draft.anime.premiereYear}
              min={1970}
              onChange={(value) =>
                onChange({
                  ...draft,
                  anime: {
                    ...draft.anime,
                    premiereYear: value
                  }
                })
              }
            />
            <NumberField
              label="首播月份"
              value={draft.anime.premiereMonth}
              min={1}
              max={12}
              onChange={(value) =>
                onChange({
                  ...draft,
                  anime: {
                    ...draft.anime,
                    premiereMonth: clampMonth(value)
                  }
                })
              }
            />
          </div>
          <TextareaField
            label="搜索别名"
            value={draft.anime.aliases.map((alias) => alias.alias).join("\n")}
            onChange={(value) =>
              onChange({
                ...draft,
                anime: {
                  ...draft.anime,
                  aliases: value
                    .split("\n")
                    .map((item) => item.trim())
                    .filter(Boolean)
                    .map((alias, index) => ({
                      id: `${draft.anime.id}-alias-${index + 1}`,
                      animeId: draft.anime.id,
                      alias,
                      language: "custom",
                      priority: 50 - index
                    }))
                }
              })
            }
          />
          <SelectField
            label="状态"
            value={draft.status}
            options={statusOptions.map(([value, label]) => ({ value, label }))}
            onChange={(value) => {
              const status = value as AnimeStatus;
              onChange({
                ...draft,
                status,
                autoDownload: canAnimeStatusAutoDownload(status) ? draft.autoDownload : false
              });
            }}
          />
            </>
          ) : (
            <>
          <SelectField
            label="默认字幕组"
            value={draft.defaultFansubGroupId ?? ""}
            options={[
              { value: "", label: "未设置" },
              ...fansubs.map((group) => ({
                value: group.id,
                label: group.name
              }))
            ]}
            onChange={(value) =>
              onChange({
                ...draft,
                defaultFansubGroupId: value || undefined
              })
            }
          />
          <SelectField
            label="自动下载"
            value={draft.autoDownload ? "on" : "off"}
            options={[
              { value: "on", label: "开启" },
              { value: "off", label: "关闭" }
            ]}
            disabled={!canAnimeStatusAutoDownload(draft.status)}
            onChange={(value) =>
              onChange({
                ...draft,
                autoDownload: value === "on"
              })
            }
          />
          <div className="grid gap-3 sm:grid-cols-2">
            <SelectField
              label="偏好分辨率"
              value={draft.preferredResolution ?? ""}
              options={resolutionOptions.map((value) => ({
                value,
                label: value || "不限"
              }))}
              onChange={(value) =>
                onChange({
                  ...draft,
                  preferredResolution: (value || undefined) as MyAnime["preferredResolution"]
                })
              }
            />
            <SelectField
              label="偏好编码"
              value={draft.preferredCodec ?? ""}
              options={codecOptions.map((value) => ({
                value,
                label: value || "不限"
              }))}
              onChange={(value) =>
                onChange({
                  ...draft,
                  preferredCodec: (value || undefined) as MyAnime["preferredCodec"]
                })
              }
            />
          </div>
          <div className="grid gap-3 sm:grid-cols-2">
            <SelectField
              label="偏好位深"
              value={draft.preferredBitDepth ? String(draft.preferredBitDepth) : ""}
              options={bitDepthOptions.map((value) => ({
                value: value ? String(value) : "",
                label: value ? `${value}bit` : "不限"
              }))}
              onChange={(value) =>
                onChange({
                  ...draft,
                  preferredBitDepth: value ? Number(value) as VideoBitDepth : undefined
                })
              }
            />
            <SubtitleLanguageToggleField
              label="偏好字幕语言（可多选）"
              value={resolveSubtitleLanguages(draft.preferredSubtitleLanguages, draft.preferredSubtitle)}
              onChange={(value) =>
                onChange({
                  ...draft,
                  preferredSubtitleLanguages: value,
                  preferredSubtitle: undefined
                })
              }
            />
          </div>
          {allowLocalPathRules && (
            <TextField
              label="下载目录覆盖"
              value={draft.downloadDir ?? ""}
              onChange={(value) =>
                onChange({
                  ...draft,
                  downloadDir: value || undefined
                })
              }
            />
          )}
            </>
          )}
        </FieldGroup>
      </CardContent>
    </Card>
  );
}

function RssSubscriptionsEditor({
  draft,
  onChange
}: {
  draft: MyAnime;
  onChange: (item: MyAnime | null) => void;
}) {
  const subscriptions = draft.rssSubscriptions ?? [];
  const mikanRssUrl = buildMikanRssUrl(draft);

  /** 更新追番草稿中的 RSS 订阅数组。 */
  function updateSubscriptions(next: AnimeRssSubscription[]) {
    onChange({
      ...draft,
      rssSubscriptions: next
    });
  }

  /** 新增一条空 RSS 订阅。 */
  function addSubscription(initial?: Partial<AnimeRssSubscription>) {
    const now = new Date().toISOString();
    updateSubscriptions([
      ...subscriptions,
      {
        id: createId("rss"),
        myAnimeId: draft.id,
        name: initial?.name ?? "RSS订阅",
        url: initial?.url ?? "",
        enabled: initial?.enabled ?? true,
        preferredSubtitleLanguages: initial?.preferredSubtitleLanguages,
        preferredSubtitle: initial?.preferredSubtitle,
        refreshIntervalMinutes: initial?.refreshIntervalMinutes ?? defaultRssRefreshIntervalMinutes,
        createdAt: now,
        updatedAt: now
      }
    ]);
  }

  /** 更新单条 RSS 订阅。 */
  function updateSubscription(id: string, patch: Partial<AnimeRssSubscription>) {
    const now = new Date().toISOString();
    updateSubscriptions(
      subscriptions.map((subscription) =>
        subscription.id === id
          ? {
              ...subscription,
              ...patch,
              updatedAt: now
            }
          : subscription
      )
    );
  }

  /** 删除单条 RSS 订阅。 */
  function removeSubscription(id: string) {
    updateSubscriptions(subscriptions.filter((subscription) => subscription.id !== id));
  }

  return (
    <FieldSet className="gap-4 rounded-md border p-3">
      <FieldLegend className="mb-0">RSS订阅</FieldLegend>
      <div className="flex min-w-0 flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <FieldDescription>可为同一番剧配置多个 RSS 源。</FieldDescription>
        <div className="flex w-full shrink-0 gap-2 sm:w-auto">
          {mikanRssUrl && (
            <Button
              className="min-h-11 min-w-0 flex-1 px-2 sm:min-h-9 sm:flex-none sm:px-3"
              type="button"
              variant="outline"
              onClick={() => addSubscription({ name: "蜜柑计划", url: mikanRssUrl })}
              disabled={subscriptions.some((subscription) => subscription.url === mikanRssUrl)}
            >
              <Rss data-icon="inline-start" />
              蜜柑RSS
            </Button>
          )}
          <Button
            className="min-h-11 min-w-0 flex-1 px-2 sm:min-h-9 sm:flex-none sm:px-3"
            type="button"
            variant="outline"
            onClick={() => addSubscription()}
          >
            <Plus data-icon="inline-start" />
            添加
          </Button>
        </div>
      </div>

      {subscriptions.length > 0 ? (
        <FieldGroup className="gap-3">
          {subscriptions.map((subscription) => (
            <FieldGroup
              className="grid min-w-0 gap-3 rounded-md bg-muted/40 p-3 md:grid-cols-2 xl:grid-cols-3 xl:items-end"
              key={subscription.id}
            >
              <Field orientation="horizontal" className="min-w-0">
                <Checkbox
                  id={`rss-enabled-${subscription.id}`}
                  checked={subscription.enabled}
                  onCheckedChange={(checked) => updateSubscription(subscription.id, { enabled: checked === true })}
                />
                <FieldLabel htmlFor={`rss-enabled-${subscription.id}`}>启用</FieldLabel>
              </Field>
              <Field className="min-w-0">
                <FieldLabel className="sr-only" htmlFor={`rss-name-${subscription.id}`}>订阅名称</FieldLabel>
                <Input
                  id={`rss-name-${subscription.id}`}
                  placeholder="订阅名称"
                  value={subscription.name}
                  onChange={(event) => updateSubscription(subscription.id, { name: event.target.value })}
                />
              </Field>
              <Field className="min-w-0">
                <FieldLabel className="sr-only" htmlFor={`rss-url-${subscription.id}`}>RSS 地址</FieldLabel>
                <Input
                  id={`rss-url-${subscription.id}`}
                  placeholder="RSS 地址"
                  title={subscription.url}
                  value={subscription.url}
                  onChange={(event) => updateSubscription(subscription.id, { url: event.target.value })}
                />
              </Field>
              <SubtitleLanguageToggleField
                label="RSS字幕（留空继承追番）"
                value={resolveSubtitleLanguages(
                  subscription.preferredSubtitleLanguages,
                  subscription.preferredSubtitle
                )}
                onChange={(value) =>
                  updateSubscription(subscription.id, {
                    preferredSubtitleLanguages: value.length > 0 ? value : undefined,
                    preferredSubtitle: undefined
                  })
                }
              />
              <Field className="min-w-0">
                <FieldLabel htmlFor={`rss-interval-${subscription.id}`}>刷新间隔（分钟）</FieldLabel>
                <Input
                  id={`rss-interval-${subscription.id}`}
                  min={1}
                  type="number"
                  value={subscription.refreshIntervalMinutes ?? defaultRssRefreshIntervalMinutes}
                  onChange={(event) =>
                    updateSubscription(subscription.id, {
                      refreshIntervalMinutes: normalizeRssRefreshInterval(Number(event.target.value))
                    })
                  }
                />
              </Field>
              <Button
                className="min-h-11 w-full xl:min-h-9"
                type="button"
                variant="ghost"
                onClick={() => removeSubscription(subscription.id)}
                aria-label="删除RSS订阅"
                title="删除RSS订阅"
              >
                <Trash2 data-icon="inline-start" />
                删除
              </Button>
            </FieldGroup>
          ))}
        </FieldGroup>
      ) : (
        <Empty className="min-h-36 p-4">
          <EmptyHeader>
            <EmptyMedia variant="icon">
              <Rss />
            </EmptyMedia>
            <EmptyTitle>未配置 RSS 订阅</EmptyTitle>
            <EmptyDescription>添加订阅后可从指定 RSS 获取发布资源。</EmptyDescription>
          </EmptyHeader>
        </Empty>
      )}
    </FieldSet>
  );
}

function AnimeDownloadPanel({
  target,
  releases,
  rssGroups,
  errors,
  downloadTasks,
  fansubs,
  fansubNames,
  activeTab,
  selectedFansubId,
  loading,
  resolved,
  rssLoading,
  rssResolved,
  addingReleaseId,
  batchAdding,
  sourceBindingState,
  sourceBindingLoading,
  sourceBindingActionKey,
  onTabChange,
  onConfirmSourceCandidate,
  onRejectSourceCandidate,
  onSetSourceExcluded,
  onRemoveSourceBinding,
  onRefreshSourceBindings,
  onFansubChange,
  onRefreshRss,
  onRefresh,
  onForceRefresh,
  onCancel,
  onAddRelease,
  onAddRssSubscription,
  onAddSelected
}: {
  target: MyAnime;
  releases: Release[];
  rssGroups: RssReleaseGroupState[];
  errors: ReleaseSearchResult["errors"];
  downloadTasks: DownloadTask[];
  fansubs: FansubGroup[];
  fansubNames: Map<string, string>;
  activeTab: DownloadResourceTab;
  selectedFansubId: string;
  loading: boolean;
  resolved: boolean;
  rssLoading: boolean;
  rssResolved: boolean;
  addingReleaseId: string | null;
  batchAdding: boolean;
  sourceBindingState: AnimeSourceBindingState | null;
  sourceBindingLoading: boolean;
  sourceBindingActionKey: string | null;
  onTabChange: (tab: DownloadResourceTab) => void;
  onConfirmSourceCandidate: (candidate: AnimeSourceCandidate) => void;
  onRejectSourceCandidate: (candidate: AnimeSourceCandidate) => void;
  onSetSourceExcluded: (sourceId: string, excluded: boolean) => void;
  onRemoveSourceBinding: (sourceId: string) => void;
  onRefreshSourceBindings: () => void;
  onFansubChange: (fansubGroupId: string) => void;
  onRefreshRss: () => void;
  onRefresh: () => void;
  onForceRefresh: () => void;
  onCancel: () => void;
  onAddRelease: (release: Release) => void;
  onAddRssSubscription: (subscription: RssSubscriptionDraft) => void;
  onAddSelected: (releases: Release[]) => void;
}) {
  const [groupCollapseOverrides, setGroupCollapseOverrides] = useState<Record<string, boolean>>({});
  const [collectionResourcesOpen, setCollectionResourcesOpen] = useState(true);
  const [otherResourcesCollapsed, setOtherResourcesCollapsed] = useState(true);
  const [selectedRssSubscriptionId, setSelectedRssSubscriptionId] = useState("");
  const [selectedFamilyKeys, setSelectedFamilyKeys] = useState<Set<string>>(() => new Set());
  const [releaseVersionSelections, setReleaseVersionSelections] = useState<Record<string, string>>({});
  const activeRssGroup = rssGroups.find((group) => group.subscription.id === selectedRssSubscriptionId) ?? rssGroups[0];
  const rssReleases = activeRssGroup?.releases ?? [];
  const tabReleases = activeTab === "rss" ? rssReleases : releases;
  const currentTabReleases = tabReleases.filter((release) => classifyAnimeRelease(release, target.anime) === "current");
  const otherTabReleases = tabReleases.filter((release) => classifyAnimeRelease(release, target.anime) === "other");
  const visibleReleases = activeTab === "rss"
    ? currentTabReleases
    : filterReleasesByFansub(currentTabReleases, selectedFansubId);
  const visibleOtherReleases = activeTab === "rss"
    ? otherTabReleases
    : filterReleasesByFansub(otherTabReleases, selectedFansubId);
  const visibleCollectionReleases = [
    ...visibleReleases.filter(isCollectionRelease),
    ...visibleOtherReleases.filter(isCollectionRelease)
  ];
  const visibleEpisodeReleases = visibleReleases.filter((release) => !isCollectionRelease(release));
  const visibleOtherEpisodeReleases = visibleOtherReleases.filter((release) => !isCollectionRelease(release));
  const releaseGroups = groupReleasesByFansub(visibleEpisodeReleases, fansubNames);
  const visibleErrors = activeTab === "rss"
    ? dedupeReleaseErrors(activeRssGroup?.errors ?? [])
    : dedupeReleaseErrors(errors);
  const unknownFansubCount = tabReleases.filter((release) => !release.fansubGroupId).length;
  const activeLoading = activeTab === "rss" ? rssLoading : loading;
  const activeResolved = activeTab === "rss" ? rssResolved : resolved;
  const sourceFailed = currentTabReleases.length === 0 && otherTabReleases.length === 0 && visibleErrors.length > 0;
  const linkedTasks = downloadTasks.filter((task) => task.animeId === target.anime.id);
  const releaseSignature = tabReleases.map(releaseKey).join("|");
  const tabFamilies = groupReleaseVersions(currentTabReleases, target, releaseVersionSelections);
  const visibleFamilies = groupReleaseVersions(visibleReleases, target, releaseVersionSelections);
  const visibleCollectionFamilies = groupReleaseVersions(
    visibleCollectionReleases,
    target,
    releaseVersionSelections
  );
  const visibleEpisodeFamilies = groupReleaseVersions(
    visibleEpisodeReleases,
    target,
    releaseVersionSelections
  );
  const visibleOtherFamilies = groupReleaseVersions(visibleOtherEpisodeReleases, target, releaseVersionSelections);
  const selectedReleases = visibleFamilies
    .filter((family) => selectedFamilyKeys.has(family.key))
    .map((family) => family.selectedRelease);
  const selectedDownloadableReleases = selectedReleases.filter((release) => isReleaseSelectable(release, linkedTasks, target.anime));
  const selectableVisibleFamilies = visibleFamilies.filter((family) => isReleaseSelectable(family.selectedRelease, linkedTasks, target.anime));
  const allSelectableVisibleSelected = selectableVisibleFamilies.length > 0 &&
    selectableVisibleFamilies.every((family) => selectedFamilyKeys.has(family.key));
  const existingRssUrls = new Set((target.rssSubscriptions ?? []).map((subscription) => subscription.url.trim()).filter(Boolean));

  useEffect(() => {
    setSelectedFamilyKeys(new Set());
    setReleaseVersionSelections({});
  }, [activeTab, releaseSignature]);

  useEffect(() => {
    if (rssGroups.length === 0) {
      setSelectedRssSubscriptionId("");
      return;
    }
    if (!rssGroups.some((group) => group.subscription.id === selectedRssSubscriptionId)) {
      setSelectedRssSubscriptionId(rssGroups[0].subscription.id);
    }
  }, [rssGroups, selectedRssSubscriptionId]);

  /** 返回分组折叠状态；首次仅展开当前列表第一组。 */
  function isGroupCollapsed(groupKey: string, groupIndex: number): boolean {
    return groupCollapseOverrides[groupKey] ?? groupIndex > 0;
  }

  /** 保存用户对字幕组资源分组的展开或折叠选择。 */
  function toggleGroup(groupKey: string, collapsed: boolean) {
    setGroupCollapseOverrides((current) => ({
      ...current,
      [groupKey]: !collapsed
    }));
  }

  /** 切换某个资源族的批量选择状态。 */
  function toggleFamilySelection(family: ReleaseVersionFamily) {
    setSelectedFamilyKeys((current) => {
      const next = new Set(current);
      if (next.has(family.key)) {
        next.delete(family.key);
      } else {
        next.add(family.key);
      }
      return next;
    });
  }

  /** 变更同一资源族内最终下载的语言版本。 */
  function selectReleaseVersion(familyKey: string, nextReleaseKey: string) {
    setReleaseVersionSelections((current) => ({
      ...current,
      [familyKey]: nextReleaseKey
    }));
  }

  /** 选择或取消当前筛选下所有可下载资源。 */
  function toggleAllVisibleReleases() {
    setSelectedFamilyKeys((current) => {
      const next = new Set(current);
      const selectableKeys = selectableVisibleFamilies.map((family) => family.key);
      const allSelected = selectableKeys.length > 0 && selectableKeys.every((key) => next.has(key));
      for (const key of selectableKeys) {
        if (allSelected) {
          next.delete(key);
        } else {
          next.add(key);
        }
      }
      return next;
    });
  }

  /** 选择或取消指定分组下所有可下载资源。 */
  function toggleGroupFamilies(families: ReleaseVersionFamily[]) {
    const selectableKeys = families
      .filter((family) => isReleaseSelectable(family.selectedRelease, linkedTasks, target.anime))
      .map((family) => family.key);
    setSelectedFamilyKeys((current) => {
      const next = new Set(current);
      const allSelected = selectableKeys.length > 0 && selectableKeys.every((key) => next.has(key));
      for (const key of selectableKeys) {
        if (allSelected) {
          next.delete(key);
        } else {
          next.add(key);
        }
      }
      return next;
    });
  }

  /** 统计指定分组的可选和已选资源数量。 */
  function getGroupSelectionState(families: ReleaseVersionFamily[]) {
    const selectable = families.filter((family) => isReleaseSelectable(family.selectedRelease, linkedTasks, target.anime));
    const selectedCount = selectable.filter((family) => selectedFamilyKeys.has(family.key)).length;
    return {
      selectableCount: selectable.length,
      selectedCount,
      allSelected: selectable.length > 0 && selectedCount === selectable.length
    };
  }

  /** 按集数渲染资源族，并保留批量选择能力。 */
  function renderEpisodeGroups(groupReleases: Release[], batchSelectable = true) {
    const families = groupReleaseVersions(groupReleases, target, releaseVersionSelections);
    return groupReleaseFamilyEpisodes(families).map((episodeGroup) => (
      <section key={episodeGroup.key}>
        <div className="flex min-h-9 items-center justify-between bg-primary/5 px-3 py-2 text-[11px] uppercase tracking-[0.04em]">
          <span className="font-semibold" title={episodeGroup.label}>
            {episodeGroup.label}
          </span>
          <span className="flex items-center gap-2 text-muted-foreground">
            {episodeGroup.families.length} 个资源
            <SlidersHorizontal aria-hidden="true" className="size-3.5" />
          </span>
        </div>
        <div className="divide-y border-t border-primary/10">
          {episodeGroup.families.map((family) => {
            const linkedTask = findReleaseDownloadTask(linkedTasks, family.selectedRelease);
            return (
              <ReleaseDownloadRow
                key={family.key}
                addingReleaseId={addingReleaseId}
                batchSelectable={batchSelectable}
                fansubNames={fansubNames}
                family={family}
                linkedTask={linkedTask}
                preferences={target}
                selected={selectedFamilyKeys.has(family.key)}
                onAddRelease={onAddRelease}
                onToggleSelected={toggleFamilySelection}
                onVersionChange={selectReleaseVersion}
              />
            );
          })}
        </div>
      </section>
    ));
  }

  /** 在普通单集资源之前渲染合集资源。 */
  function renderCollectionResources() {
    if (visibleCollectionFamilies.length === 0) {
      return null;
    }

    return (
      <Collapsible open={collectionResourcesOpen} onOpenChange={setCollectionResourcesOpen} asChild>
        <section className="shrink-0 overflow-hidden border border-primary/20 bg-background">
          <CollapsibleTrigger asChild>
            <Button className="h-auto min-h-11 w-full justify-between rounded-none px-3 py-2" type="button" variant="ghost">
              <span className="flex min-w-0 items-center gap-2">
                {collectionResourcesOpen ? <ChevronDown data-icon="inline-start" /> : <ChevronRight data-icon="inline-start" />}
                <span className="font-semibold">合集资源</span>
              </span>
              <Badge tone="primary-soft">{visibleCollectionFamilies.length} 个资源</Badge>
            </Button>
          </CollapsibleTrigger>
          <CollapsibleContent>
            <div className="divide-y border-t border-primary/10">
              {visibleCollectionFamilies.map((family) => (
                <ReleaseDownloadRow
                  key={family.key}
                  addingReleaseId={addingReleaseId}
                  batchSelectable={classifyAnimeRelease(family.selectedRelease, target.anime) === "current"}
                  fansubNames={fansubNames}
                  family={family}
                  linkedTask={findReleaseDownloadTask(linkedTasks, family.selectedRelease)}
                  preferences={target}
                  selected={selectedFamilyKeys.has(family.key)}
                  onAddRelease={onAddRelease}
                  onToggleSelected={toggleFamilySelection}
                  onVersionChange={selectReleaseVersion}
                />
              ))}
            </div>
          </CollapsibleContent>
        </section>
      </Collapsible>
    );
  }

  return (
      <div className="flex min-h-0 flex-1 flex-col gap-2 [@media(max-height:760px)]:gap-1">
        <div className="shrink-0 border-b pb-1">
          <Tabs value={activeTab} onValueChange={(value) => onTabChange(value as DownloadResourceTab)}>
            <TabsList className="grid w-full max-w-72 grid-cols-2" variant="line">
              <TabsTrigger value="rss">RSS 订阅</TabsTrigger>
              <TabsTrigger value="search">资源搜索</TabsTrigger>
            </TabsList>
          </Tabs>
        </div>

        {activeTab === "rss" ? (
          <RssResourceToolbar
            activeGroup={activeRssGroup}
            groups={rssGroups}
            loading={rssLoading}
            selectedId={activeRssGroup?.subscription.id ?? ""}
            onRefresh={onRefreshRss}
            onSelect={setSelectedRssSubscriptionId}
          />
        ) : (
          <>
            <AnimeSourceBindingPanel
              actionKey={sourceBindingActionKey}
              loading={sourceBindingLoading}
              state={sourceBindingState}
              onConfirm={onConfirmSourceCandidate}
              onReject={onRejectSourceCandidate}
              onSetSourceExcluded={onSetSourceExcluded}
              onRefresh={onRefreshSourceBindings}
              onRemove={onRemoveSourceBinding}
            />

          <div className="shrink-0 border-y border-primary/10 bg-background px-3 py-2">
            <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
              <Field className="min-w-0 sm:w-52 sm:flex-none">
                <FieldLabel className="sr-only" htmlFor="anime-release-fansub-filter">字幕组筛选</FieldLabel>
                <Select
                  value={selectedFansubId || emptySelectValue}
                  onValueChange={(value) => onFansubChange(value === emptySelectValue ? "" : value)}
                >
                  <SelectTrigger className="h-8" id="anime-release-fansub-filter">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectGroup>
                      <SelectItem value={emptySelectValue}>全部字幕组（{tabReleases.length}）</SelectItem>
                      {fansubs.map((group) => {
                        const count = countReleasesByFansub(tabReleases, group.id);
                        return (
                          <SelectItem key={group.id} value={group.id}>
                            {group.name}{count > 0 ? `（${count}）` : ""}
                          </SelectItem>
                        );
                      })}
                      {unknownFansubCount > 0 && (
                        <SelectItem value={unknownFansubFilter}>未识别字幕组（{unknownFansubCount}）</SelectItem>
                      )}
                    </SelectGroup>
                  </SelectContent>
                </Select>
              </Field>
              <div className="grid grid-cols-2 gap-1 sm:flex">
                <Button className="min-h-9 shrink-0 px-2 text-xs" variant="ghost" onClick={onRefresh} disabled={loading}>
                  <RefreshCw data-icon="inline-start" className={cn(loading && "animate-spin")} />
                  {loading ? "查询中" : "刷新"}
                </Button>
                <Button
                  className="min-h-9 shrink-0 px-2 text-xs text-destructive hover:text-destructive"
                  variant="ghost"
                  onClick={onForceRefresh}
                  disabled={loading}
                  aria-label="强制刷新"
                  title="绕过 1 天缓存重新查询下载源"
                >
                  <AlertTriangle data-icon="inline-start" />
                  <span className="hidden sm:inline">强制刷新</span>
                </Button>
              </div>
            </div>
            <div className="mt-2 flex flex-col gap-2 border-t pt-2 text-xs text-muted-foreground sm:flex-row sm:items-center sm:justify-between">
              <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                <span>显示 <strong className="text-foreground">{visibleFamilies.length}</strong> 组</span>
                <span>·</span>
                <span>共 <strong className="text-foreground">{tabFamilies.length}</strong> 组</span>
                <span>·</span>
                <span>已选 <strong className="text-primary">{selectedDownloadableReleases.length}</strong> 组</span>
              </div>
              <Button
                className="w-full sm:w-auto"
                variant="outline"
                onClick={toggleAllVisibleReleases}
                disabled={selectableVisibleFamilies.length === 0 || activeLoading}
              >
                {allSelectableVisibleSelected ? "取消选择" : "全选可下载"}
              </Button>
            </div>
          </div>
          </>
        )}

        {activeLoading && activeResolved && (
          <p aria-live="polite" className="shrink-0 text-xs text-muted-foreground" role="status">
            正在更新资源，当前结果会保留到刷新完成。
          </p>
        )}

        {activeTab === "search" && visibleErrors.length > 0 && (
          <div className="flex flex-col gap-2">
            {visibleErrors.slice(0, 3).map((error, index) => (
              <Alert key={`${error.sourceId}-${index}`}>
                <AlertTitle>{error.sourceId}</AlertTitle>
                <AlertDescription>{error.message}</AlertDescription>
              </Alert>
            ))}
          </div>
        )}

        {activeLoading && !activeResolved ? (
          <div className="flex flex-col gap-3 py-2" aria-busy="true">
            <Skeleton className="h-16 w-full" />
            <Skeleton className="h-16 w-full" />
            <span className="sr-only">{activeTab === "rss" ? "正在读取 RSS 订阅" : "正在查询发布资源"}</span>
          </div>
        ) : (
          <div className="grid min-h-0 flex-1 auto-rows-max content-start gap-3 overflow-y-auto pr-1">
            {renderCollectionResources()}

            {activeTab === "rss"
              ? visibleEpisodeFamilies.length > 0 && (
                  <section className="shrink-0">
                    <h3 className="flex items-center gap-2 border-b pb-3 text-sm font-semibold">
                      <CalendarDays />历史发布
                    </h3>
                    <div className="mt-3 divide-y border-y bg-background">
                      {visibleEpisodeFamilies.map((family) => (
                        <ReleaseDownloadRow
                          key={family.key}
                          addingReleaseId={addingReleaseId}
                          batchSelectable
                          fansubNames={fansubNames}
                          family={family}
                          linkedTask={findReleaseDownloadTask(linkedTasks, family.selectedRelease)}
                          preferences={target}
                          selected={selectedFamilyKeys.has(family.key)}
                          onAddRelease={onAddRelease}
                          onToggleSelected={toggleFamilySelection}
                          onVersionChange={selectReleaseVersion}
                        />
                      ))}
                    </div>
                  </section>
                )
              : releaseGroups.map((group, groupIndex) => {
                  const groupFamilies = groupReleaseVersions(group.releases, target, releaseVersionSelections);
                  const selection = getGroupSelectionState(groupFamilies);
                  const rssCandidate = buildMikanGroupRssSubscription(group, target);
                  const rssSubscribed = Boolean(rssCandidate && existingRssUrls.has(rssCandidate.url));
                  const collapsed = isGroupCollapsed(group.key, groupIndex);
                  return (
                    <section key={group.key} className="shrink-0 overflow-hidden border border-primary/15 bg-background">
                      <ReleaseGroupHeader
                        allSelected={selection.allSelected}
                        badgeText={`${groupFamilies.length} 个资源`}
                        collapsed={collapsed}
                        episodeCount={countReleaseFamilyEpisodes(groupFamilies)}
                        name={group.name}
                        rssCandidate={rssCandidate}
                        rssSubscribed={rssSubscribed}
                        selectableCount={selection.selectableCount}
                        selectedCount={selection.selectedCount}
                        title={group.name}
                        onAddRssSubscription={onAddRssSubscription}
                        onToggleCollapsed={() => toggleGroup(group.key, collapsed)}
                        onToggleSelected={() => toggleGroupFamilies(groupFamilies)}
                      />
                      {!collapsed && <div className="divide-y">{renderEpisodeGroups(group.releases)}</div>}
                    </section>
                  );
                })}

            {visibleReleases.length === 0 && visibleOtherReleases.length === 0 && (
              sourceFailed ? (
                <Alert variant="destructive">
                  <AlertTriangle />
                  <AlertTitle>资源获取失败</AlertTitle>
                  <AlertDescription>
                    {activeTab === "rss"
                      ? "RSS 订阅请求失败，暂时无法获取发布资源。"
                      : "下载源请求失败，暂时无法获取发布资源和字幕组文件信息。"}
                  </AlertDescription>
                </Alert>
              ) : (
                <Empty className="min-h-44 p-4 md:p-6">
                  <EmptyHeader>
                    <EmptyMedia variant="icon"><Search /></EmptyMedia>
                    <EmptyTitle>暂无可下载资源</EmptyTitle>
                    <EmptyDescription>
                      {selectedFansubId
                        ? "当前字幕组没有可下载资源。"
                        : activeTab === "rss"
                          ? "没有找到 RSS 订阅资源，或尚未配置启用的 RSS 订阅。"
                          : "没有找到可下载资源。"}
                    </EmptyDescription>
                  </EmptyHeader>
                </Empty>
              )
            )}

            {visibleOtherReleases.length > 0 && (
                <section className="shrink-0 overflow-hidden border border-primary/15 bg-background">
                <Button
                  className="h-auto min-h-11 w-full justify-between rounded-none px-3 py-2 text-left md:min-h-11"
                  type="button"
                  variant="secondary"
                  onClick={() => setOtherResourcesCollapsed((current) => !current)}
                >
                  <span className="flex min-w-0 items-center gap-2">
                    {otherResourcesCollapsed ? <ChevronRight className="h-4 w-4" /> : <ChevronDown className="h-4 w-4" />}
                    <span className="font-medium">其他资源</span>
                    <Badge>{visibleOtherFamilies.length} 个资源</Badge>
                  </span>
                  <span className="text-xs text-muted-foreground">季度待确认</span>
                </Button>
                {!otherResourcesCollapsed && (
                  <div className="divide-y border-t">{renderEpisodeGroups(visibleOtherEpisodeReleases, false)}</div>
                )}
              </section>
            )}

          </div>
        )}
        <BatchDownloadControls
          batchAdding={batchAdding}
          disabled={activeLoading}
          selectedCount={selectedDownloadableReleases.length}
          totalCount={tabFamilies.length}
          visibleCount={visibleFamilies.length}
          onCancel={onCancel}
          onAddSelected={() => onAddSelected(selectedDownloadableReleases)}
        />
      </div>
  );
}

/** 渲染 RSS 增强版来源选择、刷新动作与最近同步状态。 */
function RssResourceToolbar({
  groups,
  activeGroup,
  selectedId,
  loading,
  onSelect,
  onRefresh
}: {
  groups: RssReleaseGroupState[];
  activeGroup?: RssReleaseGroupState;
  selectedId: string;
  loading: boolean;
  onSelect: (id: string) => void;
  onRefresh: () => void;
}) {
  return (
    <div className="flex shrink-0 flex-col gap-3 border-y bg-card/50 px-3 py-3 sm:flex-row sm:items-end sm:justify-between">
      <div className="flex min-w-0 flex-1 flex-col gap-2 sm:flex-row sm:items-end">
        <Field className="min-w-0 sm:max-w-72 sm:flex-1">
          <FieldLabel htmlFor="anime-rss-source">RSS 来源</FieldLabel>
          <Select value={selectedId || undefined} onValueChange={onSelect} disabled={groups.length === 0}>
            <SelectTrigger id="anime-rss-source">
              <SelectValue placeholder="暂无 RSS 订阅" />
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                {groups.map((group) => (
                  <SelectItem key={group.subscription.id} value={group.subscription.id}>
                    {group.subscription.name}
                  </SelectItem>
                ))}
              </SelectGroup>
            </SelectContent>
          </Select>
        </Field>
        <Button variant="ghost" onClick={onRefresh} disabled={loading || groups.length === 0}>
          <RefreshCw data-icon="inline-start" className={cn(loading && "animate-spin")} />
          {loading ? "正在刷新" : "刷新 RSS"}
        </Button>
      </div>
      <div className="flex shrink-0 items-center gap-2 pb-2 text-xs text-success">
        <CheckCircle2 />
        <span>{activeGroup?.subscription.lastFetchedAt ? `最近同步：${formatReleaseDate(activeGroup.subscription.lastFetchedAt)}` : "尚未同步"}</span>
      </div>
    </div>
  );
}

/** 渲染资源列表顶部的整体批量选择与下载操作。 */
function BatchDownloadControls({
  visibleCount,
  totalCount,
  selectedCount,
  batchAdding,
  disabled,
  onCancel,
  onAddSelected
}: {
  visibleCount: number;
  totalCount: number;
  selectedCount: number;
  batchAdding: boolean;
  disabled: boolean;
  onCancel: () => void;
  onAddSelected: () => void;
}) {
  return (
    <div className="-mx-4 -mb-4 mt-auto flex shrink-0 flex-col items-stretch gap-2 border-t bg-background px-4 py-3 text-xs text-muted-foreground sm:-mx-6 sm:-mb-4 sm:flex-row sm:items-center sm:justify-between sm:px-6">
      <div>
        <div className="text-[10px] font-semibold uppercase">选择状态</div>
        <div className="mt-0.5 font-medium text-foreground">已选 {selectedCount} 项 <span className="font-normal text-muted-foreground">（显示 {visibleCount} / 共 {totalCount} 组）</span></div>
      </div>
      <div className="grid w-full grid-cols-2 gap-2 sm:flex sm:w-auto">
        <Button
          className="min-h-11 px-2 sm:min-h-9 sm:px-3"
          variant="outline"
          onClick={onCancel}
        >
          取消
        </Button>
        <Button className="min-h-11 sm:min-h-9" onClick={onAddSelected} disabled={selectedCount === 0 || batchAdding || disabled}>
          <Download data-icon="inline-start" />
          {batchAdding ? `正在添加 ${selectedCount} 项` : "批量下载"}
        </Button>
      </div>
    </div>
  );
}

/** 渲染资源分组标题，并承载分组全选和可用 RSS 订阅操作。 */
function ReleaseGroupHeader({
  name,
  title,
  badgeText,
  episodeCount,
  selectedCount,
  selectableCount,
  allSelected,
  collapsed,
  rssCandidate,
  rssSubscribed,
  onToggleCollapsed,
  onToggleSelected,
  onAddRssSubscription
}: {
  name: string;
  title: string;
  badgeText: string;
  episodeCount: number;
  selectedCount: number;
  selectableCount: number;
  allSelected: boolean;
  collapsed: boolean;
  rssCandidate?: RssSubscriptionDraft;
  rssSubscribed: boolean;
  onToggleCollapsed: () => void;
  onToggleSelected: () => void;
  onAddRssSubscription: (subscription: RssSubscriptionDraft) => void;
}) {
  return (
    <div className="flex min-h-11 w-full flex-wrap items-center gap-2 border-b border-primary/15 bg-primary/10 px-3 py-2">
      <Button
        className="h-auto min-h-11 min-w-0 flex-1 justify-start px-0 py-0 text-left md:min-h-0"
        type="button"
        variant="ghost"
        onClick={onToggleCollapsed}
        aria-expanded={!collapsed}
        title={title}
      >
        {collapsed ? (
          <ChevronRight className="h-4 w-4 shrink-0 text-muted-foreground" />
        ) : (
          <ChevronDown className="h-4 w-4 shrink-0 text-muted-foreground" />
        )}
        <span className="truncate text-xs font-bold uppercase tracking-[0.04em]">{name}</span>
        <span className="text-[10px] font-normal text-muted-foreground">共 {badgeText} · 已选 {selectedCount} · {episodeCount} 集</span>
      </Button>
      <div className="ml-auto flex shrink-0 flex-wrap items-center justify-end gap-2">
        {rssCandidate && (
          <Button
            className="h-9 px-2 text-xs sm:h-7"
            variant="ghost"
            onClick={() => onAddRssSubscription(rssCandidate)}
            disabled={rssSubscribed}
            title={rssSubscribed ? "该字幕组 RSS 已订阅" : rssCandidate.url}
          >
            <Rss data-icon="inline-start" />
            {rssSubscribed ? "已订阅" : "订阅 RSS"}
          </Button>
        )}
        <Button
          className="h-9 px-2 text-xs sm:h-7"
          variant="outline"
          onClick={onToggleSelected}
          disabled={selectableCount === 0}
        >
          {allSelected ? "取消全选" : "全选"}
        </Button>
      </div>
    </div>
  );
}

/** 渲染合并后的资源族行，并提供语言版本选择。 */
function ReleaseDownloadRow({
  family,
  linkedTask,
  batchSelectable,
  fansubNames,
  preferences,
  selected,
  addingReleaseId,
  onToggleSelected,
  onAddRelease,
  onVersionChange
}: {
  family: ReleaseVersionFamily;
  linkedTask?: DownloadTask;
  batchSelectable: boolean;
  fansubNames: Map<string, string>;
  preferences: MyAnime;
  selected: boolean;
  addingReleaseId: string | null;
  onToggleSelected: (family: ReleaseVersionFamily) => void;
  onAddRelease: (release: Release) => void;
  onVersionChange: (familyKey: string, releaseKey: string) => void;
}) {
  const release = family.selectedRelease;
  const canDownload = Boolean(release.magnetUrl ?? release.torrentUrl);
  const selectable = canDownload && !linkedTask && batchSelectable;
  const canAddIndividually = canDownload && !linkedTask;
  const actionLabel = !batchSelectable
    ? "确认本条季度待确认资源并添加下载"
    : linkedTask
      ? "已加入下载"
      : "添加下载";

  return (
    <div className={cn("px-3 py-2.5 [@media(max-height:760px)]:py-2", (linkedTask || !batchSelectable) && "bg-muted/20")}>
      <div className="flex items-start justify-between gap-2 sm:gap-3">
        <Checkbox
          className="mt-1"
          aria-label={`选择资源 ${release.title}`}
          checked={selected}
          disabled={!selectable}
          onCheckedChange={() => onToggleSelected(family)}
        />
        <div className={cn("flex min-w-0 flex-1 flex-col gap-1.5", linkedTask && "opacity-60")}>
          <div className="min-w-0">
            <div className="line-clamp-2 text-[13px] font-medium leading-5" title={release.title}>
              {release.title}
            </div>
            <div className="mt-1 flex flex-wrap items-center gap-x-1.5 gap-y-1 text-[10px] uppercase tracking-[0.03em] text-muted-foreground">
              <span>{release.sourceName}</span><span>|</span>
              <span>{getReleaseFansubName(release, fansubNames)}</span><span>|</span>
              <span>{family.episodeLabel}</span>
              {release.resolution && <><span>|</span><span>{release.resolution}</span></>}
              {release.normalizedVideoCodec && release.normalizedVideoCodec !== "Unknown" && <><span>|</span><span>{release.normalizedVideoCodec}</span></>}
              {release.bitDepth && <><span>|</span><span>{release.bitDepth}BIT</span></>}
              {release.size && <><span>|</span><span>{formatBytes(release.size)}</span></>}
              <><span>|</span><span>{formatReleaseDate(release.publishedAt)}</span></>
              {release.seeders !== undefined && <><span>|</span><span className="text-success">{release.seeders} 做种</span></>}
              {!batchSelectable && <Badge className="h-5" tone="amber">季度待确认</Badge>}
              {linkedTask && (
                <Badge className="h-5" tone={isCompletedDownload(linkedTask) ? "green" : "amber"}>
                  {downloadStatusText[linkedTask.status]}
                </Badge>
              )}
            </div>
          </div>
          <div className="flex min-w-0 flex-col items-start gap-2 text-xs text-muted-foreground sm:flex-row sm:flex-wrap sm:items-center">
            {family.releases.length > 1 && (
              <Select
                value={releaseKey(release)}
                onValueChange={(value) => onVersionChange(family.key, value)}
              >
                <SelectTrigger className="h-8 w-full min-w-0 border-primary/15 bg-background text-[11px] sm:w-72" aria-label="选择资源版本" title="选择资源版本">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    {family.releases.map((item) => {
                      const itemKey = releaseKey(item);
                      return (
                        <SelectItem key={itemKey} value={itemKey}>
                          {getReleaseVersionLabel(item, preferences, releaseKey(item) === releaseKey(release))}
                        </SelectItem>
                      );
                    })}
                  </SelectGroup>
                </SelectContent>
              </Select>
            )}
          </div>
        </div>
        <Button
          className="min-h-9 shrink-0 border-primary/20 bg-primary/10 px-2 text-primary hover:bg-primary/20 sm:px-3"
          variant="outline"
          onClick={() => onAddRelease(release)}
          disabled={!canAddIndividually || addingReleaseId === release.id || addingReleaseId === batchAddingReleaseId}
          aria-label={actionLabel}
          title={actionLabel}
        >
          <Download data-icon="inline-start" />
          <span className="hidden sm:inline">
            {linkedTask
              ? "已加入"
              : addingReleaseId === release.id
                ? "添加中"
                : batchSelectable
                  ? "添加下载"
                  : "确认下载"}
          </span>
        </Button>
      </div>
    </div>
  );
}

/** 展示精确下载源的绑定状态和待确认候选。 */
function AnimeSourceBindingPanel({
  state,
  loading,
  actionKey,
  onConfirm,
  onReject,
  onSetSourceExcluded,
  onRemove,
  onRefresh
}: {
  state: AnimeSourceBindingState | null;
  loading: boolean;
  actionKey: string | null;
  onConfirm: (candidate: AnimeSourceCandidate) => void;
  onReject: (candidate: AnimeSourceCandidate) => void;
  onSetSourceExcluded: (sourceId: string, excluded: boolean) => void;
  onRemove: (sourceId: string) => void;
  onRefresh: () => void;
}) {
  const groupedCandidates = groupSourceCandidates(
    state?.candidates ?? [],
    state?.excludedSources ?? []
  );
  const pendingGroupCount = groupedCandidates.filter((group) => !group.excluded).length;
  const excludedGroupCount = groupedCandidates.filter((group) => group.excluded).length;
  const confirmedBindings = state?.bindings.filter((binding) => binding.confirmed) ?? [];
  const hasContent = Boolean(confirmedBindings.length || groupedCandidates.length || state?.errors.length);
  const [expanded, setExpanded] = useState(false);

  useEffect(() => {
    if (!loading && confirmedBindings.length === 0 && (groupedCandidates.length > 0 || state?.errors.length)) {
      setExpanded(true);
    }
  }, [confirmedBindings.length, groupedCandidates.length, loading, state?.errors.length]);

  return (
    <section className="shrink-0 overflow-hidden border-y border-primary/15 bg-primary/[0.03]">
      <div className={cn("flex min-h-8 items-center justify-between bg-primary/5 px-1", expanded && "border-b border-primary/15")}>
        <Button
          className="h-auto min-h-8 min-w-0 flex-1 justify-start gap-2 px-2 py-1 text-left md:min-h-8"
          type="button"
          variant="ghost"
          aria-expanded={expanded}
          onClick={() => setExpanded((current) => !current)}
        >
          <span className="shrink-0 text-[10px] font-semibold uppercase tracking-[0.1em]">Source Matching</span>
          <span className="flex min-w-0 flex-1 items-center gap-1 overflow-hidden">
            {confirmedBindings.slice(0, 2).map((binding) => (
              <Badge key={binding.sourceId} className="h-5 max-w-32 truncate px-1.5 text-[10px]" tone="green">
                {binding.sourceId} Bound
              </Badge>
            ))}
            {confirmedBindings.length > 2 && <Badge className="h-5 px-1.5 text-[10px]" tone="green">+{confirmedBindings.length - 2}</Badge>}
            {pendingGroupCount > 0 && <Badge className="h-5 px-1.5 text-[10px]" tone="amber">{pendingGroupCount} Pending</Badge>}
            {excludedGroupCount > 0 && <Badge className="h-5 px-1.5 text-[10px]">{excludedGroupCount} Excluded</Badge>}
            {!hasContent && !loading && <span className="truncate text-xs font-normal text-muted-foreground">暂无精确匹配</span>}
            {loading && <span className="truncate text-xs font-normal text-muted-foreground">读取中</span>}
          </span>
          {expanded ? (
            <ChevronDown className="size-4 shrink-0 text-muted-foreground" />
          ) : (
            <ChevronRight className="size-4 shrink-0 text-muted-foreground" />
          )}
        </Button>
        <Button className="size-8 min-h-8 px-1" variant="ghost" onClick={onRefresh} disabled={loading} title="重新读取来源候选" aria-label="重新读取来源候选">
          <RefreshCw className={cn("size-4", loading && "animate-spin")} />
        </Button>
      </div>
      {expanded && (loading && !hasContent ? (
        <div className="flex flex-col gap-2 px-3 py-4" aria-label="正在匹配来源番剧">
          <Skeleton className="h-4 w-40" />
          <Skeleton className="h-4 w-64 max-w-full" />
        </div>
      ) : hasContent ? (
        <div className="max-h-72 divide-y divide-primary/10 overflow-y-auto">
          {confirmedBindings.map((binding) => (
            <div key={binding.sourceId} className="flex items-center justify-between gap-3 px-3 py-2.5">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <Badge tone="green"><Check className="mr-1 h-3 w-3" />已绑定</Badge>
                  <span className="truncate text-sm font-medium">{binding.sourceAnimeTitle ?? binding.sourceId}</span>
                </div>
                <div className="mt-1 truncate text-xs text-muted-foreground">
                  {binding.sourceId} · ID {binding.sourceAnimeId} · {getBindingMethodText(binding.matchMethod)}
                </div>
              </div>
              <Button
                variant="ghost"
                onClick={() => onRemove(binding.sourceId)}
                disabled={actionKey === binding.sourceId}
                title="移除来源绑定"
                aria-label="移除来源绑定"
              >
                <Unlink className="h-4 w-4" />
              </Button>
            </div>
          ))}
          {groupedCandidates.map((group) => (
            <div key={group.sourceId} className="px-3 py-2.5">
              <div className="mb-2 flex items-center justify-between gap-2">
                <div className="text-xs font-semibold uppercase tracking-[0.05em]">{group.sourceName}</div>
                <div className="flex shrink-0 items-center gap-2">
                  <Field
                    className="w-auto gap-1.5"
                    data-disabled={actionKey === `source-exclusion:${group.sourceId}`}
                    orientation="horizontal"
                  >
                    <Checkbox
                      checked={group.excluded}
                      disabled={actionKey === `source-exclusion:${group.sourceId}`}
                      id={`source-exclusion-${group.sourceId}`}
                      onCheckedChange={(checked) => onSetSourceExcluded(group.sourceId, checked === true)}
                    />
                    <FieldLabel
                      className="cursor-pointer text-xs font-normal"
                      htmlFor={`source-exclusion-${group.sourceId}`}
                    >
                      此来源均不匹配
                    </FieldLabel>
                  </Field>
                  <Badge tone={group.excluded ? "neutral" : "amber"}>
                    {group.excluded ? "已排除" : "待确认"}
                  </Badge>
                </div>
              </div>
              {!group.excluded && <div className="flex flex-col gap-2">
                {group.candidates.slice(0, 1).map((candidate) => {
                  const candidateKey = `${candidate.sourceId}:${candidate.sourceAnimeId}`;
                  return (
                    <div key={candidateKey} className="flex items-center justify-between gap-3 border border-primary/10 bg-background px-3 py-2">
                      <div className="min-w-0">
                        <div className="truncate text-sm" title={candidate.title}>{candidate.title}</div>
                        <div className="mt-1 truncate text-xs text-muted-foreground">
                          ID {candidate.sourceAnimeId} · {candidate.reasons.join(" · ")}
                        </div>
                      </div>
                      <div className="flex shrink-0 items-center gap-1">
                        <Button
                          aria-label={`确认 ${candidate.title} 不匹配`}
                          className="px-2"
                          disabled={actionKey === candidateKey}
                          onClick={() => onReject(candidate)}
                          variant="ghost"
                        >
                          <CircleOff data-icon="inline-start" />
                          不匹配
                        </Button>
                        <Button
                          aria-label={`确认匹配 ${candidate.title}`}
                          disabled={actionKey === candidateKey}
                          onClick={() => onConfirm(candidate)}
                          variant="outline"
                        >
                          <Check data-icon="inline-start" />
                          {candidate.score} 分
                        </Button>
                      </div>
                    </div>
                  );
                })}
                {group.candidates.length > 1 && (
                  <details>
                    <summary className="cursor-pointer py-1 text-xs text-muted-foreground hover:text-foreground">
                      其他候选（{group.candidates.length - 1}）
                    </summary>
                    <div className="mt-2 flex flex-col gap-2">
                      {group.candidates.slice(1).map((candidate) => {
                        const candidateKey = `${candidate.sourceId}:${candidate.sourceAnimeId}`;
                        return (
                          <div key={candidateKey} className="flex items-center justify-between gap-3 border border-primary/10 bg-background px-3 py-2">
                            <div className="min-w-0">
                              <div className="truncate text-sm" title={candidate.title}>{candidate.title}</div>
                              <div className="mt-1 truncate text-xs text-muted-foreground">
                                ID {candidate.sourceAnimeId} · {candidate.reasons.join(" · ")}
                              </div>
                            </div>
                            <div className="flex shrink-0 items-center gap-1">
                              <Button
                                aria-label={`确认 ${candidate.title} 不匹配`}
                                className="px-2"
                                disabled={actionKey === candidateKey}
                                onClick={() => onReject(candidate)}
                                variant="ghost"
                              >
                                <CircleOff data-icon="inline-start" />
                                不匹配
                              </Button>
                              <Button
                                aria-label={`确认匹配 ${candidate.title}`}
                                disabled={actionKey === candidateKey}
                                onClick={() => onConfirm(candidate)}
                                variant="outline"
                              >
                                <Check data-icon="inline-start" />
                                {candidate.score} 分
                              </Button>
                            </div>
                          </div>
                        );
                      })}
                    </div>
                  </details>
                )}
              </div>}
            </div>
          ))}
          {state?.errors.map((error) => (
            <div key={`${error.sourceId}:${error.message}`} className="px-3 py-2">
              <Alert>
                <AlertTitle>{error.sourceId}</AlertTitle>
                <AlertDescription>{error.message}</AlertDescription>
              </Alert>
            </div>
          ))}
        </div>
      ) : (
        <div className="px-3 py-4 text-sm text-muted-foreground">没有启用支持精确匹配的来源。</div>
      ))}
    </section>
  );
}

function groupSourceCandidates(
  candidates: AnimeSourceCandidate[],
  excludedSources: AnimeSourceBindingState["excludedSources"]
) {
  const groups = new Map<string, {
    sourceId: string;
    sourceName: string;
    candidates: AnimeSourceCandidate[];
    excluded: boolean;
  }>();
  for (const candidate of candidates) {
    const group = groups.get(candidate.sourceId) ?? {
      sourceId: candidate.sourceId,
      sourceName: candidate.sourceName,
      candidates: [],
      excluded: false
    };
    group.candidates.push(candidate);
    groups.set(candidate.sourceId, group);
  }
  for (const source of excludedSources) {
    groups.set(source.sourceId, {
      sourceId: source.sourceId,
      sourceName: source.sourceName,
      candidates: [],
      excluded: true
    });
  }
  return [...groups.values()];
}

function getBindingMethodText(method: "manual" | "external_id" | "scored"): string {
  if (method === "manual") return "人工确认";
  if (method === "external_id") return "外部ID";
  return "评分缓存";
}

function EpisodeRulesPanel({
  draft,
  persisted,
  episodes,
  episodePreferences,
  downloadTasks,
  releasePreviews,
  fansubs,
  fansubNames,
  loading,
  previewingEpisodeId,
  addingReleaseId,
  onAddEpisode,
  onStatusChange,
  onFansubChange,
  onPreviewReleases,
  onAddRelease
}: {
  draft: MyAnime | null;
  persisted: boolean;
  episodes: Episode[];
  episodePreferences: EpisodePreference[];
  downloadTasks: DownloadTask[];
  releasePreviews: Record<string, EpisodeReleasePreview>;
  fansubs: FansubGroup[];
  fansubNames: Map<string, string>;
  loading: boolean;
  previewingEpisodeId: string | null;
  addingReleaseId: string | null;
  onAddEpisode: () => void;
  onStatusChange: (episode: Episode, status: EpisodeStatus) => void;
  onFansubChange: (episode: Episode, fansubGroupId: string) => void;
  onPreviewReleases: (episode: Episode) => void;
  onAddRelease: (episode: Episode, release: Release) => void;
}) {
  if (!draft) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>单集规则</CardTitle>
        </CardHeader>
        <CardContent>
          <Empty className="min-h-40 p-4 md:p-6">
            <EmptyHeader>
              <EmptyMedia variant="icon"><SlidersHorizontal /></EmptyMedia>
              <EmptyTitle>未选择追番</EmptyTitle>
              <EmptyDescription>选择一部番剧后可管理每集的字幕组覆盖。</EmptyDescription>
            </EmptyHeader>
          </Empty>
        </CardContent>
      </Card>
    );
  }

  if (!persisted) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>单集规则</CardTitle>
        </CardHeader>
        <CardContent>
          <Empty className="min-h-40 p-4 md:p-6">
            <EmptyHeader>
              <EmptyMedia variant="icon"><Save /></EmptyMedia>
              <EmptyTitle>请先保存追番</EmptyTitle>
              <EmptyDescription>新追番需要先保存，之后才能添加单集规则。</EmptyDescription>
            </EmptyHeader>
          </Empty>
        </CardContent>
      </Card>
    );
  }

  return (
    <Card>
      <CardHeader className="flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <CardTitle>单集规则</CardTitle>
          <CardDescription className="mt-1">
            不设置时跟随番剧默认字幕组；设置后这一集会优先使用覆盖字幕组。
          </CardDescription>
        </div>
        <Button className="min-h-11 w-full sm:min-h-9 sm:w-auto" variant="outline" onClick={onAddEpisode}>
          <Plus data-icon="inline-start" />
          添加下一集
        </Button>
      </CardHeader>
      <CardContent>
        {loading ? (
          <div className="flex flex-col gap-3" aria-busy="true" aria-label="正在加载单集规则">
            <Skeleton className="h-32 w-full" />
            <Skeleton className="h-32 w-full" />
          </div>
        ) : (
          <div className="flex min-w-0 flex-col gap-3">
            {episodes.map((episode) => {
              const preference = episodePreferences.find((item) => item.episodeId === episode.id);
              const preview = releasePreviews[episode.id];
              const linkedDownload = findEpisodeDownloadLink(downloadTasks, episode);
              const inheritedFansub = draft.defaultFansubGroupId
                ? (fansubNames.get(draft.defaultFansubGroupId) ?? "默认字幕组")
                : "未设置默认字幕组";
              const effectiveFansub = preference?.fansubGroupId
                ? (fansubNames.get(preference.fansubGroupId) ?? preference.fansubGroupId)
                : inheritedFansub;

              return (
              <div key={episode.id} className="rounded-md border p-3">
                <div className="flex items-start justify-between gap-3">
                  <div className="min-w-0">
                    <div className="font-medium">第 {episode.episodeNo} 集</div>
                    <div className="mt-1 truncate text-xs text-muted-foreground">{episode.title ?? "未命名单集"}</div>
                    <div className="mt-2 flex flex-wrap gap-2 text-xs text-muted-foreground">
                      <span>当前字幕组：{effectiveFansub}</span>
                      {linkedDownload && (
                        <span>
                          下载任务：{linkedDownload.completed ? "已完成" : downloadStatusText[linkedDownload.task.status]} · {formatPercent(linkedDownload.progress)}
                        </span>
                      )}
                    </div>
                  </div>
                  <Badge tone={episode.status === "downloaded" || episode.status === "watched" ? "green" : "neutral"}>
                    {episodeStatusText[episode.status]}
                  </Badge>
                </div>
                <FieldGroup className="mt-3 grid gap-3 sm:grid-cols-2">
                  <Field className="min-w-0">
                    <FieldLabel htmlFor={`episode-status-${episode.id}`}>单集状态</FieldLabel>
                    <Select
                      value={episode.status}
                      onValueChange={(value) => onStatusChange(episode, value as EpisodeStatus)}
                    >
                      <SelectTrigger id={`episode-status-${episode.id}`}>
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectGroup>
                          {episodeStatusOptions.map(([value, label]) => (
                            <SelectItem key={value} value={value}>{label}</SelectItem>
                          ))}
                        </SelectGroup>
                      </SelectContent>
                    </Select>
                  </Field>
                  <Field className="min-w-0">
                    <FieldLabel htmlFor={`episode-fansub-${episode.id}`}>字幕组覆盖</FieldLabel>
                    <Select
                      value={preference?.fansubGroupId ?? emptySelectValue}
                      onValueChange={(value) => onFansubChange(episode, value === emptySelectValue ? "" : value)}
                    >
                      <SelectTrigger id={`episode-fansub-${episode.id}`}>
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectGroup>
                          <SelectItem value={emptySelectValue}>跟随默认：{inheritedFansub}</SelectItem>
                          {fansubs.map((group) => (
                            <SelectItem key={group.id} value={group.id}>{group.name}</SelectItem>
                          ))}
                        </SelectGroup>
                      </SelectContent>
                    </Select>
                  </Field>
                </FieldGroup>
                <div className="mt-3 flex flex-col items-stretch gap-3 sm:flex-row sm:items-center sm:justify-between">
                  <div className="min-w-0 text-xs text-muted-foreground">
                    {preview ? `候选 ${preview.candidates.length} 个` : "尚未匹配资源"}
                  </div>
                  <Button
                    className="min-h-11 w-full sm:min-h-9 sm:w-auto"
                    variant="outline"
                    onClick={() => onPreviewReleases(episode)}
                    disabled={previewingEpisodeId === episode.id}
                  >
                    <Search data-icon="inline-start" />
                    {previewingEpisodeId === episode.id ? "查询中" : "查看发布"}
                  </Button>
                </div>
                {preference?.fansubGroupId && (
                  <div className="mt-2 text-xs text-muted-foreground">
                    当前覆盖：{fansubNames.get(preference.fansubGroupId) ?? preference.fansubGroupId}
                  </div>
                )}
                {preview && (
                  <div className="mt-3 flex flex-col gap-2">
                    {preview.candidates.slice(0, 6).map((candidate) => (
                      <div key={candidate.release.id} className="rounded-md bg-muted p-3">
                        <div className="flex min-w-0 flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
                          <div className="min-w-0 flex-1">
                            <div className="truncate text-sm font-medium">{candidate.release.title}</div>
                            <div className="mt-2 flex flex-wrap gap-2">
                              <Badge tone="blue">{candidate.score} 分</Badge>
                              <Badge>匹配 {candidate.matchScore}/50</Badge>
                              <Badge>偏好 {candidate.preferenceScore}/40</Badge>
                              <Badge>{candidate.release.sourceName}</Badge>
                              <Badge>第 {candidate.release.episodeNo ?? episode.episodeNo} 集</Badge>
                              <Badge>{getReleaseFansubName(candidate.release, fansubNames)}</Badge>
                              <ReleaseMetadataBadges metadata={candidate.release} />
                              {candidate.release.size && <Badge>{formatBytes(candidate.release.size)}</Badge>}
                              {typeof candidate.release.seeders === "number" && (
                                <Badge tone={candidate.release.seeders > 0 ? "green" : "neutral"}>
                                  {candidate.release.seeders} 做种
                                </Badge>
                              )}
                            </div>
                            <div className="mt-2 text-xs text-muted-foreground">
                              {[...candidate.reasons, ...candidate.warnings.map((warning) => `注意：${warning}`)].join("，") || "规则匹配"}
                            </div>
                          </div>
                          <Button
                            className="min-h-11 w-full shrink-0 sm:min-h-9 sm:w-auto"
                            variant="outline"
                            onClick={() => onAddRelease(episode, candidate.release)}
                            disabled={addingReleaseId === candidate.release.id}
                          >
                            <Download data-icon="inline-start" />
                            {addingReleaseId === candidate.release.id ? "添加中" : "添加下载"}
                          </Button>
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>
              );
            })}

            {episodes.length === 0 && (
              <Empty className="min-h-40 p-4 md:p-6">
                <EmptyHeader>
                  <EmptyMedia variant="icon"><Plus /></EmptyMedia>
                  <EmptyTitle>暂无单集规则</EmptyTitle>
                  <EmptyDescription>还没有单集，添加后可为每集设置字幕组。</EmptyDescription>
                </EmptyHeader>
              </Empty>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function buildSearchTerms(item: MyAnime): string[] {
  return buildAnimeReleaseSearchTerms(item.anime, [], 8);
}

/** 读取当前追番已启用且地址有效的 RSS 订阅。 */
function getEnabledRssSubscriptions(item: MyAnime): AnimeRssSubscription[] {
  return (item.rssSubscriptions ?? []).filter((subscription) => subscription.enabled && subscription.url.trim());
}

/** 保存前清理 RSS 订阅内容，过滤空地址并补齐时间字段。 */
function normalizeRssSubscriptions(item: MyAnime, timestamp: string): AnimeRssSubscription[] {
  return (item.rssSubscriptions ?? [])
    .map((subscription, index) => ({
      ...subscription,
      myAnimeId: item.id,
      name: subscription.name.trim() || `RSS订阅 ${index + 1}`,
      url: subscription.url.trim(),
      preferredSubtitleLanguages: resolveSubtitleLanguages(
        subscription.preferredSubtitleLanguages,
        subscription.preferredSubtitle
      ),
      preferredSubtitle: undefined,
      refreshIntervalMinutes: normalizeRssRefreshInterval(subscription.refreshIntervalMinutes),
      lastFetchedAt: subscription.lastFetchedAt,
      createdAt: subscription.createdAt || timestamp,
      updatedAt: timestamp
    }))
    .filter((subscription) => subscription.url);
}

/** 规范化 RSS 自动下载刷新间隔，空值使用默认 20 分钟。 */
function normalizeRssRefreshInterval(value?: number): number {
  if (!Number.isFinite(value) || !value || value <= 0) {
    return defaultRssRefreshIntervalMinutes;
  }

  return Math.max(1, Math.round(value));
}

/** 根据番剧的 Mikan 外部 ID 生成蜜柑计划 RSS 地址。 */
function buildMikanRssUrl(item: MyAnime): string | undefined {
  const mikanId = item.anime.externalIds.mikan?.trim();
  return mikanId ? `https://mikanani.me/RSS/Bangumi?bangumiId=${encodeURIComponent(mikanId)}` : undefined;
}

/** 构造追番资源添加下载时需要的关联参数。 */
function buildAnimeReleaseDownloadInput(
  release: Release,
  target: MyAnime,
  confirmUnknownSeason = false
): AddReleaseDownloadInput {
  return {
    release: {
      ...release,
      animeId: target.anime.id
    },
    animeId: target.anime.id,
    episodeNo: release.episodeNo,
    fansubGroupId: release.fansubGroupId,
    savePath: target.downloadDir,
    confirmUnknownSeason
  };
}

/** 按稳定 ID 合并全局名称快照和当前番剧字幕组。 */
function mergeFansubGroups(...collections: FansubGroup[][]): FansubGroup[] {
  const groups = new Map<string, FansubGroup>();
  for (const group of collections.flat()) {
    groups.set(group.id, group);
  }
  return [...groups.values()];
}

function dedupeReleases(releases: Release[]): Release[] {
  const seen = new Set<string>();

  return releases.filter((release) => {
    const key = releaseKey(release);
    if (seen.has(key)) {
      return false;
    }

    seen.add(key);
    return true;
  });
}

function dedupeReleaseErrors(errors: ReleaseSearchResult["errors"]): ReleaseSearchResult["errors"] {
  const seen = new Set<string>();

  return errors.filter((error) => {
    const key = `${error.sourceId}:${error.message}`;
    if (seen.has(key)) {
      return false;
    }

    seen.add(key);
    return true;
  });
}

function sortReleases(releases: Release[]): Release[] {
  return [...releases].sort((left, right) => {
    return compareReleaseEpisodeDescending(left, right)
      || (right.publishedAt ?? "").localeCompare(left.publishedAt ?? "");
  });
}

function filterReleasesByFansub(releases: Release[], fansubGroupId: string): Release[] {
  if (!fansubGroupId) {
    return releases;
  }

  if (fansubGroupId === unknownFansubFilter) {
    return releases.filter((release) => !release.fansubGroupId);
  }

  return releases.filter((release) => release.fansubGroupId === fansubGroupId);
}

function countReleasesByFansub(releases: Release[], fansubGroupId: string): number {
  return releases.filter((release) => release.fansubGroupId === fansubGroupId).length;
}

interface ReleaseFansubGroup {
  key: string;
  name: string;
  releases: Release[];
}

function groupReleasesByFansub(releases: Release[], fansubNames: Map<string, string>): ReleaseFansubGroup[] {
  const groups = new Map<string, ReleaseFansubGroup>();
  for (const release of releases) {
    const key = release.fansubGroupId ?? release.fansubName ?? unknownFansubFilter;
    const group = groups.get(key) ?? {
      key,
      name: getReleaseFansubName(release, fansubNames),
      releases: []
    };
    group.releases.push(release);
    groups.set(key, group);
  }

  return [...groups.values()];
}

/** 从 Mikan 资源元信息生成字幕组级 RSS 订阅候选。 */
function buildMikanGroupRssSubscription(group: ReleaseFansubGroup, target: MyAnime): RssSubscriptionDraft | undefined {
  const release = group.releases.find((item) => item.sourceMeta?.mikanSubgroupId);
  const mikanBangumiId = release?.sourceMeta?.mikanBangumiId ?? target.anime.externalIds.mikan?.trim();
  const mikanSubgroupId = release?.sourceMeta?.mikanSubgroupId;
  if (!mikanBangumiId || !mikanSubgroupId) {
    return undefined;
  }

  return {
    name: `蜜柑 · ${release.sourceMeta?.mikanSubgroupName ?? group.name}`,
    url:
      release.sourceMeta?.rssUrl ??
      `https://mikanani.me/RSS/Bangumi?bangumiId=${encodeURIComponent(mikanBangumiId)}&subgroupid=${encodeURIComponent(mikanSubgroupId)}`
  };
}

function getReleaseFansubName(release: Release, fansubNames: Map<string, string>): string {
  if (!release.fansubGroupId) {
    return release.fansubName ?? "未识别字幕组";
  }

  return fansubNames.get(release.fansubGroupId) ?? release.fansubName ?? release.fansubGroupId;
}

function formatReleaseDate(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }

  return date.toLocaleString();
}

/** 渲染带标签的单行文本字段。 */
function TextField({
  label,
  value,
  onChange
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
}) {
  const id = useId();

  return (
    <Field>
      <FieldLabel htmlFor={id}>{label}</FieldLabel>
      <Input
        id={id}
        value={value}
        onChange={(event) => onChange(event.target.value)}
      />
    </Field>
  );
}

/** 渲染带标签的多行文本字段。 */
function TextareaField({
  label,
  value,
  onChange
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
}) {
  const id = useId();

  return (
    <Field>
      <FieldLabel htmlFor={id}>{label}</FieldLabel>
      <Textarea
        id={id}
        value={value}
        onChange={(event) => onChange(event.target.value)}
      />
    </Field>
  );
}

/** 渲染带范围约束的数字字段。 */
function NumberField({
  label,
  value,
  min,
  max,
  onChange
}: {
  label: string;
  value: number;
  min?: number;
  max?: number;
  onChange: (value: number) => void;
}) {
  const id = useId();

  return (
    <Field>
      <FieldLabel htmlFor={id}>{label}</FieldLabel>
      <Input
        id={id}
        max={max}
        min={min}
        type="number"
        value={value}
        onChange={(event) => onChange(Number(event.target.value))}
      />
    </Field>
  );
}

/** 渲染带标签的受控选择字段。 */
function SelectField({
  label,
  value,
  options,
  disabled = false,
  onChange
}: {
  label: string;
  value: string;
  options: Array<{ value: string; label: string }>;
  disabled?: boolean;
  onChange: (value: string) => void;
}) {
  const id = useId();

  return (
    <Field data-disabled={disabled || undefined}>
      <FieldLabel htmlFor={id}>{label}</FieldLabel>
      <Select
        disabled={disabled}
        value={value || emptySelectValue}
        onValueChange={(nextValue) => onChange(nextValue === emptySelectValue ? "" : nextValue)}
      >
        <SelectTrigger id={id}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectGroup>
            {options.map((option) => (
              <SelectItem key={option.value || "empty"} value={option.value || emptySelectValue}>
                {option.label}
              </SelectItem>
            ))}
          </SelectGroup>
        </SelectContent>
      </Select>
    </Field>
  );
}

/** 序列化规则草稿，用于判断侧栏是否存在未保存修改。 */
function serializeMyAnimeDraft(item: MyAnime): string {
  return JSON.stringify(item);
}

/** 使用 shadcn ToggleGroup 编辑可多选的字幕语言偏好。 */
function SubtitleLanguageToggleField({
  label,
  value,
  onChange
}: {
  label: string;
  value: SubtitleLanguage[];
  onChange: (value: SubtitleLanguage[]) => void;
}) {
  const labelId = useId();
  return (
    <Field className="min-w-0">
      <FieldLabel id={labelId}>{label}</FieldLabel>
      <ToggleGroup
        className="flex w-full flex-wrap justify-start"
        type="multiple"
        variant="outline"
        value={value}
        onValueChange={(nextValue) => onChange(nextValue as SubtitleLanguage[])}
        aria-labelledby={labelId}
      >
        {subtitleOptions.map((language) => (
          <ToggleGroupItem key={language} value={language} aria-label={subtitleLanguageText[language]}>
            {subtitleLanguageText[language]}
          </ToggleGroupItem>
        ))}
      </ToggleGroup>
    </Field>
  );
}

function createEmptyDraft(): MyAnime {
  const now = new Date();
  const animeId = createId("anime");

  return {
    id: createId("my"),
    anime: {
      id: animeId,
      title: "",
      originalTitle: "",
      aliases: [],
      premiereYear: now.getFullYear(),
      premiereMonth: now.getMonth() + 1,
      externalIds: {}
    },
    status: "watching",
    ...createDefaultMyAnimePreferences(),
    addedAt: now.toISOString(),
    updatedAt: now.toISOString()
  };
}

function cloneMyAnime(item: MyAnime): MyAnime {
  return JSON.parse(JSON.stringify(item)) as MyAnime;
}

function clampMonth(value: number): number {
  if (!Number.isFinite(value)) {
    return 1;
  }

  return Math.max(1, Math.min(12, value));
}

function createId(prefix: string): string {
  if (globalThis.crypto?.randomUUID) {
    return `${prefix}-${globalThis.crypto.randomUUID()}`;
  }

  return `${prefix}-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}
