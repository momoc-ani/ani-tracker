import {
  ChevronDown,
  ChevronRight,
  Clock3,
  Download as DownloadIcon,
  FileUp,
  FileSearch,
  Files,
  Folder,
  Gauge,
  Pause,
  Play,
  RotateCcw,
  Trash2,
  Upload
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { FormEvent } from "react";
import { toast } from "@/lib/toast";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty";
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Progress } from "@/components/ui/progress";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ConfirmActionDialog } from "@/components/confirm-action-dialog";
import { Page, PageActions, PageHeader, PageHeading } from "@/components/page-layout";
import { ReleaseMetadataBadges } from "@/components/release-metadata-badges";
import { cn } from "@/lib/cn";
import { formatBytes, formatDuration, formatPercent, formatSpeed } from "@/lib/format";
import { getAppCapabilities } from "@/lib/runtime";
import type { AppClient } from "@shared/app-client";
import {
  isActiveDownloadTask,
  isCompletedDownloadTask,
  isFinishedDownloadTask,
  isSeedingDownloadTask
} from "@shared/download-status";
import type { DownloadStatus, DownloadTask, MyAnime } from "@shared/domain";

type DownloadView = "active" | "seeding" | "completed";

export type DownloadQueueClient = Pick<
  AppClient,
  "listDownloads" | "listMyAnime" | "refreshDownloads" | "pauseDownload" | "resumeDownload"
> & Partial<Pick<
  AppClient,
  "addDownloadUrl" | "importTorrentFile" | "removeDownload" | "scanDownloadMedia" | "setDownloadFilePriority"
>>;

interface DownloadQueuePageProps {
  client: DownloadQueueClient;
  logScope: "local" | "remote";
  showLocalPaths?: boolean;
}

