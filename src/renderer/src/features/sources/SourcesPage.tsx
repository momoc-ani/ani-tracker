import {
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  KeyRound,
  Network,
  Pencil,
  PlugZap,
  Plus,
  RefreshCw,
  Save,
  Settings2,
  Timer,
  TriangleAlert,
  type LucideIcon,
  X
} from "lucide-react";
import type { ReactNode } from "react";
import { useEffect, useRef, useState } from "react";
import { toast } from "@/lib/toast";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty";
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow
} from "@/components/ui/table";
import { Page, PageActions, PageBreadcrumb, PageHeader } from "@/components/page-layout";
import { WorkbenchSheet } from "@/components/workbench-sheet";
import { appApi } from "@/lib/api";
import { cn } from "@/lib/cn";
import { useAsyncData } from "@/lib/use-async-data";
import type { SourceSyncSchedulerStatus } from "@shared/contracts";
import type { AppSettings, MetadataProxySettings, ReleaseSourceConfig, SourceKind } from "@shared/domain";
import {
  DEFAULT_SOURCE_REQUEST_INTERVAL_MS,
  getSourceMinimumRequestIntervalMs,
  isAniBtRequestTarget,
  MAX_SOURCE_REQUEST_INTERVAL_MS
} from "@shared/source-network-policy";

const kindText: Record<SourceKind, string> = {
  rss: "RSS",
  torznab: "Torznab",
  site_adapter: "站点适配器",
  manual: "手动添加"
};

interface SourceDraftErrors {
  name?: string;
  url?: string;
}

