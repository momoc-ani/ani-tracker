import { strict as assert } from "node:assert";
import { test } from "node:test";
import { buildRemoteMediaSessionRequest } from "../remote-media-session-request";

const enhancement = {
  videoEnhancement: "off",
  frameInterpolation: "off"
} as const;

test("普通直传请求不发送旧网关无法识别的字幕字段", () => {
  assert.deepEqual(buildRemoteMediaSessionRequest({
    taskId: "task-1",
    mode: "direct",
    fileIndex: 2,
    enhancement,
    startPositionSeconds: 120
  }), {
    taskId: "task-1",
    mode: "direct",
    enhancement,
    fileIndex: 2,
    startPositionSeconds: 120
  });
});

test("外部播放器未选字幕时同样保持旧请求协议", () => {
  assert.deepEqual(buildRemoteMediaSessionRequest({
    taskId: "task-2",
    mode: "direct",
    enhancement
  }), {
    taskId: "task-2",
    mode: "direct",
    enhancement
  });
});

test("只有字幕烧录请求携带新增字幕字段", () => {
  assert.deepEqual(buildRemoteMediaSessionRequest({
    taskId: "task-3",
    mode: "transcode",
    enhancement,
    subtitleMode: "burned",
    subtitleId: "subtitle-1"
  }), {
    taskId: "task-3",
    mode: "transcode",
    enhancement,
    subtitleMode: "burned",
    subtitleId: "subtitle-1"
  });
});
