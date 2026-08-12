import { Fragment, useEffect, useRef, useState } from "react";
import { AlertTriangle, CheckCircle2, Clock, DownloadCloud, FolderOpen, Play, RefreshCw } from "lucide-react";
import { toast } from "@/lib/toast";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty";
import { Progress } from "@/components/ui/progress";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
import { ReleaseMetadataBadges } from "@/components/release-metadata-badges";
import { MetricItem, MetricStrip, Page, PageActions, PageHeader, PageHeading } from "@/components/page-layout";
import { appApi } from "@/lib/api";
import { formatDuration, formatPercent, formatSpeed } from "@/lib/format";
import { getAppCapabilities } from "@/lib/runtime";
import { useAsyncData } from "@/lib/use-async-data";
import type { AutomationSchedulerStatus } from "@shared/contracts";
import { localizeDashboardAnimeTitles } from "@shared/dashboard-title";
import type { AnimeStatus, MediaFile, MyAnime } from "@shared/domain";
import type { MediaPlaybackTarget } from "@shared/player-selection";

const dashboardPreviewLimit = 4;

/** 加载首页看板和追番状态统计所需数据。 */
async function loadHomeData() {
  const [dashboard, myAnime, notifications] = await Promise.all([
    appApi.getDashboard(),
    appApi.listMyAnime(),
    appApi.listNotifications().catch((error) => {
      console.warn("[home] 未读提醒读取失败，首页其余数据继续展示", error);
      return [];
    })
  ]);
  return {
    dashboard: localizeDashboardAnimeTitles(dashboard, myAnime),
    myAnime,
    notifications
  };
}

