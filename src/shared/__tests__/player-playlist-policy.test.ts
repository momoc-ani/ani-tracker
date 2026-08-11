import { strict as assert } from "node:assert";
import { test } from "node:test";
import {
  groupEpisodePlaylistItems,
  resolveAdjacentEpisodeItem,
  type EpisodePlaylistItemLike
} from "../player-playlist-policy";

interface TestPlaylistItem extends EpisodePlaylistItemLike {
  fileIndex: number;
}

function item(
  id: string,
  episodeNo: number | undefined,
  taskId: string,
  fansubGroupId?: string,
  fansubName?: string
): TestPlaylistItem {
  return {
    id,
    episodeNo,
    fileIndex: Number(id.replace(/\D/g, "")) || 0,
    task: { id: taskId, fansubGroupId, fansubName }
  };
}

test("groupEpisodePlaylistItems 保留同集的全部字幕组版本", () => {
  const loliHouse = item("episode-4-a", 4, "task-a", "fansub-a", "LoliHouse");
  const otherFansub = item("episode-4-b", 4, "task-b", "fansub-b", "其他字幕组");

  const groups = groupEpisodePlaylistItems([3, 4, 5], [loliHouse, otherFansub]);

  assert.deepEqual(groups.map((group) => group.episodeNo), [3, 4, 5]);
  assert.deepEqual(groups[1]?.items, [loliHouse, otherFansub]);
  assert.deepEqual(groups[0]?.items, []);
});

test("resolveAdjacentEpisodeItem 跳过相同集数并优先同一合集任务", () => {
  const active = item("episode-3-a", 3, "batch-a", "fansub-a", "LoliHouse");
  const sameEpisode = item("episode-3-b", 3, "task-b", "fansub-b", "其他字幕组");
  const sameFansub = item("episode-4-b", 4, "task-c", "fansub-a", "LoliHouse");
  const sameBatch = item("episode-4-a", 4, "batch-a", "fansub-z", "合集字幕组");

  assert.equal(
    resolveAdjacentEpisodeItem([active, sameEpisode, sameFansub, sameBatch], active, "next")?.id,
    sameBatch.id
  );
});

test("resolveAdjacentEpisodeItem 在不同任务间优先字幕组标识", () => {
  const active = item("episode-3-a", 3, "task-a", "fansub-a", "LoliHouse");
  const otherFansub = item("episode-4-b", 4, "task-b", "fansub-b", "其他字幕组");
  const sameFansub = item("episode-4-a", 4, "task-c", "fansub-a", "改名后的字幕组");

  assert.equal(
    resolveAdjacentEpisodeItem([active, otherFansub, sameFansub], active, "next")?.id,
    sameFansub.id
  );
});

test("resolveAdjacentEpisodeItem 缺少字幕组标识时匹配规范化名称", () => {
  const active = item("episode-4-a", 4, "task-a", undefined, " LoliHouse ");
  const sameFansub = item("episode-3-a", 3, "task-b", undefined, "lolihouse");
  const otherFansub = item("episode-3-b", 3, "task-c", undefined, "其他字幕组");

  assert.equal(
    resolveAdjacentEpisodeItem([sameFansub, otherFansub, active], active, "previous")?.id,
    sameFansub.id
  );
});

test("resolveAdjacentEpisodeItem 对无集数内容保留相邻列表行为", () => {
  const first = item("special-1", undefined, "task-a");
  const second = item("special-2", undefined, "task-b");

  assert.equal(resolveAdjacentEpisodeItem([first, second], first, "next")?.id, second.id);
  assert.equal(resolveAdjacentEpisodeItem([first, second], second, "previous")?.id, first.id);
});

test("resolveAdjacentEpisodeItem 不接受播放列表之外的活动项", () => {
  const active = item("episode-3-a", 3, "task-a");
  const next = item("episode-4-a", 4, "task-a");

  assert.equal(resolveAdjacentEpisodeItem([next], active, "next"), undefined);
});
