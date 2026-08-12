import { strict as assert } from "node:assert";
import { test } from "node:test";
import { localizeDashboardAnimeTitles } from "../dashboard-title";
import type { DashboardData, MyAnime } from "../domain";

const japaneseTitle = "ヘルモード 2nd Season";
const chineseTitle = "地狱模式 第二季";

const followedAnime: MyAnime = {
  id: "my-anime-1",
  anime: {
    id: "anime-1",
    title: japaneseTitle,
    originalTitle: japaneseTitle,
    aliases: [
      {
        id: "alias-1",
        animeId: "anime-1",
        alias: chineseTitle,
        language: "zh",
        priority: 82
      }
    ],
    premiereYear: 2026,
    premiereMonth: 7,
    externalIds: {}
  },
  status: "watching",
  autoDownload: true,
  addedAt: "2026-07-01T00:00:00.000Z",
  updatedAt: "2026-08-06T00:00:00.000Z"
};

function dashboardFixture(): DashboardData {
  return {
    dailyReminder: {
      date: "2026-08-06",
      total: 1,
      upcoming: 0,
      aired: 1,
      downloading: 0,
      downloaded: 0,
      items: [
        {
          id: "daily-episode-1",
          animeId: "anime-1",
          animeTitle: japaneseTitle,
          episodeId: "episode-1",
          episodeNo: 6,
          status: "aired"
        }
      ]
    },
    todayEpisodes: [
      {
        id: "daily-episode-1",
        animeTitle: japaneseTitle,
        episodeNo: 6,
        status: "aired"
      }
    ],
    pendingActions: [
      {
        id: "pending-1",
        title: `《${japaneseTitle}》第 6 集`,
        description: `《${japaneseTitle}》第 6 集已开播，但默认字幕组还没有发布资源。`,
        severity: "warning",
        animeId: "anime-1",
        episodeId: "episode-1",
        episodeNo: 6
      }
    ],
    activeDownloads: [],
    recentCompleted: [],
    weeklySchedule: [
      {
        day: "周四",
        items: [
          {
            id: "daily-episode-1",
            animeTitle: japaneseTitle,
            episodeNo: 6,
            status: "aired"
          }
        ]
      }
    ],
    sourceHealth: []
  };
}

test("简体中文界面优先使用中文别名展示首页番剧标题", () => {
  const dashboard = dashboardFixture();
  const localized = localizeDashboardAnimeTitles(dashboard, [followedAnime]);

  assert.equal(localized.dailyReminder.items[0]?.animeTitle, chineseTitle);
  assert.equal(localized.todayEpisodes[0]?.animeTitle, chineseTitle);
  assert.equal(localized.weeklySchedule[0]?.items[0]?.animeTitle, chineseTitle);
  assert.equal(localized.pendingActions[0]?.title, `《${chineseTitle}》第 6 集`);
  assert.match(localized.pendingActions[0]?.description ?? "", new RegExp(chineseTitle));
  assert.equal(dashboard.todayEpisodes[0]?.animeTitle, japaneseTitle);
});

test("缺少追番元数据时保留后端返回的标题", () => {
  const dashboard = dashboardFixture();
  const localized = localizeDashboardAnimeTitles(dashboard, []);

  assert.equal(localized.dailyReminder.items[0]?.animeTitle, japaneseTitle);
  assert.equal(localized.todayEpisodes[0]?.animeTitle, japaneseTitle);
  assert.equal(localized.pendingActions[0]?.title, `《${japaneseTitle}》第 6 集`);
});