/** 渲染首页追番、下载与提醒概览。 */
export function HomePage({
  onOpenAnimeDetail,
  onOpenDownloads,
  onPlayMedia
}: {
  onOpenAnimeDetail?: (animeId: string) => void;
  onOpenDownloads?: () => void;
  onPlayMedia?: (target: MediaPlaybackTarget) => Promise<void>;
} = {}) {
  const [revision, setRevision] = useState(0);
  const [schedulerStatus, setSchedulerStatus] = useState<AutomationSchedulerStatus | null>(null);
  const [startingScan, setStartingScan] = useState(false);
  const manualScanRef = useRef(false);
  const previousScanningRef = useRef(false);
  const { data: homeData, loading, error } = useAsyncData(loadHomeData, [revision]);
  const capabilities = getAppCapabilities();
  const scanning = startingScan || Boolean(schedulerStatus?.inFlight);

  useEffect(() => {
    if (!capabilities.backgroundAutomation) return;
    let active = true;
    let refreshing = false;

    /** 轮询宿主扫描状态，使页面重新挂载后恢复执行进度。 */
    async function refreshSchedulerStatus() {
      if (refreshing) return;
      refreshing = true;
      try {
        const status = await appApi.getAutomationSchedulerStatus();
        if (active) setSchedulerStatus(status);
      } catch (caught) {
        console.warn("[home] failed to refresh automation status", caught);
      } finally {
        refreshing = false;
      }
    }

    void refreshSchedulerStatus();
    const timer = window.setInterval(() => void refreshSchedulerStatus(), 1_500);
    window.addEventListener("focus", refreshSchedulerStatus);
    return () => {
      active = false;
      window.clearInterval(timer);
      window.removeEventListener("focus", refreshSchedulerStatus);
    };
  }, [capabilities.backgroundAutomation]);

  useEffect(() => {
    const wasScanning = previousScanningRef.current;
    previousScanningRef.current = scanning;
    if (!wasScanning || scanning) return;

    setRevision((current) => current + 1);
    if (!manualScanRef.current) return;
    if (schedulerStatus?.lastError) {
      toast.error(schedulerStatus.lastError);
    } else if (schedulerStatus?.lastResult) {
      const result = schedulerStatus.lastResult;
      toast.success(`扫描完成：检查 ${result.checkedEpisodes} 集，新增 ${result.downloaded.length} 个下载`);
    }
    manualScanRef.current = false;
  }, [scanning, schedulerStatus?.lastError, schedulerStatus?.lastResult]);

  /** 将一次手动扫描交给宿主后台执行。 */
  async function scanUpdates() {
    if (!capabilities.backgroundAutomation || scanning) return;
    setStartingScan(true);
    console.info("[home] background scan requested");
    try {
      manualScanRef.current = true;
      const status = await appApi.startAutomationScan();
      setSchedulerStatus(status);
      toast.info("正在后台扫描进行中。");
      console.info("[home] background scan accepted");
    } catch (caught) {
      manualScanRef.current = false;
      const message = caught instanceof Error ? caught.message : "扫描更新失败";
      toast.error(message);
      console.error("[home] background scan request failed", { message });
    } finally {
      setStartingScan(false);
    }
  }

  /** 使用默认播放器打开首页最近完成的媒体。 */
  async function playRecentMedia(file: MediaFile): Promise<void> {
    try {
      const target: MediaPlaybackTarget = {
        filePath: file.filePath,
        taskId: file.downloadTaskId
      };
      if (onPlayMedia) await onPlayMedia(target);
      else await appApi.playMedia(file.filePath);
    } catch (caught) {
      const message = caught instanceof Error ? caught.message : "播放失败";
      toast.error(message);
      console.error("[home] 媒体播放失败", { mediaId: file.id, message });
    }
  }

  if (loading && !homeData) {
    return <HomePageSkeleton />;
  }

  if (!homeData) {
    return (
      <Alert variant="destructive">
        <AlertTriangle />
        <AlertTitle>首页数据加载失败</AlertTitle>
        <AlertDescription>{error?.message ?? "暂时无法读取首页数据，请稍后重试。"}</AlertDescription>
      </Alert>
    );
  }

  const data = homeData.dashboard;
  const activeDownloadPreview = data.activeDownloads.slice(0, dashboardPreviewLimit);
  const recentCompletedPreview = data.recentCompleted.slice(0, dashboardPreviewLimit);
  const todayPreview = (data.todayEpisodes.length > 0 ? data.todayEpisodes : data.dailyReminder.items)
    .slice(0, dashboardPreviewLimit);
  const todayCount = data.dailyReminder.total || data.todayEpisodes.length;
  const pendingPreview = data.pendingActions.slice(0, dashboardPreviewLimit);

  return (
    <Page>
      <PageHeader>
        <PageHeading
          description={`${formatReminderDate(data.dailyReminder.date)}，更新、下载与异常集中处理。`}
          title="今日追番"
        />
        {capabilities.backgroundAutomation && (
          <PageActions>
            <Button disabled={scanning} onClick={() => void scanUpdates()}>
              <RefreshCw className={scanning ? "animate-spin" : undefined} data-icon="inline-start" />
              {scanning ? "扫描中" : "扫描更新"}
            </Button>
          </PageActions>
        )}
      </PageHeader>

      <MetricStrip className="sm:grid-cols-4">
        <MetricItem label="在追" value={countMyAnimeStatus(homeData.myAnime, "watching")} />
        <MetricItem label="今日更新" value={todayCount} />
        <MetricItem label="下载中" value={data.activeDownloads.length} />
        <MetricItem
          label="未读提醒"
          value={homeData.notifications.filter((item) => !item.readAt).length}
        />
      </MetricStrip>

      {error && (
        <Alert variant="destructive">
          <AlertTriangle />
          <AlertTitle>首页摘要刷新失败</AlertTitle>
          <AlertDescription>当前仍展示上一次成功数据：{error.message}</AlertDescription>
        </Alert>
      )}

      <div className="grid min-w-0 items-start gap-5 xl:grid-cols-[minmax(0,1.5fr)_minmax(18rem,0.8fr)]">
        <Card className="min-w-0 shadow-none">
          <CardHeader className="flex-row items-start justify-between gap-3 border-b">
            <div className="min-w-0">
              <CardTitle>今日更新</CardTitle>
              <CardDescription className="mt-1">{formatReminderDate(data.dailyReminder.date)}</CardDescription>
            </div>
            <Badge tone="primary">{todayCount} 集</Badge>
          </CardHeader>
          <CardContent className="p-0 sm:p-0">
            {todayPreview.length > 0 ? (
              <div className="flex min-w-0 flex-col">
                {todayPreview.map((item, index) => (
                  <Fragment key={item.id}>
                    <div className="flex min-w-0 flex-col items-start gap-2 px-4 py-3 sm:flex-row sm:items-center sm:justify-between sm:px-5">
                      <div className="min-w-0">
                        <div className="font-medium">{item.animeTitle}</div>
                        <div className="mt-1 text-sm text-muted-foreground">
                          第 {item.episodeNo} 集 · {formatAirTime(item.airTime)} · {item.fansubName ?? "未选字幕组"}
                        </div>
                      </div>
                      <Badge className="shrink-0" tone={getEpisodeStatusTone(item.status)}>
                        {formatEpisodeStatus(item.status)}
                      </Badge>
                    </div>
                    {index < todayPreview.length - 1 && <Separator />}
                  </Fragment>
                ))}
              </div>
            ) : (
              <Empty className="min-h-40 p-4 md:p-6">
                <EmptyHeader>
                  <EmptyMedia variant="icon">
                    <Clock />
                  </EmptyMedia>
                  <EmptyTitle>今日暂无更新</EmptyTitle>
                  <EmptyDescription>今天没有已登记的追番更新。</EmptyDescription>
                </EmptyHeader>
              </Empty>
            )}
          </CardContent>
        </Card>

        <Card className="min-w-0 shadow-none">
          <CardHeader className="flex-row items-start justify-between gap-3 border-b">
            <CardTitle>需关注</CardTitle>
            <Badge tone={data.pendingActions.length > 0 ? "amber" : "green"}>{data.pendingActions.length}</Badge>
          </CardHeader>
          <CardContent className="flex flex-col gap-0 p-0 sm:p-0">
            {data.pendingActions.length > 0 ? (
              pendingPreview.map((item, index) => (
                <Fragment key={item.id}>
                  <Button
                    className="h-auto min-h-0 w-full justify-start rounded-none border-l-2 border-primary px-4 py-3 text-left sm:px-5"
                    disabled={!item.animeId}
                    onClick={() => item.animeId && onOpenAnimeDetail?.(item.animeId)}
                    variant="ghost"
                  >
                    <div className="flex items-start justify-between gap-3">
                      <div className="min-w-0">
                        <div className="text-sm font-semibold">{item.title}</div>
                        <p className="mt-1 text-xs leading-5 text-muted-foreground">{item.description}</p>
                      </div>
                      <Badge className="shrink-0" tone={item.severity === "warning" ? "amber" : "blue"}>
                        {item.severity === "warning" ? "待处理" : "提示"}
                      </Badge>
                    </div>
                  </Button>
                  {index < pendingPreview.length - 1 && <Separator />}
                </Fragment>
              ))
            ) : (
              <Empty className="min-h-40 p-4 md:p-6">
                <EmptyHeader>
                  <EmptyMedia variant="icon">
                    <CheckCircle2 />
                  </EmptyMedia>
                  <EmptyTitle>暂无待处理事项</EmptyTitle>
                  <EmptyDescription>当前没有需要手动处理的任务。</EmptyDescription>
                </EmptyHeader>
              </Empty>
            )}
          </CardContent>
        </Card>
      </div>

      <div className="grid min-w-0 items-start gap-5 md:grid-cols-2 xl:grid-cols-3">
        <Card className="min-w-0 shadow-none">
          <CardHeader className="flex-row items-start justify-between gap-3">
            <div className="min-w-0">
              <CardTitle>下载中</CardTitle>
              <CardDescription className="mt-1">显示 {activeDownloadPreview.length} / {data.activeDownloads.length}</CardDescription>
            </div>
            {data.activeDownloads.length > 0 && onOpenDownloads && (
              <Button className="h-auto min-h-0 shrink-0 p-0 text-xs" onClick={onOpenDownloads} variant="ghost">
                查看全部
              </Button>
            )}
          </CardHeader>
          <CardContent>
            {data.activeDownloads.length > 0 ? (
              <div className="flex min-w-0 flex-col">
                {activeDownloadPreview.map((task, index) => (
                  <Fragment key={task.id}>
                    <div className="flex min-w-0 flex-col gap-2 py-3">
                      <div className="flex items-start justify-between gap-3">
                        <div className="min-w-0">
                          <div className="truncate text-sm font-medium" title={task.name}>
                            {task.name}
                          </div>
                          <div className="mt-1 flex flex-wrap gap-1">
                            <ReleaseMetadataBadges metadata={task} />
                          </div>
                          <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
                            <span>{formatSpeed(task.downloadSpeed)}</span>
                            <span>剩余 {formatDuration(task.etaSeconds)}</span>
                          </div>
                        </div>
                        <Badge className="shrink-0" tone="blue">
                          {formatPercent(task.progress)}
                        </Badge>
                      </div>
                      <Progress value={task.progress} />
                    </div>
                    {index < activeDownloadPreview.length - 1 && <Separator />}
                  </Fragment>
                ))}
              </div>
            ) : (
              <Empty className="min-h-40 p-4 md:p-6">
                <EmptyHeader>
                  <EmptyMedia variant="icon">
                    <DownloadCloud />
                  </EmptyMedia>
                  <EmptyTitle>暂无下载任务</EmptyTitle>
                  <EmptyDescription>当前没有正在下载的资源。</EmptyDescription>
                </EmptyHeader>
              </Empty>
            )}
          </CardContent>
        </Card>

        <Card className="min-w-0 shadow-none">
          <CardHeader className="flex-row items-start justify-between gap-3">
            <div className="min-w-0">
              <CardTitle>最近完成</CardTitle>
              <CardDescription className="mt-1">显示 {recentCompletedPreview.length} / {data.recentCompleted.length}</CardDescription>
            </div>
            {data.recentCompleted.length > 0 && onOpenDownloads && (
              <Button className="h-auto min-h-0 shrink-0 p-0 text-xs" onClick={onOpenDownloads} variant="ghost">
                查看全部
              </Button>
            )}
          </CardHeader>
          <CardContent>
            {data.recentCompleted.length > 0 ? (
              <div className="flex min-w-0 flex-col">
                {recentCompletedPreview.map((file, index) => (
                  <Fragment key={file.id}>
                    <div className="flex min-w-0 flex-col gap-3 py-3 sm:flex-row sm:items-start sm:justify-between">
                      <div className="min-w-0">
                        <div className="flex min-w-0 items-center gap-2 text-sm font-medium">
                          <CheckCircle2 className="size-4 shrink-0 text-primary" />
                          <span className="truncate" title={file.fileName}>
                            {file.fileName}
                          </span>
                        </div>
                        <div className="mt-2 flex flex-wrap gap-2">
                          <Badge tone="green">{file.normalizedVideoCodec}</Badge>
                          {file.resolution && <Badge>{file.resolution}</Badge>}
                          {file.bitDepth && <Badge>{file.bitDepth}bit</Badge>}
                        </div>
                      </div>
                      {(onPlayMedia || capabilities.nativePlayer || capabilities.runtime === "desktop") && (
                        <div className="flex shrink-0 self-end gap-2 sm:self-auto">
                        {(onPlayMedia || capabilities.nativePlayer) && <Button
                          className="size-11 p-0 sm:size-9"
                          variant="outline"
                          aria-label="播放"
                          title="播放"
                          onClick={() => void playRecentMedia(file)}
                        >
                          <Play data-icon="inline-start" />
                        </Button>}
                        {capabilities.runtime === "desktop" && <Button
                          className="size-11 p-0 sm:size-9"
                          variant="outline"
                          aria-label="定位文件"
                          title="定位文件"
                          onClick={() => void appApi.revealMedia(file.filePath)}
                        >
                          <FolderOpen data-icon="inline-start" />
                        </Button>}
                      </div>)}
                    </div>
                    {index < recentCompletedPreview.length - 1 && <Separator />}
                  </Fragment>
                ))}
              </div>
            ) : (
              <Empty className="min-h-40 p-4 md:p-6">
                <EmptyHeader>
                  <EmptyMedia variant="icon">
                    <CheckCircle2 />
                  </EmptyMedia>
                  <EmptyTitle>暂无完成记录</EmptyTitle>
                  <EmptyDescription>最近还没有完成下载的媒体文件。</EmptyDescription>
                </EmptyHeader>
              </Empty>
            )}
          </CardContent>
        </Card>

        <Card className="min-w-0 shadow-none md:col-span-2 xl:col-span-1">
          <CardHeader>
            <CardTitle>本周放送</CardTitle>
          </CardHeader>
          <CardContent>
            {data.weeklySchedule.length > 0 ? (
              <div className="flex flex-col">
                {data.weeklySchedule.map((day, index) => (
                  <Fragment key={day.day}>
                    <div className="flex items-center justify-between gap-3 py-3">
                      <div className="text-sm font-medium">{day.day}</div>
                      <div className="flex items-center gap-2 text-xs text-muted-foreground">
                        <Clock className="size-4" />
                        {day.items.length ? `${day.items.length} 部` : "无更新"}
                      </div>
                    </div>
                    {index < data.weeklySchedule.length - 1 && <Separator />}
                  </Fragment>
                ))}
              </div>
            ) : (
              <Empty className="min-h-40 p-4 md:p-6">
                <EmptyHeader>
                  <EmptyMedia variant="icon">
                    <Clock />
                  </EmptyMedia>
                  <EmptyTitle>暂无放送安排</EmptyTitle>
                  <EmptyDescription>本周还没有已登记的放送信息。</EmptyDescription>
                </EmptyHeader>
              </Empty>
            )}
          </CardContent>
        </Card>
      </div>

      {capabilities.sourceManagement && (
        <section className="min-w-0 border-y py-4" aria-labelledby="home-source-health-title">
          <div className="mb-2 flex items-center justify-between gap-3">
            <h2 className="text-sm font-semibold" id="home-source-health-title">下载源状态</h2>
            <span className="text-xs text-muted-foreground">{data.sourceHealth.length} 个来源</span>
          </div>
            {data.sourceHealth.length > 0 ? (
              <div className="grid min-w-0 gap-x-5 gap-y-2 md:grid-cols-2 xl:grid-cols-3">
                {data.sourceHealth.map((source) => (
                  <div key={source.sourceId} className="flex min-w-0 items-center justify-between gap-3 py-2">
                    <div className="min-w-0">
                      <div className="truncate text-sm font-medium" title={source.name}>
                        {source.name}
                      </div>
                      <div className="mt-1 truncate text-xs text-muted-foreground">
                        最近检查 {source.lastCheckedAt ?? "--"}
                      </div>
                    </div>
                    <Badge className="shrink-0" tone={source.status === "ok" ? "green" : "amber"}>
                      {source.status === "ok" ? "正常" : "待检查"}
                    </Badge>
                  </div>
                ))}
              </div>
            ) : (
              <Empty className="min-h-40 p-4 md:p-6">
                <EmptyHeader>
                  <EmptyMedia variant="icon">
                    <DownloadCloud />
                  </EmptyMedia>
                  <EmptyTitle>暂无下载源</EmptyTitle>
                  <EmptyDescription>当前没有可显示状态的下载源。</EmptyDescription>
                </EmptyHeader>
              </Empty>
            )}
        </section>
      )}
    </Page>
  );
}

