import {
  CheckCircle2,
  CircleDashed,
  Download,
  ListVideo,
  Radio,
  RotateCw
} from "lucide-react";
import { useEffect, useRef } from "react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/cn";
import type { PlayerEpisodeUiItem, PlayerEpisodeUiStatus } from "./player-ui-model";

interface PlayerEpisodeListProps {
  animeTitle: string;
  items: PlayerEpisodeUiItem[];
  onSelect: (item: PlayerEpisodeUiItem) => void;
  scrollable?: boolean;
  showHeader?: boolean;
}

/** 展示当前番剧的完整集数状态，并阻止未下载条目触发播放。 */
export function PlayerEpisodeList({
  animeTitle,
  items,
  onSelect,
  scrollable = false,
  showHeader = true
}: PlayerEpisodeListProps) {
  const episodeItems = items.filter((item) => item.section === "episodes");
  const specialItems = items.filter((item) => item.section === "specials");
  const episodeKeys = new Set(episodeItems.map(episodeItemKey));
  const viewedKeys = new Set(episodeItems
    .filter((item) => item.status === "watched" || item.status === "playing")
    .map(episodeItemKey));
  const episodeCount = episodeKeys.size;
  const viewedCount = viewedKeys.size;
  const episodeVersionLabel = episodeItems.length === episodeCount
    ? String(episodeCount)
    : `${episodeCount} 集/${episodeItems.length} 版本`;
  const activeItemId = items.find((item) => item.status === "playing")?.id;
  const scrollAreaRef = useRef<HTMLDivElement>(null);
  const activeRowRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!scrollable || !activeItemId) return;
    const viewport = scrollAreaRef.current?.querySelector<HTMLElement>("[data-radix-scroll-area-viewport]");
    if (!viewport) return;
    let frame: number | undefined;
    /** 将当前集定位到列表第二项，并在旋转后重新对齐。 */
    const alignActiveRow = (): void => {
      if (frame !== undefined) window.cancelAnimationFrame(frame);
      frame = window.requestAnimationFrame(() => {
        frame = undefined;
        const activeRow = activeRowRef.current;
        if (!activeRow) return;
        const viewportRect = viewport.getBoundingClientRect();
        const rowRect = activeRow.getBoundingClientRect();
        const rowOffset = rowRect.top - viewportRect.top + viewport.scrollTop;
        viewport.scrollTop = Math.max(0, rowOffset - rowRect.height);
      });
    };
    const resizeObserver = new ResizeObserver(alignActiveRow);
    resizeObserver.observe(viewport);
    window.addEventListener("resize", alignActiveRow);
    alignActiveRow();
    return () => {
      resizeObserver.disconnect();
      window.removeEventListener("resize", alignActiveRow);
      if (frame !== undefined) window.cancelAnimationFrame(frame);
    };
  }, [activeItemId, scrollable]);

  /** 渲染单个内容分组并保持组内分隔线稳定。 */
  const renderRows = (groupItems: PlayerEpisodeUiItem[]) => groupItems.map((item, index) => (
    <div
      key={item.id}
      ref={item.id === activeItemId ? activeRowRef : undefined}
      className="min-w-0 max-w-full overflow-hidden"
      role="listitem"
    >
      <EpisodeRow item={item} onSelect={onSelect} />
      {index < groupItems.length - 1 && <Separator />}
    </div>
  ));

  const content = (
    <div className="flex min-w-0 max-w-full flex-col overflow-hidden" role="list" aria-label={`${animeTitle} 播放列表`}>
      {specialItems.length > 0 && episodeItems.length > 0 && (
        <div className="flex items-center justify-between gap-3 px-4 py-2 text-xs font-medium text-muted-foreground" role="presentation">
          <span>正片</span><Badge>{episodeVersionLabel}</Badge>
        </div>
      )}
      {renderRows(episodeItems)}
      {specialItems.length > 0 && (
        <>
          {episodeItems.length > 0 && <Separator />}
          <div className="flex items-center justify-between gap-3 px-4 py-2 text-xs font-medium text-muted-foreground" role="presentation">
            <span>特别内容</span><Badge>{specialItems.length}</Badge>
          </div>
          {renderRows(specialItems)}
        </>
      )}
    </div>
  );

  return (
    <TooltipProvider delayDuration={300}>
      <section
        aria-label={showHeader ? undefined : `${animeTitle} 播放列表`}
        aria-labelledby={showHeader ? "player-playlist-title" : undefined}
        className={cn("flex min-w-0 max-w-full flex-col overflow-hidden", scrollable && "h-full min-h-0")}
      >
        {showHeader && (
          <div className="flex items-center justify-between gap-3 px-4 py-3 sm:px-5">
            <div className="min-w-0">
              <h2 id="player-playlist-title" className="text-base font-semibold">播放列表</h2>
            </div>
            <Badge>{episodeItems.length > 0 ? `${viewedCount}/${episodeCount}` : `${specialItems.length} 项`}</Badge>
          </div>
        )}
        {items.length === 0 ? (
          <div className="flex min-h-32 flex-col items-center justify-center gap-2 px-4 text-center text-muted-foreground">
            <ListVideo />
            <p className="text-sm font-medium text-foreground">没有可播放视频</p>
            <p className="text-xs">当前番剧暂时没有已完成的视频文件</p>
          </div>
        ) : scrollable ? (
          <ScrollArea
            ref={scrollAreaRef}
            className="min-h-0 min-w-0 max-w-full flex-1 px-2 [&_[data-radix-scroll-area-viewport]>div]:!block [&_[data-radix-scroll-area-viewport]>div]:!w-full"
          >
            {content}
          </ScrollArea>
        ) : content}
      </section>
    </TooltipProvider>
  );
}

