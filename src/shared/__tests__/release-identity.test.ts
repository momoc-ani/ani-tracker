import { strict as assert } from "node:assert";
import { test } from "node:test";
import type { DownloadTask, Release } from "../domain";
import {
  compareReleaseEpisodeDescending,
  dedupeReleasesByEpisodeContent,
  extractMagnetInfoHash,
  extractTorrentUrlInfoHash,
  findReleaseDownloadTask,
  matchesReleaseDownloadTask,
  normalizeTorrentInfoHash
} from "../release-identity";

const hexInfoHash = "5448ae0ed36912eb0dfba53c3e495b9988841e68";
const base32InfoHash = "KREK4DWTNEJOWDP3UU6D4SK3TGEIIHTI";

test("十六进制与 Base32 磁链规范为同一 BTIH", () => {
  assert.equal(normalizeTorrentInfoHash(hexInfoHash.toUpperCase()), hexInfoHash);
  assert.equal(normalizeTorrentInfoHash(base32InfoHash), hexInfoHash);
  assert.equal(
    extractMagnetInfoHash(`magnet:?dn=Episode&xt=urn:btih:${base32InfoHash}&tr=https%3A%2F%2Ftracker.example`),
    hexInfoHash
  );
});

test("仅从严格的 torrent 文件名提取 BTIH", () => {
  assert.equal(
    extractTorrentUrlInfoHash(`https://mikanani.me/Download/20260730/${hexInfoHash}.torrent`),
    hexInfoHash
  );
  assert.equal(extractTorrentUrlInfoHash("https://mikanani.me/Download/123.torrent"), undefined);
  assert.equal(extractTorrentUrlInfoHash(`https://example.test/file.torrent?hash=${hexInfoHash}`), undefined);
});

test("同集同 BTIH 跨来源合并但不同集保留", () => {
  const releases = dedupeReleasesByEpisodeContent([
    release("source-a", 8, { infoHash: hexInfoHash.toUpperCase() }),
    release("source-b", 8, { magnetUrl: `magnet:?xt=urn:btih:${base32InfoHash}&tr=udp%3A%2F%2Ftracker` }),
    release("mikan", 8, { torrentUrl: `https://mikanani.me/Download/20260730/${hexInfoHash}.torrent` }),
    release("source-c", 9, { infoHash: hexInfoHash })
  ]);

  assert.deepEqual(releases.map((item) => [item.sourceId, item.episodeNo]), [["source-a", 8], ["source-c", 9]]);
});

test("资源按集数倒序且未识别集数位于末尾", () => {
  const releases = [release("unknown", undefined), release("episode-8", 8), release("episode-12", 12)];
  releases.sort(compareReleaseEpisodeDescending);
  assert.deepEqual(releases.map((item) => item.episodeNo), [12, 8, undefined]);
});

test("同集同字幕组的不同 Hash 不共享下载状态", () => {
  const completed2160p = downloadTask("task-2160p", 2, {
    fansubGroupId: "fansub-feibanyama",
    torrentHash: "5c2fbeb8c2ffd445920ef9e9523894339c2c4818",
    resolution: "2160p"
  });
  const release1080p = release("mikan-1080p", 2, {
    fansubGroupId: "fansub-feibanyama",
    infoHash: "158312c00e88c103e476e456069039b199e2c2b6",
    resolution: "1080p"
  });

  assert.equal(matchesReleaseDownloadTask(completed2160p, release1080p), false);
  assert.equal(findReleaseDownloadTask([completed2160p], release1080p), undefined);
});

test("同集同 BTIH 可跨来源关联但不同集保持独立", () => {
  const task = downloadTask("task-anibt", 2, {
    torrentHash: hexInfoHash
  });
  const sameEpisode = release("mikan", 2, {
    infoHash: hexInfoHash.toUpperCase()
  });
  const otherEpisode = release("mikan-other-episode", 1, {
    infoHash: hexInfoHash
  });

  assert.equal(findReleaseDownloadTask([task], sameEpisode)?.id, task.id);
  assert.equal(findReleaseDownloadTask([task], otherEpisode), undefined);
});

test("无 Hash 的旧任务仅关联标题或版本特征一致的资源", () => {
  const legacyTask = downloadTask("legacy-task", 2, {
    releaseId: "rss-subscription:rss-1:[组] 测试番 - 02 [1080p]",
    fansubGroupId: "fansub-a",
    resolution: "1080p"
  });
  const sameTitle = release("mikan-same-title", 2, {
    title: "[组] 测试番 - 02 [1080p]",
    fansubGroupId: "fansub-a"
  });
  const differentVersion = release("mikan-2160p", 2, {
    title: "[组] 测试番 - 02 [2160p]",
    fansubGroupId: "fansub-a",
    resolution: "2160p"
  });
  const matchingVersion = release("mikan-legacy-1080p", 2, {
    fansubGroupId: "fansub-a",
    resolution: "1080p"
  });

  assert.equal(findReleaseDownloadTask([legacyTask], sameTitle)?.id, legacyTask.id);
  assert.equal(findReleaseDownloadTask([legacyTask], matchingVersion)?.id, legacyTask.id);
  assert.equal(findReleaseDownloadTask([legacyTask], differentVersion), undefined);
});

/** 创建资源身份测试所需的最小发布数据。 */
function release(
  sourceId: string,
  episodeNo: number | undefined,
  overrides: Partial<Release> = {}
): Release {
  return {
    id: `release-${sourceId}`,
    title: `测试番 - ${episodeNo ?? "未知"}`,
    episodeNo,
    contentKind: episodeNo === undefined ? "unknown" : "episode",
    sourceId,
    sourceName: sourceId,
    publishedAt: "2026-08-04T00:00:00.000Z",
    ...overrides
  };
}

/** 创建资源下载关联测试所需的最小任务数据。 */
function downloadTask(
  id: string,
  episodeNo: number,
  overrides: Partial<DownloadTask> = {}
): DownloadTask {
  return {
    id,
    episodeNo,
    engine: "qbittorrent",
    name: `测试番 - ${episodeNo}`,
    status: "completed",
    progress: 1,
    downloadSpeed: 0,
    uploadSpeed: 0,
    savePath: "/Anime",
    files: [],
    createdAt: "2026-08-04T00:00:00.000Z",
    ...overrides
  };
}