/** 渲染下载源配置；远程端可关闭会立即触发采集的操作。 */
export function SourcesPage({ allowImmediateSync = true }: { allowImmediateSync?: boolean } = {}) {
  const { data, error: sourcesError, loading } = useAsyncData(appApi.listSources, []);
  const { data: settingsData, error: settingsError, loading: settingsLoading } = useAsyncData(appApi.getSettings, []);
  const { data: syncStatusData, error: syncStatusError, loading: syncStatusLoading } = useAsyncData(appApi.getSourceSyncStatus, []);
  const [sources, setSources] = useState<ReleaseSourceConfig[]>([]);
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [credentials, setCredentials] = useState<Record<string, string>>({});
  const [intervalDrafts, setIntervalDrafts] = useState<Record<string, string>>({});
  const intervalDraftsRef = useRef<Record<string, string>>({});
  const [expandedSourceIds, setExpandedSourceIds] = useState<Set<string>>(new Set());
  const [sourceMutationId, setSourceMutationId] = useState<string | null>(null);
  const [syncStatus, setSyncStatus] = useState<SourceSyncSchedulerStatus | null>(null);
  const [syncing, setSyncing] = useState(false);
  const [syncTimeDraft, setSyncTimeDraft] = useState("09:00");
  const [proxyEditing, setProxyEditing] = useState(false);
  const [proxyDraft, setProxyDraft] = useState<MetadataProxySettings>(defaultMetadataProxySettings);
  const [proxySaveState, setProxySaveState] = useState<"idle" | "saving" | "saved">("idle");
  const [proxyError, setProxyError] = useState<string | null>(null);
  const [addSheetOpen, setAddSheetOpen] = useState(false);
  const [addingSource, setAddingSource] = useState(false);
  const [draftErrors, setDraftErrors] = useState<SourceDraftErrors>({});
  const [draft, setDraft] = useState({
    name: "",
    kind: "rss" as SourceKind,
    url: "",
    apiKey: ""
  });

  useEffect(() => {
    if (data) {
      const nextIntervalDrafts = Object.fromEntries(data.map((source) => [
        source.id,
        String(normalizeSourceInterval(source.requestIntervalMs, source))
      ]));
      setSources(data);
      setCredentials(Object.fromEntries(data.map((source) => [source.id, source.apiKey ?? ""])));
      intervalDraftsRef.current = nextIntervalDrafts;
      setIntervalDrafts(nextIntervalDrafts);
    }
  }, [data]);

  useEffect(() => {
    if (settingsData) {
      setSettings(settingsData);
      setProxyDraft(getMetadataProxySettings(settingsData));
      setSyncTimeDraft(getSourceSyncSettings(settingsData).dailyTime);
    }
  }, [settingsData]);

  useEffect(() => {
    if (syncStatusData) setSyncStatus(syncStatusData);
  }, [syncStatusData]);

  /** 同步保存采集间隔草稿，确保立即点击保存时读取到最新输入。 */
  function updateIntervalDraft(sourceId: string, value: string) {
    intervalDraftsRef.current = { ...intervalDraftsRef.current, [sourceId]: value };
    setIntervalDrafts((current) => ({ ...current, [sourceId]: value }));
  }

  /** 串行执行单个下载源变更，并统一处理错误与忙碌状态。 */
  async function runSourceMutation(
    source: ReleaseSourceConfig,
    operation: () => Promise<ReleaseSourceConfig[]>,
    failureMessage: string,
    successMessage?: string
  ) {
    setSourceMutationId(source.id);
    try {
      const nextSources = await operation();
      setSources(nextSources);
      if (successMessage) toast.success(successMessage);
      return nextSources;
    } catch (error) {
      toast.error(error instanceof Error ? error.message : failureMessage);
      return undefined;
    } finally {
      setSourceMutationId(null);
    }
  }

  /** 切换下载源启用状态。 */
  async function toggleSource(source: ReleaseSourceConfig) {
    await runSourceMutation(
      source,
      () => appApi.setSourceEnabled(source.id, !source.enabled),
      "下载源状态更新失败"
    );
  }

  /** 保存 Torznab 或站点适配器访问凭据。 */
  async function saveCredential(source: ReleaseSourceConfig) {
    await runSourceMutation(
      source,
      () => appApi.upsertSource({ ...source, apiKey: credentials[source.id]?.trim() || undefined }),
      "访问凭据保存失败",
      "访问凭据已保存"
    );
  }

  /** 切换单个下载源是否使用全局代理。 */
  async function toggleSourceProxy(source: ReleaseSourceConfig) {
    if (isAniBtRequestTarget(source)) return;
    await runSourceMutation(
      source,
      () => appApi.upsertSource({
        ...source,
        useProxy: !(source.useProxy ?? false),
        requestIntervalMs: normalizeSourceInterval(source.requestIntervalMs, source)
      }),
      "下载源代理策略更新失败"
    );
  }

  /** 保存单个下载源的最小采集间隔。 */
  async function saveSourceInterval(source: ReleaseSourceConfig) {
    const draft = intervalDraftsRef.current[source.id] ?? intervalDrafts[source.id];
    const requestIntervalMs = normalizeSourceInterval(Number(draft), source);
    const savedSources = await runSourceMutation(
      source,
      () => appApi.upsertSource({ ...source, requestIntervalMs }),
      "采集策略保存失败"
    );
    if (!savedSources) return;

    const persistedSource = savedSources.find((item) => item.id === source.id);
    const persistedInterval = persistedSource
      ? normalizeSourceInterval(persistedSource.requestIntervalMs, persistedSource)
      : undefined;
    if (persistedInterval !== requestIntervalMs) {
      if (persistedInterval !== undefined) {
        updateIntervalDraft(source.id, String(persistedInterval));
      }
      toast.error("采集策略未正确持久化，请重试");
      return;
    }

    updateIntervalDraft(source.id, String(requestIntervalMs));
    console.info("[sources] 采集间隔已持久化", { sourceId: source.id, requestIntervalMs });
    toast.success("采集策略已保存");
  }

  /** 校验并创建新的下载源。 */
  async function addSource() {
    const name = draft.name.trim();
    const url = draft.url.trim();
    const nextErrors: SourceDraftErrors = {};
    if (!name) nextErrors.name = "请输入下载源名称";
    if (!isValidSourceUrl(url)) nextErrors.url = "请输入有效的 HTTP(S) 服务地址";
    setDraftErrors(nextErrors);
    if (Object.keys(nextErrors).length > 0) return;

    const sourceBase: ReleaseSourceConfig = {
      id: createSourceId(name),
      name,
      kind: draft.kind,
      enabled: true,
      useProxy: true,
      rssUrl: draft.kind === "rss" ? url : undefined,
      baseUrl: draft.kind !== "rss" ? url : undefined,
      apiKey: draft.kind !== "rss" ? draft.apiKey.trim() || undefined : undefined,
      tags: [draft.kind]
    };
    const source: ReleaseSourceConfig = {
      ...sourceBase,
      requestIntervalMs: normalizeSourceInterval(undefined, sourceBase)
    };

    setAddingSource(true);
    try {
      setSources(await appApi.upsertSource(source));
      setCredentials((current) => ({ ...current, [source.id]: source.apiKey ?? "" }));
      updateIntervalDraft(source.id, "600");
      setDraft({ name: "", kind: "rss", url: "", apiKey: "" });
      setDraftErrors({});
      setAddSheetOpen(false);
      toast.success("下载源已添加");
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "下载源添加失败");
    } finally {
      setAddingSource(false);
    }
  }

  /** 打开元数据代理编辑状态并恢复当前已保存值。 */
  function startEditProxy() {
    if (!settings) return;
    setProxyDraft(getMetadataProxySettings(settings));
    setProxyError(null);
    setProxyEditing(true);
  }

  /** 校验并保存元数据代理配置。 */
  async function saveMetadataProxy() {
    if (!settings) return;
    const nextProxy = normalizeMetadataProxyDraft(proxyDraft);
    if (nextProxy.mode === "manual" && !nextProxy.url) {
      setProxyError("请输入手动代理地址");
      return;
    }

    setProxySaveState("saving");
    setProxyError(null);
    try {
      const saved = await appApi.updateSettings({
        network: { ...settings.network, metadataProxy: nextProxy }
      });
      setSettings(saved);
      setProxyDraft(getMetadataProxySettings(saved));
      setProxyEditing(false);
      setProxySaveState("saved");
      toast.success("元数据代理已保存");
      window.setTimeout(() => setProxySaveState("idle"), 1200);
    } catch (error) {
      setProxyError(error instanceof Error ? error.message : "元数据代理保存失败");
      setProxySaveState("idle");
    }
  }

  /** 更新每日下载源同步的启用状态或执行时间。 */
  async function updateSourceSyncSettings(patch: { enabled?: boolean; dailyTime?: string }) {
    if (!settings) return;
    try {
      const current = getSourceSyncSettings(settings);
      const saved = await appApi.updateSettings({
        sourceSync: {
          enabled: patch.enabled ?? current.enabled,
          dailyTime: patch.dailyTime ?? current.dailyTime
        }
      });
      setSettings(saved);
      setSyncTimeDraft(getSourceSyncSettings(saved).dailyTime);
      setSyncStatus(await appApi.getSourceSyncStatus());
      toast.success("同步计划已保存");
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "同步计划保存失败");
    }
  }

  /** 立即执行一次下载源增量同步。 */
  async function syncSourcesNow() {
    setSyncing(true);
    try {
      const result = await appApi.syncSourcesNow();
      setSyncStatus(await appApi.getSourceSyncStatus());
      if (result.errors.length) toast.warning(`同步完成，${result.errors.length} 个来源失败`);
      else toast.success(`同步完成，新增 ${result.addedReleaseCount} 条资源`);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "下载源同步失败");
    } finally {
      setSyncing(false);
    }
  }

  /** 展开或收起单个来源的采集与凭据配置。 */
  function toggleSourceDetails(sourceId: string) {
    setExpandedSourceIds((current) => {
      const next = new Set(current);
      if (next.has(sourceId)) next.delete(sourceId);
      else next.add(sourceId);
      return next;
    });
  }

  if (loading || settingsLoading || syncStatusLoading) return <SourcesPageSkeleton />;

  const loadingError = sourcesError ?? settingsError ?? syncStatusError;
  if (loadingError) {
    return (
      <Page>
        <PageHeader className="sm:items-center">
          <h1 className="sr-only">下载源</h1>
          <PageBreadcrumb current="下载源" />
        </PageHeader>
        <Alert variant="destructive">
          <AlertTitle>下载源加载失败</AlertTitle>
          <AlertDescription>{loadingError.message || "请重新进入下载源页面或重启应用后再试。"}</AlertDescription>
        </Alert>
      </Page>
    );
  }

  const enabledCount = sources.filter((source) => source.enabled).length;
  const credentialRequiredCount = sources.filter((source) => needsCredential(source, source.apiKey)).length;
  const sourceSyncSettings = getSourceSyncSettings(settings);
  const syncTimeChanged = syncTimeDraft !== sourceSyncSettings.dailyTime;

  return (
    <Page>
      <PageHeader className="sm:items-center">
        <h1 className="sr-only">下载源</h1>
        <PageBreadcrumb current="下载源" />
      </PageHeader>

      <div className="flex min-w-0 flex-col gap-3 border-y py-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex flex-wrap gap-x-4 gap-y-2 text-xs font-medium text-muted-foreground">
          <span className="flex items-center gap-1.5"><span className="size-1.5 rounded-full bg-success" />已启用 {enabledCount}</span>
          <span className="flex items-center gap-1.5"><span className="size-1.5 rounded-full bg-muted-foreground" />已停用 {sources.length - enabledCount}</span>
          <span className="flex items-center gap-1.5 text-warning"><span className="size-1.5 rounded-full bg-warning" />需要凭据 {credentialRequiredCount}</span>
        </div>
        <PageActions className="sm:w-auto sm:justify-end">
          <Button className="w-full sm:w-auto" onClick={() => setAddSheetOpen(true)}>
            <Plus data-icon="inline-start" />
            添加下载源
          </Button>
        </PageActions>
      </div>

      <section className="min-w-0">
        <SectionHeading description="用于元数据采集，以及已开启全局代理的下载源请求。" title="元数据代理" />
        <div className={cn("mt-4 rounded-md border bg-card p-4", proxyEditing && "border-primary")}>
          {proxyEditing ? (
            <>
              <FieldGroup className="grid gap-3 sm:grid-cols-2 xl:grid-cols-[180px_minmax(0,1fr)_140px]">
                <Field>
                  <FieldLabel htmlFor="metadata-proxy-mode">模式</FieldLabel>
                  <Select
                    value={proxyDraft.mode}
                    onValueChange={(value) => setProxyDraft({ ...proxyDraft, mode: value as MetadataProxySettings["mode"] })}
                  >
                    <SelectTrigger id="metadata-proxy-mode"><SelectValue /></SelectTrigger>
                    <SelectContent>
                      <SelectGroup>
                        <SelectItem value="off">关闭</SelectItem>
                        <SelectItem value="system">系统代理</SelectItem>
                        <SelectItem value="manual">手动代理</SelectItem>
                      </SelectGroup>
                    </SelectContent>
                  </Select>
                </Field>
                <Field data-disabled={proxyDraft.mode !== "manual"} data-invalid={Boolean(proxyError && proxyDraft.mode === "manual")}>
                  <FieldLabel htmlFor="metadata-proxy-url">代理地址</FieldLabel>
                  <Input
                    id="metadata-proxy-url"
                    aria-invalid={Boolean(proxyError && proxyDraft.mode === "manual")}
                    disabled={proxyDraft.mode !== "manual"}
                    placeholder="http://127.0.0.1:7890 或 socks5://127.0.0.1:7890"
                    value={proxyDraft.url ?? ""}
                    onChange={(event) => setProxyDraft({ ...proxyDraft, url: event.target.value })}
                  />
                </Field>
                <Field>
                  <FieldLabel htmlFor="metadata-proxy-timeout">超时秒数</FieldLabel>
                  <Input
                    id="metadata-proxy-timeout"
                    min={1}
                    type="number"
                    value={Math.round(proxyDraft.timeoutMs / 1000)}
                    onChange={(event) => setProxyDraft({ ...proxyDraft, timeoutMs: Number(event.target.value) * 1000 })}
                  />
                </Field>
              </FieldGroup>
              {proxyError && <p className="mt-3 text-sm text-destructive">{proxyError}</p>}
              <div className="mt-4 flex flex-wrap justify-end gap-2">
                <Button variant="outline" onClick={() => setProxyEditing(false)} disabled={proxySaveState === "saving"}>
                  <X data-icon="inline-start" />取消
                </Button>
                <Button onClick={() => void saveMetadataProxy()} disabled={proxySaveState === "saving"}>
                  <Save data-icon="inline-start" />
                  {proxySaveState === "saving" ? "保存中" : "保存"}
                </Button>
              </div>
            </>
          ) : (
            <div className="grid min-w-0 grid-cols-1 gap-px overflow-hidden rounded-md border bg-border sm:grid-cols-[180px_minmax(0,1fr)_140px_auto]">
              <ProxySummaryItem icon={<Network />} label="模式" value={formatProxyMode(getMetadataProxySettings(settings).mode)} />
              <ProxySummaryItem
                label="代理地址"
                value={getMetadataProxySettings(settings).mode === "manual" ? getMetadataProxySettings(settings).url || "--" : "--"}
              />
              <ProxySummaryItem label="请求超时" value={`${Math.round(getMetadataProxySettings(settings).timeoutMs / 1000)} 秒`} />
              <div className="flex items-center justify-end bg-card p-3">
                <Button className="size-11 p-0 md:size-9" variant="ghost" onClick={startEditProxy} disabled={!settings} aria-label="编辑元数据代理" title="编辑元数据代理">
                  <Pencil />
                </Button>
              </div>
            </div>
          )}
        </div>
      </section>

      <section className="min-w-0">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-end sm:justify-between">
          <SectionHeading description="当天未成功时，会在应用启动后自动补跑。" title="每日增量同步" />
          {allowImmediateSync && (
            <Button variant="outline" disabled={syncing || syncStatus?.inFlight} onClick={() => void syncSourcesNow()}>
              <RefreshCw data-icon="inline-start" className={cn((syncing || syncStatus?.inFlight) && "animate-spin")} />
              {syncing || syncStatus?.inFlight ? "同步中" : "立即同步"}
            </Button>
          )}
        </div>
        <div className="mt-4 overflow-hidden rounded-md border bg-card">
          <div className="bg-muted/30 p-4">
            <div className="flex flex-col gap-4 lg:flex-row lg:items-center">
              <FieldGroup className="gap-4 sm:flex-row sm:items-center lg:flex-1 lg:gap-6">
                <Field className="w-auto flex-none justify-start gap-3" orientation="horizontal">
                  <FieldLabel className="flex-none" htmlFor="source-sync-enabled">启用每日同步</FieldLabel>
                  <Switch
                    id="source-sync-enabled"
                    checked={sourceSyncSettings.enabled}
                    onCheckedChange={(enabled) => void updateSourceSyncSettings({ enabled })}
                  />
                </Field>
                <Field
                  className="min-w-0 sm:w-auto sm:flex-none"
                  data-disabled={!sourceSyncSettings.enabled}
                  orientation="responsive"
                >
                  <FieldLabel className="flex-none" htmlFor="source-sync-time">每日同步时间</FieldLabel>
                  <Input
                    className="min-w-0 sm:w-40"
                    id="source-sync-time"
                    type="time"
                    disabled={!sourceSyncSettings.enabled}
                    value={syncTimeDraft}
                    onChange={(event) => setSyncTimeDraft(event.target.value)}
                  />
                </Field>
              </FieldGroup>
              {syncTimeChanged ? (
                <Button
                  className="w-full shrink-0 lg:ml-auto lg:w-auto"
                  disabled={!sourceSyncSettings.enabled}
                  onClick={() => void updateSourceSyncSettings({ dailyTime: syncTimeDraft })}
                >
                  <Save data-icon="inline-start" />保存
                </Button>
              ) : null}
            </div>
          </div>
          <Separator />
          <div className="grid min-w-0 grid-cols-1 sm:grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)_auto_minmax(0,1fr)]">
            <SourceSyncSummaryItem icon={Timer} label="计划时间" value={sourceSyncSettings.dailyTime} />
            <ResponsiveSummarySeparator />
            <SourceSyncSummaryItem icon={CheckCircle2} label="上次完成" value={formatOptionalDateTime(syncStatus?.lastRunAt)} />
            <ResponsiveSummarySeparator />
            <SourceSyncSummaryItem icon={RefreshCw} label="下次同步" value={formatOptionalDateTime(syncStatus?.nextRunAt)} />
          </div>
          {(syncStatus?.lastError || syncStatus?.lastResult?.errors.length) ? (
            <div className="px-4 pb-4">
              <Alert variant="destructive">
                <TriangleAlert />
                <AlertTitle>上次同步存在异常</AlertTitle>
                <AlertDescription>
                  {syncStatus.lastError ?? `${syncStatus.lastResult?.errors.length ?? 0} 个来源同步失败，成功来源数据已保留。`}
                </AlertDescription>
              </Alert>
            </div>
          ) : null}
        </div>
      </section>

      <section className="min-w-0">
        <div className="flex items-end justify-between gap-3 border-b pb-2">
          <SectionHeading description="展开来源可编辑凭据和最小采集间隔。" title="下载源列表" />
          <span className="shrink-0 text-xs text-muted-foreground">共 {sources.length} 个源</span>
        </div>

        {sources.length > 0 ? (
          <div className="mt-4 overflow-hidden rounded-md border bg-card">
            <Table className="min-w-[920px]">
              <TableHeader>
                <TableRow className="bg-muted/50 hover:bg-muted/50">
                  <TableHead className="px-4">名称</TableHead>
                  <TableHead>类型</TableHead>
                  <TableHead className="w-[260px]">地址</TableHead>
                  <TableHead>标签</TableHead>
                  <TableHead>状态</TableHead>
                  <TableHead>代理</TableHead>
                  <TableHead>启用</TableHead>
                  <TableHead className="w-16 text-right">配置</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {sources.map((source) => {
                  const expanded = expandedSourceIds.has(source.id);
                  const status = getSourceStatus(source, source.apiKey);
                  const sourceBusy = sourceMutationId === source.id;
                  const proxyLocked = isAniBtRequestTarget(source);
                  return (
                    <SourceRows key={source.id}>
                      <TableRow data-state={expanded ? "selected" : undefined}>
                        <TableCell className="px-4 font-semibold">{source.name}</TableCell>
                        <TableCell><Badge tone={source.kind === "rss" ? "green" : "primary"}>{kindText[source.kind]}</Badge></TableCell>
                        <TableCell>
                          <div className="max-w-[260px] truncate font-mono text-xs text-muted-foreground" title={source.baseUrl ?? source.rssUrl ?? "本地输入"}>
                            {source.baseUrl ?? source.rssUrl ?? "本地输入"}
                          </div>
                        </TableCell>
                        <TableCell>
                          <div className="flex max-w-40 flex-wrap gap-1">
                            {(source.tags?.length ? source.tags : ["--"]).map((tag) => <Badge className="h-5" key={tag}>{tag}</Badge>)}
                          </div>
                        </TableCell>
                        <TableCell><Badge tone={status.tone}>{status.label}</Badge></TableCell>
                        <TableCell>
                          <Switch
                            aria-label={proxyLocked ? `${source.name} 固定直连` : `${source.name} 使用全局代理`}
                            checked={!proxyLocked && (source.useProxy ?? false)}
                            disabled={proxyLocked || Boolean(sourceMutationId)}
                            onCheckedChange={() => void toggleSourceProxy(source)}
                            title={proxyLocked ? "AniBT 固定直连" : undefined}
                          />
                        </TableCell>
                        <TableCell>
                          <Switch
                            aria-label={`${source.name} 启用状态`}
                            checked={source.enabled}
                            disabled={Boolean(sourceMutationId)}
                            onCheckedChange={() => void toggleSource(source)}
                          />
                        </TableCell>
                        <TableCell className="text-right">
                          <Button
                            className="size-11 p-0 md:size-9"
                            variant="ghost"
                            aria-expanded={expanded}
                            aria-label={expanded ? `收起 ${source.name} 配置` : `展开 ${source.name} 配置`}
                            title="来源配置"
                            onClick={() => toggleSourceDetails(source.id)}
                          >
                            {expanded ? <ChevronDown /> : <Settings2 />}
                          </Button>
                        </TableCell>
                      </TableRow>
                      {expanded && (
                        <TableRow className="bg-muted/30 hover:bg-muted/30">
                          <TableCell className="px-4 py-4" colSpan={8}>
                            <FieldGroup className="grid grid-cols-[repeat(auto-fit,minmax(min(100%,20rem),1fr))] items-start gap-4">
                              <Field className="min-w-0">
                                <FieldLabel htmlFor={`source-interval-${source.id}`}>最小采集间隔（毫秒）</FieldLabel>
                                <div className="flex min-w-0 flex-wrap gap-2">
                                  <Input
                                    className="min-w-48 flex-1"
                                    id={`source-interval-${source.id}`}
                                    type="number"
                                    min={getSourceMinimumRequestIntervalMs(source)}
                                    max={MAX_SOURCE_REQUEST_INTERVAL_MS}
                                    step={250}
                                    value={intervalDrafts[source.id] ?? String(normalizeSourceInterval(source.requestIntervalMs, source))}
                                    onChange={(event) => updateIntervalDraft(source.id, event.target.value)}
                                  />
                                  <Button className="shrink-0 whitespace-nowrap" variant="outline" disabled={Boolean(sourceMutationId)} onClick={() => void saveSourceInterval(source)}>
                                    <Save data-icon="inline-start" />保存
                                  </Button>
                                </div>
                                <FieldDescription>
                                  {getSourceMinimumRequestIntervalMs(source) > 250
                                    ? "AniBT 同域请求固定不低于 500 毫秒，并遵循服务端退避响应头。"
                                    : "同一域名请求会串行执行，并遵循服务端退避响应头。"}
                                </FieldDescription>
                              </Field>
                              {canUseCredential(source) ? (
                                <Field className="min-w-0">
                                  <FieldLabel htmlFor={`source-credential-${source.id}`}>
                                    <KeyRound className="size-4" />访问凭据
                                  </FieldLabel>
                                  <div className="flex min-w-0 flex-wrap gap-2">
                                    <Input
                                      className="min-w-48 flex-1"
                                      id={`source-credential-${source.id}`}
                                      placeholder={source.kind === "site_adapter" ? "Token / Cookie" : "API Key"}
                                      type="password"
                                      value={credentials[source.id] ?? ""}
                                      onChange={(event) => setCredentials({ ...credentials, [source.id]: event.target.value })}
                                    />
                                    <Button className="shrink-0 whitespace-nowrap" disabled={Boolean(sourceMutationId)} onClick={() => void saveCredential(source)}>
                                      <Save data-icon="inline-start" />保存凭据
                                    </Button>
                                  </div>
                                </Field>
                              ) : (
                                <div className="flex min-h-16 items-center rounded-md border border-dashed px-4 text-sm text-muted-foreground">
                                  RSS 来源无需访问凭据。
                                </div>
                              )}
                            </FieldGroup>
                            {sourceBusy && <div className="mt-2 text-xs text-muted-foreground">正在保存来源配置...</div>}
                          </TableCell>
                        </TableRow>
                      )}
                    </SourceRows>
                  );
                })}
              </TableBody>
            </Table>
          </div>
        ) : (
          <Empty className="mt-4 min-h-72">
            <EmptyHeader>
              <EmptyMedia variant="icon"><PlugZap /></EmptyMedia>
              <EmptyTitle>暂无下载源</EmptyTitle>
              <EmptyDescription>添加 RSS、Torznab 或站点适配器后会显示在这里。</EmptyDescription>
            </EmptyHeader>
            <Button onClick={() => setAddSheetOpen(true)}><Plus data-icon="inline-start" />添加下载源</Button>
          </Empty>
        )}
      </section>

      {addSheetOpen && (
        <WorkbenchSheet
          className="sm:max-w-xl"
          description="支持 RSS、Torznab 与已内置解析器的站点适配器。"
          footer={(
            <div className="grid grid-cols-2 gap-2">
              <Button variant="outline" onClick={() => setAddSheetOpen(false)} disabled={addingSource}>取消</Button>
              <Button onClick={() => void addSource()} disabled={addingSource}>
                <Plus data-icon="inline-start" />{addingSource ? "创建中" : "创建下载源"}
              </Button>
            </div>
          )}
          onClose={() => setAddSheetOpen(false)}
          title="添加下载源"
        >
          <FieldGroup>
            <Field data-invalid={Boolean(draftErrors.name)}>
              <FieldLabel htmlFor="source-name">下载源名称</FieldLabel>
              <Input
                id="source-name"
                aria-invalid={Boolean(draftErrors.name)}
                placeholder="例如 Bangumi.moe"
                value={draft.name}
                onChange={(event) => {
                  setDraft({ ...draft, name: event.target.value });
                  if (draftErrors.name) setDraftErrors({ ...draftErrors, name: undefined });
                }}
              />
              {draftErrors.name && <FieldDescription className="text-destructive">{draftErrors.name}</FieldDescription>}
            </Field>
            <Field>
              <FieldLabel htmlFor="source-kind">类型</FieldLabel>
              <Select value={draft.kind} onValueChange={(value) => setDraft({ ...draft, kind: value as SourceKind, apiKey: value === "rss" ? "" : draft.apiKey })}>
                <SelectTrigger id="source-kind"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    <SelectItem value="rss">RSS</SelectItem>
                    <SelectItem value="torznab">Torznab</SelectItem>
                    <SelectItem value="site_adapter">站点适配器</SelectItem>
                  </SelectGroup>
                </SelectContent>
              </Select>
            </Field>
            <Field data-invalid={Boolean(draftErrors.url)}>
              <FieldLabel htmlFor="source-url">服务地址</FieldLabel>
              <Input
                id="source-url"
                aria-invalid={Boolean(draftErrors.url)}
                placeholder={draft.kind === "rss" ? "https://example.com/feed.xml" : "https://example.com"}
                value={draft.url}
                onChange={(event) => {
                  setDraft({ ...draft, url: event.target.value });
                  if (draftErrors.url) setDraftErrors({ ...draftErrors, url: undefined });
                }}
              />
              {draftErrors.url && <FieldDescription className="text-destructive">{draftErrors.url}</FieldDescription>}
            </Field>
            {draft.kind !== "rss" && (
              <Field>
                <FieldLabel htmlFor="source-api-key">访问凭据</FieldLabel>
                <Input
                  id="source-api-key"
                  placeholder={draft.kind === "site_adapter" ? "Token / Cookie（可选）" : "API Key（可选）"}
                  type="password"
                  value={draft.apiKey}
                  onChange={(event) => setDraft({ ...draft, apiKey: event.target.value })}
                />
              </Field>
            )}
            <Alert>
              <PlugZap />
              <AlertTitle>来源能力</AlertTitle>
              <AlertDescription>站点适配器需本地解析器支持；通用订阅请选择 RSS 或 Torznab。</AlertDescription>
            </Alert>
          </FieldGroup>
        </WorkbenchSheet>
      )}
    </Page>
  );
}

