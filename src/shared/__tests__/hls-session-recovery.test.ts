import { strict as assert } from "node:assert";
import { test } from "node:test";
import {
  MAX_AUTOMATIC_HLS_SESSION_RECOVERIES,
  planHlsSessionRecovery
} from "../hls-session-recovery";

test("HLS 中断只为当前会话生成有限次的绝对时间恢复计划", () => {
  assert.deepEqual(planHlsSessionRecovery({
    activeSessionId: "session-1",
    failedSessionId: "session-1",
    positionSeconds: 88.5,
    durationSeconds: 80,
    attempts: 0
  }), { positionSeconds: 80, nextAttempts: 1 });
  assert.equal(planHlsSessionRecovery({
    activeSessionId: "session-2",
    failedSessionId: "session-1",
    positionSeconds: 12,
    attempts: 0
  }), undefined);
  assert.equal(planHlsSessionRecovery({
    activeSessionId: "session-1",
    failedSessionId: "session-1",
    positionSeconds: 12,
    attempts: MAX_AUTOMATIC_HLS_SESSION_RECOVERIES
  }), undefined);
});
