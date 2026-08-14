export type DirectEnhancementContainer = "mp4" | "webm";
export type DirectEnhancementVideoCodec = "h264" | "vp9" | "av1";

export interface DirectEnhancementMediaCandidate {
  container: string;
  videoCodec?: string | null;
  audioCodec?: string | null;
  durationSeconds?: number | null;
}

export interface DirectEnhancementMediaSupport {
  supported: boolean;
  container?: DirectEnhancementContainer;
  videoCodec?: DirectEnhancementVideoCodec;
  audioCodec?: string;
  durationSeconds?: number;
  reason?: string;
}

export interface DirectEnhancementClockSnapshot {
  positionSeconds: number;
  playbackRate: number;
  running: boolean;
}

export interface DirectEnhancementQueuedFrame {
  timestampSeconds: number;
}

export interface DirectEnhancementFrameSelection<T extends DirectEnhancementQueuedFrame> {
  frame?: T;
  discarded: T[];
}

export interface DirectEnhancementSubtitleCue {
  startSeconds: number;
  endSeconds: number;
  text: string;
}

export type DirectEnhancementPerformanceAction = "keep" | "degrade" | "fallback";

export interface DirectEnhancementPerformanceSnapshot {
  action: DirectEnhancementPerformanceAction;
  currentAvDriftMs: number;
  maximumAvDriftMs: number;
  droppedFrameRatio: number;
  frameBudgetMs: number;
  gpuQueueP95Ms?: number;
  reason?: string;
}

interface DirectEnhancementFrameOutcome {
  droppedFrames: number;
  renderedFrames: number;
}

const DIRECT_ENHANCEMENT_DRIFT_LIMIT_MS = 250;
const DIRECT_ENHANCEMENT_DRIFT_VIOLATION_LIMIT = 12;
const DIRECT_ENHANCEMENT_GPU_SAMPLE_LIMIT = 60;
const DIRECT_ENHANCEMENT_GPU_SAMPLE_MINIMUM = 10;
const DIRECT_ENHANCEMENT_FRAME_INTERVAL_LIMIT = 60;
const DIRECT_ENHANCEMENT_OUTCOME_LIMIT = 120;
const DIRECT_ENHANCEMENT_DEFAULT_FRAME_BUDGET_MS = 32;
const DIRECT_ENHANCEMENT_MAX_FRAME_BUDGET_MS = 1000 / 30;

/** 汇总短窗口性能样本，并给出清晰档降级或增强链退出建议。 */
export class DirectEnhancementPerformanceMonitor {
  private readonly gpuQueueDurationsMs: number[] = [];
  private readonly frameIntervalsMs: number[] = [];
  private readonly frameOutcomes: DirectEnhancementFrameOutcome[] = [];
  private consecutiveDriftViolations = 0;
  private currentAvDriftMs = 0;
  private maximumAvDriftMs = 0;
  private previousFrameTimeSeconds?: number;

  recordPresentation(
    mediaTimeSeconds: number,
    frameTimeSeconds: number,
    droppedFrames = 0
  ): void {
    if (
      !Number.isFinite(mediaTimeSeconds)
      || !Number.isFinite(frameTimeSeconds)
      || !Number.isSafeInteger(droppedFrames)
      || droppedFrames < 0
    ) {
      throw new RangeError("直传增强性能样本无效");
    }
    this.recordClockDrift(mediaTimeSeconds, frameTimeSeconds);
    if (this.previousFrameTimeSeconds !== undefined) {
      const frameIntervalMs = (frameTimeSeconds - this.previousFrameTimeSeconds) * 1_000;
      if (frameIntervalMs >= 4 && frameIntervalMs <= 250) {
        this.frameIntervalsMs.push(frameIntervalMs);
        trimOldest(this.frameIntervalsMs, DIRECT_ENHANCEMENT_FRAME_INTERVAL_LIMIT);
      }
    }
    this.previousFrameTimeSeconds = frameTimeSeconds;
    this.frameOutcomes.push({ droppedFrames, renderedFrames: 1 });
    trimOldest(this.frameOutcomes, DIRECT_ENHANCEMENT_OUTCOME_LIMIT);
  }