/** 渲染一个下载源的主行与可选详情行，避免额外 DOM 破坏表格结构。 */
function SourceRows({ children }: { children: ReactNode }) {
  return <>{children}</>;
}

/** 渲染配置分区标题与说明。 */
function SectionHeading({ title, description }: { title: string; description: string }) {
  return (
    <div className="min-w-0">
      <h2 className="text-sm font-bold">{title}</h2>
      <p className="mt-1 text-xs text-muted-foreground">{description}</p>
    </div>
  );
}

function canUseCredential(source: ReleaseSourceConfig): boolean {
  return source.kind === "torznab" || source.kind === "site_adapter";
}

function needsCredential(source: ReleaseSourceConfig, credential?: string): boolean {
  return source.enabled && source.kind === "torznab" && !credential?.trim();
}

function getSourceStatus(
  source: ReleaseSourceConfig,
  credential?: string
): { label: string; tone: "neutral" | "green" | "amber" } {
  if (!source.enabled) return { label: "已停用", tone: "neutral" };
  if (needsCredential(source, credential)) return { label: "需要凭据", tone: "amber" };
  return { label: "正常", tone: "green" };
}

const defaultMetadataProxySettings: MetadataProxySettings = {
  mode: "system",
  timeoutMs: 30_000
};

function getMetadataProxySettings(settings: AppSettings | null): MetadataProxySettings {
  return settings?.network?.metadataProxy ?? defaultMetadataProxySettings;
}

