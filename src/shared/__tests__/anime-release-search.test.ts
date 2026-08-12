import { strict as assert } from "node:assert";
import { test } from "node:test";
import { classifyAnimeRelease } from "../anime-release-search";
import type { Anime, Release } from "../domain";

const unmarkedRelease: Release = {
  id: "release-lolihouse-hell-mode-08",
  title: "[LoliHouse] 地狱模式～喜欢速通游戏的玩家在废设定异世界无双～ / Hell Mode - 08 [WebRip 1080p HEVC-10bit AAC][简繁内封字幕]",
  episodeNo: 8,
  contentKind: "episode",
  sourceId: "nyaa",
  sourceName: "Nyaa",
  publishedAt: "2026-08-03T00:00:00.000Z",
};

test("续作将未标季数的同名资源归入季度待确认", () => {
  assert.equal(classifyAnimeRelease(unmarkedRelease, anime("地狱模式 第二季", "Hell Mode 2nd Season")), "other");
});

test("续作仍接受明确标注当前季的资源", () => {
  assert.equal(classifyAnimeRelease({ ...unmarkedRelease, seriesSeasonNo: 2 }, anime("地狱模式 第二季", "Hell Mode 2nd Season")), "current");
});

test("第一季保持接受未标季数的单集资源", () => {
  assert.equal(classifyAnimeRelease(unmarkedRelease, anime("地狱模式 第一季", "Hell Mode 1st Season")), "current");
});

/** 创建季度分类测试所需的最小番剧数据。 */
function anime(title: string, originalTitle: string): Anime {
  return {
    id: "anime-hell-mode",
    title,
    originalTitle,
    aliases: [],
    premiereYear: 2026,
    premiereMonth: 7,
    externalIds: {},
  };
}
