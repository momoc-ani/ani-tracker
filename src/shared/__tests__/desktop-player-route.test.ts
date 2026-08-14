import { strict as assert } from "node:assert";
import { test } from "node:test";
import {
  createDesktopPlayerSearchParams,
  resolveDesktopPlayerWindowInput
} from "../desktop-player-route";

test("桌面播放器路由参数可往返解析", () => {
  const params = createDesktopPlayerSearchParams({ taskId: "task:episode-02", fileIndex: 3 });

  assert.deepEqual(resolveDesktopPlayerWindowInput(`?${params.toString()}`), {
    taskId: "task:episode-02",
    fileIndex: 3
  });
});

test("桌面播放器路由拒绝错误视图和异常文件索引", () => {
  assert.equal(resolveDesktopPlayerWindowInput("?aniView=page&taskId=task-1"), null);
  assert.equal(resolveDesktopPlayerWindowInput("?aniView=desktop-player&taskId=task-1&fileIndex=-1"), null);
  assert.equal(resolveDesktopPlayerWindowInput("?aniView=desktop-player&taskId=%2Ftmp%2Fvideo"), null);
});
