import {
  DownloadQueuePage,
  type DownloadQueueClient
} from "@/features/downloads/DownloadQueuePage";
import { appApi } from "@/lib/api";

// 远端 RPC 每开放一项下载能力，只需在此注入，无需复制页面实现。
const remoteDownloadQueueClient: DownloadQueueClient = {
  addDownloadUrl: (input) => appApi.addDownloadUrl(input),
  listDownloads: () => appApi.listDownloads(),
  listMyAnime: () => appApi.listMyAnime(),
  removeDownload: (taskId, deleteFiles) => appApi.removeDownload(taskId, deleteFiles),
  refreshDownloads: () => appApi.refreshDownloads(),
  pauseDownload: (taskId) => appApi.pauseDownload(taskId),
  resumeDownload: (taskId) => appApi.resumeDownload(taskId),
  setDownloadFilePriority: (taskId, fileIndexes, priority) =>
    appApi.setDownloadFilePriority(taskId, fileIndexes, priority)
};

/** 为远端 PWA 装配已开放的下载能力。 */
export function RemoteDownloadsPage() {
  return (
    <DownloadQueuePage
      client={remoteDownloadQueueClient}
      logScope="remote"
    />
  );
}