/** 返回用于播放进度统计的集数键，同集多个字幕组只统计一次。 */
function episodeItemKey(item: PlayerEpisodeUiItem): string {
  return item.episodeNo === undefined ? item.id : `episode:${item.episodeNo}`;
}

/** 渲染单集的编号、标题、媒体规格、进度和状态。 */
function EpisodeRow({
  item,
  onSelect
}: {
  item: PlayerEpisodeUiItem;
  onSelect: (item: PlayerEpisodeUiItem) => void;
}) {
  const active = item.status === "playing";
  const disabled = !item.playlistItem;

  return (
    <Button
      aria-current={active ? "true" : undefined}
      aria-label={`${item.numberLabel} ${item.title}，${item.statusLabel}`}
      className={cn(
        "h-auto min-h-14 w-full min-w-0 max-w-full justify-start overflow-hidden rounded-none border-l-2 border-transparent px-4 py-2 text-left sm:px-3",
        active && "border-primary bg-primary/10 hover:bg-primary/15"
      )}
      disabled={disabled}
      onClick={() => onSelect(item)}
      variant="ghost"
    >
      <span className="w-14 shrink-0 text-center font-mono text-xs font-semibold tabular-nums">
        {item.numberLabel}
      </span>
      <span className="min-w-0 flex-1">
        <span className="flex min-w-0 items-center gap-2">
          <Tooltip>
            <TooltipTrigger asChild>
              <span className="min-w-0 truncate text-sm font-medium">{item.title}</span>
            </TooltipTrigger>
            <TooltipContent className="max-w-[min(28rem,calc(100vw-2rem))] break-words" side="left">
              {item.title}
            </TooltipContent>
          </Tooltip>
        </span>
        <span className="mt-0.5 block truncate text-xs font-normal text-muted-foreground">
          {item.meta}{active ? null : ` · ${item.statusLabel}`}
        </span>
        {item.progress > 0 && item.progress < 1 && (
          <Progress className="mt-1 h-1" value={item.progress} />
        )}
      </span>
      <EpisodeStatusIcon status={item.status} />
    </Button>
  );
}

/** 使用图标和文本共同表达播放列表状态。 */
function EpisodeStatusIcon({ status }: { status: PlayerEpisodeUiStatus }) {
  const Icon = status === "playing"
    ? Radio
    : status === "watched"
      ? CheckCircle2
      : status === "ready"
        ? Download
        : status === "downloading"
          ? RotateCw
          : CircleDashed;
  return <Icon className={cn("shrink-0", status === "downloading" && "animate-spin")} aria-hidden="true" />;
}
