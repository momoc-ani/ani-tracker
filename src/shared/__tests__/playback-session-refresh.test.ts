import { strict as assert } from "node:assert";
import { test } from "node:test";
import { startPlaybackSessionRefresh } from "../playback-session-refresh";

test("播放会话诊断立即刷新并在清理后丢弃迟到结果", async () => {
  let scheduled: (() => void) | undefined;
  let resolveRefresh: ((value: string) => void) | undefined;
  const sessions: string[] = [];
  const errors: unknown[] = [];
  let cancelledTimer: number | undefined;
  const refresh = () => new Promise<string>((resolve) => {
    resolveRefresh = resolve;
  });

  const stop = startPlaybackSessionRefresh({
    intervalMs: 2_000,
    refresh,
    onSession: (session) => sessions.push(session),
    onError: (error) => errors.push(error),
    schedule: (callback, intervalMs) => {
      assert.equal(intervalMs, 2_000);
      scheduled = callback;
      return 17;
    },
    cancel: (timer) => {
      cancelledTimer = timer;
    }
  });

  assert.ok(resolveRefresh, "启动时应立即刷新");
  resolveRefresh("initial");
  await Promise.resolve();
  assert.deepEqual(sessions, ["initial"]);

  scheduled?.();
  assert.ok(resolveRefresh, "定时器应继续刷新");
  stop();
  resolveRefresh("late");
  await Promise.resolve();

  assert.equal(cancelledTimer, 17);
  assert.deepEqual(sessions, ["initial"]);
  assert.deepEqual(errors, []);
});
