import { strict as assert } from "node:assert";
import { test } from "node:test";
import { planExternalPlayback } from "../external-playback-plan";

test("外部播放器仅在选中字幕时切换 HLS 并请求后增强烧录", () => {
  const enhancement = { videoEnhancement: "clear", frameInterpolation: "rife-realtime" } as const;
  assert.deepEqual(planExternalPlayback("direct", enhancement), {
    mode: "direct",
    enhancement,
    subtitleMode: "off"
  });
  assert.deepEqual(planExternalPlayback("direct", enhancement, " subtitle-3 "), {
    mode: "transcode",
    enhancement: { videoEnhancement: "off", frameInterpolation: "off" },
    subtitleMode: "burned",
    subtitleId: "subtitle-3"
  });
  assert.deepEqual(planExternalPlayback("transcode", enhancement, "subtitle-3"), {
    mode: "transcode",
    enhancement,
    subtitleMode: "burned",
    subtitleId: "subtitle-3"
  });
});
