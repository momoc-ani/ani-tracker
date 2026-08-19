import type { ReactNode } from "react";
import { useEffect, useId, useState } from "react";
import { toast } from "@/lib/toast";
import {
  Activity,
  Bell,
  ChevronDown,
  Clock3,
  Copy,
  Download,
  ExternalLink,
  FileSearch,
  FolderCog,
  FolderOpen,
  Github,
  GitFork,
  HardDrive,
  Info,
  KeyRound,
  Languages,
  Minus,
  Monitor,
  Palette,
  PlayCircle,
  Power,
  Plus,
  RefreshCw,
  RotateCcw,
  Save,
  Smartphone,
  TimerReset,
  Unplug
} from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import { Command, CommandEmpty, CommandGroup, CommandItem, CommandList } from "@/components/ui/command";
import { Field, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  InputGroup,
  InputGroupAddon,
  InputGroupButton,
  InputGroupInput
} from "@/components/ui/input-group";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Slider } from "@/components/ui/slider";
import { Separator } from "@/components/ui/separator";
import { Switch } from "@/components/ui/switch";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { appApi } from "@/lib/api";
import { cn } from "@/lib/cn";
import { getAppCapabilities, getAppRuntime } from "@/lib/runtime";
import { useAsyncData } from "@/lib/use-async-data";
import { useTheme } from "@/components/theme-provider";
import { ConfirmActionDialog } from "@/components/confirm-action-dialog";
import { StickyActionBar } from "@/components/page-layout";
import { AppearanceSettingsSection } from "./AppearanceSettingsSection";
import { LocalMediaLibrarySettingsSection } from "./LocalMediaLibrarySettingsSection";
import { RemotePlaybackSettingsSection } from "./RemotePlaybackSettingsSection";
import type {
  AutomationSchedulerStatus,
  EmbeddedTorrentCoreStatus,
  MobileNotificationPermission,
  PlayerDetectionResult,
  QbittorrentManagedStatus,
  RemoteGatewayStatus,
  RemotePairingChallenge
} from "@shared/contracts";
import type { AppSettings } from "@shared/domain";
import {
  createDefaultAppearanceSettings,
  listAvailableThemePacks,
  type AppearanceSettings
} from "@shared/theme";
import { normalizeCandidateFansubNames, normalizeFansubMatchName } from "@shared/fansub-name-matcher";
import { BUILTIN_PLAYER_PROFILE_ID } from "@shared/player-selection";

type SettingsCategoryId =
  | "appearance"
  | "storage"
  | "interface"
  | "remote"
  | "media"
  | "download"
  | "automation"
  | "about";

const settingsCategories: Array<{
  id: SettingsCategoryId;
  label: string;
  icon: typeof Palette;
}> = [
  { id: "appearance", label: "外观", icon: Palette },
  { id: "storage", label: "存储与目录", icon: HardDrive },
  { id: "interface", label: "语言与桌面集成", icon: Monitor },
  { id: "remote", label: "远程设备", icon: Smartphone },
  { id: "media", label: "播放器与媒体", icon: PlayCircle },
  { id: "download", label: "下载核心", icon: Download },
  { id: "automation", label: "自动化", icon: RefreshCw },
  { id: "about", label: "关于", icon: Info }
];