/** 渲染首页加载中的结构化占位状态。 */
function HomePageSkeleton() {
  return (
    <Page aria-busy="true" aria-label="正在加载首页">
      <div className="flex flex-col gap-2">
        <Skeleton className="h-7 w-32" />
        <Skeleton className="h-4 w-72 max-w-full" />
      </div>
      <MetricStrip className="sm:grid-cols-4">
        {["watching", "today", "downloading", "unread"].map((status) => (
          <MetricItem key={status} label={<Skeleton className="h-4 w-16" />} value={<Skeleton className="h-7 w-10" />} />
        ))}
      </MetricStrip>
      <div className="grid gap-5 xl:grid-cols-2">
        {["daily", "pending"].map((section) => (
          <Card key={section}>
            <CardHeader>
              <Skeleton className="h-5 w-24" />
              <Skeleton className="h-4 w-40" />
            </CardHeader>
            <CardContent className="flex flex-col gap-3">
              <Skeleton className="h-16 w-full" />
              <Skeleton className="h-16 w-full" />
            </CardContent>
          </Card>
        ))}
      </div>
    </Page>
  );
}

/** 统计指定追番状态的数量。 */
function countMyAnimeStatus(items: MyAnime[], status: AnimeStatus): number {
  return items.filter((item) => item.status === status).length;
}

