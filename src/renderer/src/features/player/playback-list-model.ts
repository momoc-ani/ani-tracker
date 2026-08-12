import type { DownloadTask, MediaContentKind } from "@shared/domain";
import {
  formatMediaDisplayTitle,
  inferMediaContent,
  isSpecialMediaContent
} from "@shared/media-content";
import {
  resolveAdjacentEpisodeItem,
  type EpisodeNavigationDirection
} from "@shared/player-playlist-policy";

const VIDEO_EXTENSIONS = new Set([
  ".avi", ".flv", ".m2ts", ".m4v", ".mkv", ".mov", ".mp4", ".mpeg",
  ".mpg", ".mts", ".ogv", ".ts", ".vob", ".webm", ".wmv"
]);
const fileNameCollator = new Intl.Collator("zh-CN", { numeric: true, sensitivity: "base" });

export interface RemotePlaylistItem {
  id: string;
  task: DownloadTask;
  fileIndex?: number;
  episodeNo?: number;
  fileName: string;
  displayTitle: string;
  contentKind: MediaContentKind;
  specialNo?: string;
  size?: number;
}

/** 将同一番剧的已完成下载文件整理为稳定的播放顺序。 */
export function buildRemotePlaylist(tasks: DownloadTask[], currentTask: DownloadTask): RemotePlaylistItem[] {
  const animeTasks = currentTask.animeId
    ? tasks.filter((task) => task.animeId === currentTask.animeId)
    : tasks.filter((task) => task.id === currentTask.id);
  const items = animeTasks.flatMap((task) => {
    const playableFiles = task.files
      .filter((file) => file.selected && file.progress >= 1 && isVideoFileName(file.name))
      .map((file) => buildPlaylistItem(task, file.name, file.index, file.size, file.episodeNo));
    if (playableFiles.length > 0) return playableFiles;
    return task.files.length === 0 && isCompletedTask(task)
      ? [buildPlaylistItem(task, task.name, undefined, undefined, task.episodeNo)]
      : [];
  });

  return items.sort((left, right) => {
    const sectionDifference = Number(isSpecialMediaContent(left.contentKind))
      - Number(isSpecialMediaContent(right.contentKind));
    const episodeDifference = (left.episodeNo ?? Number.MAX_SAFE_INTEGER)
      - (right.episodeNo ?? Number.MAX_SAFE_INTEGER);
    return sectionDifference || episodeDifference || fileNameCollator.compare(left.fileName, right.fileName);
  });
}

/** 返回播放器顶部和自动下一集使用的短标签。 */
export function playlistItemLabel(item: RemotePlaylistItem): string {
  if (isSpecialMediaContent(item.contentKind)) return item.specialNo ?? "特别内容";
  return item.episodeNo === undefined
    ? "当前视频"
    : `第 ${String(item.episodeNo).padStart(2, "0")} 集`;
}

/** 定位路由任务及可选文件索引对应的初始播放项。 */
export function resolveInitialPlaylistItem(
  items: RemotePlaylistItem[],
  taskId: string,
  fileIndex?: number
): RemotePlaylistItem | undefined {
  return items.find((item) => item.task.id === taskId && item.fileIndex === fileIndex)
    ?? items.find((item) => item.task.id === taskId);
}

/** 返回严格相邻集的首选版本，同集其他字幕组不会成为上一集或下一集。 */
export function resolveAdjacentPlaylistItem(
  items: RemotePlaylistItem[],
  activeItem: RemotePlaylistItem | null,
  direction: EpisodeNavigationDirection
): RemotePlaylistItem | undefined {
  return resolveAdjacentEpisodeItem(items, activeItem, direction);
}

/** 返回播放器 URL 中合法的文件索引。 */
export function readPlaylistFileIndex(search: string): number | undefined {
  const value = new URLSearchParams(search).get("file");
  if (value === null || !/^\d+$/.test(value)) return undefined;
  const fileIndex = Number(value);
  return Number.isSafeInteger(fileIndex) ? fileIndex : undefined;
}

/** 判断下载任务是否具备媒体扫描兜底播放条件。 */
function isCompletedTask(task: DownloadTask): boolean {
  return task.progress >= 1 || task.status === "completed" || task.status === "seeding";
}

/** 判断文件名是否属于常见视频容器。 */
function isVideoFileName(fileName: string): boolean {
  const normalized = fileName.toLowerCase();
  const dotIndex = normalized.lastIndexOf(".");
  return dotIndex >= 0 && VIDEO_EXTENSIONS.has(normalized.slice(dotIndex));
}

/** 隐藏播放列表中的本地相对目录，仅显示文件名。 */
function displayFileName(fileName: string): string {
  return fileName.split(/[\\/]/).filter(Boolean).at(-1) ?? fileName;
}

/** 组合稳定的单文件播放项和面向用户的主标题。 */
function buildPlaylistItem(
  task: DownloadTask,
  sourceName: string,
  fileIndex?: number,
  size?: number,
  fileEpisodeNo?: number
): RemotePlaylistItem {
  const episodeNo = fileEpisodeNo ?? task.episodeNo;
  const content = inferMediaContent(sourceName, episodeNo);
  const animeTitle = task.animeTitle?.trim() || displayFileName(task.name);
  return {
    id: fileIndex === undefined ? `${task.id}:auto` : `${task.id}:file:${fileIndex}`,
    task,
    fileIndex,
    episodeNo: isSpecialMediaContent(content.contentKind) ? undefined : episodeNo,
    fileName: displayFileName(sourceName),
    displayTitle: formatMediaDisplayTitle(animeTitle, content, episodeNo),
    contentKind: content.contentKind,
    specialNo: content.specialNo,
    size
  };
}