function getSourceSyncSettings(settings: AppSettings | null): { enabled: boolean; dailyTime: string } {
  const dailyTime = settings?.sourceSync?.dailyTime;
  return {
    enabled: settings?.sourceSync?.enabled ?? true,
    dailyTime: /^([01]\d|2[0-3]):[0-5]\d$/.test(dailyTime ?? "") ? dailyTime! : "09:00"
  };
}

/** 规范化下载源间隔，并同步展示站点级最低限制。 */
function normalizeSourceInterval(value: number | undefined, source: ReleaseSourceConfig): number {
  const minimumMs = getSourceMinimumRequestIntervalMs(source);
  if (!Number.isFinite(value)) return Math.max(DEFAULT_SOURCE_REQUEST_INTERVAL_MS, minimumMs);
  return Math.max(minimumMs, Math.min(MAX_SOURCE_REQUEST_INTERVAL_MS, Math.round(value!)));
}

function formatOptionalDateTime(value?: string): string {
  if (!value) return "--";
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? "--" : date.toLocaleString();
}

function normalizeMetadataProxyDraft(draft: MetadataProxySettings): MetadataProxySettings {
  const timeoutMs = Number.isFinite(draft.timeoutMs) ? Math.round(draft.timeoutMs) : defaultMetadataProxySettings.timeoutMs;
  return {
    mode: draft.mode,
    url: draft.mode === "manual" ? draft.url?.trim() || undefined : undefined,
    timeoutMs: Math.max(1_000, Math.min(60_000, timeoutMs))
  };
}

