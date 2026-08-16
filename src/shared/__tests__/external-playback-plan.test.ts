import { strict as assert } from "node:assert";
import { test } from "node:test";
import {
  planExternalPlayback,
  resolveExternalPlaybackStartPosition
} from "../external-playback-plan";

test("外部播放器直传始终保持完整原文件且不烧录默认字幕", () => {
  const enhancement = { videoEnhancement: "clear", frameInterpolation: "rife-realtime" } as const;
  assert.deepEqual(planExternalPlayback("direct", enhancement), {
    mode: "direct",
    enhancement,
    subtitleMode: "off"
  });
  assert.deepEqual(planExternalPlayback("direct", enhancement, " subtitle-3 "), {
    mode: "direct",
    enhancement,
    subtitleMode: "off"
  });
});

test("只有明确选择转码时才烧录当前字幕", () => {
  const enhancement = { videoEnhancement: "clear", frameInterpolation: "rife-realtime" } as const;
  assert.deepEqual(planExternalPlayback("transcode", enhancement, "subtitle-3"), {
    mode: "transcode",
    enhancement,
    subtitleMode: "burned",
    subtitleId: "subtitle-3"
  });
});

test("外部直传不携带网页进度，HLS 保留当前绝对时间", () => {
  assert.equal(resolveExternalPlaybackStartPosition("direct", 321.5), undefined);
  assert.equal(resolveExternalPlaybackStartPosition("transcode", 321.5), 321.5);
  assert.equal(resolveExternalPlaybackStartPosition("transcode", -1), undefined);
});
