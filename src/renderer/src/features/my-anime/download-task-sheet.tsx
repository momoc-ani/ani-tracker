import { ChevronDown, ChevronRight, Download, FolderOpen, Play, Trash2 } from "lucide-react";
import { useState } from "react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty";
import { Field, FieldLabel } from "@/components/ui/field";
import { Progress } from "@/components/ui/progress";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ConfirmActionDialog } from "@/components/confirm-action-dialog";
import { ReleaseMetadataBadges } from "@/components/release-metadata-badges";
import { WorkbenchSheet } from "@/components/workbench-sheet";
import { appApi } from "@/lib/api";
import { cn } from "@/lib/cn";
import { formatDateTime, formatPercent, formatSpeed } from "@/lib/format";
import { getAppRuntime } from "@/lib/runtime";
import { resolveAnimeTitleDisplay } from "@shared/anime-title";
import { isActiveDownloadTask, isCompletedDownloadTask } from "@shared/download-status";
import type { DownloadTask, MyAnime, TorrentFile } from "@shared/domain";
import type { MediaPlaybackTarget } from "@shared/player-selection";

export type AnimeDownloadDetailFilter = "all" | "active" | "completed";

export interface AnimeDownloadDetailState {
  item: MyAnime;
  filter: AnimeDownloadDetailFilter;
}

interface AnimeDownloadTaskSheetProps {
  detail: AnimeDownloadDetailState;
  downloadTasks: DownloadTask[];
  fansubNames: Map<string, string>;
  onClose: () => void;
  onFilterChange: (filter: AnimeDownloadDetailFilter) => void;
  onPlayMedia?: (target: MediaPlaybackTarget) => Promise<void>;
  onRemoveTask?: (taskId: string, deleteFiles: boolean) => Promise<void>;
}

const filterOptions: Array<{ value: AnimeDownloadDetailFilter; label: string }> = [
  { value: "all", label: "全部" },
  { value: "active", label: "下载中" },
  { value: "completed", label: "已完成" }
];

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

/** 渲染单部番剧的任务筛选、进度、播放与文件定位侧栏。 */
export function AnimeDownloadTaskSheet({
  detail,
  downloadTasks,
  fansubNames,
  onClose,
  onFilterChange,
  onPlayMedia,
  onRemoveTask
}: AnimeDownloadTaskSheetProps) {
  const titleDisplay = resolveAnimeTitleDisplay(detail.item.anime);
  const animeTasks = getAnimeDownloadTasks(downloadTasks, detail.item.anime.id);
  const visibleTasks = filterDownloadTasks(animeTasks, detail.filter);
  const [removeTarget, setRemoveTarget] = useState<DownloadTask | null>(null);
  const [deleteFilesOnRemove, setDeleteFilesOnRemove] = useState(false);
  const counts = {
    all: animeTasks.length,
    active: animeTasks.filter(isActiveDownload).length,
    completed: animeTasks.filter(isCompletedDownload).length
  };
  return (
    <WorkbenchSheet
      className="sm:max-w-2xl"
      description={titleDisplay.subtitle ?? "下载任务明细"}
      headerContent={
        <Tabs
          value={detail.filter}
          onValueChange={(value) => onFilterChange(value as AnimeDownloadDetailFilter)}
        >
          <TabsList className="grid w-full grid-cols-3">
            {filterOptions.map((filter) => (
              <TabsTrigger className="min-w-0 px-2" key={filter.value} value={filter.value}>
                {filter.label} {counts[filter.value]}
              </TabsTrigger>
            ))}
          </TabsList>
        </Tabs>
      }
      onClose={onClose}
      title={titleDisplay.title}
    >
      {visibleTasks.length > 0 ? (
        <div className="flex min-w-0 flex-col gap-3">
          {visibleTasks.map((task) => (
            <DownloadTaskCard
              key={task.id}
              task={task}
              fansubNames={fansubNames}
              onPlayMedia={onPlayMedia}
              showLocalDetails={detail.filter !== "completed"}
              onRequestRemove={detail.filter === "completed" && onRemoveTask
                ? () => setRemoveTarget(task)
                : undefined}
            />
          ))}
        </div>
      ) : (
        <Empty className="min-h-56 p-4 md:p-8">
          <EmptyHeader>
            <EmptyMedia variant="icon"><Download /></EmptyMedia>
            <EmptyTitle>暂无下载任务</EmptyTitle>
            <EmptyDescription>当前筛选下没有下载任务。</EmptyDescription>
          </EmptyHeader>
        </Empty>
      )}
      {onRemoveTask && (
        <ConfirmActionDialog
          confirmLabel={deleteFilesOnRemove ? "删除任务和文件" : "移除任务"}
          content={
            <Field className="rounded-md border p-3" orientation="horizontal">
              <Checkbox
                checked={deleteFilesOnRemove}
                id="my-anime-delete-completed-files"
                onCheckedChange={(checked) => setDeleteFilesOnRemove(checked === true)}
              />
              <FieldLabel className="min-w-0 cursor-pointer font-normal" htmlFor="my-anime-delete-completed-files">
                <span className="block text-sm font-medium">同时删除原文件</span>
                <span className="mt-1 block text-xs text-muted-foreground">文件删除后无法从应用内恢复。</span>
              </FieldLabel>
            </Field>
          }
          description={removeTarget
            ? deleteFilesOnRemove
              ? `下载任务「${removeTarget.name}」及其原文件将被永久删除。`
              : `下载任务「${removeTarget.name}」将从任务列表中移除，已下载文件会保留。`
            : "该下载任务将从任务列表中移除。"}
          onConfirm={async () => {
            if (!removeTarget) return;
            await onRemoveTask(removeTarget.id, deleteFilesOnRemove);
          }}
          onOpenChange={(open) => {
            if (!open) {
              setRemoveTarget(null);
              setDeleteFilesOnRemove(false);
            }
          }}
          open={Boolean(removeTarget)}
          title="确认移除已完成资源？"
        />
      )}
    </WorkbenchSheet>
  );
}

