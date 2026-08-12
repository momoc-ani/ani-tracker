import { formatSubtitleLanguages, formatVideoBitDepth, getSubtitleCoverage, resolveSubtitleLanguages } from "@shared/release-metadata";
import { classifyAnimeRelease } from "@shared/anime-release-search";
import { compareReleaseEpisodeDescending, getReleaseEpisodeContentKey } from "@shared/release-identity";
import type { MyAnime, Release } from "@shared/domain";

export interface ReleaseVersionFamily {
  key: string;
  releases: Release[];
  selectedRelease: Release;
  episodeKey: string;
  episodeLabel: string;
}

export interface ReleaseEpisodeFamilyGroup {
  key: string;
  label: string;
  families: ReleaseVersionFamily[];
}

/** 按 episode 将资源族归组。 */
export function groupReleaseFamilyEpisodes(families: ReleaseVersionFamily[]): ReleaseEpisodeFamilyGroup[] {
  const groups = new Map<string, ReleaseEpisodeFamilyGroup>();
  for (const family of families) {
    const group = groups.get(family.episodeKey) ?? {
      key: family.episodeKey,
      label: family.episodeLabel,
      families: []
    };
    group.families.push(family);
    groups.set(family.episodeKey, group);
  }
  return [...groups.values()];
}

/** 统计资源族覆盖的唯一集数或连集范围数量。 */
export function countReleaseFamilyEpisodes(families: ReleaseVersionFamily[]): number {
  return new Set(families.map((family) => family.episodeKey)).size;
}

/** 判断资源是否为需要独立展示和文件级关联的合集。 */
export function isCollectionRelease(release: Release): boolean {
  return Boolean(release.episodeRange) || release.contentKind === "range" || release.contentKind === "batch";
}

/** 将同一资源的字幕、编码、位深和分辨率版本合并为一个资源族。 */
export function groupReleaseVersions(
  releases: Release[],
  preferences: MyAnime,
  selections: Record<string, string> = {}
): ReleaseVersionFamily[] {
  const families = new Map<string, ReleaseVersionFamily>();
  for (const release of releases) {
    const key = buildReleaseFamilyKey(release);
    const family = families.get(key) ?? createReleaseVersionFamily(key, release);
    family.releases.push(release);
    families.set(key, family);
  }

  const groupedFamilies = [...families.values()]
    .map((family) => {
      const ordered = sortReleaseVersions(family.releases, preferences);
      const selectedRelease = family.releases.find((item) => releaseKey(item) === selections[family.key]) ?? ordered[0] ?? family.releases[0];
      return { ...family, releases: ordered, selectedRelease };
    });

  return groupedFamilies.sort((left, right) => {
    const episodeDelta = compareReleaseEpisodeDescending(left.selectedRelease, right.selectedRelease);
    return episodeDelta || right.selectedRelease.publishedAt.localeCompare(left.selectedRelease.publishedAt);
  });
}

/** 生成资源版本下拉框中的完整技术信息文案。 */
export function getReleaseVersionLabel(release: Release, preferences: MyAnime, active = false): string {
  const preferredText = getReleaseVersionPreferenceScore(release, preferences) > 0 ? "偏好匹配" : "";
  const parts = [
    formatSubtitleLanguages(release.subtitleLanguages, release.subtitle),
    release.normalizedVideoCodec ?? release.declaredVideoCodec ?? "编码未知",
    formatVideoBitDepth(release.bitDepth),
    release.resolution ?? "",
    release.sourceName
  ].filter(Boolean);
  return `${active ? "当前 · " : ""}${parts.join(" · ")}${preferredText ? ` · ${preferredText}` : ""}`;
}

/** 生成稳定的资源主键，用于批量选择和任务关联。 */
export function releaseKey(release: Release): string {
  return getReleaseEpisodeContentKey(release);
}

/** 判断资源是否可被批量选择下载。 */
export function isReleaseSelectable(release: Release, linkedTasks: ReleaseTaskLink[], anime: MyAnime["anime"]): boolean {
  return classifyAnimeRelease(release, anime) === "current" && Boolean(release.magnetUrl ?? release.torrentUrl) && !findLinkedReleaseTask(linkedTasks, release);
}

export interface ReleaseTaskLink {
  releaseId?: string;
  episodeNo?: number;
  fansubGroupId?: string;
  fansubName?: string;
}

