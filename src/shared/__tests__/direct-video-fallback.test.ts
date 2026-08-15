import { strict as assert } from "node:assert";
import { test } from "node:test";
import {
  DIRECT_VIDEO_FRAME_TIMEOUT_MS,
  shouldFallbackDirectVideo
} from "../direct-video-fallback";

const stalledVideo = {
  mode: "direct",
  directEnhancementActive: false,
  playing: true,
  elapsedMs: DIRECT_VIDEO_FRAME_TIMEOUT_MS,
  mediaTimeProgressSeconds: 2,
  videoWidth: 0,
  videoHeight: 0
} as const;

test("直传只有音频进度且超时未出现视频帧时切换 HLS", () => {
  assert.equal(shouldFallbackDirectVideo(stalledVideo), true);
});

test("已有画面、暂停、HLS 或终端增强时不触发直传回退", () => {
  assert.equal(shouldFallbackDirectVideo({ ...stalledVideo, videoWidth: 1920, videoHeight: 1080 }), false);
  assert.equal(shouldFallbackDirectVideo({ ...stalledVideo, playing: false }), false);
  assert.equal(shouldFallbackDirectVideo({ ...stalledVideo, mode: "hls" }), false);
  assert.equal(shouldFallbackDirectVideo({ ...stalledVideo, directEnhancementActive: true }), false);
});

test("首帧等待时间或音频进度不足时继续等待", () => {
  assert.equal(shouldFallbackDirectVideo({
    ...stalledVideo,
    elapsedMs: DIRECT_VIDEO_FRAME_TIMEOUT_MS - 1
  }), false);
  assert.equal(shouldFallbackDirectVideo({ ...stalledVideo, mediaTimeProgressSeconds: 0.5 }), false);
});