function formatProxyMode(mode: MetadataProxySettings["mode"]): string {
  if (mode === "system") return "系统代理";
  if (mode === "manual") return "手动代理";
  return "关闭";
}

function ProxySummaryItem({ icon, label, value }: { icon?: ReactNode; label: string; value: string }) {
  return (
    <div className="min-w-0 bg-card p-3">
      <div className="flex items-center gap-2 text-xs text-muted-foreground">
        {icon && <span className="text-primary [&_svg]:size-4">{icon}</span>}
        {label}
      </div>
      <div className="mt-1 truncate text-sm font-medium" title={value}>{value}</div>
    </div>
  );
}

/** 渲染每日同步的单项状态数据。 */
function SourceSyncSummaryItem({ icon: Icon, label, value }: { icon: LucideIcon; label: string; value: string }) {
  return (
    <div className="min-w-0 p-4">
      <div className="flex items-center gap-2 text-xs text-muted-foreground">
        <Icon className="size-4 text-primary" />
        {label}
      </div>
      <div className="mt-2 truncate text-base font-semibold" title={value}>{value}</div>
    </div>
  );
}

/** 在窄屏和桌面端分别渲染横向或纵向状态分隔线。 */
function ResponsiveSummarySeparator() {
  return (
    <>
      <Separator className="sm:hidden" />
      <Separator className="hidden h-full sm:block" orientation="vertical" />
    </>
  );
}

/** 渲染下载源页面加载骨架。 */
function SourcesPageSkeleton() {
  return (
    <Page aria-busy="true" aria-label="正在加载下载源">
      <PageHeader><Skeleton className="h-4 w-24" /></PageHeader>
      <div className="flex flex-wrap items-center justify-between gap-3 border-y py-3">
        <Skeleton className="h-5 w-64 max-w-full" />
        <Skeleton className="h-9 w-28" />
      </div>
      <Skeleton className="h-40 w-full" />
      <Skeleton className="h-56 w-full" />
    </Page>
  );
}

function createSourceId(name: string): string {
  const slug = name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9\u4e00-\u9fa5]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return `${slug || "source"}-${Date.now()}`;
}

function isValidSourceUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "http:" || url.protocol === "https:";
  } catch {
    return false;
  }
}