  recordClockDrift(mediaTimeSeconds: number, frameTimeSeconds: number): void {
    if (!Number.isFinite(mediaTimeSeconds) || !Number.isFinite(frameTimeSeconds)) {
      throw new RangeError("直传增强时钟样本无效");
    }
    this.currentAvDriftMs = Math.abs(mediaTimeSeconds - frameTimeSeconds) * 1_000;
    this.maximumAvDriftMs = Math.max(this.maximumAvDriftMs, this.currentAvDriftMs);
    this.consecutiveDriftViolations = this.currentAvDriftMs > DIRECT_ENHANCEMENT_DRIFT_LIMIT_MS
      ? this.consecutiveDriftViolations + 1
      : 0;
  }

  recordGpuQueueDuration(durationMs: number): void {
    if (!Number.isFinite(durationMs) || durationMs < 0) {
      throw new RangeError("GPU 队列耗时样本无效");
    }
    this.gpuQueueDurationsMs.push(durationMs);
    trimOldest(this.gpuQueueDurationsMs, DIRECT_ENHANCEMENT_GPU_SAMPLE_LIMIT);
  }

  snapshot(preset: "balanced" | "clear"): DirectEnhancementPerformanceSnapshot {
    const droppedFrames = this.frameOutcomes.reduce((sum, item) => sum + item.droppedFrames, 0);
    const renderedFrames = this.frameOutcomes.reduce((sum, item) => sum + item.renderedFrames, 0);
    const totalFrames = droppedFrames + renderedFrames;
    const droppedFrameRatio = totalFrames > 0 ? droppedFrames / totalFrames : 0;
    const gpuQueueP95Ms = percentile(this.gpuQueueDurationsMs, 0.95);
    const sourceFrameIntervalMs = percentile(this.frameIntervalsMs, 0.5);
    const frameBudgetMs = sourceFrameIntervalMs === undefined
      ? DIRECT_ENHANCEMENT_DEFAULT_FRAME_BUDGET_MS
      : Math.min(DIRECT_ENHANCEMENT_MAX_FRAME_BUDGET_MS, sourceFrameIntervalMs * 0.8);
    const base = {
      currentAvDriftMs: this.currentAvDriftMs,
      maximumAvDriftMs: this.maximumAvDriftMs,
      droppedFrameRatio,
      frameBudgetMs,
      ...(gpuQueueP95Ms === undefined ? {} : { gpuQueueP95Ms })
    };

    if (this.consecutiveDriftViolations >= DIRECT_ENHANCEMENT_DRIFT_VIOLATION_LIMIT) {
      return {
        ...base,
        action: "fallback",
        reason: `连续音画漂移超过 ${DIRECT_ENHANCEMENT_DRIFT_LIMIT_MS} ms`
      };
    }
    const enoughGpuSamples = this.gpuQueueDurationsMs.length >= DIRECT_ENHANCEMENT_GPU_SAMPLE_MINIMUM;
    const enoughFrameSamples = totalFrames >= DIRECT_ENHANCEMENT_OUTCOME_LIMIT;
    if (
      preset === "clear"
      && (
        (
          enoughGpuSamples
          && gpuQueueP95Ms !== undefined
          && gpuQueueP95Ms > frameBudgetMs * 0.75
        )
        || (enoughFrameSamples && droppedFrameRatio > 0.2)
      )
    ) {
      return {
        ...base,
        action: "degrade",
        reason: gpuQueueP95Ms !== undefined && gpuQueueP95Ms > frameBudgetMs * 0.75
          ? `GPU 队列 P95 ${gpuQueueP95Ms.toFixed(1)} ms 超过清晰档 ${frameBudgetMs.toFixed(1)} ms 帧预算`
          : `丢帧比例 ${(droppedFrameRatio * 100).toFixed(1)}% 超过清晰档预算`
      };
    }
    if (
      preset === "balanced"
      && (
        (enoughGpuSamples && gpuQueueP95Ms !== undefined && gpuQueueP95Ms > frameBudgetMs)
        || (enoughFrameSamples && droppedFrameRatio > 0.35)
      )
    ) {
      return {
        ...base,
        action: "fallback",
        reason: gpuQueueP95Ms !== undefined && gpuQueueP95Ms > frameBudgetMs
          ? `GPU 队列 P95 ${gpuQueueP95Ms.toFixed(1)} ms 超过均衡档 ${frameBudgetMs.toFixed(1)} ms 帧预算`
          : `丢帧比例 ${(droppedFrameRatio * 100).toFixed(1)}% 超过均衡档预算`
      };
    }
    return { ...base, action: "keep" };
  }