/** 判断任务是否处于需要持续关注的活动状态。 */
export function isActiveDownload(task: DownloadTask): boolean {
  return isActiveDownloadTask(task);
}

/** 判断任务是否拥有已完成内容。 */
export function isCompletedDownload(task: DownloadTask): boolean {
  return isCompletedDownloadTask(task);
}

/** 渲染单个下载任务的完整进度与文件动作。 */
function DownloadTaskCard({
  task,
  fansubNames,
  onPlayMedia,
  showLocalDetails,
  onRequestRemove
}: {
  task: DownloadTask;
  fansubNames: Map<string, string>;
  onPlayMedia?: (target: MediaPlaybackTarget) => Promise<void>;
  showLocalDetails: boolean;
  onRequestRemove?: () => void;
}) {
  const runtime = getAppRuntime();
  const mobileRuntime = runtime === "android" || runtime === "ios";
  const androidRuntime = runtime === "android";
  const supportsReveal = runtime === "desktop" || androidRuntime;
  const fansubName = (task.fansubGroupId ? fansubNames.get(task.fansubGroupId) : undefined) ?? task.fansubName ?? "未识别字幕组";
  const playableFile = resolveTaskFile(task, true);
  const revealFile = resolveTaskFile(task, false);
  const playableFilePath = playableFile?.filePath;
  const revealFilePath = revealFile?.filePath;
  const collectionFiles = task.files.filter((file) => file.selected && isVideoTaskFile(file));
  const visibleFiles = mobileRuntime ? task.files.filter((file) => file.selected) : collectionFiles;
  const isCollection = collectionFiles.length > 1;
  const showFileList = mobileRuntime || isCollection;
  const [collectionOpen, setCollectionOpen] = useState(mobileRuntime);
  const [activeFileAction, setActiveFileAction] = useState<string | null>(null);
  const [fileActionError, setFileActionError] = useState<string | null>(null);

  /** 播放已完成视频或在文件管理器中定位下载文件。 */
  async function runFileAction(action: "play" | "reveal", target?: ResolvedTaskFile) {
    const resolved = target ?? (action === "play" ? playableFile : revealFile);
    if (!resolved) return;
    const filePath = resolved.filePath;

    setActiveFileAction(`${action}:${resolved.fileIndex}`);
    try {
      if (action === "play" && onPlayMedia) {
        await onPlayMedia({
          filePath: resolved.filePath,
          taskId: task.id,
          fileIndex: resolved.fileIndex
        });
      } else if (action === "play") await appApi.playMedia(filePath);
      else await appApi.revealMedia(filePath);
      setFileActionError(null);
    } catch (error) {
      setFileActionError(error instanceof Error ? error.message : action === "play" ? "播放失败" : "打开目录失败");
    } finally {
      setActiveFileAction(null);
    }
  }

  return (
    <article className="rounded-md border bg-background p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 flex-wrap items-center gap-2">
            {task.episodeNo !== undefined && <Badge tone="blue">第 {task.episodeNo} 集</Badge>}
            <Badge tone={getDownloadStatusTone(task.status)}>{downloadStatusText[task.status]}</Badge>
            <Badge>{fansubName}</Badge>
            <ReleaseMetadataBadges metadata={task} />
          </div>
          <h3 className="mt-2 truncate text-sm font-medium" title={task.name}>{task.name}</h3>
        </div>
        <div className="shrink-0 text-sm font-medium tabular-nums">{formatPercent(task.progress)}</div>
      </div>

      <Progress className="mt-3" value={task.progress} />

      {showLocalDetails && (
        <dl className="mt-4 grid grid-cols-1 gap-x-4 gap-y-2 text-xs sm:grid-cols-2">
          <DownloadTaskMeta className="sm:col-span-2" label="保存路径" value={task.savePath} />
          <DownloadTaskMeta label="创建时间" value={formatDateTime(task.createdAt)} />
          <DownloadTaskMeta label="完成时间" value={formatDateTime(task.completedAt)} />
          <DownloadTaskMeta label="下载速度" value={formatSpeed(task.downloadSpeed)} />
          <DownloadTaskMeta label="上传速度" value={formatSpeed(task.uploadSpeed)} />
        </dl>
      )}

      {fileActionError && (
        <Alert className="mt-4" variant="destructive">
          <AlertTitle>文件操作失败</AlertTitle>
          <AlertDescription>{fileActionError}</AlertDescription>
        </Alert>
      )}

      {showFileList && (
        <Collapsible className="mt-4 border-t pt-3" open={collectionOpen} onOpenChange={setCollectionOpen}>
          <CollapsibleTrigger asChild>
            <Button className="w-full justify-between" type="button" variant="secondary">
              <span className="flex min-w-0 items-center gap-2">
                {collectionOpen ? <ChevronDown data-icon="inline-start" /> : <ChevronRight data-icon="inline-start" />}
                {mobileRuntime ? "下载文件" : "合集文件"}
              </span>
              <Badge>{visibleFiles.length} 个</Badge>
            </Button>
          </CollapsibleTrigger>
          <CollapsibleContent>
            <div className="mt-2 divide-y border-y">
              {visibleFiles.map((file) => {
                const target = resolveTorrentFile(task, file);
                const completed = file.progress >= 1;
                const playable = isVideoTaskFile(file);
                return (
                  <div className="flex min-w-0 items-center gap-2 px-2 py-2" key={file.id}>
                    <Badge tone={completed ? "green" : "neutral"}>
                      {file.episodeNo !== undefined ? `第 ${file.episodeNo} 集` : `文件 ${file.index + 1}`}
                    </Badge>
                    <span className="min-w-0 flex-1 truncate text-xs" title={file.name}>{file.name}</span>
                    <span className="shrink-0 text-xs tabular-nums text-muted-foreground">{formatPercent(file.progress)}</span>
                    {playable && (
                      <Button
                        aria-label={`播放${file.episodeNo !== undefined ? `第 ${file.episodeNo} 集` : file.name}`}
                        disabled={!completed || activeFileAction !== null}
                        onClick={() => void runFileAction("play", target)}
                        size="icon"
                        title={completed ? "播放文件" : "文件尚未下载完成"}
                        variant="ghost"
                      >
                        <Play />
                      </Button>
                    )}
                    {supportsReveal && (
                      <Button
                        aria-label={`定位${file.episodeNo !== undefined ? `第 ${file.episodeNo} 集` : file.name}`}
                        disabled={activeFileAction !== null}
                        onClick={() => void runFileAction("reveal", target)}
                        size="icon"
                        title={androidRuntime ? "打开系统目录" : "打开文件目录"}
                        variant="ghost"
                      >
                        <FolderOpen />
                      </Button>
                    )}
                  </div>
                );
              })}
            </div>
          </CollapsibleContent>
        </Collapsible>
      )}

      {isCompletedDownload(task) && (
        <div className="mt-4 flex flex-col gap-3 border-t pt-3 sm:flex-row sm:items-center sm:justify-between">
          {onRequestRemove && (
            <Button
              className="w-full sm:mr-auto sm:w-auto"
              onClick={onRequestRemove}
              size="compact"
              variant="destructive"
            >
              <Trash2 data-icon="inline-start" />
              删除资源
            </Button>
          )}
          {!isCollection && (
            <div className={cn(
              "grid w-full gap-2 sm:ml-auto sm:flex sm:w-auto",
              supportsReveal ? "grid-cols-2" : "grid-cols-1"
            )}>
              <Button
                aria-label="播放已完成视频"
                className="h-11 px-2 text-xs sm:h-8"
                disabled={!playableFilePath || activeFileAction !== null}
                onClick={() => void runFileAction("play")}
                title={playableFilePath ? "播放已完成视频" : "未找到可播放的视频文件"}
                variant="outline"
              >
                <Play data-icon="inline-start" />
                播放
              </Button>
              {supportsReveal && (
                <Button
                  aria-label={androidRuntime ? "打开系统目录" : "打开文件目录"}
                  className="h-11 px-2 text-xs sm:h-8"
                  disabled={!revealFilePath || activeFileAction !== null}
                  onClick={() => void runFileAction("reveal")}
                  title={revealFilePath ? (androidRuntime ? "打开系统目录" : "打开文件所在目录") : "未找到已完成文件"}
                  variant="outline"
                >
                  <FolderOpen data-icon="inline-start" />
                  {androidRuntime ? "系统目录" : "打开目录"}
                </Button>
              )}
            </div>
          )}
        </div>
      )}
    </article>
  );
}

