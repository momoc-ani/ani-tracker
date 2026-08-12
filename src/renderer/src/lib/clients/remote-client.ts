import type { AppClient } from "@shared/app-client";
import type { AppSettings } from "@shared/domain";
import { createDefaultAppearanceSettings } from "@shared/theme";
import {
  exportRemoteThemePackage,
  openRemoteExternalUrl,
  pruneRemoteThemeBackgrounds,
  resolveRemoteThemeBackground,
  saveRemoteThemeBackground
} from "./remote-theme-assets";

const REMOTE_METHODS = new Set([
  "getDashboard",
  "listNotifications",
  "getUnreadNotificationCount",
  "markNotificationRead",
  "markAllNotificationsRead",
  "listMyAnime",
  "upsertMyAnime",
  "followBangumiAnime",
  "removeMyAnime",
  "listMyAnimeWatchProgress",
  "setAnimeWatchProgress",
  "reportPlaybackProgress",
  "savePlaybackCheckpoint",
  "listAnimeCatalog",
  "getAnimeDetail",
  "searchAnimeCatalog",
  "browseBangumiAnime",
  "listFansubs",
  "listEpisodes",
  "upsertEpisode",
  "listEpisodePreferences",
  "upsertEpisodePreference",
  "removeEpisodePreference",
  "previewEpisodeReleases",
  "searchReleases",
  "searchAnimeReleases",
  "searchRssSubscriptionReleases",
  "getAnimeSourceBindingState",
  "confirmAnimeSourceBinding",
  "reportAnimeSourceCandidateMismatch",
  "removeAnimeSourceCandidateMismatch",
  "setAnimeSourceExcluded",
  "removeAnimeSourceBinding",
  "listDownloads",
  "refreshDownloads",
  "pauseDownload",
  "resumeDownload",
  "removeDownload",
  "setDownloadFilePriority",
  "addDownloadUrl",
  "addReleaseDownload",
  "listSources",
  "setSourceEnabled",
  "upsertSource",
  "getSourceSyncStatus",
  "getSettings",
  "updateSettings",
  "testQbittorrent",
  "getAutomationSchedulerStatus",
  "getQbittorrentManagedStatus",
  "startQbittorrentManaged",
  "stopQbittorrentManaged",
  "getEmbeddedTorrentStatus",
  "startEmbeddedTorrent",
  "stopEmbeddedTorrent",
  "restartEmbeddedTorrent"
]);

export type RemoteClientInvoker = (method: string, args: unknown[]) => Promise<unknown>;

/** 创建只开放远程白名单方法的 HTTP 客户端。 */
export function createRemoteClient(invoke: RemoteClientInvoker): AppClient {
  return new Proxy({ platform: "remote" } as AppClient, {
    get(target, property) {
      if (property === "platform") {
        return target.platform;
      }
      if (typeof property !== "string") {
        return undefined;
      }
      if (property === "saveThemeBackground") return saveRemoteThemeBackground;
      if (property === "resolveThemeBackground") return resolveRemoteThemeBackground;
      if (property === "pruneThemeBackgrounds") return pruneRemoteThemeBackgrounds;
      if (property === "exportThemePackage") return exportRemoteThemePackage;
      if (property === "openExternal") return openRemoteExternalUrl;
      if (!REMOTE_METHODS.has(property)) {
        return async () => {
          throw new Error("当前远程客户端未开放此功能");
        };
      }
      if (property === "getSettings" || property === "updateSettings") {
        return (...args: unknown[]) => invoke(property, args).then(normalizeRemoteSettings);
      }
      return (...args: unknown[]) => invoke(property, args);
    }
  });
}

/** 将远程脱敏设置补齐为共享设置页可安全读取的结构。 */
function normalizeRemoteSettings(value: unknown): AppSettings {
  const settings = value as Partial<AppSettings>;
  if (!settings.download || !settings.automation || !settings.network?.metadataProxy) {
    throw new Error("远程设置响应缺少必要字段");
  }
  return {
    appearance: createDefaultAppearanceSettings(),
    download: settings.download,
    storage: {
      userDataDir: "",
      databasePath: "",
      cacheDir: "",
      logDir: ""
    },
    players: [],
    automation: settings.automation,
    sourceSync: settings.sourceSync,
    media: {
      ffprobePath: "",
      ffprobeTimeoutSeconds: 20,
      videoExtensions: []
    },
    desktop: {
      minimizeToTray: false,
      launchAtLogin: false
    },
    network: {
      metadataProxy: settings.network.metadataProxy,
      remoteAccess: { lanEnabled: false, port: 18_083 }
    }
  };
}