  /** 清晰档自动降级后重新采样负载，但保留会话最大音画漂移。 */
  resetLoadWindow(): void {
    this.gpuQueueDurationsMs.length = 0;
    this.frameOutcomes.length = 0;
  }
}

/** 按媒体时间选择最新可展示帧，并保证解码输出队列有界。 */
export class DirectEnhancementFrameQueue<T extends DirectEnhancementQueuedFrame> {
  private readonly frames: T[] = [];

  constructor(
    private readonly maximumFrames = 8,
    private readonly presentationLeadSeconds = 0.025
  ) {
    if (!Number.isSafeInteger(maximumFrames) || maximumFrames < 2) {
      throw new RangeError("直传增强帧队列上限至少为 2");
    }
  }

  get size(): number {
    return this.frames.length;
  }

  push(frame: T): T[] {
    if (!Number.isFinite(frame.timestampSeconds)) {
      throw new RangeError("视频帧时间戳必须是有限数");
    }
    const insertionIndex = this.frames.findIndex(
      (item) => item.timestampSeconds > frame.timestampSeconds
    );
    if (insertionIndex < 0) this.frames.push(frame);
    else this.frames.splice(insertionIndex, 0, frame);

    const overflow = Math.max(0, this.frames.length - this.maximumFrames);
    return overflow > 0 ? this.frames.splice(0, overflow) : [];
  }

  take(positionSeconds: number): DirectEnhancementFrameSelection<T> {
    if (!Number.isFinite(positionSeconds) || positionSeconds < 0) {
      throw new RangeError("展示位置必须是非负有限数");
    }
    let selectedIndex = -1;
    const maximumTimestamp = positionSeconds + this.presentationLeadSeconds;
    for (let index = 0; index < this.frames.length; index += 1) {
      if (this.frames[index].timestampSeconds > maximumTimestamp) break;
      selectedIndex = index;
    }
    if (selectedIndex < 0) return { discarded: [] };

    const ready = this.frames.splice(0, selectedIndex + 1);
    return {
      frame: ready.pop(),
      discarded: ready
    };
  }

  clear(): T[] {
    return this.frames.splice(0);
  }
}

/** 解析受控 ASS 转换结果或原生 WebVTT，供独立音频时钟驱动 DOM 字幕。 */
export function parseDirectEnhancementSubtitleCues(
  vttText: string
): DirectEnhancementSubtitleCue[] {
  const blocks = vttText.replace(/^\uFEFF/, "").split(/\r?\n\s*\r?\n/);
  const cues: DirectEnhancementSubtitleCue[] = [];
  for (const block of blocks) {
    const lines = block.split(/\r?\n/).map((line) => line.trimEnd());
    const timingIndex = lines.findIndex((line) => line.includes("-->"));
    if (timingIndex < 0) continue;
    const [rawStart, rawEnd] = lines[timingIndex].split("-->").map((value) => value.trim());
    const startSeconds = parseSubtitleTimestamp(rawStart);
    const endSeconds = parseSubtitleTimestamp(rawEnd.split(/\s+/)[0]);
    const text = lines.slice(timingIndex + 1).join("\n").trim();
    if (startSeconds === undefined || endSeconds === undefined || endSeconds <= startSeconds || !text) continue;
    cues.push({ startSeconds, endSeconds, text });
  }
  return cues.sort((left, right) => left.startSeconds - right.startSeconds);
}

function trimOldest<T>(values: T[], maximumLength: number): void {
  const overflow = values.length - maximumLength;
  if (overflow > 0) values.splice(0, overflow);
}

function percentile(values: number[], fraction: number): number | undefined {
  if (values.length === 0) return undefined;
  const sorted = [...values].sort((left, right) => left - right);
  const index = Math.max(0, Math.ceil(sorted.length * fraction) - 1);
  return sorted[index];
}

function parseSubtitleTimestamp(value: string): number | undefined {
  const parts = value.replace(",", ".").split(":");
  if (parts.length < 2 || parts.length > 3) return undefined;
  const seconds = Number(parts.pop());
  const minutes = Number(parts.pop());
  const hours = parts.length === 1 ? Number(parts[0]) : 0;
  if (![hours, minutes, seconds].every(Number.isFinite)) return undefined;
  return hours * 3_600 + minutes * 60 + seconds;
}