export function SettingsPage() {
  const capabilities = getAppCapabilities();
  const runtime = getAppRuntime();
  const mobileRuntime = runtime === "android" || runtime === "ios";
  const remoteRuntime = runtime === "remote";
  const hostExternalQbittorrent = capabilities.externalQbittorrent || remoteRuntime;
  const hostManagedQbittorrent = capabilities.managedQbittorrent || remoteRuntime;
  const visibleSettingsCategories = settingsCategories
    .filter((category) => category.id !== "remote" || (capabilities.remoteGateway && !remoteRuntime))
    .map((category) => category.id === "interface" && (mobileRuntime || remoteRuntime)
      ? { ...category, label: "语言" }
      : category.id === "media" && remoteRuntime
        ? { ...category, label: "远程播放" }
      : category);
  const { appearance, commitAppearance } = useTheme();
  const { data, loading } = useAsyncData(appApi.getSettings, []);
  const [draft, setDraft] = useState<AppSettings | null>(null);
  const [persistedSettings, setPersistedSettings] = useState<AppSettings | null>(null);
  const [activeCategory, setActiveCategory] = useState<SettingsCategoryId>("appearance");
  const [saveState, setSaveState] = useState<"idle" | "saving" | "saved">("idle");
  const [resetState, setResetState] = useState<"idle" | "resetting" | "reset">("idle");
  const [resetDialogOpen, setResetDialogOpen] = useState(false);
  const [restoreDialogOpen, setRestoreDialogOpen] = useState(false);
  const [backupAction, setBackupAction] = useState<"idle" | "exporting" | "restoring">("idle");
  const [logExporting, setLogExporting] = useState(false);
  const [schedulerStatus, setSchedulerStatus] = useState<AutomationSchedulerStatus | null>(null);
  const [qbManagedStatus, setQbManagedStatus] = useState<QbittorrentManagedStatus | null>(null);
  const [qbManagedAction, setQbManagedAction] = useState<"idle" | "starting" | "stopping" | "restarting">("idle");
  const [qbConnectionState, setQbConnectionState] = useState<"idle" | "testing" | "online" | "offline">("idle");
  const [embeddedStatus, setEmbeddedStatus] = useState<EmbeddedTorrentCoreStatus | null>(null);
  const [embeddedAction, setEmbeddedAction] = useState<"idle" | "starting" | "stopping" | "restarting">("idle");
  const [embeddedError, setEmbeddedError] = useState<string | null>(null);
  const [remoteStatus, setRemoteStatus] = useState<RemoteGatewayStatus | null>(null);
  const [remotePairing, setRemotePairing] = useState<RemotePairingChallenge | null>(null);
  const [remoteAction, setRemoteAction] = useState<"idle" | "loading" | "creating" | "revoking">("idle");
  const [revokingDeviceId, setRevokingDeviceId] = useState<string | null>(null);
  const [remoteError, setRemoteError] = useState<string | null>(null);
  const [qbTest, setQbTest] = useState<{ state: "idle" | "testing" | "success" | "error"; message?: string }>({
    state: "idle"
  });
  const [playerDetection, setPlayerDetection] = useState<PlayerDetectionResult | null>(null);
  const [playerDetectionState, setPlayerDetectionState] = useState<"idle" | "detecting" | "error">("idle");
  const [playerDetectionError, setPlayerDetectionError] = useState<string | null>(null);
  const [notificationPermission, setNotificationPermission] = useState<MobileNotificationPermission | null>(null);
  const [requestingNotificationPermission, setRequestingNotificationPermission] = useState(false);
  const settingsReady = !loading && Boolean(data && draft);

  useEffect(() => {
    if (data) {
      const next = remoteRuntime ? { ...data, appearance } : data;
      setDraft(next);
      setPersistedSettings(next);
      if (!remoteRuntime) void refreshPlayerDetection(data.players);
    }
  }, [data]);

  useEffect(() => {
    if (!settingsReady) {
      return;
    }
    const sections = visibleSettingsCategories
      .map((category) => document.getElementById(`settings-${category.id}`))
      .filter((section): section is HTMLElement => Boolean(section));
    if (sections.length === 0) {
      return;
    }
    const scrollContainer = sections[0].closest<HTMLElement>("main");

    /** 按吸顶定位线校准当前分区，处理页面末尾无法完整进入观察区的情况。 */
    function syncActiveCategory() {
      const containerTop = scrollContainer?.getBoundingClientRect().top ?? 0;
      const remainingScroll = scrollContainer
        ? scrollContainer.scrollHeight - scrollContainer.clientHeight - scrollContainer.scrollTop
        : Number.POSITIVE_INFINITY;
      const activeSection = remainingScroll <= 2
        ? sections[sections.length - 1]
        : [...sections].reverse().find((section) => {
            const scrollMarginTop = Number.parseFloat(window.getComputedStyle(section).scrollMarginTop) || 0;
            return section.getBoundingClientRect().top <= containerTop + scrollMarginTop + 2;
          }) ?? sections[0];
      setActiveCategory(activeSection.id.replace("settings-", "") as SettingsCategoryId);
    }

    let scrollSettleTimer: number | undefined;
    /** 滚动停止后执行一次最终校准，避免平滑滚动途中状态停留在前一分区。 */
    function scheduleActiveCategorySync() {
      window.clearTimeout(scrollSettleTimer);
      scrollSettleTimer = window.setTimeout(syncActiveCategory, 80);
    }

    const observer = new IntersectionObserver(
      (entries) => {
        const activeEntry = entries
          .filter((entry) => entry.isIntersecting)
          .sort((left, right) => right.intersectionRatio - left.intersectionRatio)[0];
        if (activeEntry) {
          setActiveCategory(activeEntry.target.id.replace("settings-", "") as SettingsCategoryId);
        }
      },
      { rootMargin: "-24% 0px -62% 0px", threshold: [0.1, 0.35, 0.65] }
    );
    sections.forEach((section) => observer.observe(section));
    scrollContainer?.addEventListener("scroll", scheduleActiveCategorySync, { passive: true });
    return () => {
      observer.disconnect();
      scrollContainer?.removeEventListener("scroll", scheduleActiveCategorySync);
      window.clearTimeout(scrollSettleTimer);
    };
  }, [settingsReady, capabilities.remoteGateway, remoteRuntime]);

  useEffect(() => {
    void refreshSchedulerStatus();
    void refreshQbittorrentManagedStatus();
    void refreshEmbeddedTorrentStatus();
    if (capabilities.remoteGateway) {
      void refreshRemoteStatus();
    }
    if (mobileRuntime) {
      void appApi.getMobilePlatformStatus?.()
        .then((status) => setNotificationPermission(status.notificationPermission))
        .catch((error) => console.warn("[settings] 移动通知权限读取失败", error));
    }
  }, []);

  /** 由用户操作触发移动系统通知授权。 */
  async function requestNotificationPermission() {
    setRequestingNotificationPermission(true);
    try {
      const result = await appApi.requestMobileNotificationPermission?.();
      if (result) setNotificationPermission(result);
      if (result === "granted") {
        toast.success("系统通知已允许");
      } else {
        toast.warning("系统通知未获授权");
      }
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "请求系统通知权限失败");
    } finally {
      setRequestingNotificationPermission(false);
    }
  }

  async function refreshSchedulerStatus() {
    setSchedulerStatus(await appApi.getAutomationSchedulerStatus());
  }

  async function refreshQbittorrentManagedStatus() {
    if (!hostManagedQbittorrent) {
      setQbManagedStatus(null);
      return;
    }
    try {
      setQbManagedStatus(await appApi.getQbittorrentManagedStatus());
    } catch (error) {
      setQbTest({
        state: "error",
        message: error instanceof Error ? error.message : "读取 qBittorrent 托管状态失败"
      });
    }
  }

  /** 刷新内置 libtorrent 核心的进程与版本状态。 */
  async function refreshEmbeddedTorrentStatus() {
    try {
      const status = await appApi.getEmbeddedTorrentStatus();
      setEmbeddedStatus(status);
      setEmbeddedError(status.lastError ?? null);
      return status;
    } catch (error) {
      setEmbeddedError(error instanceof Error ? error.message : "读取内置下载核心状态失败");
      return null;
    }
  }

  /** 刷新本机远程网关与已配对设备状态。 */
  async function refreshRemoteStatus() {
    setRemoteAction("loading");
    try {
      const status = await appApi.getRemoteGatewayStatus();
      setRemoteStatus(status);
      if (remotePairing && Date.parse(remotePairing.expiresAt) <= Date.now()) {
        setRemotePairing(null);
      }
      setRemoteError(null);
      return status;
    } catch (error) {
      setRemoteError(error instanceof Error ? error.message : "读取远程设备状态失败");
      return null;
    } finally {
      setRemoteAction("idle");
    }
  }

  /** 创建两分钟有效的一次性远程配对码。 */
  async function createRemotePairingCode() {
    setRemoteAction("creating");
    try {
      setRemotePairing(await appApi.createRemotePairingCode());
      setRemoteError(null);
    } catch (error) {
      setRemoteError(error instanceof Error ? error.message : "创建远程配对码失败");
    } finally {
      setRemoteAction("idle");
    }
  }

  /** 吊销指定远程设备的访问令牌。 */
  async function revokeRemoteDevice(deviceId: string) {
    setRemoteAction("revoking");
    setRevokingDeviceId(deviceId);
    try {
      setRemoteStatus(await appApi.revokeRemoteDevice(deviceId));
      setRemotePairing(null);
      setRemoteError(null);
    } catch (error) {
      setRemoteError(error instanceof Error ? error.message : "吊销远程设备失败");
    } finally {
      setRevokingDeviceId(null);
      setRemoteAction("idle");
    }
  }

  /** 复制本地 CA 下载地址，便于在移动设备中安装证书。 */
  async function copyAuthorityCertificateUrl() {
    if (!remoteStatus?.certificate) {
      return;
    }
    try {
      await navigator.clipboard.writeText(`${remoteStatus.baseUrl}/ani-tracker-ca.crt`);
      toast.success("CA 下载地址已复制");
    } catch {
      toast.error("复制失败，请手动复制 CA 下载地址");
    }
  }

  /** 使用当前平台的外链能力打开项目代码仓库。 */
  async function openProjectUrl(projectName: string, url: string) {
    console.info("[settings] 正在打开项目地址", { projectName, url, runtime });
    try {
      await appApi.openExternal(url);
      console.info("[settings] 项目地址已打开", { projectName, url, runtime });
    } catch (error) {
      console.error("[settings] 项目地址打开失败", { projectName, url, runtime, error });
      toast.error(`${projectName} 地址打开失败`);
    }
  }

  useEffect(() => {
    const currentTorrentEngineMode = !hostExternalQbittorrent || draft?.download.defaultTorrentEngine === "embedded"
      ? "embedded"
      : hostManagedQbittorrent && draft?.download.qbittorrent.managed.enabled
        ? "managed"
        : "external";
    if (!settingsReady || currentTorrentEngineMode === "embedded") {
      setQbConnectionState("idle");
      return;
    }
    void refreshQbittorrentConnection();
  }, [
    draft?.download.defaultTorrentEngine,
    draft?.download.qbittorrent.baseUrl,
    draft?.download.qbittorrent.managed.enabled,
    hostExternalQbittorrent,
    hostManagedQbittorrent,
    settingsReady
  ]);

  if (loading) {
    return (
      <div className="flex flex-col gap-4" aria-label="正在加载设置">
        <Skeleton className="h-10 w-48" />
        <Skeleton className="h-48 w-full" />
        <Skeleton className="h-64 w-full" />
      </div>
    );
  }

  if (!data || !draft) {
    return (
      <Alert variant="destructive">
        <AlertTitle>设置加载失败</AlertTitle>
        <AlertDescription>请重新进入设置页或重启应用后再试。</AlertDescription>
      </Alert>
    );
  }

  /** 保存当前草稿；远程端仅提交允许管理的 PC 设置字段。 */
  async function persistDraftSettings(current: AppSettings): Promise<AppSettings> {
    const saved = await appApi.updateSettings(remoteRuntime
      ? {
          download: current.download,
          automation: current.automation
        }
      : current);
    const next = remoteRuntime
      ? {
          ...current,
          download: saved.download,
          automation: saved.automation,
          sourceSync: saved.sourceSync,
          network: {
            ...current.network,
            metadataProxy: saved.network.metadataProxy
          }
        }
      : saved;
    setDraft(next);
    setPersistedSettings(next);
    commitAppearance(next.appearance);
    console.info("[settings] 设置草稿已持久化", {
      runtime,
      torrentEngine: next.download.defaultTorrentEngine,
      automationEnabled: next.automation.scheduledCheckEnabled
    });
    return next;
  }

  async function saveSettings() {
    if (!draft) {
      return;
    }

    setSaveState("saving");
    try {
      const saved = await persistDraftSettings(draft);
      const [, , , remote] = await Promise.all([
        refreshSchedulerStatus(),
        refreshQbittorrentManagedStatus(),
        refreshEmbeddedTorrentStatus(),
        capabilities.remoteGateway ? refreshRemoteStatus() : Promise.resolve(null),
        remoteRuntime ? Promise.resolve() : refreshPlayerDetection(saved.players),
        pruneUnusedThemeBackgrounds(saved.appearance)
      ]);
      setSaveState("saved");
      if (!remoteRuntime && saved.network.remoteAccess.lanEnabled && (!remote?.lanEnabled || remote.lastError)) {
        toast.warning("设置已保存，但局域网 HTTPS 启动失败，已恢复本机访问");
      } else {
        toast.success("设置已保存");
      }
      window.setTimeout(() => setSaveState("idle"), 1200);
    } catch (error) {
      setSaveState("idle");
      toast.error(error instanceof Error ? error.message : "设置保存失败");
    }
  }

  async function resetSettingsToDefaults() {
    if (!draft) return;
    setResetState("resetting");
    try {
      if (remoteRuntime) {
        const next = { ...draft, appearance: createDefaultAppearanceSettings() };
        setDraft(next);
        setPersistedSettings(next);
        commitAppearance(next.appearance);
        await pruneUnusedThemeBackgrounds(next.appearance);
        setResetState("reset");
        toast.success("当前设备外观已恢复默认");
        window.setTimeout(() => setResetState("idle"), 1200);
        return;
      }
      const saved = await appApi.resetSettingsToDefaults();
      setDraft(saved);
      setPersistedSettings(saved);
      commitAppearance(saved.appearance);
      await refreshPlayerDetection(saved.players);
      setQbTest({ state: "idle" });
      await refreshSchedulerStatus();
      await refreshQbittorrentManagedStatus();
      await refreshEmbeddedTorrentStatus();
      await pruneUnusedThemeBackgrounds(saved.appearance);
      setResetState("reset");
      window.setTimeout(() => setResetState("idle"), 1200);
    } catch (error) {
      setResetState("idle");
      toast.error(error instanceof Error ? error.message : "恢复默认设置失败");
      throw error;
    }
  }

  /** 使用系统保存面板导出一致性 SQLite 备份。 */
  async function exportDatabaseBackup() {
    if (!appApi.exportDatabaseBackup) return;
    setBackupAction("exporting");
    try {
      const fileName = await appApi.exportDatabaseBackup();
      if (fileName) toast.success(`数据备份已导出：${fileName}`);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "数据备份导出失败");
    } finally {
      setBackupAction("idle");
    }
  }

  /** 使用系统保存面板导出当前及轮转日志。 */
  async function exportLogs() {
    if (!appApi.exportLogs) return;
    setLogExporting(true);
    try {
      const fileName = await appApi.exportLogs();
      if (fileName) toast.success(`日志已导出：${fileName}`);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "日志导出失败");
    } finally {
      setLogExporting(false);
    }
  }

  /** 恢复用户选择的备份，并重新载入主题与运行设置。 */
  async function restoreDatabaseBackup() {
    if (!appApi.restoreDatabaseBackup) return;
    setBackupAction("restoring");
    try {
      const rollbackFileName = await appApi.restoreDatabaseBackup();
      if (!rollbackFileName) return;
      const restored = await appApi.getSettings();
      setDraft(restored);
      setPersistedSettings(restored);
      commitAppearance(restored.appearance);
      await Promise.all([
        refreshSchedulerStatus(),
        refreshQbittorrentManagedStatus(),
        refreshEmbeddedTorrentStatus(),
        capabilities.remoteGateway ? refreshRemoteStatus() : Promise.resolve(null),
        refreshPlayerDetection(restored.players),
        pruneUnusedThemeBackgrounds(restored.appearance)
      ]);
      toast.success(`数据已恢复，恢复前快照：${rollbackFileName}`);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "数据备份恢复失败");
      throw error;
    } finally {
      setBackupAction("idle");
    }
  }

  /** 清理设置中已无引用的主题图片；失败不回滚已持久化设置。 */
  async function pruneUnusedThemeBackgrounds(appearance: AppearanceSettings): Promise<void> {
    const references = listAvailableThemePacks(appearance)
      .filter((pack) => Boolean(pack.backgroundImage))
      .map((pack) => ({
        themeId: pack.id,
        fileName: pack.backgroundImage!.file
      }));
    try {
      await appApi.pruneThemeBackgrounds(references);
    } catch (error) {
      console.warn("[settings] 未引用主题背景清理失败", error);
    }
  }

  /** 切换默认下载引擎，同时保留各引擎原有配置。 */
  function updateTorrentEngineMode(mode: "embedded" | "managed" | "external") {
    if (!draft) {
      return;
    }
    if (!hostExternalQbittorrent && mode !== "embedded") {
      return;
    }
    const managed = mode === "managed";
    const embedded = mode === "embedded";
    setDraft({
      ...draft,
      download: {
        ...draft.download,
        defaultTorrentEngine: embedded ? "embedded" : "qbittorrent",
        embedded: {
          ...draft.download.embedded,
          enabled: embedded
        },
        qbittorrent: {
          ...draft.download.qbittorrent,
          autoConnect: embedded ? draft.download.qbittorrent.autoConnect : managed,
          managed: {
            ...draft.download.qbittorrent.managed,
            enabled: embedded ? draft.download.qbittorrent.managed.enabled : managed
          }
        }
      }
    });
    setQbTest({ state: "idle" });
    setEmbeddedError(null);
  }

  /** 保存设置并启动内置 libtorrent 核心。 */
  async function startEmbeddedTorrent() {
    if (!draft) return;
    setEmbeddedAction("starting");
    try {
      await persistDraftSettings(draft);
      const status = await appApi.startEmbeddedTorrent();
      setEmbeddedStatus(status);
      setEmbeddedError(status.lastError ?? null);
    } catch (error) {
      setEmbeddedError(error instanceof Error ? error.message : "内置下载核心启动失败");
    } finally {
      setEmbeddedAction("idle");
    }
  }

  /** 停止内置 libtorrent 核心并刷新状态。 */
  async function stopEmbeddedTorrent() {
    setEmbeddedAction("stopping");
    try {
      const status = await appApi.stopEmbeddedTorrent();
      setEmbeddedStatus(status);
      setEmbeddedError(status.lastError ?? null);
    } catch (error) {
      setEmbeddedError(error instanceof Error ? error.message : "内置下载核心停止失败");
    } finally {
      setEmbeddedAction("idle");
    }
  }

  /** 保存设置并重启内置 libtorrent 核心。 */
  async function restartEmbeddedTorrent() {
    if (!draft) return;
    setEmbeddedAction("restarting");
    try {
      await persistDraftSettings(draft);
      const status = await appApi.restartEmbeddedTorrent();
      setEmbeddedStatus(status);
      setEmbeddedError(status.lastError ?? null);
    } catch (error) {
      setEmbeddedError(error instanceof Error ? error.message : "内置下载核心重启失败");
    } finally {
      setEmbeddedAction("idle");
    }
  }

  /** 保存当前配置并测试 qBittorrent WebUI 连接。 */
  async function testQbittorrent() {
    if (!draft) {
      return;
    }

    setQbTest({ state: "testing", message: "正在测试 qBittorrent 连接..." });
    setQbConnectionState("testing");
    try {
      await persistDraftSettings(draft);
      const result = await appApi.testQbittorrent();
      const connectionState = result.ok ? "online" : "offline";
      setQbConnectionState(connectionState);
      setQbTest({
        state: result.ok ? "success" : "error",
        message: result.ok ? `${result.message}，当前任务 ${result.taskCount ?? 0} 个` : result.message
      });
      console.info("[settings] qBittorrent WebUI 连接检测完成", {
        ok: result.ok,
        taskCount: result.taskCount
      });
      await refreshQbittorrentManagedStatus();
    } catch (error) {
      setQbConnectionState("offline");
      setQbTest({
        state: "error",
        message: error instanceof Error ? error.message : "qBittorrent 连接测试失败"
      });
    }
  }

  /** 在设置页打开时探测当前 qBittorrent WebUI，避免只依赖托管进程句柄。 */
  async function refreshQbittorrentConnection(): Promise<void> {
    const currentTorrentEngineMode = !hostExternalQbittorrent || draft?.download.defaultTorrentEngine === "embedded"
      ? "embedded"
      : hostManagedQbittorrent && draft?.download.qbittorrent.managed.enabled
        ? "managed"
        : "external";
    if (!draft || currentTorrentEngineMode === "embedded") {
      setQbConnectionState("idle");
      return;
    }
    setQbConnectionState("testing");
    try {
      const result = await appApi.testQbittorrent();
      setQbConnectionState(result.ok ? "online" : "offline");
      setQbTest({
        state: result.ok ? "success" : "error",
        message: result.ok ? `${result.message}，当前任务 ${result.taskCount ?? 0} 个` : result.message
      });
      console.info("[settings] qBittorrent WebUI 初始状态已刷新", {
        ok: result.ok,
        taskCount: result.taskCount
      });
    } catch (error) {
      setQbConnectionState("offline");
      setQbTest({
        state: "error",
        message: error instanceof Error ? error.message : "qBittorrent 连接检测失败"
      });
    }
  }

  async function startQbittorrentManaged() {
    if (!draft) {
      return;
    }

    setQbManagedAction("starting");
    try {
      await persistDraftSettings(draft);
      const status = await appApi.startQbittorrentManaged();
      setQbManagedStatus(status);
      setQbConnectionState(status.lastError ? "offline" : "online");
      setQbTest({
        state: status.lastError ? "error" : "success",
        message: status.lastError ?? "托管 qBittorrent 已启动"
      });
    } catch (error) {
      setQbTest({
        state: "error",
        message: error instanceof Error ? error.message : "托管 qBittorrent 启动失败"
      });
    } finally {
      setQbManagedAction("idle");
    }
  }

  async function stopQbittorrentManaged() {
    setQbManagedAction("stopping");
    try {
      const status = await appApi.stopQbittorrentManaged();
      setQbManagedStatus(status);
      setQbConnectionState("offline");
      setQbTest({ state: "idle", message: "托管 qBittorrent 已停止" });
    } catch (error) {
      setQbTest({
        state: "error",
        message: error instanceof Error ? error.message : "托管 qBittorrent 停止失败"
      });
    } finally {
      setQbManagedAction("idle");
    }
  }

  /** 保存设置后重启内置 qBittorrent-nox 进程。 */
  async function restartQbittorrentManaged() {
    if (!draft) {
      return;
    }

    setQbManagedAction("restarting");
    try {
      await persistDraftSettings(draft);
      if (qbManagedStatus?.running) {
        await appApi.stopQbittorrentManaged();
      }
      const status = await appApi.startQbittorrentManaged();
      setQbManagedStatus(status);
      setQbConnectionState(status.lastError ? "offline" : "online");
      setQbTest({
        state: status.lastError ? "error" : "success",
        message: status.lastError ?? "内置 qBittorrent-nox 已重启"
      });
    } catch (error) {
      setQbTest({
        state: "error",
        message: error instanceof Error ? error.message : "内置 qBittorrent-nox 重启失败"
      });
    } finally {
      setQbManagedAction("idle");
    }
  }

  /** 使用系统浏览器打开当前 qBittorrent WebUI。 */
  async function openQbittorrentWebUi() {
    const url = qbManagedStatus?.webUiUrl || draft?.download.qbittorrent.baseUrl;
    if (!url) {
      return;
    }
    try {
      await appApi.openExternal(url);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "打开 qBittorrent WebUI 失败");
    }
  }

  /** 探测当前系统可用的播放器路径并刷新状态。 */
  async function refreshPlayerDetection(players = draft?.players, notify = false) {
    if (!capabilities.externalPlayerConfiguration || !players) {
      setPlayerDetection(null);
      setPlayerDetectionError(null);
      return;
    }
    setPlayerDetectionState("detecting");
    try {
      const result = await appApi.detectPlayers(players);
      setPlayerDetection(result);
      setPlayerDetectionError(null);
      if (notify) {
        if (result.detectedProfileId) {
          toast.success("播放器探测完成");
        } else {
          toast.warning("未探测到可用播放器");
        }
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : "播放器探测失败";
      setPlayerDetectionError(message);
      if (notify) {
        toast.error(message);
      }
    } finally {
      setPlayerDetectionState("idle");
    }
  }

  /** 更新指定播放器的可执行文件路径并等待用户保存。 */
  function updatePlayerPath(profileId: string, executablePath: string) {
    if (!draft) {
      return;
    }
    setDraft({
      ...draft,
      players: draft.players.map((player) => player.id === profileId ? { ...player, executablePath } : player)
    });
    setPlayerDetection(null);
    setPlayerDetectionError(null);
  }

  /** 打开系统文件选择器并写入当前播放器路径。 */
  async function selectPlayerExecutable(profileId: string) {
    if (!draft) {
      return;
    }
    const player = draft.players.find((item) => item.id === profileId);
    if (!player) {
      return;
    }
    try {
      const selectedPath = await appApi.selectPlayerExecutable({
        profileId,
        currentPath: player.executablePath
      });
      if (!selectedPath) {
        return;
      }
      const players = draft.players.map((item) => item.id === profileId
        ? { ...item, executablePath: selectedPath }
        : item);
      setDraft({ ...draft, players });
      await refreshPlayerDetection(players);
      toast.success("播放器路径已选择，请保存设置");
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "播放器文件选择失败");
    }
  }

  /** 滚动至指定设置分区，并让导航立即反映当前选择。 */
  function navigateToCategory(categoryId: SettingsCategoryId) {
    setActiveCategory(categoryId);
    const section = document.getElementById(`settings-${categoryId}`);
    const scrollContainer = section?.closest<HTMLElement>("main");
    if (!section || !scrollContainer) return;

    const scrollMarginTop = Number.parseFloat(window.getComputedStyle(section).scrollMarginTop) || 0;
    const targetScrollTop = scrollContainer.scrollTop
      + section.getBoundingClientRect().top
      - scrollContainer.getBoundingClientRect().top
      - scrollMarginTop;
    const maxScrollTop = Math.max(0, scrollContainer.scrollHeight - scrollContainer.clientHeight);
    const nextScrollTop = Math.min(Math.max(0, targetScrollTop), maxScrollTop);

    scrollContainer.scrollTo({
      behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth",
      top: nextScrollTop
    });
    console.info("[settings] 已定位设置分区", { categoryId, scrollTop: nextScrollTop });
  }

  const playerOptions = remoteRuntime ? [] : draft.players.filter((player) =>
    !playerDetection || playerDetection.candidates.some((candidate) => candidate.profileId === player.id)
  );
  const selectedPlayerId = draft.defaultPlayerProfileId ?? "auto";
  const selectedPlayer = remoteRuntime
    ? undefined
    : draft.players.find((player) => player.id === selectedPlayerId);
  const selectedCandidate = playerDetection?.candidates.find((candidate) => candidate.profileId === selectedPlayerId);
  const autoCandidate = playerDetection?.candidates.find((candidate) => candidate.profileId === playerDetection.detectedProfileId);
  const torrentEngineMode = !hostExternalQbittorrent || draft.download.defaultTorrentEngine === "embedded"
    ? "embedded"
    : hostManagedQbittorrent && draft.download.qbittorrent.managed.enabled ? "managed" : "external";
  const embeddedSeedingLimits = draft.download.embedded.seedingLimits
    ?? draft.download.qbittorrent.seedingLimits;
  const hasUnsavedChanges = persistedSettings ? !areSettingsEqual(draft, persistedSettings) : false;

  return (
    <div className={cn("flex min-w-0 flex-col gap-6", hasUnsavedChanges && "pb-20")}>
      <div className="sticky top-[var(--app-mobile-header-height)] z-20 -mx-4 border-b bg-background px-4 md:top-0 md:-mx-5 md:px-5 xl:-mx-6 xl:px-6">
        <header className="py-3">
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-2">
              <h1 className="text-xl font-semibold">设置</h1>
              {hasUnsavedChanges && (
                <Badge className="gap-1.5 rounded-full" tone="primary-soft">
                  <span aria-hidden="true" className="size-1.5 rounded-full bg-primary" />
                  有未保存修改
                </Badge>
              )}
            </div>
            <p className="mt-1 text-sm text-muted-foreground">
              {remoteRuntime
                ? "远程播放偏好与 PC 宿主配置集中管理。"
                : "目录、下载引擎、播放器和自动化规则集中管理。"}
            </p>
          </div>
        </header>
        <div className="pb-3 lg:hidden">
          <SettingsCategorySelect
            activeCategory={activeCategory}
            categories={visibleSettingsCategories}
            onNavigate={navigateToCategory}
          />
        </div>
      </div>

      <div className="grid min-w-0 items-start gap-6 lg:grid-cols-[15rem_minmax(0,1fr)]">
        <SettingsCategoryNavigation
          activeCategory={activeCategory}
          categories={visibleSettingsCategories}
          onNavigate={navigateToCategory}
        />

        <div className="flex min-w-0 flex-col gap-12">
          <SettingsCategory
            action={(
              <Button
                className="w-full sm:w-auto"
                variant="outline"
                onClick={() => setResetDialogOpen(true)}
                disabled={resetState === "resetting" || saveState === "saving"}
              >
                <RotateCcw data-icon="inline-start" />
                {resetState === "resetting"
                  ? "恢复中"
                  : resetState === "reset"
                    ? "已恢复"
                    : remoteRuntime ? "恢复外观默认" : "恢复默认"}
              </Button>
            )}
            description="明暗模式、主题预设与导入的用户主题包。"
            id="appearance"
            title="外观"
          >
            <AppearanceSettingsSection
              appearance={draft.appearance}
              onChange={(appearance) => setDraft({ ...draft, appearance })}
            />
          </SettingsCategory>

          <SettingsCategory
            description={remoteRuntime
              ? "管理 PC 宿主的默认下载目录和未完成目录。"
              : "默认下载目录、未完成目录和应用用户数据位置。"}
            id="storage"
            title="存储与目录"
          >
            <div className="flex flex-col gap-5">
        <SettingsSection title="下载目录" description="支持全局默认目录，后续单部番可以覆盖。">
          <div className="flex flex-col gap-4">
            <ToggleSetting
              icon={<FolderCog className="h-4 w-4" />}
              label="创建番剧目录"
              description="按目录模板为每部追番生成独立保存目录。"
              checked={draft.download.createAnimeFolder}
              onChange={(value) =>
                setDraft({
                  ...draft,
                  download: { ...draft.download, createAnimeFolder: value }
                })
              }
            />
            <TextSetting
              icon={<FolderCog className="h-4 w-4" />}
              label="默认下载目录"
              value={draft.download.defaultDownloadDir}
              onChange={(value) =>
                setDraft({
                  ...draft,
                  download: {
                    ...draft.download,
                    defaultDownloadDir: value
                  }
                })
              }
            />
            <TextSetting
              label="临时下载目录"
              value={draft.download.temporaryDownloadDir ?? ""}
              onChange={(value) =>
                setDraft({
                  ...draft,
                  download: {
                    ...draft.download,
                    temporaryDownloadDir: value
                  }
                })
              }
            />
            <TextSetting
              label="番剧目录模板（{year}、{month}、{title}、{originalTitle}）"
              value={draft.download.animeFolderPattern}
              onChange={(value) =>
                setDraft({
                  ...draft,
                  download: {
                    ...draft.download,
                    animeFolderPattern: value
                  }
                })
              }
            />
          </div>
        </SettingsSection>

        {!mobileRuntime && !remoteRuntime && (
          <SettingsSection
            title="本地媒体库"
            description="扫描本机番剧目录、确认匹配结果并维护媒体可用状态。"
          >
            <LocalMediaLibrarySettingsSection />
          </SettingsSection>
        )}

        {!remoteRuntime && (
        <SettingsSection title="用户数据" description="数据库、缓存、日志和备份都应随用户数据目录迁移。">
          <div className="flex flex-col gap-4">
            <TextSetting
              icon={<HardDrive className="h-4 w-4" />}
              label="用户数据目录"
              value={draft.storage.userDataDir}
              disabled={mobileRuntime}
              onChange={(value) =>
                setDraft({
                  ...draft,
                  storage: {
                    ...draft.storage,
                    userDataDir: value
                  }
                })
              }
            />
            <SettingRow label="数据库" value={draft.storage.databasePath} />
            <SettingRow label="缓存" value={draft.storage.cacheDir} />
            <SettingRow label="日志" value={draft.storage.logDir} />
            {(appApi.exportDatabaseBackup || appApi.restoreDatabaseBackup || appApi.exportLogs) && (
              <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap">
                {appApi.exportDatabaseBackup && (
                  <Button
                    disabled={backupAction !== "idle"}
                    onClick={() => void exportDatabaseBackup()}
                    type="button"
                    variant="outline"
                  >
                    <Save data-icon="inline-start" />
                    {backupAction === "exporting" ? "导出中" : "导出数据备份"}
                  </Button>
                )}
                {appApi.restoreDatabaseBackup && (
                  <Button
                    disabled={backupAction !== "idle"}
                    onClick={() => setRestoreDialogOpen(true)}
                    type="button"
                    variant="outline"
                  >
                    <RotateCcw data-icon="inline-start" />
                    {backupAction === "restoring" ? "恢复中" : "恢复数据备份"}
                  </Button>
                )}
                {appApi.exportLogs && (
                  <Button
                    disabled={logExporting}
                    onClick={() => void exportLogs()}
                    type="button"
                    variant="outline"
                  >
                    <Download data-icon="inline-start" />
                    {logExporting ? "导出中" : "导出日志"}
                  </Button>
                )}
              </div>
            )}
          </div>
        </SettingsSection>
        )}
      </div>

          </SettingsCategory>

          <SettingsCategory
            description={mobileRuntime || remoteRuntime
              ? "界面语言和番剧标题显示规则。"
              : "语言、标题显示规则和桌面端后台行为。"}
            id="interface"
            title={mobileRuntime || remoteRuntime ? "语言" : "语言与桌面集成"}
          >
            <div className="flex flex-col gap-5">

      <SettingsSection title="语言与标题" description="界面语言保持固定，番剧元数据按当前标题策略展示和检索。">
        <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
          <SettingRow icon={<Languages className="h-4 w-4" />} label="界面语言" value="简体中文" />
          <SettingRow label="标题显示" value="中文优先，副标题显示原名" />
          <SettingRow label="搜索名称" value="标题、原名、罗马音、英文名和自定义别名" />
        </div>
      </SettingsSection>

      {!mobileRuntime && !remoteRuntime && (
      <SettingsSection title="桌面集成" description="控制后台运行、系统登录启动等本地桌面行为。">
        <div className="grid gap-4 lg:grid-cols-2">
          <ToggleSetting
            icon={<Monitor className="h-4 w-4" />}
            label="关闭到托盘"
            description="关闭主窗口后继续保留后台扫描和提醒。"
            checked={draft.desktop.minimizeToTray}
            onChange={(value) =>
              setDraft({
                ...draft,
                desktop: {
                  ...draft.desktop,
                  minimizeToTray: value
                }
              })
            }
          />
          <ToggleSetting
            icon={<Power className="h-4 w-4" />}
            label="开机启动"
            description="系统登录后自动启动 Ani Tracker。"
            checked={draft.desktop.launchAtLogin}
            onChange={(value) =>
              setDraft({
                ...draft,
                desktop: {
                  ...draft.desktop,
                  launchAtLogin: value
                }
              })
            }
          />
        </div>
      </SettingsSection>
      )}
            </div>
          </SettingsCategory>

          {capabilities.remoteGateway && !remoteRuntime && (
          <SettingsCategory
            description="局域网 HTTPS、一次性配对码和已配对设备的访问范围。"
            id="remote"
            title="远程设备"
          >
      <SettingsSection title="远程服务与设备" description="管理通过一次性配对码登记的浏览器和移动设备。">
        <div className="flex flex-col gap-4">
          <div className="grid gap-4 lg:grid-cols-2">
            <ToggleSetting
              icon={<Smartphone />}
              label="局域网 HTTPS"
              description="桌面新安装默认开启，仅允许本机回环和当前网卡内网地址；不会开放裸 HTTP 或公网映射。"
              checked={draft.network.remoteAccess.lanEnabled}
              onChange={(value) =>
                setDraft({
                  ...draft,
                  network: {
                    ...draft.network,
                    remoteAccess: {
                      ...draft.network.remoteAccess,
                      lanEnabled: value
                    }
                  }
                })
              }
            />
            <NumberSetting
              label="远程服务端口"
              value={draft.network.remoteAccess.port}
              min={1024}
              max={65_535}
              onChange={(value) =>
                setDraft({
                  ...draft,
                  network: {
                    ...draft.network,
                    remoteAccess: {
                      ...draft.network.remoteAccess,
                      port: value
                    }
                  }
                })
              }
            />
          </div>

          <Alert>
            <Monitor />
            <AlertTitle>{remoteStatus?.lanEnabled ? "局域网 HTTPS 已开启" : "当前仅开放本机回环访问"}</AlertTitle>
            <AlertDescription>
              {remoteStatus?.lanEnabled
                ? "首次连接前需在移动设备中信任 Ani Tracker 本地 CA；桌面端仅持久化令牌摘要，可随时吊销设备。"
                : "启用局域网 HTTPS 并保存后，移动设备才能通过同一私有网络访问；已配对设备会在重启后保留。"}
            </AlertDescription>
          </Alert>

          {remoteStatus?.lastError && (
            <Alert variant="destructive">
              <Unplug />
              <AlertTitle>远程服务启动失败</AlertTitle>
              <AlertDescription>{remoteStatus.lastError}</AlertDescription>
            </Alert>
          )}

          {remoteError && (
            <Alert variant="destructive">
              <Unplug />
              <AlertTitle>远程服务操作失败</AlertTitle>
              <AlertDescription>{remoteError}</AlertDescription>
            </Alert>
          )}

          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="flex flex-wrap items-center gap-2">
              <Badge tone={remoteStatus?.running ? "green" : "amber"}>
                {remoteStatus?.running ? "服务运行中" : "服务未运行"}
              </Badge>
              <span className="break-all text-sm text-muted-foreground">
                {remoteStatus?.baseUrl ?? "正在读取服务地址"}
              </span>
            </div>
            <div className="flex flex-wrap gap-2">
              <Button
                variant="outline"
                onClick={() => void refreshRemoteStatus()}
                disabled={remoteAction !== "idle"}
              >
                <RefreshCw data-icon="inline-start" />
                刷新状态
              </Button>
              <Button onClick={() => void createRemotePairingCode()} disabled={!remoteStatus?.running || remoteAction !== "idle"}>
                <KeyRound data-icon="inline-start" />
                生成配对码
              </Button>
            </div>
          </div>

          {remoteStatus?.lanEnabled && (
            <div className="flex flex-col gap-3">
              <div className="text-sm font-medium">局域网访问地址</div>
              <div className="flex flex-wrap gap-2">
                {remoteStatus.addresses.map((address) => (
                  <Badge key={address}>https://{address}:{remoteStatus.port}</Badge>
                ))}
              </div>
              {remoteStatus.certificate && (
                <div className="flex flex-col gap-1 text-xs text-muted-foreground">
                  <div className="flex items-start gap-2">
                    <span className="min-w-0 flex-1 break-all">CA 下载：{remoteStatus.baseUrl}/ani-tracker-ca.crt</span>
                    <Button
                      aria-label="复制 CA 下载地址"
                      variant="ghost"
                      className="size-11 shrink-0 p-0 md:size-9"
                      onClick={() => void copyAuthorityCertificateUrl()}
                    >
                      <Copy />
                    </Button>
                  </div>
                  <span className="break-all">证书指纹：{remoteStatus.certificate.fingerprint}</span>
                  <span>证书到期：{formatDateTime(remoteStatus.certificate.expiresAt)}</span>
                </div>
              )}
            </div>
          )}

          {remotePairing && (
            <Alert>
              <KeyRound />
              <AlertTitle>一次性配对码：{remotePairing.code}</AlertTitle>
              <AlertDescription>有效期至 {formatDateTime(remotePairing.expiresAt)}，使用后立即失效。</AlertDescription>
            </Alert>
          )}

          {remoteStatus && remoteStatus.devices.length > 0 ? (
            <div className="grid gap-3 md:grid-cols-2">
              {remoteStatus.devices.map((device) => (
                <Card key={device.id}>
                  <CardHeader>
                    <CardTitle className="flex items-center gap-2">
                      <Smartphone />
                      {device.name}
                    </CardTitle>
                    <CardDescription>
                      配对于 {formatDateTime(device.createdAt)} · 最近访问 {formatDateTime(device.lastAccessedAt ?? undefined)}
                    </CardDescription>
                  </CardHeader>
                  <CardContent className="flex flex-wrap gap-2">
                    {device.scopes.map((scope) => <Badge key={scope}>{scope}</Badge>)}
                  </CardContent>
                  <CardFooter>
                    <Button
                      variant="outline"
                      onClick={() => void revokeRemoteDevice(device.id)}
                      disabled={remoteAction !== "idle"}
                    >
                      <Unplug data-icon="inline-start" />
                      {revokingDeviceId === device.id ? "正在吊销" : "吊销设备"}
                    </Button>
                  </CardFooter>
                </Card>
              ))}
            </div>
          ) : (
            <Alert>
              <Smartphone />
              <AlertTitle>暂无已配对设备</AlertTitle>
              <AlertDescription>生成配对码后，在同源 PWA 配对页登记设备。</AlertDescription>
            </Alert>
          )}
        </div>
      </SettingsSection>

          </SettingsCategory>
          )}

          <SettingsCategory
            description={remoteRuntime
              ? "当前设备的播放模式和字幕显示偏好。"
              : mobileRuntime
                ? "移动端统一使用应用内置 libVLC。"
                : "默认播放器、可执行文件路径与媒体文件扫描参数。"}
            id="media"
            title={remoteRuntime ? "远程播放" : "播放器与媒体"}
          >
            <div className="flex flex-col gap-5">

      {remoteRuntime ? (
      <SettingsSection title="远程播放配置" description="配置只保存在当前浏览器，不修改 PC 播放器实现。">
        <RemotePlaybackSettingsSection />
      </SettingsSection>
      ) : capabilities.externalPlayerConfiguration ? (
      <SettingsSection title="播放器配置" description="按当前操作系统提供播放器选项。">
        <div className="flex flex-col gap-4">
          <div className="grid gap-4 md:grid-cols-[minmax(0,1fr)_auto] md:items-end">
            <Field>
              <FieldLabel htmlFor="default-player">默认播放器</FieldLabel>
              <Select
                value={selectedPlayerId}
                onValueChange={(value) => setDraft({ ...draft, defaultPlayerProfileId: value })}
              >
                <SelectTrigger id="default-player">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    <SelectItem value="auto">自动</SelectItem>
                    <SelectItem value={BUILTIN_PLAYER_PROFILE_ID}>内置</SelectItem>
                    {playerOptions.map((player) => (
                      <SelectItem key={player.id} value={player.id}>{player.name}</SelectItem>
                    ))}
                  </SelectGroup>
                </SelectContent>
              </Select>
            </Field>
            <Button
              variant="outline"
              onClick={() => void refreshPlayerDetection(draft.players, true)}
              disabled={playerDetectionState === "detecting"}
            >
              <RefreshCw data-icon="inline-start" />
              {playerDetectionState === "detecting" ? "探测中" : "重新探测"}
            </Button>
          </div>

          {selectedPlayerId === "auto" ? (
            <Alert>
              <PlayCircle />
              <AlertTitle>{autoCandidate ? `自动选择：${autoCandidate.name}` : "未探测到可用播放器"}</AlertTitle>
              <AlertDescription className="break-all">
                {autoCandidate?.resolvedPath ?? "请安装播放器，或选择具体播放器并设置可执行文件路径。"}
              </AlertDescription>
            </Alert>
          ) : selectedPlayer ? (
            <Field data-invalid={Boolean(playerDetection && !selectedCandidate?.available)}>
              <FieldLabel htmlFor="player-executable-path">可执行文件路径</FieldLabel>
              <InputGroup>
                <InputGroupInput
                  id="player-executable-path"
                  value={selectedPlayer.executablePath}
                  aria-invalid={Boolean(playerDetection && !selectedCandidate?.available)}
                  onChange={(event) => updatePlayerPath(selectedPlayer.id, event.target.value)}
                />
                <InputGroupAddon>
                  <InputGroupButton
                    onClick={() => void selectPlayerExecutable(selectedPlayer.id)}
                    aria-label="选择播放器可执行文件"
                    title="选择播放器可执行文件"
                  >
                    <FolderOpen />
                  </InputGroupButton>
                </InputGroupAddon>
              </InputGroup>
              <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
                <Badge tone={selectedCandidate?.available ? "green" : selectedCandidate ? "amber" : "neutral"}>
                  {selectedCandidate?.available ? "路径可用" : selectedCandidate ? "路径不可用" : "待探测"}
                </Badge>
                {selectedCandidate?.resolvedPath && <span className="break-all">{selectedCandidate.resolvedPath}</span>}
              </div>
            </Field>
          ) : null}

          {playerDetectionError && (
            <Alert variant="destructive">
              <PlayCircle />
              <AlertTitle>播放器探测失败</AlertTitle>
              <AlertDescription>{playerDetectionError}</AlertDescription>
            </Alert>
          )}
        </div>
      </SettingsSection>
      ) : (
      <SettingsSection title="播放器配置" description="本地文件和网络媒体均由平台原生 libVLC 播放器处理。">
        <Alert>
          <PlayCircle />
          <AlertTitle>应用内置 libVLC</AlertTitle>
          <AlertDescription>播放器随应用提供，不使用外部可执行文件路径。</AlertDescription>
        </Alert>
      </SettingsSection>
      )}

      {capabilities.mediaScan && !remoteRuntime && (
      <SettingsSection title="媒体探测" description="用于读取已下载视频的编码、分辨率、音轨和字幕轨。">
        <div className="grid gap-4 md:grid-cols-[minmax(0,1fr)_180px]">
          <TextSetting
            icon={<FileSearch className="h-4 w-4" />}
            label="ffprobe 路径"
            value={draft.media.ffprobePath}
            onChange={(value) =>
              setDraft({
                ...draft,
                media: {
                  ...draft.media,
                  ffprobePath: value
                }
              })
            }
          />
          <NumberSetting
            label="探测超时"
            value={draft.media.ffprobeTimeoutSeconds}
            suffix="秒"
            min={3}
            onChange={(value) =>
              setDraft({
                ...draft,
                media: {
                  ...draft.media,
                  ffprobeTimeoutSeconds: value
                }
              })
            }
          />
        </div>
        <div className="mt-4">
          <TextSetting
            label="视频扩展名"
            value={draft.media.videoExtensions.join(", ")}
            onChange={(value) =>
              setDraft({
                ...draft,
                media: {
                  ...draft.media,
                  videoExtensions: parseExtensions(value)
                }
              })
            }
          />
        </div>
      </SettingsSection>
      )}

            </div>
          </SettingsCategory>

          <SettingsCategory
            description="内置 libtorrent、qBittorrent 连接、速率限制、做种策略和核心状态。"
            id="download"
            title="下载核心"
          >

      <Card className="overflow-hidden shadow-none">
        <CardHeader className="gap-3 border-b bg-muted/50 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex min-w-0 flex-col gap-1.5">
            <CardTitle><h3>引擎配置</h3></CardTitle>
            <CardDescription>
              {torrentEngineMode === "embedded"
                ? "应用托管本地 libtorrent 核心。"
                : torrentEngineMode === "managed"
                  ? "应用托管 qBittorrent-nox。"
                  : "连接外部 qBittorrent WebUI。"}
            </CardDescription>
          </div>
          {hostExternalQbittorrent && <ToggleGroup
            aria-label="下载引擎"
            className={cn("grid w-full shrink-0 sm:w-auto", hostManagedQbittorrent ? "grid-cols-3" : "grid-cols-2")}
            onValueChange={(value) => value && updateTorrentEngineMode(value as "embedded" | "managed" | "external")}
            type="single"
            value={torrentEngineMode}
            variant="outline"
          >
            <ToggleGroupItem className="h-auto min-h-9 whitespace-normal px-2" value="embedded">
              内置引擎
            </ToggleGroupItem>
            {hostManagedQbittorrent && (
              <ToggleGroupItem className="h-auto min-h-9 whitespace-normal px-2" value="managed">
                内置 qBittorrent-nox
              </ToggleGroupItem>
            )}
            <ToggleGroupItem className="h-auto min-h-9 whitespace-normal px-2" value="external">
              外部 qBittorrent WebUI
            </ToggleGroupItem>
          </ToggleGroup>}
        </CardHeader>
        <CardContent className="pt-4 sm:pt-5">
          <div className="grid min-w-0 gap-5 lg:grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)]">
            <FieldGroup className="gap-4">
              {torrentEngineMode === "embedded" ? (
                <>
                  <Field data-disabled>
                    <FieldLabel htmlFor="embedded-webui-address">WebUI 地址</FieldLabel>
                    <Input disabled id="embedded-webui-address" readOnly value="无（本地 IPC）" />
                  </Field>
                  <div className="grid gap-4 sm:grid-cols-2">
                    <Field data-disabled>
                      <FieldLabel htmlFor="embedded-webui-username">用户名</FieldLabel>
                      <Input disabled id="embedded-webui-username" readOnly value="不适用" />
                    </Field>
                    <Field data-disabled>
                      <FieldLabel htmlFor="embedded-webui-password">密码</FieldLabel>
                      <Input disabled id="embedded-webui-password" readOnly value="不适用" />
                    </Field>
                  </div>
                  <div className="grid gap-4 sm:grid-cols-2">
                    <NumberSetting
                      label="监听端口"
                      max={65535}
                      min={1024}
                      onChange={(value) => setDraft({
                        ...draft,
                        download: {
                          ...draft.download,
                          embedded: { ...draft.download.embedded, listenPort: value }
                        }
                      })}
                      value={draft.download.embedded.listenPort ?? 51413}
                    />
                    <NumberSetting
                      label="活动下载数"
                      max={100}
                      min={1}
                      onChange={(value) => setDraft({
                        ...draft,
                        download: {
                          ...draft.download,
                          embedded: { ...draft.download.embedded, maxActiveDownloads: value }
                        }
                      })}
                      value={draft.download.embedded.maxActiveDownloads ?? 3}
                    />
                  </div>
                  <Field orientation="horizontal">
                    <FieldLabel className="cursor-pointer" htmlFor="embedded-dht">DHT 与本地发现</FieldLabel>
                    <Switch
                      checked={draft.download.embedded.dhtEnabled ?? true}
                      id="embedded-dht"
                      onCheckedChange={(value) => setDraft({
                        ...draft,
                        download: {
                          ...draft.download,
                          embedded: { ...draft.download.embedded, dhtEnabled: value }
                        }
                      })}
                    />
                  </Field>
                  <Field orientation="horizontal">
                    <FieldLabel className="cursor-pointer" htmlFor="embedded-upnp">UPnP 与 NAT-PMP</FieldLabel>
                    <Switch
                      checked={draft.download.embedded.upnpEnabled ?? true}
                      id="embedded-upnp"
                      onCheckedChange={(value) => setDraft({
                        ...draft,
                        download: {
                          ...draft.download,
                          embedded: { ...draft.download.embedded, upnpEnabled: value }
                        }
                      })}
                    />
                  </Field>
                </>
              ) : (
                <>
                  <TextSetting
                    label="WebUI 地址"
                    value={draft.download.qbittorrent.baseUrl}
                    onChange={(value) => setDraft({
                      ...draft,
                      download: {
                        ...draft.download,
                        qbittorrent: { ...draft.download.qbittorrent, baseUrl: value }
                      }
                    })}
                  />
                  <div className="grid gap-4 sm:grid-cols-2">
                    <TextSetting
                      label="用户名"
                      value={draft.download.qbittorrent.username}
                      onChange={(value) => setDraft({
                        ...draft,
                        download: {
                          ...draft.download,
                          qbittorrent: { ...draft.download.qbittorrent, username: value }
                        }
                      })}
                    />
                    <TextSetting
                      label="密码"
                      type="password"
                      value={draft.download.qbittorrent.password ?? ""}
                      onChange={(value) => setDraft({
                        ...draft,
                        download: {
                          ...draft.download,
                          qbittorrent: { ...draft.download.qbittorrent, password: value }
                        }
                      })}
                    />
                  </div>
                  <Field orientation="horizontal">
                    <FieldLabel>运行模式</FieldLabel>
                    <span className="text-right text-sm text-muted-foreground">
                      {torrentEngineMode === "managed" ? "内置 qBittorrent-nox" : "外部 WebUI"}
                    </span>
                  </Field>
                  {hostManagedQbittorrent && (
                  <Field data-disabled={torrentEngineMode !== "managed"} orientation="horizontal">
                    <FieldLabel htmlFor="qbittorrent-auto-start">随应用启动</FieldLabel>
                    <Switch
                      checked={torrentEngineMode === "managed" && draft.download.qbittorrent.autoConnect}
                      disabled={torrentEngineMode !== "managed"}
                      id="qbittorrent-auto-start"
                      onCheckedChange={(value) => setDraft({
                        ...draft,
                        download: {
                          ...draft.download,
                          qbittorrent: { ...draft.download.qbittorrent, autoConnect: value }
                        }
                      })}
                    />
                  </Field>
                  )}
                  <Button onClick={() => void testQbittorrent()} disabled={qbTest.state === "testing"}>
                    {qbTest.state === "testing" ? "测试并保存中" : "测试连接并保存"}
                  </Button>
                  {qbTest.message && (
                    <p className={cn(
                      "text-sm",
                      qbTest.state === "error" ? "text-destructive" : "text-muted-foreground"
                    )}>
                      {qbTest.message}
                    </p>
                  )}
                </>
              )}
            </FieldGroup>

            <Separator className="hidden h-full lg:block" orientation="vertical" />
            <Separator className="lg:hidden" />

            <FieldGroup className="gap-4">
              <div>
                <h3 className="font-medium">流量与做种控制</h3>
                <p className="mt-1 text-sm text-muted-foreground">限速为 0 时不限制传输速度。</p>
              </div>
              {mobileRuntime && (
                <Field className="items-center justify-between gap-4" orientation="horizontal">
                  <FieldLabel
                    className="min-w-0 flex-1 cursor-pointer flex-col items-start"
                    htmlFor="allow-metered-downloads"
                  >
                    <span>允许移动网络下载</span>
                    <span className="text-sm font-normal leading-6 text-muted-foreground">
                      关闭后任务等待 Wi-Fi，下载、上传和做种会一起暂停。
                    </span>
                  </FieldLabel>
                  <Switch
                    checked={draft.download.allowMeteredDownloads ?? false}
                    id="allow-metered-downloads"
                    onCheckedChange={(value) => setDraft({
                      ...draft,
                      download: { ...draft.download, allowMeteredDownloads: value }
                    })}
                  />
                </Field>
              )}
              <SpeedLimitSetting
                label="全局下载限制"
                value={torrentEngineMode === "embedded"
                  ? draft.download.embedded.maxDownloadSpeed ?? 0
                  : draft.download.qbittorrent.downloadLimitKiBps ?? 0}
                onChange={(value) => torrentEngineMode === "embedded"
                  ? setDraft({
                    ...draft,
                    download: {
                      ...draft.download,
                      embedded: { ...draft.download.embedded, maxDownloadSpeed: value }
                    }
                  })
                  : setDraft({
                    ...draft,
                    download: {
                      ...draft.download,
                      qbittorrent: { ...draft.download.qbittorrent, downloadLimitKiBps: value }
                    }
                  })}
              />
              <SpeedLimitSetting
                label="全局上传限制"
                value={torrentEngineMode === "embedded"
                  ? draft.download.embedded.maxUploadSpeed ?? 0
                  : draft.download.qbittorrent.uploadLimitKiBps ?? 0}
                onChange={(value) => torrentEngineMode === "embedded"
                  ? setDraft({
                    ...draft,
                    download: {
                      ...draft.download,
                      embedded: { ...draft.download.embedded, maxUploadSpeed: value }
                    }
                  })
                  : setDraft({
                    ...draft,
                    download: {
                      ...draft.download,
                      qbittorrent: { ...draft.download.qbittorrent, uploadLimitKiBps: value }
                    }
                  })}
              />
              <Field orientation="horizontal">
                <FieldLabel className="cursor-pointer" htmlFor="torrent-seeding-limits">启用做种限制</FieldLabel>
                <Switch
                  checked={torrentEngineMode === "embedded"
                    ? embeddedSeedingLimits.enabled
                    : draft.download.qbittorrent.seedingLimits.enabled}
                  id="torrent-seeding-limits"
                  onCheckedChange={(value) => torrentEngineMode === "embedded"
                    ? setDraft({
                      ...draft,
                      download: {
                        ...draft.download,
                        embedded: {
                          ...draft.download.embedded,
                          seedingLimits: {
                            ...embeddedSeedingLimits,
                            enabled: value,
                            ratioEnabled: value,
                            timeEnabled: value
                          }
                        }
                      }
                    })
                    : setDraft({
                      ...draft,
                      download: {
                        ...draft.download,
                        qbittorrent: {
                          ...draft.download.qbittorrent,
                          seedingLimits: {
                            ...draft.download.qbittorrent.seedingLimits,
                            enabled: value,
                            ratioEnabled: value,
                            timeEnabled: value
                          }
                        }
                      }
                    })}
                />
              </Field>
              <div className="grid gap-4 sm:grid-cols-2">
                <NumberSetting
                  disabled={torrentEngineMode === "embedded"
                    ? !embeddedSeedingLimits.enabled
                    : !draft.download.qbittorrent.seedingLimits.enabled}
                  label="分享率"
                  min={0.1}
                  onChange={(value) => torrentEngineMode === "embedded"
                    ? setDraft({
                      ...draft,
                      download: {
                        ...draft.download,
                        embedded: {
                          ...draft.download.embedded,
                          seedingLimits: { ...embeddedSeedingLimits, ratioEnabled: true, ratioLimit: value }
                        }
                      }
                    })
                    : setDraft({
                      ...draft,
                      download: {
                        ...draft.download,
                        qbittorrent: {
                          ...draft.download.qbittorrent,
                          seedingLimits: {
                            ...draft.download.qbittorrent.seedingLimits,
                            ratioEnabled: true,
                            ratioLimit: value
                          }
                        }
                      }
                    })}
                  step={0.1}
                  suffix="倍"
                  value={torrentEngineMode === "embedded"
                    ? embeddedSeedingLimits.ratioLimit
                    : draft.download.qbittorrent.seedingLimits.ratioLimit}
                />
                <NumberSetting
                  disabled={torrentEngineMode === "embedded"
                    ? !embeddedSeedingLimits.enabled
                    : !draft.download.qbittorrent.seedingLimits.enabled}
                  label="做种时间"
                  min={1}
                  onChange={(value) => torrentEngineMode === "embedded"
                    ? setDraft({
                      ...draft,
                      download: {
                        ...draft.download,
                        embedded: {
                          ...draft.download.embedded,
                          seedingLimits: { ...embeddedSeedingLimits, timeEnabled: true, timeLimitMinutes: value }
                        }
                      }
                    })
                    : setDraft({
                      ...draft,
                      download: {
                        ...draft.download,
                        qbittorrent: {
                          ...draft.download.qbittorrent,
                          seedingLimits: {
                            ...draft.download.qbittorrent.seedingLimits,
                            timeEnabled: true,
                            timeLimitMinutes: value
                          }
                        }
                      }
                    })}
                  suffix="分钟"
                  value={torrentEngineMode === "embedded"
                    ? embeddedSeedingLimits.timeLimitMinutes
                    : draft.download.qbittorrent.seedingLimits.timeLimitMinutes}
                />
              </div>
            </FieldGroup>
          </div>

          {torrentEngineMode === "embedded" && embeddedError && (
            <Alert className="mt-5" variant="destructive">
              <AlertTitle>内置核心异常</AlertTitle>
              <AlertDescription className="break-all">{embeddedError}</AlertDescription>
            </Alert>
          )}
          {torrentEngineMode === "managed" && qbManagedStatus?.lastError && (
            <Alert className="mt-5" variant="destructive">
              <AlertTitle>内置进程异常</AlertTitle>
              <AlertDescription className="break-all">{qbManagedStatus.lastError}</AlertDescription>
            </Alert>
          )}
        </CardContent>

        {torrentEngineMode === "embedded" && (
          <CardFooter className="flex-col justify-between gap-3 border-t bg-muted/30 pt-4 sm:flex-row sm:pt-5">
            <div className="flex min-w-0 flex-col gap-2 text-xs text-muted-foreground">
              <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
                <Badge tone={embeddedStatus?.running ? "green" : "neutral"}>
                  {embeddedStatus?.running ? "运行中" : "未运行"}
                </Badge>
                <span>版本: {embeddedStatus?.version ?? "--"}</span>
                <span>PID: {embeddedStatus?.pid ?? "--"}</span>
                <span>架构: {embeddedStatus?.arch ?? "--"}</span>
                <span>任务: {embeddedStatus?.taskCount ?? "--"}</span>
              </div>
              <span className="break-all">数据目录: {embeddedStatus?.dataDir ?? "--"}</span>
            </div>
            <div className="flex w-full shrink-0 gap-2 sm:w-auto">
              <Button
                className="flex-1 sm:flex-none"
                disabled={embeddedAction !== "idle"}
                onClick={() => void (embeddedStatus?.running ? stopEmbeddedTorrent() : startEmbeddedTorrent())}
                variant="outline"
              >
                <Power data-icon="inline-start" />
                {embeddedAction === "starting"
                  ? "启动中"
                  : embeddedAction === "stopping"
                    ? "停止中"
                    : embeddedStatus?.running ? "停止核心" : "启动核心"}
              </Button>
              <Button
                className="flex-1 sm:flex-none"
                disabled={embeddedAction !== "idle"}
                onClick={() => void restartEmbeddedTorrent()}
              >
                <RefreshCw data-icon="inline-start" />
                {embeddedAction === "restarting" ? "重启中" : "重启核心"}
              </Button>
            </div>
          </CardFooter>
        )}

        {torrentEngineMode === "managed" && (
          <CardFooter className="flex-col justify-between gap-3 border-t bg-muted/30 pt-4 sm:flex-row sm:pt-5">
            <div className="flex min-w-0 flex-wrap items-center gap-x-4 gap-y-2 text-xs text-muted-foreground">
              <Badge tone={qbManagedStatus?.running || qbConnectionState === "online" ? "green" : "neutral"}>
                {qbManagedStatus?.running
                  ? "应用进程运行中"
                  : qbConnectionState === "online"
                    ? "WebUI 已连接"
                    : qbConnectionState === "testing"
                      ? "检测中"
                      : "未运行"}
              </Badge>
              <span>PID: {qbManagedStatus?.pid ?? "--"}</span>
              <span>架构: {qbManagedStatus?.arch ?? "--"}</span>
              <Button className="h-auto min-h-0 min-w-0 px-0 text-xs" onClick={() => void openQbittorrentWebUi()} variant="ghost">
                <ExternalLink data-icon="inline-start" />
                <span className="truncate underline underline-offset-4">
                  {qbManagedStatus?.webUiUrl || draft.download.qbittorrent.baseUrl}
                </span>
              </Button>
            </div>
            <div className="flex w-full shrink-0 gap-2 sm:w-auto">
              <Button
                className="flex-1 sm:flex-none"
                disabled={qbManagedAction !== "idle"}
                onClick={() => void (qbManagedStatus?.running ? stopQbittorrentManaged() : startQbittorrentManaged())}
                variant="outline"
              >
                <Power data-icon="inline-start" />
                {qbManagedAction === "starting"
                  ? "启动中"
                  : qbManagedAction === "stopping"
                    ? "停止中"
                    : qbManagedStatus?.running ? "停止服务" : "启动服务"}
              </Button>
              <Button
                className="flex-1 sm:flex-none"
                disabled={qbManagedAction !== "idle"}
                onClick={() => void restartQbittorrentManaged()}
              >
                <RefreshCw data-icon="inline-start" />
                {qbManagedAction === "restarting" ? "重启中" : "重启服务"}
              </Button>
            </div>
          </CardFooter>
        )}
      </Card>

          </SettingsCategory>

          <SettingsCategory
            description="扫描节奏、新集通知、自动下载和默认字幕组缺失策略。"
            id="automation"
            title="自动化"
          >

      <Card className="overflow-hidden shadow-none">
        <CardContent className="pt-4 sm:pt-5">
          <h3 className="sr-only">扫描与下载规则</h3>
          <FieldGroup className="grid grid-cols-[repeat(auto-fit,minmax(min(100%,18rem),1fr))] gap-x-12 gap-y-5">
            <FieldGroup className="gap-5">
              <Field className="items-center justify-between" orientation="horizontal">
                <FieldLabel className="cursor-pointer" htmlFor="automation-scheduled-check">定时扫描</FieldLabel>
                <Switch
                  checked={draft.automation.scheduledCheckEnabled}
                  id="automation-scheduled-check"
                  onCheckedChange={(checked) =>
                    setDraft({
                      ...draft,
                      automation: {
                        ...draft.automation,
                        scheduledCheckEnabled: checked
                      }
                    })
                  }
                />
              </Field>
              <Field className="items-center justify-between" orientation="horizontal">
                <FieldLabel htmlFor="automation-check-interval">扫描间隔</FieldLabel>
                <div className="flex min-w-0 items-center gap-2">
                  <Input
                    className="w-24 text-right tabular-nums"
                    id="automation-check-interval"
                    min={5}
                    onChange={(event) =>
                      setDraft({
                        ...draft,
                        automation: {
                          ...draft.automation,
                          checkIntervalMinutes: Number(event.target.value)
                        }
                      })
                    }
                    type="number"
                    value={draft.automation.checkIntervalMinutes}
                  />
                  <span className="shrink-0 text-sm text-muted-foreground">分钟</span>
                </div>
              </Field>
              <Field className="items-center justify-between" orientation="horizontal">
                <FieldLabel htmlFor="automation-fansub-fallback">默认字幕组缺失</FieldLabel>
                <Select
                  value={draft.automation.fallbackWhenDefaultFansubMissing}
                  onValueChange={(value) =>
                    setDraft({
                      ...draft,
                      automation: {
                        ...draft.automation,
                        fallbackWhenDefaultFansubMissing: value as AppSettings["automation"]["fallbackWhenDefaultFansubMissing"]
                      }
                    })
                  }
                >
                  <SelectTrigger className="w-40" id="automation-fansub-fallback">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectGroup>
                      <SelectItem value="wait">等待</SelectItem>
                      <SelectItem value="candidate">候补字幕组</SelectItem>
                      <SelectItem value="notify_only">只提醒</SelectItem>
                    </SelectGroup>
                  </SelectContent>
                </Select>
              </Field>
            </FieldGroup>
            <FieldGroup className="gap-5">
              <Field className="items-center justify-between" orientation="horizontal">
                <FieldLabel className="cursor-pointer" htmlFor="automation-auto-download">全局自动下载</FieldLabel>
                <Switch
                  checked={draft.automation.autoDownloadEnabledGlobally}
                  id="automation-auto-download"
                  onCheckedChange={(checked) =>
                    setDraft({
                      ...draft,
                      automation: {
                        ...draft.automation,
                        autoDownloadEnabledGlobally: checked
                      }
                    })
                  }
                />
              </Field>
              <Field className="items-center justify-between" orientation="horizontal">
                <FieldLabel className="cursor-pointer" htmlFor="automation-new-episode-notification">新集提醒</FieldLabel>
                <Switch
                  checked={draft.automation.notifyOnNewEpisode}
                  id="automation-new-episode-notification"
                  onCheckedChange={(checked) =>
                    setDraft({
                      ...draft,
                      automation: {
                        ...draft.automation,
                        notifyOnNewEpisode: checked
                      }
                    })
                  }
                />
              </Field>
              {mobileRuntime && (
                <Field className="items-center justify-between gap-3" orientation="horizontal">
                  <FieldLabel>系统通知权限</FieldLabel>
                  <div className="flex min-w-0 items-center gap-2">
                    <Badge>{formatNotificationPermission(notificationPermission)}</Badge>
                    {notificationPermission !== "granted" && (
                      <Button
                        disabled={requestingNotificationPermission}
                        onClick={() => void requestNotificationPermission()}
                        size="compact"
                        type="button"
                        variant="outline"
                      >
                        <Bell data-icon="inline-start" />
                        {requestingNotificationPermission ? "请求中" : "允许"}
                      </Button>
                    )}
                  </div>
                </Field>
              )}
              {draft.automation.fallbackWhenDefaultFansubMissing === "candidate" && (
                <Field>
                  <FieldLabel className="sr-only" htmlFor="automation-candidate-fansubs">候补字幕组</FieldLabel>
                  <CandidateFansubMultiSelect
                    id="automation-candidate-fansubs"
                    onChange={(candidateFansubNames) =>
                      setDraft({
                        ...draft,
                        automation: {
                          ...draft.automation,
                          candidateFansubNames
                        }
                      })
                    }
                    value={draft.automation.candidateFansubNames}
                  />
                </Field>
              )}
            </FieldGroup>
          </FieldGroup>
        </CardContent>
        <CardFooter className="grid grid-cols-[repeat(auto-fit,minmax(min(100%,14rem),1fr))] items-start gap-x-6 gap-y-3 border-t bg-muted/50 pt-4 sm:pt-5">
          <AutomationStatusItem icon={Activity} label="调度状态" value={formatSchedulerState(schedulerStatus)} />
          <AutomationStatusItem icon={Clock3} label="下次扫描" value={formatDateTime(schedulerStatus?.nextRunAt)} />
          <AutomationStatusItem icon={RefreshCw} label="上次扫描" value={formatDateTime(schedulerStatus?.lastRunAt)} />
          <AutomationStatusItem icon={TimerReset} label="手动冷却至" value={formatDateTime(schedulerStatus?.manualCooldownUntil)} />
          {schedulerStatus?.lastResult && (
            <AutomationStatusItem
              icon={Download}
              label="上次结果"
              value={`下载 ${schedulerStatus.lastResult.downloaded.length}，跳过 ${schedulerStatus.lastResult.skipped.length}，错误 ${schedulerStatus.lastResult.errors.length}`}
            />
          )}
        </CardFooter>
      </Card>
          </SettingsCategory>

          <SettingsCategory
            description="应用版本、项目仓库、版权与许可信息。"
            id="about"
            title="关于"
          >
            <Card className="overflow-hidden shadow-none">
              <CardHeader>
                <div className="flex flex-wrap items-center gap-2">
                  <CardTitle>Ani Tracker</CardTitle>
                  <Badge tone="neutral">版本 {__ANI_TRACKER_VERSION__}</Badge>
                </div>
                <CardDescription>本地追番、资源管理与播放工具。</CardDescription>
              </CardHeader>
              <CardContent className="flex flex-col gap-3 sm:flex-row sm:flex-wrap">
                <Button
                  onClick={() => void openProjectUrl("GitHub", "https://github.com/momoc-ani/ani-tracker.git")}
                  type="button"
                  variant="outline"
                >
                  <Github data-icon="inline-start" />
                  GitHub
                </Button>
                <Button
                  onClick={() => void openProjectUrl("Gitee", "https://gitee.com/aurora-momoc/ani")}
                  type="button"
                  variant="outline"
                >
                  <GitFork data-icon="inline-start" />
                  Gitee
                </Button>
              </CardContent>
              <CardFooter className="flex-col items-start gap-2 border-t bg-muted/50 pt-4 text-sm text-muted-foreground sm:pt-5">
                <p>Copyright (c) 2026 Ani Tracker contributors.</p>
                <p>原创源码采用 PolyForm Noncommercial License 1.0.0。</p>
                <p>允许个人及其他非商业用途；商业使用须获得版权所有者书面许可。</p>
                <p>第三方组件遵循各自许可证。</p>
              </CardFooter>
            </Card>
          </SettingsCategory>
        </div>
      </div>

      {hasUnsavedChanges && (
        <StickyActionBar className="justify-center bg-background/95">
          <span className="text-sm text-muted-foreground">更改尚未保存</span>
          <Button onClick={saveSettings} disabled={saveState === "saving" || resetState === "resetting"}>
            <Save data-icon="inline-start" />
            {saveState === "saving" ? "保存中" : "保存设置"}
          </Button>
        </StickyActionBar>
      )}

      <ConfirmActionDialog
        confirmLabel={remoteRuntime ? "恢复外观默认" : "恢复默认"}
        description={remoteRuntime
          ? "仅恢复当前远程设备的主题，不修改 PC 宿主设置。"
          : "当前未保存的设置将被平台默认配置覆盖，主题与运行参数也会立即更新。"}
        onConfirm={resetSettingsToDefaults}
        onOpenChange={setResetDialogOpen}
        open={resetDialogOpen}
        title={remoteRuntime ? "确认恢复当前设备外观？" : "确认恢复默认设置？"}
      />
      {!remoteRuntime && (
      <ConfirmActionDialog
        confirmLabel="选择备份并恢复"
        description="播放器与下载引擎会安全停止，所选备份通过完整性检查后覆盖当前数据；操作前会自动保留回滚快照。"
        onConfirm={restoreDatabaseBackup}
        onOpenChange={setRestoreDialogOpen}
        open={restoreDialogOpen}
        title="确认恢复数据备份？"
      />
      )}
    </div>
  );
}

