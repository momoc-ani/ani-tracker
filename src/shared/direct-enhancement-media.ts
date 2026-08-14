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