/** 渲染任务定义列表中的单项元信息。 */
function DownloadTaskMeta({ className, label, value }: { className?: string; label: string; value: string }) {
  return (
    <div className={cn("min-w-0", className)}>
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="mt-1 truncate font-medium" title={value}>{value}</dd>
    </div>
  );
}

/** 读取并按集数、创建时间排序某部番的下载任务。 */
function getAnimeDownloadTasks(downloadTasks: DownloadTask[], animeId: string): DownloadTask[] {
  return downloadTasks
    .filter((task) => task.animeId === animeId)
    .sort((left, right) => {
      const episodeOrder = (right.episodeNo ?? -1) - (left.episodeNo ?? -1);
      return episodeOrder || right.createdAt.localeCompare(left.createdAt);
    });
}

/** 按当前标签筛选下载任务。 */
function filterDownloadTasks(downloadTasks: DownloadTask[], filter: AnimeDownloadDetailFilter): DownloadTask[] {
  if (filter === "active") return downloadTasks.filter(isActiveDownload);
  if (filter === "completed") return downloadTasks.filter(isCompletedDownload);
  return downloadTasks;
}

/** 将任务状态映射为语义标签色。 */
function getDownloadStatusTone(status: DownloadTask["status"]): "neutral" | "green" | "amber" | "red" | "blue" {
  if (status === "completed" || status === "seeding") return "green";
  if (status === "error" || status === "missing_files") return "red";
  if (status === "paused" || status === "stalled" || status === "waiting_network") return "amber";
  if (status === "downloading") return "blue";
  return "neutral";
}