/** 保持资源分组模块不依赖页面实现的匹配逻辑。 */
function findLinkedReleaseTask(tasks: ReleaseTaskLink[], release: Release): ReleaseTaskLink | undefined {
  return tasks.find((task) => {
    if (task.releaseId === release.id) return true;
    const releaseFansubKey = release.fansubGroupId ?? release.fansubName;
    return Boolean(
      releaseFansubKey &&
      task.episodeNo !== undefined &&
      task.episodeNo === release.episodeNo &&
      (task.fansubGroupId ?? task.fansubName) === releaseFansubKey
    );
  });
}

function createReleaseVersionFamily(key: string, release: Release): ReleaseVersionFamily {
  return {
    key,
    releases: [],
    selectedRelease: release,
    episodeKey: getReleaseEpisodeKey(release),
    episodeLabel: getReleaseEpisodeLabel(release)
  };
}

function buildReleaseFamilyKey(release: Release): string {
  return [
    release.sourceId,
    release.fansubGroupId ?? normalizeFamilyText(release.fansubName ?? ""),
    getReleaseEpisodeKey(release),
    normalizeFamilyText(stripReleaseVariantTokens(release.title))
  ].join("|");
}

function getReleaseEpisodeKey(release: Release): string {
  if (release.episodeRange) return `range:${release.episodeRange.start}-${release.episodeRange.end}`;
  if (release.episodeNo === undefined) return release.contentKind === "batch" ? `batch:${release.seriesSeasonNo ?? "unknown"}` : "unknown";
  return `episode:${release.episodeNo}`;
}

function getReleaseEpisodeLabel(release: Release): string {
  if (release.episodeRange) return `第 ${formatEpisodeNumber(release.episodeRange.start)}-${formatEpisodeNumber(release.episodeRange.end)} 集`;
  if (release.episodeNo === undefined) return release.contentKind === "batch" ? "合集" : "未识别集数";
  return `第 ${formatEpisodeNumber(release.episodeNo)} 集`;
}

function sortReleaseVersions(releases: Release[], preferences: MyAnime): Release[] {
  return [...releases].sort((left, right) => {
    const scoreDelta = getReleaseVersionPreferenceScore(right, preferences) - getReleaseVersionPreferenceScore(left, preferences);
    return scoreDelta || right.publishedAt.localeCompare(left.publishedAt) || releaseKey(left).localeCompare(releaseKey(right));
  });
}

function getReleaseVersionPreferenceScore(release: Release, preferences: MyAnime): number {
  let score = 0;
  const preferredSubtitleLanguages = resolveSubtitleLanguages(preferences.preferredSubtitleLanguages, preferences.preferredSubtitle);
  if (preferredSubtitleLanguages.length > 0) score += getSubtitleCoverage(release, preferredSubtitleLanguages) * 10;
  if (preferences.preferredResolution && release.resolution === preferences.preferredResolution) score += 5;
  if (preferences.preferredCodec && release.normalizedVideoCodec === preferences.preferredCodec) score += 5;
  if (preferences.preferredBitDepth && release.bitDepth === preferences.preferredBitDepth) score += 6;
  return score;
}

function normalizeFamilyText(value: string): string {
  return value
    .normalize("NFKC")
    .toLocaleLowerCase()
    .replace(/[\s_./\-]+/g, " ")
    .replace(/\[(?:chs|cht|gb|big5|multi|简体|繁体|简繁|繁简|简日|繁日|内封|内嵌|字幕)\]/gi, "")
    .replace(/[【\[][^】\]]*(?:chs|cht|gb|big5|multi|简体|繁体|简繁|繁简|简日|繁日|内封|内嵌|字幕)[^】\]]*[】\]]/gi, "")
    .replace(/(?:chs|cht|gb|big5|multi|简体|繁体|简繁|繁简|简日|繁日|内封|内嵌|字幕)/gi, "")
    .replace(/\s+/g, " ")
    .trim();
}

function stripReleaseVariantTokens(value: string): string {
  return value
    .replace(/(?:chs|cht|gb|big5|multi|简体|繁体|简繁|繁简|简日|繁日|日语|日語|英文|英语|英語|内封|内嵌|字幕)/gi, " ")
    .replace(/\b(?:h\.?264|x264|avc|h\.?265|x265|hevc|av1|vp9)\b/gi, " ")
    .replace(/\b(?:8|10|12)\s*[- ]?\s*bits?\b|\b(?:hi10p|main\s*10)\b/gi, " ")
    .replace(/\b(?:720p|1080p|2160p|4k|1280x720|1920x1080|3840x2160)\b/gi, " ")
    .replace(/[\[【(（]\s*[\]】)）]/g, " ");
}

function formatEpisodeNumber(value: number): string {
  return Number.isInteger(value) ? String(value).padStart(2, "0") : String(value);
}
