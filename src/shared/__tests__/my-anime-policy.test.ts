import { strict as assert } from "node:assert";
import { test } from "node:test";
import type { MyAnime } from "../domain";
import { canAnimeStatusAutoDownload, normalizeMyAnimeAutoDownload } from "../my-anime-policy";

const followedAnime: MyAnime = {
  id: "my-anime-policy-1",
  anime: {
    id: "anime-policy-1",
    title: "测试番剧",
    aliases: [],
    premiereYear: 2026,
    premiereMonth: 10,
    externalIds: {}
  },
  status: "watching",
  autoDownload: true,
  addedAt: "2026-10-01T00:00:00.000Z",
  updatedAt: "2026-10-27T00:00:00.000Z"
};

test("在追和暂停状态允许保持自动下载", () => {
  assert.equal(canAnimeStatusAutoDownload("watching"), true);
  assert.equal(canAnimeStatusAutoDownload("paused"), true);
  assert.equal(normalizeMyAnimeAutoDownload({ ...followedAnime, status: "paused" }).autoDownload, true);
});

test("已完成和已弃状态会关闭自动下载", () => {
  assert.equal(canAnimeStatusAutoDownload("completed"), false);
  assert.equal(canAnimeStatusAutoDownload("dropped"), false);
  assert.equal(normalizeMyAnimeAutoDownload({ ...followedAnime, status: "completed" }).autoDownload, false);
  assert.equal(normalizeMyAnimeAutoDownload({ ...followedAnime, status: "dropped" }).autoDownload, false);
});