const downloadStatusText: Record<DownloadStatus, string> = {
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

/** 渲染本地与远端共用的下载队列页面。 */
export function DownloadQueuePage({
  client,
  logScope,
  showLocalPaths = true
}: DownloadQueuePageProps) {
  const capabilities = getAppCapabilities();
  const [tasks, setTasks] = useState<DownloadTask[]>([]);
  const [myAnime, setMyAnime] = useState<MyAnime[]>([]);
  const [view, setView] = useState<DownloadView>("active");
  const [collapsedGroupKeys, setCollapsedGroupKeys] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [mutatingTaskId, setMutatingTaskId] = useState<string | null>(null);
  const [removeTarget, setRemoveTarget] = useState<DownloadTask | null>(null);
  const [deleteFilesOnRemove, setDeleteFilesOnRemove] = useState(false);
  const [mutatingFileId, setMutatingFileId] = useState<string | null>(null);
  const [scanningTaskId, setScanningTaskId] = useState<string | null>(null);
  const [downloadUrl, setDownloadUrl] = useState("");
  const [downloadUrlError, setDownloadUrlError] = useState<string | null>(null);
  const [addingDownload, setAddingDownload] = useState(false);
  const [importingTorrent, setImportingTorrent] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [updatedAt, setUpdatedAt] = useState<string | null>(null);
  const downloadUrlInputRef = useRef<HTMLInputElement>(null);

  const activeTasks = useMemo(() => tasks.filter((task) => !isCompletedTask(task)), [tasks]);
  const seedingTasks = useMemo(() => tasks.filter(isSeedingDownloadTask), [tasks]);
  const completedTasks = useMemo(() => tasks.filter(isFinishedDownloadTask), [tasks]);
  const visibleTasks = view === "active"
    ? activeTasks
    : view === "seeding"
      ? seedingTasks
      : completedTasks;
  const animeGroups = useMemo(() => groupDownloadTasks(visibleTasks, myAnime), [visibleTasks, myAnime]);

  const refresh = useCallback(async (silent = false) => {
    if (!silent) setRefreshing(true);
    try {
      const updated = await client.refreshDownloads();
      setTasks(updated);
      setUpdatedAt(new Date().toLocaleTimeString());
      setError(null);
      if (!silent) console.info(`[${logScope}] 下载队列刷新完成`, { taskCount: updated.length });
    } catch (caught) {
      console.error(`[${logScope}] 下载队列刷新失败`, { error: caught });
      setError(caught instanceof Error ? caught.message : "刷新下载状态失败");
    } finally {
      if (!silent) setRefreshing(false);
      setLoading(false);
    }
  }, [client, logScope]);

  /** 对下载任务执行暂停、继续或移除操作。 */
  async function mutateTask(
    taskId: string,
    action: "pause" | "resume" | "remove",
    deleteFiles = false
  ): Promise<boolean> {
    setMutatingTaskId(taskId);
    try {
      const updated = action === "pause"
        ? await client.pauseDownload(taskId)
        : action === "resume"
          ? await client.resumeDownload(taskId)
          : await requireCapability(client.removeDownload, "移除下载任务")(taskId, deleteFiles);
      setTasks(updated);
      setError(null);
      console.info(`[${logScope}] 下载任务操作完成`, { taskId, action });
      return true;
    } catch (caught) {
      console.error(`[${logScope}] 下载任务操作失败`, { taskId, action, error: caught });
      setError(caught instanceof Error ? caught.message : "下载任务操作失败");
      return false;
    } finally {
      setMutatingTaskId(null);
    }
  }

  /** 校验并添加 magnet 或 torrent 下载地址。 */
  async function addDownload(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const url = downloadUrl.trim();
    if (!isValidDownloadUrl(url)) {
      setDownloadUrlError("请输入有效的 magnet 或 HTTP(S) torrent 地址");
      return;
    }

    setAddingDownload(true);
    setDownloadUrlError(null);
    try {
      const updated = await requireCapability(client.addDownloadUrl, "添加下载任务")({ url });
      setTasks(updated);
      setDownloadUrl("");
      setError(null);
      setView("active");
      console.info(`[${logScope}] 下载任务添加完成`, { taskCount: updated.length });
      toast.success("已添加到下载队列");
    } catch (caught) {
      console.error(`[${logScope}] 下载任务添加失败`, { error: caught });
      setError(caught instanceof Error ? caught.message : "添加下载失败");
    } finally {
      setAddingDownload(false);
    }
  }

  /** 使用系统文件选择器导入本地 torrent 文件。 */
  async function importTorrentFile() {
    if (!client.importTorrentFile) return;
    setImportingTorrent(true);
    try {
      const updated = await client.importTorrentFile();
      if (!updated) return;
      setTasks(updated);
      setView("active");
      setError(null);
      console.info(`[${logScope}] torrent 文件导入完成`, { taskCount: updated.length });
      toast.success("torrent 文件已加入下载队列");
    } catch (caught) {
      console.error(`[${logScope}] torrent 文件导入失败`, { error: caught });
      setError(caught instanceof Error ? caught.message : "torrent 文件导入失败");
    } finally {
      setImportingTorrent(false);
    }
  }

  /** 扫描已下载文件并反馈媒体入库结果。 */
  async function scanTask(taskId: string) {
    setScanningTaskId(taskId);
    try {
      const result = await requireCapability(client.scanDownloadMedia, "扫描下载媒体")(taskId);
      const summary = `媒体扫描完成：入库 ${result.mediaFiles.length} 个，跳过 ${result.skippedFiles.length} 个，失败 ${result.errors.length} 个`;
      if (result.errors.length) {
        setError(result.errors[0]?.message ?? summary);
        toast.warning(summary);
      } else {
        setError(null);
        toast.success(summary);
      }
    } catch (caught) {
      console.error(`[${logScope}] 下载媒体扫描失败`, { taskId, error: caught });
      setError(caught instanceof Error ? caught.message : "媒体信息扫描失败");
    } finally {
      setScanningTaskId(null);
    }
  }

  /** 切换单个 torrent 文件的下载优先级。 */
  async function toggleFileSelection(task: DownloadTask, file: DownloadTask["files"][number]) {
    setMutatingFileId(file.id);
    try {
      const updated = await requireCapability(client.setDownloadFilePriority, "设置文件优先级")(
        task.id,
        [file.index],
        file.selected ? 0 : 1
      );
      setTasks(updated);
      setError(null);
    } catch (caught) {
      console.error(`[${logScope}] 下载文件优先级更新失败`, { taskId: task.id, fileIndex: file.index, error: caught });
      setError(caught instanceof Error ? caught.message : "文件选择更新失败");
    } finally {
      setMutatingFileId(null);
    }
  }

  useEffect(() => {
    let active = true;

    Promise.all([client.listDownloads(), client.listMyAnime()])
      .then(([items, animeItems]) => {
        if (active) {
          setTasks(items);
          setMyAnime(animeItems);
          setLoading(false);
          console.info(`[${logScope}] 下载队列读取完成`, { taskCount: items.length });
        }
      })
      .catch((caught) => {
        if (active) {
          console.error(`[${logScope}] 下载队列读取失败`, { error: caught });
          setError(caught instanceof Error ? caught.message : "加载下载队列失败");
          setLoading(false);
        }
      });

    void refresh(true);
    const timer = window.setInterval(() => void refresh(true), 2000);

    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, [client, logScope, refresh]);

  /** 切换单个番剧任务区的折叠状态。 */
  function toggleGroup(groupKey: string) {
    setCollapsedGroupKeys((current) => {
      const next = new Set(current);
      if (next.has(groupKey)) next.delete(groupKey);
      else next.add(groupKey);
      return next;
    });
  }

  /** 定位到添加下载输入框，提供顶部快捷入口。 */
  function focusDownloadInput() {
    downloadUrlInputRef.current?.scrollIntoView({ behavior: "smooth", block: "center" });
    window.setTimeout(() => downloadUrlInputRef.current?.focus({ preventScroll: true }), 250);
  }

  if (loading) return <DownloadQueuePageSkeleton />;

  return (
    <Page>
      <PageHeader className="border-b pb-4 sm:items-center">
        <PageHeading
          description="管理磁力与种子任务，观察实时进度与文件选择。"
          title={<span className="text-primary">下载队列</span>}
        />
      </PageHeader>

      {error && (
        <Alert variant="destructive">
          <AlertTitle>下载队列操作失败</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}

      {client.addDownloadUrl && (
        <section className="rounded-md border bg-card p-4">
          <form onSubmit={(event) => void addDownload(event)}>
            <FieldGroup className="gap-3 md:flex-row md:items-start">
              <Field className="min-w-0 flex-1" data-invalid={Boolean(downloadUrlError)}>
                <FieldLabel className="sr-only" htmlFor="download-url">magnet 或 torrent 地址</FieldLabel>
                <Input
                  ref={downloadUrlInputRef}
                  id="download-url"
                  aria-invalid={Boolean(downloadUrlError)}
                  disabled={addingDownload}
                  placeholder="magnet 或 torrent 地址"
                  value={downloadUrl}
                  onChange={(event) => {
                    setDownloadUrl(event.target.value);
                    if (downloadUrlError) setDownloadUrlError(null);
                  }}
                />
                {downloadUrlError && <FieldDescription className="text-destructive">{downloadUrlError}</FieldDescription>}
              </Field>
              <Field className="w-full md:w-auto">
                <FieldLabel className="sr-only" htmlFor="add-download">添加下载</FieldLabel>
                <Button id="add-download" className="w-full md:w-auto" type="submit" disabled={addingDownload}>
                  <DownloadIcon data-icon="inline-start" />
                  {addingDownload ? "添加中" : "添加下载"}
                </Button>
              </Field>
              {client.importTorrentFile && (
                <Field className="w-full md:w-auto">
                  <FieldLabel className="sr-only" htmlFor="import-torrent">导入 torrent 文件</FieldLabel>
                  <Button
                    className="w-full md:w-auto"
                    disabled={addingDownload || importingTorrent}
                    id="import-torrent"
                    onClick={() => void importTorrentFile()}
                    type="button"
                    variant="outline"
                  >
                    <FileUp data-icon="inline-start" />
                    {importingTorrent ? "导入中" : "导入文件"}
                  </Button>
                </Field>
              )}
            </FieldGroup>
          </form>
        </section>
      )}

      <Tabs value={view} onValueChange={(value) => setView(value as DownloadView)}>
        <div className="flex min-w-0 flex-col gap-3 sm:flex-row sm:flex-wrap sm:items-end sm:justify-between">
          <TabsList className="grid w-full grid-cols-3 sm:flex sm:w-fit" variant="line" aria-label="下载任务视图">
            <TabsTrigger value="active">
              正在下载
              <Badge className="ml-1 h-5 border-0 px-1.5">{activeTasks.length}</Badge>
            </TabsTrigger>
            <TabsTrigger value="seeding">
              做种中
              <Badge className="ml-1 h-5 border-0 px-1.5">{seedingTasks.length}</Badge>
            </TabsTrigger>
            <TabsTrigger value="completed">
              完成任务
              <Badge className="ml-1 h-5 border-0 px-1.5">{completedTasks.length}</Badge>
            </TabsTrigger>
          </TabsList>
          <PageActions className="sm:w-auto sm:flex-nowrap sm:justify-end">
            <div className="min-w-24 text-left sm:text-right">
              <div className="text-xs text-muted-foreground">最后刷新</div>
              <div className="text-sm font-semibold tabular-nums">{updatedAt ?? "尚未刷新"}</div>
            </div>
            <Button variant="outline" onClick={() => void refresh()} disabled={refreshing}>
              <RotateCcw data-icon="inline-start" className={cn(refreshing && "animate-spin")} />
              {refreshing ? "刷新中" : "刷新状态"}
            </Button>
            {client.addDownloadUrl && (
              <Button
                aria-label="定位到添加下载"
                className="size-11 p-0 md:size-9"
                onClick={focusDownloadInput}
                title="添加下载"
                variant="ghost"
              >
                <DownloadIcon />
              </Button>
            )}
          </PageActions>
        </div>

        <TabsContent className="mt-6" forceMount value={view}>
          {animeGroups.length > 0 ? (
            <div className="flex min-w-0 flex-col gap-8">
              {animeGroups.map((animeGroup) => {
                const collapsed = collapsedGroupKeys.has(animeGroup.key);
                return (
                  <section className="min-w-0" key={animeGroup.key}>
                    <div className="flex flex-col gap-3 border-b-2 border-primary/20 pb-2 sm:flex-row sm:items-end sm:justify-between">
                      <div className="flex min-w-0 items-center gap-2">
                        <Button
                          className="size-11 shrink-0 p-0 md:size-9"
                          variant="ghost"
                          aria-expanded={!collapsed}
                          aria-label={collapsed ? "展开任务组" : "收起任务组"}
                          onClick={() => toggleGroup(animeGroup.key)}
                        >
                          {collapsed ? <ChevronRight /> : <ChevronDown />}
                        </Button>
                        <div className="min-w-0">
                          <h2 className="truncate text-base font-semibold text-primary">{animeGroup.title}</h2>
                          <div className="mt-1 flex flex-wrap gap-1.5">
                            <Badge>{animeGroup.tasks.length} 个任务</Badge>
                            <Badge tone="green">{animeGroup.completedEpisodes} 集已完成</Badge>
                            <Badge tone="blue">{animeGroup.activeEpisodes} 集进行中</Badge>
                          </div>
                        </div>
                      </div>
                      {showLocalPaths && <div className="flex min-w-0 items-center gap-2 text-xs text-muted-foreground sm:max-w-[45%]">
                        <Folder className="size-4 shrink-0" />
                        <span className="truncate" title={animeGroup.savePath}>{animeGroup.savePath}</span>
                      </div>}
                    </div>

                    {!collapsed && (
                      <div className="mt-4 flex min-w-0 flex-col gap-4">
                        {animeGroup.fansubGroups.map((fansubGroup) => (
                          <div className="min-w-0 overflow-hidden rounded-md border bg-card" key={fansubGroup.key}>
                            <div className="flex items-center justify-between gap-3 border-b bg-muted/50 px-4 py-2">
                              <span className="truncate text-xs font-semibold text-muted-foreground">{fansubGroup.name}</span>
                              <span className="shrink-0 text-xs text-muted-foreground">{formatEpisodeRange(fansubGroup.tasks)}</span>
                            </div>
                            <div className="divide-y">
                              {fansubGroup.tasks.map((task) => (
                                <DownloadTaskRow
                                  key={task.id}
                                  task={task}
                                  mutatingTaskId={mutatingTaskId}
                                  mutatingFileId={mutatingFileId}
                                  scanningTaskId={scanningTaskId}
                                  onMutate={mutateTask}
                                  onRequestRemove={client.removeDownload ? setRemoveTarget : undefined}
                                  onScan={capabilities.mediaScan && client.scanDownloadMedia ? scanTask : undefined}
                                  onToggleFile={client.setDownloadFilePriority ? toggleFileSelection : undefined}
                                />
                              ))}
                            </div>
                          </div>
                        ))}
                      </div>
                    )}
                  </section>
                );
              })}
            </div>
          ) : (
            <Empty className="min-h-72">
              <EmptyHeader>
                <EmptyMedia variant="icon"><DownloadIcon /></EmptyMedia>
                <EmptyTitle>{getEmptyViewTitle(view)}</EmptyTitle>
                <EmptyDescription>
                  {getEmptyViewDescription(view)}
                </EmptyDescription>
              </EmptyHeader>
            </Empty>
          )}
        </TabsContent>
      </Tabs>

      {client.removeDownload && (
        <ConfirmActionDialog
          confirmLabel={deleteFilesOnRemove ? "删除任务和文件" : "移除任务"}
          content={removeTarget ? (
            <div
              className={cn(
                "flex items-start gap-3 rounded-md border p-3",
                deleteFilesOnRemove && "border-destructive bg-destructive/5"
              )}
            >
              <Checkbox
                checked={deleteFilesOnRemove}
                id="downloads-delete-files"
                onCheckedChange={(checked) => setDeleteFilesOnRemove(checked === true)}
              />
              <label className="min-w-0 cursor-pointer" htmlFor="downloads-delete-files">
                <span className="block text-sm font-medium">同时删除原文件</span>
                <span className="mt-1 block text-xs text-muted-foreground">
                  会删除任务已写入的完整或部分文件，且无法从应用内恢复。
                </span>
              </label>
            </div>
          ) : undefined}
          description={removeTarget
            ? deleteFilesOnRemove
              ? `下载任务「${removeTarget.name}」及其原文件将被永久删除。`
              : `下载任务「${removeTarget.name}」将从队列中移除，已下载文件会保留。`
            : "该下载任务将从队列中移除。"}
          onConfirm={async () => {
            if (removeTarget && !(await mutateTask(removeTarget.id, "remove", deleteFilesOnRemove))) {
              throw new Error("下载任务移除失败");
            }
            toast.success(deleteFilesOnRemove ? "任务和原文件已删除" : "任务已移除，文件已保留");
          }}
          onOpenChange={(open) => {
            if (!open) {
              setRemoveTarget(null);
              setDeleteFilesOnRemove(false);
            }
          }}
          open={Boolean(removeTarget)}
          title="确认移除下载任务？"
        />
      )}
    </Page>
  );
}

function DownloadTaskRow({
  task,
  mutatingTaskId,
  mutatingFileId,
  scanningTaskId,
  onMutate,
  onRequestRemove,
  onScan,
  onToggleFile
}: {
  task: DownloadTask;
  mutatingTaskId: string | null;
  mutatingFileId: string | null;
  scanningTaskId: string | null;
  onMutate: (taskId: string, action: "pause" | "resume" | "remove") => Promise<boolean>;
  onRequestRemove?: (task: DownloadTask) => void;
  onScan?: (taskId: string) => Promise<void>;
  onToggleFile?: (task: DownloadTask, file: DownloadTask["files"][number]) => Promise<void>;
}) {
  const [filesExpanded, setFilesExpanded] = useState(false);
  const totalSize = task.files.reduce((sum, file) => sum + file.size, 0);
  const downloadedSize = task.files.reduce((sum, file) => sum + file.size * file.progress, 0);
  const busy = mutatingTaskId === task.id;

  return (
    <article className={cn("min-w-0 border-l-2 border-l-transparent p-4", isErrorTask(task) && "border-l-destructive bg-destructive/5")}>
      <div className="flex min-w-0 flex-col gap-4">
        <div className="flex min-w-0 flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div className="flex min-w-0 flex-1 items-start gap-3">
            <div className="flex size-10 shrink-0 items-center justify-center rounded-md bg-primary/10 font-semibold text-primary tabular-nums">
              {task.episodeNo === undefined ? "--" : String(task.episodeNo).padStart(2, "0")}
            </div>
            <div className="min-w-0 flex-1">
              <div className="flex min-w-0 flex-wrap items-center gap-2">
                <Badge tone={getDownloadStatusTone(task)}>{getDownloadStatusText(task)}</Badge>
                <h3 className="min-w-0 break-words text-sm font-semibold leading-5">{task.name}</h3>
              </div>
              {isErrorTask(task) && (
                <p className="mt-1 text-xs text-destructive">下载引擎报告任务异常，可尝试继续任务或刷新状态。</p>
              )}
            </div>
          </div>
          <div className="flex shrink-0 flex-wrap items-center gap-1 sm:justify-end">
            {task.files.length > 0 && (
              <Button
                className="size-11 p-0 md:size-9"
                variant="ghost"
                aria-expanded={filesExpanded}
                aria-label={filesExpanded ? "收起文件列表" : "展开文件列表"}
                title={filesExpanded ? "收起文件列表" : "展开文件列表"}
                onClick={() => setFilesExpanded((current) => !current)}
              >
                {filesExpanded ? <ChevronDown /> : <ChevronRight />}
              </Button>
            )}
            {canPauseTask(task) && (
              <Button
                className="size-11 p-0 md:size-9"
                variant="ghost"
                aria-label="暂停下载"
                title="暂停下载"
                disabled={busy}
                onClick={() => void onMutate(task.id, "pause")}
              >
                <Pause />
              </Button>
            )}
            {canResumeTask(task) && (
              <Button
                className="size-11 p-0 md:size-9"
                variant="ghost"
                aria-label="继续下载"
                title="继续下载"
                disabled={busy}
                onClick={() => void onMutate(task.id, "resume")}
              >
                <Play />
              </Button>
            )}
            {onScan && (
              <Button
                className="size-11 p-0 md:size-9"
                variant="ghost"
                aria-label="扫描媒体信息"
                title="扫描媒体信息"
                disabled={scanningTaskId === task.id || !canScanTask(task)}
                onClick={() => void onScan(task.id)}
              >
                <FileSearch />
              </Button>
            )}
            {onRequestRemove && (
              <Button
                className="size-11 p-0 md:size-9"
                variant="ghost"
                aria-label="移除任务"
                title="移除任务"
                disabled={busy}
                onClick={() => onRequestRemove(task)}
              >
                <Trash2 />
              </Button>
            )}
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-x-5 gap-y-2 text-xs text-muted-foreground">
          <span className="flex items-center gap-1.5"><Gauge className="size-4" />{task.engine === "embedded" ? "内置引擎" : "qBittorrent"}</span>
          <ReleaseMetadataBadges metadata={task} />
          <span className="flex min-w-24 items-center gap-1.5 tabular-nums" title="下载速度">
            <DownloadIcon className="size-4" />
            {formatSpeed(task.downloadSpeed)}
          </span>
          <span className="flex min-w-24 items-center gap-1.5 tabular-nums" title="上传速度">
            <Upload className="size-4" />
            {formatSpeed(task.uploadSpeed)}
          </span>
          <span className="flex min-w-28 items-center gap-1.5 tabular-nums sm:ml-auto" title="预计剩余时间">
            <Clock3 className="size-4" />
            {formatDownloadEta(task)}
          </span>
        </div>

        <div className="grid min-w-0 grid-cols-[minmax(0,1fr)_3rem] items-center gap-3">
          <div className="min-w-0">
            <Progress value={task.progress} />
            <div className="mt-1 flex justify-between gap-3 text-xs text-muted-foreground">
              <span>{formatBytes(downloadedSize)} / {formatBytes(totalSize)}</span>
              <span className="truncate">{getDownloadStatusText(task)}</span>
            </div>
          </div>
          <span className="text-right text-sm font-semibold tabular-nums">{formatPercent(task.progress)}</span>
        </div>

        {filesExpanded && (
          <div className="flex min-w-0 flex-col gap-2 border-t pt-3">
            <div className="flex items-center gap-2 text-xs font-semibold text-muted-foreground">
              <Files className="size-4" />
              {onToggleFile ? "文件选择" : "文件列表"} · {task.files.length} 个
            </div>
            {task.files.map((file) => (
              <Field
                key={file.id}
                className="flex-wrap rounded-md bg-muted p-3"
                data-disabled={!onToggleFile || mutatingFileId === file.id}
                orientation="horizontal"
              >
                <Checkbox
                  id={`download-file-${file.id}`}
                  checked={file.selected}
                  disabled={!onToggleFile || mutatingFileId === file.id}
                  onCheckedChange={onToggleFile ? () => void onToggleFile(task, file) : undefined}
                />
                <FieldLabel
                  className={cn("min-w-0 flex-1 font-normal", !file.selected && "text-muted-foreground line-through")}
                  htmlFor={`download-file-${file.id}`}
                >
                  <span className="truncate">{file.name}</span>
                </FieldLabel>
                <div className="flex basis-full items-center gap-3 pl-8 text-xs text-muted-foreground sm:basis-auto sm:shrink-0 sm:pl-0">
                  <span>{formatBytes(file.size)}</span>
                  <span className="w-10 text-right tabular-nums">{formatPercent(file.progress)}</span>
                </div>
              </Field>
            ))}
          </div>
        )}
      </div>
    </article>
  );
}

/** 按任务状态格式化稳定的剩余时间文本。 */
function formatDownloadEta(task: DownloadTask): string {
  if (isCompletedDownloadTask(task)) return "已完成";
  if (task.status === "waiting_network") return "等待 Wi-Fi";
  if (task.status === "paused") return "已暂停";
  const etaSeconds = task.etaSeconds;
  if (etaSeconds === undefined || !Number.isFinite(etaSeconds) || etaSeconds <= 0) return "计算中";
  return `剩余 ${formatDuration(etaSeconds)}`;
}

interface DownloadFansubGroup {
  key: string;
  name: string;
  tasks: DownloadTask[];
}

interface DownloadAnimeGroup {
  key: string;
  title: string;
  savePath: string;
  tasks: DownloadTask[];
  fansubGroups: DownloadFansubGroup[];
  linkedEpisodes: number;
  completedEpisodes: number;
  activeEpisodes: number;
}

/** 依次按追番和字幕组归并任务，同时保留未关联的手动任务。 */
function groupDownloadTasks(tasks: DownloadTask[], myAnime: MyAnime[]): DownloadAnimeGroup[] {
  const animeById = new Map(myAnime.map((item) => [item.anime.id, item]));
  const grouped = new Map<string, DownloadAnimeGroup>();

  for (const task of tasks) {
    const anime = task.animeId ? animeById.get(task.animeId) : undefined;
    const animeKey = task.animeId ?? "__manual__";
    const group = grouped.get(animeKey) ?? {
      key: animeKey,
      title: task.animeTitle ?? anime?.anime.title ?? "未关联下载",
      savePath: task.savePath,
      tasks: [],
      fansubGroups: [],
      linkedEpisodes: 0,
      completedEpisodes: 0,
      activeEpisodes: 0
    };
    group.tasks.push(task);
    grouped.set(animeKey, group);
  }

  return [...grouped.values()].map((group) => {
    const fansubs = new Map<string, DownloadFansubGroup>();
    for (const task of group.tasks) {
      const key = task.fansubGroupId ?? task.fansubName ?? "__unknown__";
      const fansub = fansubs.get(key) ?? {
        key: `${group.key}:${key}`,
        name: task.fansubName ?? task.fansubGroupId ?? "未识别字幕组",
        tasks: []
      };
      fansub.tasks.push(task);
      fansubs.set(key, fansub);
    }

    const episodeTasks = group.tasks.filter((task) => task.episodeNo !== undefined);
    group.linkedEpisodes = countUniqueEpisodes(episodeTasks);
    group.completedEpisodes = countUniqueEpisodes(episodeTasks.filter(isCompletedTask));
    group.activeEpisodes = countUniqueEpisodes(episodeTasks.filter(isActiveTask));
    group.fansubGroups = [...fansubs.values()]
      .map((fansub) => ({ ...fansub, tasks: sortTasksByEpisode(fansub.tasks) }))
      .sort((left, right) => left.name.localeCompare(right.name, "zh-CN"));
    return group;
  });
}

/** 渲染下载队列加载中的页面骨架。 */
function DownloadQueuePageSkeleton() {
  return (
    <Page aria-busy="true" aria-label="正在加载下载队列">
      <div className="flex flex-col gap-2 border-b pb-4">
        <Skeleton className="h-7 w-32" />
        <Skeleton className="h-4 w-64 max-w-full" />
      </div>
      <Skeleton className="h-20 w-full" />
      <div className="flex flex-wrap items-end justify-between gap-3">
        <Skeleton className="h-9 w-64 max-w-full" />
        <Skeleton className="h-9 w-72 max-w-full" />
      </div>
      <Skeleton className="h-56 w-full" />
    </Page>
  );
}

function sortTasksByEpisode(tasks: DownloadTask[]): DownloadTask[] {
  return [...tasks].sort((left, right) => (right.episodeNo ?? -1) - (left.episodeNo ?? -1));
}

function countUniqueEpisodes(tasks: DownloadTask[]): number {
  return new Set(tasks.map((task) => task.episodeNo).filter((value) => value !== undefined)).size;
}

function formatEpisodeRange(tasks: DownloadTask[]): string {
  const episodes = [...new Set(tasks.map((task) => task.episodeNo).filter((value): value is number => value !== undefined))]
    .sort((left, right) => left - right);
  if (episodes.length === 0) return "未关联集数";
  if (episodes.length === 1) return `第 ${episodes[0]} 集`;
  return `第 ${episodes[0]}-${episodes.at(-1)} 集`;
}

function isCompletedTask(task: DownloadTask): boolean {
  return isCompletedDownloadTask(task);
}

/** 返回下载视图的空状态标题。 */
function getEmptyViewTitle(view: DownloadView): string {
  if (view === "active") return "当前没有下载任务";
  if (view === "seeding") return "当前没有做种任务";
  return "暂无完成任务";
}

/** 返回下载视图的空状态说明。 */
function getEmptyViewDescription(view: DownloadView): string {
  if (view === "active") return "添加 magnet 或 torrent 地址后，任务会显示在这里。";
  if (view === "seeding") return "正在上传分享的任务会显示在这里。";
  return "已完成且不再做种的任务会保留在这里。";
}

function isActiveTask(task: DownloadTask): boolean {
  return isActiveDownloadTask(task);
}

function isErrorTask(task: DownloadTask): boolean {
  return task.status === "error" || task.status === "missing_files";
}

function canPauseTask(task: DownloadTask): boolean {
  return !["paused", "completed", "error", "missing_files"].includes(task.status);
}

function canResumeTask(task: DownloadTask): boolean {
  return ["paused", "error", "stalled", "missing_files"].includes(task.status);
}

function getDownloadStatusTone(task: DownloadTask): "neutral" | "green" | "amber" | "red" | "blue" {
  if (task.status === "waiting_network") return "amber";
  if (isCompletedDownloadTask(task)) return "green";
  const { status } = task;
  if (status === "error" || status === "missing_files") return "red";
  if (status === "paused" || status === "stalled") return "amber";
  if (status === "downloading") return "blue";
  return "neutral";
}

/** 保留已完成任务的做种状态，暂停做种时明确区分于未完成暂停。 */
function getDownloadStatusText(task: DownloadTask): string {
  if (task.status === "waiting_network") return "等待 Wi-Fi";
  if (task.status === "seeding") return "做种中";
  if (task.status === "paused" && isCompletedDownloadTask(task)) return "已暂停做种";
  return isCompletedDownloadTask(task) ? "已完成" : downloadStatusText[task.status];
}

function canScanTask(task: DownloadTask): boolean {
  return isCompletedDownloadTask(task) || task.files.some((file) => file.progress >= 1);
}

/** 限制下载地址为应用当前支持的 magnet 与远程 torrent URL。 */
function isValidDownloadUrl(value: string): boolean {
  if (value.startsWith("magnet:?")) return true;
  try {
    const url = new URL(value);
    return url.protocol === "http:" || url.protocol === "https:";
  } catch {
    return false;
  }
}

/** 获取已装配的客户端能力，缺失时返回明确错误。 */
function requireCapability<T>(capability: T | undefined, action: string): NonNullable<T> {
  if (!capability) throw new Error(`当前客户端暂不支持${action}`);
  return capability as NonNullable<T>;
}
