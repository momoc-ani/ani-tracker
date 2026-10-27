import { ChevronDown, Download, ImageOff, MoreHorizontal, SlidersHorizontal } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { CachedImage } from "@/components/cached-image";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger
} from "@/components/ui/dropdown-menu";
import { Progress } from "@/components/ui/progress";
import { WatchProgressDisplay } from "@/features/my-anime/watch-progress-display";
import { formatMonth } from "@/lib/format";
import { resolveAnimeTitleDisplay } from "@shared/anime-title";
import type { AnimeWatchProgress } from "@shared/contracts";
import type { AnimeStatus, MyAnime } from "@shared/domain";
import { canAnimeStatusAutoDownload } from "@shared/my-anime-policy";
import {
  formatSubtitleLanguages,
  formatVideoBitDepth,
  resolveSubtitleLanguages
} from "@shared/release-metadata";

const statusText: Record<AnimeStatus, string> = {
  watching: "在追",
  planned: "想看",
  completed: "已完成",
  paused: "暂停",
  dropped: "已弃"
};
const statusOptions = Object.entries(statusText) as Array<[AnimeStatus, string]>;

const seasonLabels = {
  winter: "冬季",
  spring: "春季",
  summer: "夏季",
  fall: "秋季"
} as const;

export interface MyAnimeDownloadSummary {
  active: number;
  completed: number;
  linked: number;
}

export interface MyAnimeSeasonGroup {
  key: string;
  label: string;
  order: number;
  items: MyAnime[];
}

interface MyAnimeRowProps {
  defaultFansubName: string;
  downloadSummary: MyAnimeDownloadSummary;
  item: MyAnime;
  preferenceUpdating: boolean;
  watchProgress: AnimeWatchProgress;
  onAutoDownloadChange: (enabled: boolean) => void;
  onOpenActive: () => void;
  onOpenCompleted: () => void;
  onOpenDetail: () => void;
  onOpenDownloads: () => void;
  onOpenRules: () => void;
  onRemove: () => void;
  onStatusChange: (status: AnimeStatus) => void;
}

/** 按首播年份和季度归组追番，并优先展示较新的季度。 */
export function groupMyAnimeBySeason(items: MyAnime[]): MyAnimeSeasonGroup[] {
  const groups = new Map<string, MyAnimeSeasonGroup>();
  for (const item of items) {
    const season = item.anime.season ?? resolveSeasonFromMonth(item.anime.premiereMonth);
    const key = `${item.anime.premiereYear}-${season}`;
    const group = groups.get(key) ?? {
      key,
      label: `${item.anime.premiereYear} 年 ${seasonLabels[season]}`,
      order: item.anime.premiereYear * 10 + seasonOrder(season),
      items: []
    };
    group.items.push(item);
    groups.set(key, group);
  }

  return [...groups.values()]
    .map((group) => ({
      ...group,
      items: [...group.items].sort((left, right) => right.updatedAt.localeCompare(left.updatedAt))
    }))
    .sort((left, right) => right.order - left.order);
}

