import type { Anime, Release } from "./domain";

const bracketPairPattern = /[「『《【\[(（]([^」』》】\])）]{2,80})[」』》】\])）]/g;
const separatorPattern = /[|｜／/]+|(?:\s+-\s+)|(?:\s+–\s+)|(?:\s+—\s+)|[:：]/g;
const punctuationPattern = /["'“”‘’「」『』《》【】[\]()（）.,，。:：;；!?！？·・~～_-]+/g;
const seasonSuffixPatterns = [
  /\s*第\s*[〇零一二三四五六七八九十百两\d]+\s*[季期部篇章]\s*$/u,
  /\s+\d+(?:st|nd|rd|th)\s+season\s*$/i,
  /\s+(?:season|part)\s*\d+\s*$/i,
  /\s+s\d+\s*$/i
];
const seriesSeasonPatterns = [
  /第\s*([〇零一二三四五六七八九十百两\d]+)\s*(?:季|期|部)/u,
  /\b(\d{1,2})(?:st|nd|rd|th)\s+season\b/i,
  /\bseason\s*0*(\d{1,2})\b/i,
  /(?:^|[^a-z0-9])s0*(\d{1,2})(?=(?:e\d{1,3})|[^a-z0-9]|$)/i
];

export type AnimeReleaseCompatibility = "current" | "other" | "mismatch";

export function buildAnimeReleaseSearchTerms(anime: Anime, extraTerms: string[] = [], limit = 12): string[] {
  const rawTerms = [
    ...extraTerms,
    anime.title,
    anime.originalTitle ?? "",
    ...anime.aliases.map((alias) => alias.alias)
  ];
  const expanded = rawTerms.flatMap(expandSearchTerm);

  return uniqueBySearchKey(expanded).slice(0, limit);
}

export function normalizeReleaseSearchText(value: string): string {
  return value
    .normalize("NFKC")
    .replace(punctuationPattern, " ")
    .replace(/\s+/g, " ")
    .trim()
    .toLowerCase();
}

/** 判断关键词是否能匹配番剧标题、原名或任一别名，用于追番输入联想。 */
export function matchesAnimeSearchKeyword(anime: Anime, keyword: string): boolean {
  const normalizedKeyword = normalizeReleaseSearchText(keyword);
  if (!normalizedKeyword) {
    return false;
  }

  const compactKeyword = normalizedKeyword.replace(/\s+/g, "");
  return buildAnimeReleaseSearchTerms(anime).some((term) => {
    const normalizedTerm = normalizeReleaseSearchText(term);
    return normalizedTerm.includes(normalizedKeyword) || normalizedTerm.replace(/\s+/g, "").includes(compactKeyword);
  });
}

/** 判断输入是否完整对应番剧标题、原名或别名，用于维持已选择的追番关联。 */
export function isAnimeSearchTerm(anime: Anime, keyword: string): boolean {
  const normalizedKeyword = normalizeReleaseSearchText(keyword);
  return Boolean(
    normalizedKeyword &&
    buildAnimeReleaseSearchTerms(anime).some((term) => normalizeReleaseSearchText(term) === normalizedKeyword)
  );
}

/** 判断资源标题是否包含目标番剧的任一有效标题，过滤下载源的模糊误匹配。 */
export function matchesAnimeReleaseTitle(releaseTitle: string, animeTitleTerms: string[]): boolean {
  const normalizedTitle = normalizeReleaseSearchText(releaseTitle);
  const compactTitle = normalizedTitle.replace(/\s+/g, "");
  const terms = uniqueBySearchKey(animeTitleTerms.flatMap(expandSearchTerm))
    .map(normalizeReleaseSearchText)
    .filter(isDistinctiveSearchTerm);

  return terms.some((term) => {
    const compactTerm = term.replace(/\s+/g, "");
    return normalizedTitle.includes(term) || compactTitle.includes(compactTerm);
  });
}

/** 从标题中的中文季数、Season N、Nth Season 或 Sxx 标记解析系列季度。 */
export function detectSeriesSeasonNo(value: string): number | undefined {
  for (const pattern of seriesSeasonPatterns) {
    const matched = value.match(pattern)?.[1];
    if (!matched) {
      continue;
    }
    const seasonNo = parseSeasonNumber(matched);
    if (seasonNo !== undefined && seasonNo > 0) {
      return seasonNo;
    }
  }
  return undefined;
}

/** 从番剧标题、原名和别名中解析当前作品的系列季度。 */
export function resolveAnimeSeriesSeasonNo(anime: Anime): number | undefined {
  return [anime.title, anime.originalTitle, ...anime.aliases.map((alias) => alias.alias)]
    .filter((value): value is string => Boolean(value))
    .map(detectSeriesSeasonNo)
    .find((value) => value !== undefined);
}

/** 判断资源属于当前季度、待确认的其他资源，或明确冲突的旧季度。 */
export function classifyAnimeRelease(release: Release, anime: Anime): AnimeReleaseCompatibility {
  const targetSeasonNo = resolveAnimeSeriesSeasonNo(anime);
  const releaseSeasonNo = release.seriesSeasonNo ?? detectSeriesSeasonNo(release.title);
  if (targetSeasonNo !== undefined && releaseSeasonNo !== undefined && targetSeasonNo !== releaseSeasonNo) {
    return "mismatch";
  }
  if (targetSeasonNo !== undefined && targetSeasonNo > 1 && releaseSeasonNo === undefined) {
    return "other";
  }
  if (release.contentKind === "batch" && (targetSeasonNo === undefined || releaseSeasonNo === undefined)) {
    return "other";
  }
  return "current";
}

function expandSearchTerm(value: string): string[] {
  const trimmed = value.trim();
  if (!trimmed) {
    return [];
  }

  const terms = [trimmed];
  for (const match of trimmed.matchAll(bracketPairPattern)) {
    terms.push(match[1]);
  }

  terms.push(trimmed.replace(bracketPairPattern, " "));
  terms.push(...trimmed.split(separatorPattern));
  terms.push(stripSeasonSuffix(trimmed));
  terms.push(normalizeReleaseSearchText(trimmed));

  return terms.map((term) => term.trim()).filter(isUsefulSearchTerm);
}

function stripSeasonSuffix(value: string): string {
  let result = value.trim();
  for (const pattern of seasonSuffixPatterns) {
    result = result.replace(pattern, "").trim();
  }
  return result;
}

function isDistinctiveSearchTerm(value: string): boolean {
  const withoutSeason = stripSeasonSuffix(value);
  const compact = normalizeReleaseSearchText(withoutSeason).replace(/\s+/g, "");
  return compact.length >= 2 && /[\p{L}\p{N}]/u.test(compact);
}

function isUsefulSearchTerm(value: string): boolean {
  if (value.length < 2) {
    return false;
  }

  return /[\p{L}\p{N}]/u.test(value);
}

function uniqueBySearchKey(values: string[]): string[] {
  const seen = new Set<string>();
  const unique: string[] = [];

  for (const value of values) {
    const key = normalizeReleaseSearchText(value);
    if (!key || seen.has(key)) {
      continue;
    }

    seen.add(key);
    unique.push(value);
  }

  return unique;
}

/** 将阿拉伯数字或常见中文数字转换为系列季度编号。 */
function parseSeasonNumber(value: string): number | undefined {
  if (/^\d+$/.test(value)) {
    return Number(value);
  }

  const normalized = value.replace(/[〇零]/g, "").replace(/两/g, "二");
  const digits: Record<string, number> = { 一: 1, 二: 2, 三: 3, 四: 4, 五: 5, 六: 6, 七: 7, 八: 8, 九: 9 };
  if (normalized === "十") {
    return 10;
  }
  if (normalized.includes("十")) {
    const [tens, ones] = normalized.split("十");
    return (tens ? digits[tens] : 1) * 10 + (ones ? digits[ones] : 0);
  }
  return digits[normalized];
}
