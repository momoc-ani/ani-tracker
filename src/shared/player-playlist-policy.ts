export interface EpisodePlaylistItemLike {
  id: string;
  episodeNo?: number;
  task: {
    id: string;
    fansubGroupId?: string;
    fansubName?: string;
  };
}

export interface EpisodePlaylistGroup<T> {
  episodeNo: number;
  items: T[];
}

export type EpisodeNavigationDirection = "previous" | "next";

/** 按集数保留全部播放版本，未下载的已知集数会返回空分组。 */
export function groupEpisodePlaylistItems<T extends { episodeNo?: number }>(
  episodeNumbers: Iterable<number>,
  items: readonly T[]
): EpisodePlaylistGroup<T>[] {
  const grouped = new Map<number, T[]>();
  for (const episodeNo of episodeNumbers) grouped.set(episodeNo, []);
  for (const item of items) {
    if (item.episodeNo === undefined) continue;
    const group = grouped.get(item.episodeNo) ?? [];
    group.push(item);
    grouped.set(item.episodeNo, group);
  }
  return [...grouped.entries()]
    .sort(([left], [right]) => left - right)
    .map(([episodeNo, groupedItems]) => ({ episodeNo, items: groupedItems }));
}

/** 选择严格相邻集，优先同一合集任务、字幕组标识和字幕组名称。 */
export function resolveAdjacentEpisodeItem<T extends EpisodePlaylistItemLike>(
  items: readonly T[],
  activeItem: T | null,
  direction: EpisodeNavigationDirection
): T | undefined {
  if (!activeItem) return undefined;
  const activeIndex = items.findIndex((item) => item.id === activeItem.id);
  if (activeIndex < 0) return undefined;
  if (activeItem.episodeNo === undefined) {
    return items[activeIndex + (direction === "next" ? 1 : -1)];
  }

  const activeEpisodeNo = activeItem.episodeNo;
  const numberedItems = items.filter((item): item is T & { episodeNo: number } =>
    item.episodeNo !== undefined
  );
  const candidateEpisodeNumbers = numberedItems
    .map((item) => item.episodeNo)
    .filter((episodeNo) => direction === "next"
      ? episodeNo > activeEpisodeNo
      : episodeNo < activeEpisodeNo);
  if (candidateEpisodeNumbers.length === 0) return undefined;

  const targetEpisodeNo = direction === "next"
    ? Math.min(...candidateEpisodeNumbers)
    : Math.max(...candidateEpisodeNumbers);
  const candidates = numberedItems.filter((item) => item.episodeNo === targetEpisodeNo);
  return pickPreferredVersion(candidates, activeItem);
}

/** 按当前来源亲和度选择目标集版本，最后保留播放列表的稳定顺序。 */
function pickPreferredVersion<T extends EpisodePlaylistItemLike>(
  candidates: readonly T[],
  activeItem: T
): T | undefined {
  return candidates.find((item) => item.task.id === activeItem.task.id)
    ?? candidates.find((item) => Boolean(activeItem.task.fansubGroupId)
      && item.task.fansubGroupId === activeItem.task.fansubGroupId)
    ?? candidates.find((item) => normalizeFansubName(item.task.fansubName)
      === normalizeFansubName(activeItem.task.fansubName)
      && normalizeFansubName(activeItem.task.fansubName) !== undefined)
    ?? candidates[0];
}

/** 规范字幕组名称，兼容来源大小写和首尾空白差异。 */
function normalizeFansubName(value: string | undefined): string | undefined {
  const normalized = value?.trim().toLocaleLowerCase();
  return normalized || undefined;
}
