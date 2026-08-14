import { strict as assert } from "node:assert";
import { test } from "node:test";
import {
  DirectEnhancementMediaClock,
  evaluateDirectEnhancementMediaCandidate,
  normalizeDirectEnhancementVideoCodec
} from "../direct-enhancement-media";

test("F5-B 识别 MP4/WebM 首批 codec string", () => {
  assert.equal(normalizeDirectEnhancementVideoCodec("avc1.640028"), "h264");
  assert.equal(normalizeDirectEnhancementVideoCodec("vp09.00.10.08"), "vp9");
  assert.equal(normalizeDirectEnhancementVideoCodec("av01.0.08M.08"), "av1");
  assert.equal(normalizeDirectEnhancementVideoCodec("hvc1.2.4.L153"), undefined);
});

test("F5-B 拒绝 MKV、H.265 和 WebM/H.264", () => {
  assert.equal(evaluateDirectEnhancementMediaCandidate({
    container: "mkv",
    videoCodec: "vp09.00.10.08"
  }).supported, false);
  assert.equal(evaluateDirectEnhancementMediaCandidate({
    container: "mp4",
    videoCodec: "hvc1.2.4.L153"
  }).supported, false);
  assert.equal(evaluateDirectEnhancementMediaCandidate({
    container: "webm",
    videoCodec: "avc1.640028"
  }).supported, false);
});

test("F5-B 接受受控 MP4/WebM 组合并保留媒体元数据", () => {
  assert.deepEqual(evaluateDirectEnhancementMediaCandidate({
    container: "mp4",
    videoCodec: "av01.0.08M.08",
    audioCodec: "mp4a.40.2",
    durationSeconds: 1_440.5
  }), {
    supported: true,
    container: "mp4",
    videoCodec: "av1",
    audioCodec: "mp4a.40.2",
    durationSeconds: 1_440.5
  });
});

test("F5-B 媒体时钟保持暂停、倍速和拖动的绝对时间轴", () => {
  let now = 10;
  const clock = new DirectEnhancementMediaClock(() => now);

  clock.seek(30);
  clock.play();
  now = 12;
  assert.equal(clock.snapshot().positionSeconds, 32);

  clock.setPlaybackRate(2);
  now = 13.5;
  assert.equal(clock.snapshot().positionSeconds, 35);

  clock.pause();
  now = 20;
  assert.equal(clock.snapshot().positionSeconds, 35);

  clock.seek(5);
  assert.deepEqual(clock.snapshot(), {
    positionSeconds: 5,
    playbackRate: 2,
    running: false
  });
});

test("F5-B 媒体时钟拒绝非法位置和倍速", () => {
  const clock = new DirectEnhancementMediaClock(() => 0);
  assert.throws(() => clock.seek(-1), RangeError);
  assert.throws(() => clock.setPlaybackRate(0), RangeError);
  assert.throws(() => clock.setPlaybackRate(5), RangeError);
});
