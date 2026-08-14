import { strict as assert } from "node:assert";
import { test } from "node:test";
import {
  DirectEnhancementFrameQueue,
  DirectEnhancementMediaClock,
  DirectEnhancementPerformanceMonitor,
  evaluateDirectEnhancementMediaCandidate,
  normalizeDirectEnhancementVideoCodec
} from "../direct-enhancement-media";

test("F5-B 识别 MP4/WebM 首批 codec string", () => {
  assert.equal(normalizeDirectEnhancementVideoCodec("avc1.640028"), "h264");
  assert.equal(normalizeDirectEnhancementVideoCodec("vp09.00.10.08"), "vp9");
  assert.equal(normalizeDirectEnhancementVideoCodec("av01.0.08M.08"), "av1");
  assert.equal(normalizeDirectEnhancementVideoCodec("hvc1.2.4.L153"), undefined);
});

test("F5-D 帧队列按媒体时间选择最新帧并返回应丢弃帧", () => {
  const queue = new DirectEnhancementFrameQueue<{ id: string; timestampSeconds: number }>(4);
  queue.push({ id: "late", timestampSeconds: 1.08 });
  queue.push({ id: "first", timestampSeconds: 1 });
  queue.push({ id: "current", timestampSeconds: 1.02 });

  assert.deepEqual(queue.take(1), {
    frame: { id: "current", timestampSeconds: 1.02 },
    discarded: [{ id: "first", timestampSeconds: 1 }]
  });
  assert.deepEqual(queue.take(1.06), {
    frame: { id: "late", timestampSeconds: 1.08 },
    discarded: []
  });
});

test("F5-D 帧队列溢出时优先丢弃最旧帧", () => {
  const queue = new DirectEnhancementFrameQueue<{ id: number; timestampSeconds: number }>(2);
  queue.push({ id: 1, timestampSeconds: 1 });
  queue.push({ id: 2, timestampSeconds: 2 });
  assert.deepEqual(queue.push({ id: 3, timestampSeconds: 3 }), [
    { id: 1, timestampSeconds: 1 }
  ]);
  assert.equal(queue.size, 2);
  assert.deepEqual(queue.clear(), [
    { id: 2, timestampSeconds: 2 },
    { id: 3, timestampSeconds: 3 }
  ]);
});

test("F5-E 清晰档负载超限时先降到均衡档", () => {
  const monitor = new DirectEnhancementPerformanceMonitor();
  for (let index = 0; index < 10; index += 1) monitor.recordGpuQueueDuration(21 + index);

  const result = monitor.snapshot("clear");
  assert.equal(result.action, "degrade");
  assert.match(result.reason ?? "", /GPU 队列 P95/);
  assert.equal(result.gpuQueueP95Ms, 30);
});

test("F5-E 均衡档持续丢帧或音画漂移时退出增强", () => {
  const pressure = new DirectEnhancementPerformanceMonitor();
  for (let index = 0; index < 80; index += 1) {
    pressure.recordPresentation(index / 24, index / 24, 1);
  }
  assert.equal(pressure.snapshot("balanced").action, "fallback");

  const drift = new DirectEnhancementPerformanceMonitor();
  for (let index = 0; index < 12; index += 1) {
    drift.recordPresentation(index, index - 0.3);
  }
  const driftResult = drift.snapshot("clear");
  assert.equal(driftResult.action, "fallback");
  assert.match(driftResult.reason ?? "", /音画漂移/);
});

test("F5-E 短时压力不足以触发误降级", () => {
  const monitor = new DirectEnhancementPerformanceMonitor();
  for (let index = 0; index < 9; index += 1) monitor.recordGpuQueueDuration(40);
  for (let index = 0; index < 20; index += 1) {
    monitor.recordPresentation(index / 24, index / 24 - 0.01, 1);
  }
  assert.equal(monitor.snapshot("clear").action, "keep");
});

test("F5-E 帧预算随源帧率收紧且不按 GPU 品牌硬编码", () => {
  const source24Fps = new DirectEnhancementPerformanceMonitor();
  const source60Fps = new DirectEnhancementPerformanceMonitor();
  for (let index = 0; index < 60; index += 1) {
    source24Fps.recordPresentation(index / 24, index / 24);
    source60Fps.recordPresentation(index / 60, index / 60);
  }
  for (let index = 0; index < 10; index += 1) {
    source24Fps.recordGpuQueueDuration(14);
    source60Fps.recordGpuQueueDuration(14);
  }

  assert.equal(source24Fps.snapshot("balanced").action, "keep");
  assert.equal(source60Fps.snapshot("balanced").action, "fallback");
  assert.equal(Math.round(source24Fps.snapshot("balanced").frameBudgetMs * 10) / 10, 33.3);
  assert.equal(Math.round(source60Fps.snapshot("balanced").frameBudgetMs * 10) / 10, 13.3);
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
