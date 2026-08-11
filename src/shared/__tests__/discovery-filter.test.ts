import { strict as assert } from "node:assert";
import { test } from "node:test";
import {
  countDiscoveryBrowseFilters,
  createEmptyDiscoveryBrowseFilters,
  filterDiscoveryBrowseItems
} from "../discovery-filter";
import type { Anime } from "../domain";

const anime: Anime[] = [
  {
    id: "bangumi-1",
    title: "日本科幻番",
    aliases: [],
    premiereDate: "2025-01-02",
    premiereYear: 2025,
    premiereMonth: 1,
    rating: { score: 8.6, count: 5000, source: "bangumi" },
    externalIds: { bangumi: "1" },
    detail: {
      format: "tv",
      sourceMaterial: "MANGA",
      genres: ["Sci-Fi", "Action"],
      demographic: "Shounen",
      countryOfOrigin: "JP",
      airingStatus: "airing",
      ranking: { rank: 12, source: "bangumi" },
      metadataSources: ["bangumi"]
    }
  },
  {
    id: "anilist-2",
    title: "中国原创动画",
    aliases: [],
    premiereDate: "2026-04-01",
    premiereYear: 2026,
    premiereMonth: 4,
    rating: { score: 9.1, source: "anilist" },
    externalIds: { anilist: "2" },
    detail: {
      format: "ona",
      sourceMaterial: "ORIGINAL",
      genres: ["Fantasy"],
      countryOfOrigin: "CN",
      airingStatus: "finished"
    }
  },
  {
    id: "sparse-3",
    title: "资料待补全",
    aliases: [],
    premiereYear: 2024,
    premiereMonth: 7,
    externalIds: {}
  }
];

test("分类浏览只让具备真实元数据的番剧命中筛选", () => {
  const filters = createEmptyDiscoveryBrowseFilters();
  filters.formats = ["tv"];
  filters.sourceMaterials = ["manga"];
  filters.genres = ["sciFi"];
  filters.regions = ["japan"];
  filters.airingStatuses = ["airing"];
  filters.minRating = 8;

  assert.deepEqual(
    filterDiscoveryBrowseItems(anime, "科幻", filters, "rating").map((item) => item.id),
    ["bangumi-1"]
  );
  assert.equal(countDiscoveryBrowseFilters(filters), 6);
});

test("Bangumi 排名优先使用真实排名，缺失时再按评分排列", () => {
  const result = filterDiscoveryBrowseItems(anime, "", createEmptyDiscoveryBrowseFilters(), "bangumiRank");
  assert.equal(result[0]?.id, "bangumi-1");
  assert.equal(result[1]?.id, "anilist-2");
});

test("地区筛选兼容 ISO 国家码并排除缺失地区的目录", () => {
  const filters = createEmptyDiscoveryBrowseFilters();
  filters.regions = ["china"];
  assert.deepEqual(
    filterDiscoveryBrowseItems(anime, "", filters, "recent").map((item) => item.id),
    ["anilist-2"]
  );
});

test("题材筛选区分推理和悬疑，并覆盖 Bangumi 长尾题材", () => {
  const reasoningAnime: Anime = {
    ...anime[0],
    id: "reasoning-4",
    title: "推理番",
    detail: { ...anime[0].detail, genres: ["推理"] }
  };
  const mysteryAnime: Anime = {
    ...anime[0],
    id: "mystery-5",
    title: "悬疑番",
    detail: { ...anime[0].detail, genres: ["悬疑"] }
  };

  const reasoningFilters = createEmptyDiscoveryBrowseFilters();
  reasoningFilters.genres = ["reasoning"];
  assert.deepEqual(
    filterDiscoveryBrowseItems([reasoningAnime, mysteryAnime], "", reasoningFilters, "rating").map((item) => item.id),
    ["reasoning-4"]
  );

  const mechaFilters = createEmptyDiscoveryBrowseFilters();
  mechaFilters.genres = ["mecha"];
  const mechaAnime = { ...reasoningAnime, id: "mecha-6", detail: { ...reasoningAnime.detail, genres: ["机战"] } };
  assert.deepEqual(
    filterDiscoveryBrowseItems([reasoningAnime, mechaAnime], "", mechaFilters, "rating").map((item) => item.id),
    ["mecha-6"]
  );
});

test("时间筛选支持未来年份和更早年份区间", () => {
  const futureFilters = createEmptyDiscoveryBrowseFilters();
  futureFilters.yearRange = { kind: "future", startYear: 2026 };
  assert.deepEqual(
    filterDiscoveryBrowseItems(anime, "", futureFilters, "recent").map((item) => item.id),
    ["anilist-2"]
  );

  const earlierFilters = createEmptyDiscoveryBrowseFilters();
  earlierFilters.yearRange = { kind: "earlier", endYear: 2025 };
  assert.deepEqual(
    filterDiscoveryBrowseItems(anime, "", earlierFilters, "recent").map((item) => item.id),
    ["sparse-3"]
  );
});