/** 渲染追番紧凑条目，并承载资源、规则与任务快捷入口。 */
export function MyAnimeRow({
  defaultFansubName,
  downloadSummary,
  item,
  preferenceUpdating,
  watchProgress,
  onAutoDownloadChange,
  onOpenActive,
  onOpenCompleted,
  onOpenDetail,
  onOpenDownloads,
  onOpenRules,
  onRemove,
  onStatusChange
}: MyAnimeRowProps) {
  const titleDisplay = resolveAnimeTitleDisplay(item.anime);
  const ratingText = item.anime.rating ? item.anime.rating.score.toFixed(1) : "暂无";
  const autoDownloadAllowed = canAnimeStatusAutoDownload(item.status);
  const autoDownloadDisabled = preferenceUpdating || (!item.autoDownload && !autoDownloadAllowed);
  const [menuOpen, setMenuOpen] = useState(false);
  const menuCloseTimerRef = useRef<number>();

  /** 取消操作菜单的延迟关闭。 */
  function cancelMenuClose(): void {
    if (menuCloseTimerRef.current !== undefined) {
      window.clearTimeout(menuCloseTimerRef.current);
      menuCloseTimerRef.current = undefined;
    }
  }

  /** 鼠标离开菜单交互区后延迟关闭操作菜单。 */
  function scheduleMenuClose(): void {
    cancelMenuClose();
    menuCloseTimerRef.current = window.setTimeout(() => {
      setMenuOpen(false);
      menuCloseTimerRef.current = undefined;
    }, 500);
  }

  useEffect(() => () => cancelMenuClose(), []);

  return (
    <article className="group relative flex min-w-0 gap-3 overflow-hidden border-b bg-transparent py-3 transition-colors last:border-b-0 hover:bg-accent/30 sm:gap-4 sm:px-2">
      <button
        aria-label={`查看${titleDisplay.title}详情`}
        className="relative aspect-[2/3] w-16 shrink-0 overflow-hidden rounded-md bg-muted outline-none focus-visible:ring-2 focus-visible:ring-ring sm:w-[4.5rem]"
        onClick={onOpenDetail}
        type="button"
      >
        {item.anime.coverUrl ? (
          <CachedImage
            alt={titleDisplay.title}
            className="size-full object-cover"
            loading="lazy"
            sourceUrl={item.anime.coverUrl}
          />
        ) : (
          <div className="flex size-full items-center justify-center text-muted-foreground">
            <ImageOff />
          </div>
        )}
      </button>

      <div className="grid min-w-0 flex-1 gap-3 md:grid-cols-[minmax(0,1.35fr)_minmax(13rem,0.8fr)_auto] md:items-center">
        <div className="min-w-0 self-start md:self-center">
          <div className="flex min-w-0 items-center gap-2">
            <button
              className="min-w-0 truncate text-left text-sm font-semibold hover:text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              onClick={onOpenDetail}
              title={titleDisplay.title}
              type="button"
            >
              {titleDisplay.title}
            </button>
            <Badge className="h-5 shrink-0 px-1.5" tone="amber">{ratingText}</Badge>
          </div>
          <p className="mt-1 truncate text-xs text-muted-foreground" title={titleDisplay.subtitle ?? "无原名"}>
            {titleDisplay.subtitle ?? "无原名"}
          </p>
          <div className="mt-2 flex min-w-0 flex-wrap gap-1.5">
            <Badge>{formatMonth(item.anime.premiereYear, item.anime.premiereMonth)}</Badge>
            <DropdownMenu modal={false}>
              <DropdownMenuTrigger asChild>
                <button
                  aria-label={`修改追番状态，当前${statusText[item.status]}`}
                  className="rounded-md outline-none transition-opacity hover:opacity-80 focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-wait disabled:opacity-60"
                  disabled={preferenceUpdating}
                  title="点击修改追番状态"
                  type="button"
                >
                  <Badge className="gap-0.5 pr-1.5" tone="primary">
                    {statusText[item.status]}
                    <ChevronDown aria-hidden="true" className="size-3" />
                  </Badge>
                </button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="start" className="w-36">
                <DropdownMenuRadioGroup
                  onValueChange={(value) => {
                    if (value !== item.status) {
                      onStatusChange(value as AnimeStatus);
                    }
                  }}
                  value={item.status}
                >
                  {statusOptions.map(([value, label]) => (
                    <DropdownMenuRadioItem key={value} value={value}>
                      {label}
                    </DropdownMenuRadioItem>
                  ))}
                </DropdownMenuRadioGroup>
              </DropdownMenuContent>
            </DropdownMenu>
            <button
              aria-label={`${item.autoDownload ? "关闭自动下载，切换为手动" : "开启自动下载"}${autoDownloadDisabled && !preferenceUpdating ? "，当前状态不允许开启" : ""}`}
              aria-pressed={item.autoDownload}
              className="rounded-md outline-none transition-opacity hover:opacity-80 focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-60"
              disabled={autoDownloadDisabled}
              onClick={() => onAutoDownloadChange(!item.autoDownload)}
              title={autoDownloadDisabled && !preferenceUpdating
                ? "已完成或已弃的追番不能开启自动下载"
                : item.autoDownload ? "点击切换为手动" : "点击开启自动下载"}
              type="button"
            >
              <Badge tone={item.autoDownload ? "green" : "neutral"}>
                {item.autoDownload ? "自动" : "手动"}
              </Badge>
            </button>
            <Badge className="max-w-full truncate" title={defaultFansubName}>{defaultFansubName}</Badge>
            {item.preferredResolution && <Badge>{item.preferredResolution}</Badge>}
            {item.preferredCodec && <Badge tone="blue">{item.preferredCodec}</Badge>}
            {item.preferredBitDepth && <Badge>{formatVideoBitDepth(item.preferredBitDepth)}</Badge>}
            {resolveSubtitleLanguages(item.preferredSubtitleLanguages, item.preferredSubtitle).length > 0 && (
              <Badge>
                {formatSubtitleLanguages(resolveSubtitleLanguages(item.preferredSubtitleLanguages, item.preferredSubtitle))}
              </Badge>
            )}
          </div>
        </div>

        <div className="min-w-0">
          <WatchProgressDisplay progress={watchProgress} />
          <button
            aria-label={`查看${titleDisplay.title}已完成下载任务，共 ${downloadSummary.completed} 个`}
            className="mt-3 block w-full rounded-sm text-left outline-none transition-colors hover:bg-accent/50 focus-visible:ring-2 focus-visible:ring-ring"
            onClick={onOpenCompleted}
            type="button"
          >
            <div className="flex items-end justify-between gap-3 text-xs">
              <span className="text-muted-foreground">下载进度</span>
              <span className="font-medium tabular-nums">
                {downloadSummary.completed} / {downloadSummary.linked}
              </span>
            </div>
            <Progress
              className="mt-2 h-1.5"
              value={downloadSummary.linked ? downloadSummary.completed / downloadSummary.linked : 0}
            />
          </button>
        </div>

        <div className="flex min-w-0 items-center gap-2 md:justify-end">
          <Button className="min-w-0 flex-1 px-2 md:flex-none" onClick={onOpenDownloads}>
            <Download data-icon="inline-start" />
            <span className="hidden sm:inline">下载资源</span>
          </Button>
          <Button className="min-w-0 flex-1 px-2 md:flex-none" onClick={onOpenRules} variant="outline">
            <SlidersHorizontal data-icon="inline-start" />
            <span className="hidden sm:inline">规则</span>
          </Button>
          <DropdownMenu
            modal={false}
            open={menuOpen}
            onOpenChange={(open) => {
              cancelMenuClose();
              setMenuOpen(open);
            }}
          >
            <DropdownMenuTrigger asChild>
              <Button
                aria-label="更多操作"
                className="size-11 p-0 md:size-9"
                onPointerEnter={(event) => {
                  if (event.pointerType === "mouse") {
                    cancelMenuClose();
                    setMenuOpen(true);
                  }
                }}
                onPointerLeave={(event) => {
                  if (event.pointerType === "mouse") scheduleMenuClose();
                }}
                title="更多操作"
                type="button"
                variant="outline"
              >
                <MoreHorizontal />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent
              align="end"
              className="w-36"
              onPointerEnter={(event) => {
                if (event.pointerType === "mouse") cancelMenuClose();
              }}
              onPointerLeave={(event) => {
                if (event.pointerType === "mouse") scheduleMenuClose();
              }}
            >
              <DropdownMenuGroup>
                <DropdownMenuItem onSelect={onOpenActive}>查看进行中</DropdownMenuItem>
                <DropdownMenuItem onSelect={onOpenCompleted}>查看已完成</DropdownMenuItem>
                <DropdownMenuItem className="text-destructive focus:text-destructive" onSelect={onRemove}>
                  移除追番
                </DropdownMenuItem>
              </DropdownMenuGroup>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>
    </article>
  );
}

/** 将月份映射为自然季度。 */
function resolveSeasonFromMonth(month: number): keyof typeof seasonLabels {
  if (month <= 3) return "winter";
  if (month <= 6) return "spring";
  if (month <= 9) return "summer";
  return "fall";
}

/** 返回季度排序值。 */
function seasonOrder(season: keyof typeof seasonLabels): number {
  return { winter: 1, spring: 2, summer: 3, fall: 4 }[season];
}