/** 提供候补字幕组的下拉多选、新增和删除操作。 */
function CandidateFansubMultiSelect({
  id,
  value,
  onChange
}: {
  id: string;
  value: string[];
  onChange: (value: string[]) => void;
}) {
  const [open, setOpen] = useState(false);
  const [input, setInput] = useState("");
  const normalizedInput = normalizeFansubMatchName(input);
  const canAdd = Boolean(
    normalizedInput && !value.some((name) => normalizeFansubMatchName(name) === normalizedInput)
  );

  /** 将有效且不重复的输入加入候补名单。 */
  function addCandidateFansub() {
    if (!canAdd) {
      return;
    }
    onChange(normalizeCandidateFansubNames([...value, input]));
    setInput("");
  }

  /** 从候补名单移除指定字幕组。 */
  function removeCandidateFansub(name: string) {
    const normalizedName = normalizeFansubMatchName(name);
    onChange(value.filter((item) => normalizeFansubMatchName(item) !== normalizedName));
  }

  return (
    <Popover
      onOpenChange={(nextOpen) => {
        setOpen(nextOpen);
        if (!nextOpen) setInput("");
      }}
      open={open}
    >
      <PopoverTrigger asChild>
        <Button
          aria-expanded={open}
          className="w-full justify-between font-normal"
          id={id}
          role="combobox"
          type="button"
          variant="outline"
        >
          <span className={cn("truncate", !value.length && "text-muted-foreground")}>
            {value.length ? `已选择 ${value.length} 个` : "未选择候补字幕组"}
          </span>
          <ChevronDown className="text-muted-foreground" data-icon="inline-end" />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-[var(--radix-popover-trigger-width)] min-w-64 p-1">
        <Command shouldFilter={false}>
          <div className="border-b p-2">
            <InputGroup>
              <InputGroupInput
                aria-label="候补字幕组名称"
                onChange={(event) => setInput(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" && canAdd) {
                    event.preventDefault();
                    event.stopPropagation();
                    addCandidateFansub();
                  }
                }}
                placeholder="字幕组名称"
                value={input}
              />
              {canAdd && (
                <InputGroupAddon>
                  <InputGroupButton
                    aria-label="添加候补字幕组"
                    onClick={addCandidateFansub}
                    title="添加候补字幕组"
                  >
                    <Plus />
                  </InputGroupButton>
                </InputGroupAddon>
              )}
            </InputGroup>
          </div>
          <CommandList className="max-h-56">
            <CommandEmpty>尚未选择候补字幕组</CommandEmpty>
            {value.length > 0 && (
              <CommandGroup heading="已选择">
                {value.map((name) => (
                  <CommandItem
                    aria-label={`删除候补字幕组 ${name}`}
                    key={normalizeFansubMatchName(name)}
                    onSelect={() => removeCandidateFansub(name)}
                    title={`删除 ${name}`}
                    value={name}
                  >
                    <span className="min-w-0 flex-1 truncate">{name}</span>
                    <Minus className="ml-auto" />
                  </CommandItem>
                ))}
              </CommandGroup>
            )}
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  );
}