interface ResolvedTaskFile {
  filePath: string;
  fileIndex: number;
}

/** 选择任务中的完整文件，并生成播放器或文件管理器可用的目标。 */
function resolveTaskFile(task: DownloadTask, videoOnly: boolean): ResolvedTaskFile | undefined {
  const completedFiles = task.files.filter((file) => file.selected && file.progress >= 1);
  const videoFile = completedFiles.find(isVideoTaskFile);
  const targetFile = videoOnly ? videoFile : videoFile ?? completedFiles[0];
  return targetFile ? resolveTorrentFile(task, targetFile) : undefined;
}

/** 将任务文件转换为播放器和文件管理器使用的目标。 */
function resolveTorrentFile(task: DownloadTask, file: TorrentFile): ResolvedTaskFile {
  return {
    filePath: joinDownloadFilePath(task.savePath, file.name),
    fileIndex: file.index
  };
}

/** 判断下载文件是否为播放器支持的视频类型。 */
function isVideoTaskFile(file: TorrentFile): boolean {
  return /\.(mkv|mp4|avi|mov|webm|m4v|ts)$/i.test(file.name);
}

/** 按任务保存路径的格式拼接 qBittorrent 返回的相对文件名。 */
function joinDownloadFilePath(savePath: string, fileName: string): string {
  if (/^(?:[A-Za-z]:[\\/]|\/|\\\\)/.test(fileName)) return fileName;
  const separator = savePath.includes("\\") ? "\\" : "/";
  const basePath = savePath.replace(/[\\/]+$/, "");
  const relativePath = fileName.replace(/^[\\/]+/, "").replace(/[\\/]+/g, separator);
  return `${basePath}${separator}${relativePath}`;
}