/** 将容器返回的 codec string 约束到首批 WebCodecs 视频编码。 */
export function normalizeDirectEnhancementVideoCodec(
  codec: string | null | undefined
): DirectEnhancementVideoCodec | undefined {
  const value = codec?.trim().toLowerCase() ?? "";
  if (/^(?:avc1|avc3|h264)(?:[.\s]|$)/.test(value)) return "h264";
  if (/^(?:vp09|vp9)(?:[.\s]|$)/.test(value)) return "vp9";
  if (/^(?:av01|av1)(?:[.\s]|$)/.test(value)) return "av1";
  return undefined;
}

/** 只允许 F5-B 首批明确覆盖的 MP4/WebM 与视频编码组合。 */
export function evaluateDirectEnhancementMediaCandidate(
  candidate: DirectEnhancementMediaCandidate
): DirectEnhancementMediaSupport {
  const container = normalizeContainer(candidate.container);
  const videoCodec = normalizeDirectEnhancementVideoCodec(candidate.videoCodec);
  const audioCodec = candidate.audioCodec?.trim() || undefined;
  const durationSeconds = Number.isFinite(candidate.durationSeconds) && Number(candidate.durationSeconds) > 0
    ? Number(candidate.durationSeconds)
    : undefined;

  if (!container) {
    return { supported: false, reason: "F5-B 首批只支持 MP4 和 WebM 容器" };
  }
  if (!videoCodec) {
    return { supported: false, container, reason: "视频编码不在 H.264、VP9 或 AV1 首批范围内" };
  }
  if (container === "webm" && videoCodec === "h264") {
    return { supported: false, container, videoCodec, reason: "WebM/H.264 不属于受控兼容组合" };
  }

  return {
    supported: true,
    container,
    videoCodec,
    ...(audioCodec ? { audioCodec } : {}),
    ...(durationSeconds ? { durationSeconds } : {})
  };
}

/**
 * F5-B 的绝对媒体时钟。后续接入 AudioContext 时可注入其 currentTime，
 * 现阶段用于固定暂停、倍速和拖动的时间轴语义。
 */
export class DirectEnhancementMediaClock {
  private anchorPositionSeconds = 0;
  private anchorTimeSeconds = 0;
  private playbackRate = 1;
  private running = false;

  constructor(private readonly nowSeconds: () => number) {
    this.anchorTimeSeconds = this.readNow();
  }

  snapshot(): DirectEnhancementClockSnapshot {
    return {
      positionSeconds: this.positionSeconds(),
      playbackRate: this.playbackRate,
      running: this.running
    };
  }

  play(): DirectEnhancementClockSnapshot {
    if (!this.running) {
      this.anchorTimeSeconds = this.readNow();
      this.running = true;
    }
    return this.snapshot();
  }

  pause(): DirectEnhancementClockSnapshot {
    if (this.running) {
      this.anchorPositionSeconds = this.positionSeconds();
      this.anchorTimeSeconds = this.readNow();
      this.running = false;
    }
    return this.snapshot();
  }

  seek(positionSeconds: number): DirectEnhancementClockSnapshot {
    if (!Number.isFinite(positionSeconds) || positionSeconds < 0) {
      throw new RangeError("媒体时钟位置必须是非负有限数");
    }
    this.anchorPositionSeconds = positionSeconds;
    this.anchorTimeSeconds = this.readNow();
    return this.snapshot();
  }

  setPlaybackRate(playbackRate: number): DirectEnhancementClockSnapshot {
    if (!Number.isFinite(playbackRate) || playbackRate < 0.25 || playbackRate > 4) {
      throw new RangeError("媒体时钟倍速必须在 0.25 到 4 之间");
    }
    this.anchorPositionSeconds = this.positionSeconds();
    this.anchorTimeSeconds = this.readNow();
    this.playbackRate = playbackRate;
    return this.snapshot();
  }

  private positionSeconds(): number {
    if (!this.running) return this.anchorPositionSeconds;
    return this.anchorPositionSeconds
      + Math.max(0, this.readNow() - this.anchorTimeSeconds) * this.playbackRate;
  }

  private readNow(): number {
    const value = this.nowSeconds();
    if (!Number.isFinite(value) || value < 0) {
      throw new RangeError("媒体时钟时间源必须是非负有限数");
    }
    return value;
  }
}

function normalizeContainer(value: string): DirectEnhancementContainer | undefined {
  const normalized = value.trim().toLowerCase();
  if (normalized === "mp4" || normalized === "webm") return normalized;
  return undefined;
}