function parseExtensions(value: string): string[] {
  return value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

/** 以持久化快照为准判断草稿是否真的发生变更。 */
function areSettingsEqual(left: AppSettings, right: AppSettings): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

/** 渲染随主内容滚动吸顶的桌面设置分区导航。 */
function SettingsCategoryNavigation({
  activeCategory,
  categories,
  onNavigate
}: {
  activeCategory: SettingsCategoryId;
  categories: typeof settingsCategories;
  onNavigate: (categoryId: SettingsCategoryId) => void;
}) {
  return (
    <aside className="sticky top-24 hidden max-h-[calc(100dvh-7rem)] min-w-0 self-start overflow-y-auto pr-4 lg:block">
      <nav aria-label="设置分区" className="flex flex-col gap-1 border-r">
        {categories.map((category) => {
          const Icon = category.icon;
          return (
            <Button
              aria-current={activeCategory === category.id ? "location" : undefined}
              className="w-full justify-start"
              data-active={activeCategory === category.id}
              key={category.id}
              onClick={() => onNavigate(category.id)}
              variant="navigation"
            >
              <Icon aria-hidden="true" data-icon="inline-start" />
              <span className="truncate">{category.label}</span>
            </Button>
          );
        })}
      </nav>
    </aside>
  );
}

/** 渲染固定在页面标题下方的小屏幕设置分区选择器。 */
function SettingsCategorySelect({
  activeCategory,
  categories,
  onNavigate
}: {
  activeCategory: SettingsCategoryId;
  categories: typeof settingsCategories;
  onNavigate: (categoryId: SettingsCategoryId) => void;
}) {
  const selectId = useId();

  return (
    <Field>
      <FieldLabel className="sr-only" htmlFor={selectId}>设置分区</FieldLabel>
      <Select value={activeCategory} onValueChange={(value) => onNavigate(value as SettingsCategoryId)}>
        <SelectTrigger id={selectId}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectGroup>
            {categories.map((category) => (
              <SelectItem key={category.id} value={category.id}>{category.label}</SelectItem>
            ))}
          </SelectGroup>
        </SelectContent>
      </Select>
    </Field>
  );
}

/** 提供符合 Stitch 长页层级的设置分区锚点。 */
function SettingsCategory({
  id,
  title,
  description,
  action,
  children
}: {
  id: SettingsCategoryId;
  title: string;
  description: string;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="scroll-mt-[calc(12rem+var(--safe-area-top))] lg:scroll-mt-24" id={`settings-${id}`}>
      <header className="flex flex-col gap-3 border-b pb-3 sm:flex-row sm:items-end sm:justify-between">
        <div className="min-w-0">
          <h2 className="text-xl font-semibold">{title}</h2>
          <p className="mt-1 text-sm text-muted-foreground">{description}</p>
        </div>
        {action && <div className="w-full shrink-0 sm:w-auto">{action}</div>}
      </header>
      <div className="mt-5">{children}</div>
    </section>
  );
}

function formatSchedulerState(status: AutomationSchedulerStatus | null): string {
  if (!status) {
    return "未知";
  }

  if (status.inFlight) {
    return "扫描中";
  }

  if (!status.enabled) {
    return "已关闭";
  }

  return status.running ? `运行中，每 ${status.intervalMinutes} 分钟` : "未启动";
}

function formatDateTime(value?: string): string {
  return value ? new Date(value).toLocaleString() : "--";
}

/** 将移动通知权限映射为简短状态文本。 */
function formatNotificationPermission(value: MobileNotificationPermission | null): string {
  switch (value) {
    case "granted":
      return "已允许";
    case "denied":
      return "已拒绝";
    case "prompt-with-rationale":
      return "需要说明";
    case "prompt":
      return "未请求";
    case "not-required":
      return "无需授权";
    default:
      return "读取中";
  }
}

/** 在自动化面板底部紧凑展示单项调度状态。 */
function AutomationStatusItem({
  icon: Icon,
  label,
  value
}: {
  icon: typeof Activity;
  label: string;
  value: string;
}) {
  return (
    <div className="flex min-w-0 items-center gap-2 text-xs text-muted-foreground">
      <Icon aria-hidden="true" className="size-4 shrink-0" />
      <span className="shrink-0 font-medium text-foreground">{label}</span>
      <span className="truncate" title={value}>{value}</span>
    </div>
  );
}

/** 统一设置页分区的标题、说明和内容布局。 */
function SettingsSection({
  title,
  description,
  children
}: {
  title: string;
  description?: string;
  children: ReactNode;
}) {
  return (
    <Card className="overflow-hidden shadow-none">
      <CardHeader className="border-b bg-muted/50">
        <CardTitle><h3>{title}</h3></CardTitle>
        {description && <CardDescription>{description}</CardDescription>}
      </CardHeader>
      <CardContent className="pt-4 sm:pt-5">{children}</CardContent>
    </Card>
  );
}

/** 渲染文本类设置项。 */
function TextSetting({
  icon,
  label,
  value,
  type = "text",
  disabled = false,
  onChange
}: {
  icon?: ReactNode;
  label: string;
  value: string;
  type?: "text" | "password";
  disabled?: boolean;
  onChange: (value: string) => void;
}) {
  const inputId = useId();

  return (
    <Field data-disabled={disabled || undefined}>
      <FieldLabel htmlFor={inputId}>
        {icon && <span className="text-primary">{icon}</span>}
        {label}
      </FieldLabel>
      <Input
        id={inputId}
        type={type}
        value={value}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
      />
    </Field>
  );
}

/** 渲染支持触控和键盘操作的开关设置项。 */
function ToggleSetting({
  icon,
  label,
  description,
  checked,
  disabled = false,
  onChange
}: {
  icon?: ReactNode;
  label: string;
  description: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (value: boolean) => void;
}) {
  const switchId = useId();
  const descriptionId = `${switchId}-description`;

  return (
    <Field
      className="min-h-[104px] items-center justify-between rounded-md border p-4 data-[disabled=true]:cursor-not-allowed data-[disabled=true]:opacity-60"
      data-disabled={disabled}
      orientation="horizontal"
    >
      <FieldLabel className="min-w-0 flex-1 cursor-pointer flex-col items-start" htmlFor={switchId}>
        <span className="flex items-center gap-2">
          {icon && <span className="text-primary">{icon}</span>}
          {label}
        </span>
        <span id={descriptionId} className="text-sm font-normal leading-6 text-muted-foreground">
          {description}
        </span>
      </FieldLabel>
      <Switch
        aria-describedby={descriptionId}
        id={switchId}
        checked={checked}
        disabled={disabled}
        onCheckedChange={onChange}
      />
    </Field>
  );
}

/** 渲染带单位提示的数值设置项。 */
function NumberSetting({
  label,
  value,
  suffix,
  min = 0,
  max,
  step = 1,
  disabled = false,
  onChange
}: {
  label: string;
  value: number;
  suffix?: string;
  min?: number;
  max?: number;
  step?: number;
  disabled?: boolean;
  onChange: (value: number) => void;
}) {
  const inputId = useId();

  return (
    <Field className="rounded-md border p-4 data-[disabled=true]:opacity-60" data-disabled={disabled}>
      <FieldLabel htmlFor={inputId}>{label}</FieldLabel>
      <div className="flex items-center gap-2">
        <Input
          id={inputId}
          className="min-w-0 flex-1"
          disabled={disabled}
          min={min}
          max={max}
          step={step}
          type="number"
          value={value}
          onChange={(event) => onChange(Number(event.target.value))}
        />
        {suffix && <span className="text-sm text-muted-foreground">{suffix}</span>}
      </div>
    </Field>
  );
}

/** 渲染可精确输入的 qBittorrent 速率限制滑块。 */
function SpeedLimitSetting({
  label,
  value,
  onChange
}: {
  label: string;
  value: number;
  onChange: (value: number) => void;
}) {
  const inputId = useId();
  const normalizedValue = Number.isFinite(value) ? Math.max(0, Math.round(value)) : 0;
  const sliderMax = Math.max(10_240, Math.ceil(normalizedValue / 1_024) * 1_024);

  /** 统一约束输入值，避免向设置草稿写入负数或非数字。 */
  function updateValue(nextValue: number) {
    onChange(Number.isFinite(nextValue) ? Math.max(0, Math.round(nextValue)) : 0);
  }

  return (
    <Field>
      <div className="flex items-center justify-between gap-3">
        <FieldLabel htmlFor={inputId}>{label} (KiB/s)</FieldLabel>
        <Input
          className="h-8 w-28 shrink-0 text-right tabular-nums"
          id={inputId}
          min={0}
          onChange={(event) => updateValue(Number(event.target.value))}
          step={128}
          type="number"
          value={normalizedValue}
        />
      </div>
      <Slider
        aria-label={`${label}，单位 KiB/s`}
        max={sliderMax}
        min={0}
        onValueChange={(nextValue) => updateValue(nextValue[0] ?? 0)}
        step={128}
        value={[normalizedValue]}
      />
    </Field>
  );
}

/** 渲染使用 Radix Select 的选项设置项。 */
function SelectSetting({
  label,
  value,
  options,
  disabled = false,
  onChange
}: {
  label: string;
  value: string;
  options: Array<{ label: string; value: string }>;
  disabled?: boolean;
  onChange: (value: string) => void;
}) {
  const selectId = useId();

  return (
    <Field className="rounded-md border p-4" data-disabled={disabled}>
      <FieldLabel htmlFor={selectId}>{label}</FieldLabel>
      <Select disabled={disabled} value={value} onValueChange={onChange}>
        <SelectTrigger id={selectId}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectGroup>
            {options.map((option) => (
              <SelectItem key={option.value} value={option.value}>
                {option.label}
              </SelectItem>
            ))}
          </SelectGroup>
        </SelectContent>
      </Select>
    </Field>
  );
}

/** 展示不可编辑的设置摘要。 */
function SettingRow({
  icon,
  label,
  value
}: {
  icon?: ReactNode;
  label: string;
  value: string;
}) {
  return (
    <div className="flex items-start gap-3">
      {icon && <div className="mt-0.5 text-primary">{icon}</div>}
      <div className="min-w-0">
        <div className="text-sm font-medium">{label}</div>
        <div className="mt-1 break-all rounded-md bg-muted px-3 py-2 text-sm text-muted-foreground">{value}</div>
      </div>
    </div>
  );
}
