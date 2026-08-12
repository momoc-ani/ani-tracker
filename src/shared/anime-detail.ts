import type {
  AnimeAiringStatus,
  AnimeBroadcastSchedule,
  AnimeDetailMetadata,
  AnimeFormat,
  AnimeRanking,
  AnimeStaffCredit
} from "./domain";

const animeFormats = new Set<AnimeFormat>(["tv", "movie", "ova", "ona", "special", "music", "unknown"]);
const airingStatuses = new Set<AnimeAiringStatus>([
  "upcoming",
  "airing",
  "finished",
  "hiatus",
  "cancelled",
  "unknown"
]);

/** 将未知详情数据收敛为可安全展示和持久化的字段。 */
export function normalizeAnimeDetailMetadata(value: unknown): AnimeDetailMetadata | undefined {
  if (!isRecord(value)) {
    return undefined;
  }

  const detail: AnimeDetailMetadata = compact({
    bannerUrl: readString(value.bannerUrl),
    format: readEnum(value.format, animeFormats),
    episodeCount: readPositiveInteger(value.episodeCount),
    airingStatus: readEnum(value.airingStatus, airingStatuses),
    endDate: readDateString(value.endDate),
    nextAiringAt: readFutureDateTime(value.nextAiringAt),
    nextAiringEpisodeNo: readPositiveInteger(value.nextAiringEpisodeNo),
    broadcast: normalizeBroadcast(value.broadcast),
    genres: normalizeStringList(value.genres),
    studios: normalizeStringList(value.studios),
    staff: normalizeStaff(value.staff),
    sourceMaterial: readString(value.sourceMaterial),
    durationMinutes: readPositiveInteger(value.durationMinutes),
    contentRating: readString(value.contentRating),
    demographic: readString(value.demographic),
    countryOfOrigin: readString(value.countryOfOrigin),
    ranking: normalizeRanking(value.ranking),
    metadataSources: normalizeStringList(value.metadataSources),
    refreshedAt: readDateTime(value.refreshedAt)
  });

  return Object.keys(detail).length ? detail : undefined;
}

/** 按主来源优先规则合并详情，数组字段保留稳定顺序并去重。 */
export function mergeAnimeDetailMetadata(
  primary: AnimeDetailMetadata | undefined,
  secondary: AnimeDetailMetadata | undefined
): AnimeDetailMetadata | undefined {
  const left = normalizeAnimeDetailMetadata(primary);
  const right = normalizeAnimeDetailMetadata(secondary);
  if (!left) return right;
  if (!right) return left;

  const nextAiring = pickNextAiring(left, right);

  return normalizeAnimeDetailMetadata({
    ...right,
    ...left,
    genres: mergeStrings(left.genres, right.genres),
    studios: mergeStrings(left.studios, right.studios),
    staff: mergeStaff(left.staff, right.staff),
    metadataSources: mergeStrings(left.metadataSources, right.metadataSources),
    nextAiringAt: nextAiring.at,
    nextAiringEpisodeNo: nextAiring.episodeNo,
    refreshedAt: pickLatestDateTime(left.refreshedAt, right.refreshedAt)
  });
}

/** 选择时间更晚且带集数的播出锚点，避免合并后时间与集数来自不同来源。 */
function pickNextAiring(
  left: AnimeDetailMetadata,
  right: AnimeDetailMetadata
): { at?: string; episodeNo?: number } {
  const candidates = [left, right]
    .filter((item) => item.nextAiringAt)
    .sort((a, b) => Date.parse(b.nextAiringAt!) - Date.parse(a.nextAiringAt!));
  const complete = candidates.find((item) => item.nextAiringEpisodeNo);
  const selected = complete ?? candidates[0];
  return {
    at: selected?.nextAiringAt,
    episodeNo: selected?.nextAiringEpisodeNo
  };
}

function normalizeBroadcast(value: unknown): AnimeBroadcastSchedule | undefined {
  if (!isRecord(value)) return undefined;
  const weekday = Number.isInteger(value.weekday) && Number(value.weekday) >= 0 && Number(value.weekday) <= 6
    ? Number(value.weekday)
    : undefined;
  const broadcast = compact({ weekday, time: readClockTime(value.time), timezone: readString(value.timezone) });
  return Object.keys(broadcast).length ? broadcast : undefined;
}

function normalizeStaff(value: unknown): AnimeStaffCredit[] | undefined {
  if (!Array.isArray(value)) return undefined;
  const credits: AnimeStaffCredit[] = [];
  const seen = new Set<string>();
  for (const item of value) {
    if (!isRecord(item)) continue;
    const name = readString(item.name);
    const role = readString(item.role);
    if (!name || !role) continue;
    const key = `${name.toLocaleLowerCase()}\u0000${role.toLocaleLowerCase()}`;
    if (seen.has(key)) continue;
    seen.add(key);
    credits.push(compact({ name, role, source: readString(item.source) }));
  }
  return credits.length ? credits : undefined;
}

function normalizeRanking(value: unknown): AnimeRanking | undefined {
  if (!isRecord(value)) return undefined;
  const rank = readPositiveInteger(value.rank);
  const source = readString(value.source);
  return rank && source ? compact({ rank, source, category: readString(value.category) }) : undefined;
}

function mergeStrings(left: string[] | undefined, right: string[] | undefined): string[] | undefined {
  return normalizeStringList([...(left ?? []), ...(right ?? [])]);
}

function mergeStaff(left: AnimeStaffCredit[] | undefined, right: AnimeStaffCredit[] | undefined) {
  return normalizeStaff([...(left ?? []), ...(right ?? [])]);
}

function normalizeStringList(value: unknown): string[] | undefined {
  if (!Array.isArray(value)) return undefined;
  const items: string[] = [];
  const seen = new Set<string>();
  for (const entry of value) {
    const item = readString(entry);
    const key = item?.toLocaleLowerCase();
    if (!item || !key || seen.has(key)) continue;
    seen.add(key);
    items.push(item);
  }
  return items.length ? items : undefined;
}

function readEnum<T extends string>(value: unknown, allowed: Set<T>): T | undefined {
  return typeof value === "string" && allowed.has(value as T) ? value as T : undefined;
}

function readString(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const normalized = value.trim();
  return normalized || undefined;
}

function readPositiveInteger(value: unknown): number | undefined {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0 ? value : undefined;
}

function readClockTime(value: unknown): string | undefined {
  const time = readString(value);
  return time && /^(?:[01]\d|2[0-3]):[0-5]\d$/.test(time) ? time : undefined;
}

function readDateString(value: unknown): string | undefined {
  const date = readString(value);
  return date && /^\d{4}-\d{2}-\d{2}$/.test(date) && Number.isFinite(Date.parse(`${date}T00:00:00Z`))
    ? date
    : undefined;
}

function readDateTime(value: unknown): string | undefined {
  const dateTime = readString(value);
  return dateTime && Number.isFinite(Date.parse(dateTime)) ? new Date(dateTime).toISOString() : undefined;
}

function readFutureDateTime(value: unknown): string | undefined {
  const dateTime = readDateTime(value);
  return dateTime && Date.parse(dateTime) > Date.now() ? dateTime : undefined;
}

function pickLatestDateTime(left: string | undefined, right: string | undefined): string | undefined {
  if (!left) return right;
  if (!right) return left;
  return Date.parse(left) >= Date.parse(right) ? left : right;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value && typeof value === "object" && !Array.isArray(value));
}

function compact<T extends Record<string, unknown>>(value: T): T {
  return Object.fromEntries(Object.entries(value).filter(([, item]) => item !== undefined)) as T;
}