/** 格式化首页提醒日期描述。 */
function formatReminderDate(value: string): string {
  return `${value} 的更新摘要`;
}

/** 将放送时间格式化为本地时分。 */
function formatAirTime(value?: string): string {
  if (!value) {
    return "未知时间";
  }

  const timeOnly = value.match(/^(\d{1,2}):(\d{2})/);
  if (timeOnly) {
    return `${timeOnly[1].padStart(2, "0")}:${timeOnly[2]}`;
  }

  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return "未知时间";
  }

  return `${date.getHours().toString().padStart(2, "0")}:${date.getMinutes().toString().padStart(2, "0")}`;
}

/** 将单集状态转换为中文标签。 */
function formatEpisodeStatus(status: string): string {
  const labels: Record<string, string> = {
    upcoming: "未播",
    aired: "已播",
    matched: "已匹配",
    downloading: "下载中",
    downloaded: "已下载",
    watched: "已看"
  };

  return labels[status] ?? status;
}

/** 根据单集状态返回对应徽标色调。 */
function getEpisodeStatusTone(status: string): "neutral" | "green" | "amber" | "red" | "blue" {
  if (status === "downloading") {
    return "blue";
  }

  if (status === "downloaded" || status === "watched" || status === "matched") {
    return "green";
  }

  if (status === "aired") {
    return "amber";
  }

  return "neutral";
}
