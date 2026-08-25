import { EncodedPacketSink, MP4, type EncodedPacket } from "mediabunny";
import {
  DirectEnhancementFrameQueue,
  DirectEnhancementPerformanceMonitor,
  evaluateDirectEnhancementMediaCandidate,
  hasDirectEnhancementDecodeCapacity
} from "@shared/direct-enhancement-media";
import {
  createDirectEnhancementMediaInput,
  type DirectEnhancementRangeTelemetry
} from "./direct-enhancement-demuxer";
import {
  createDirectEnhancementAudioPlayback,
  type DirectEnhancementAudioPlayback
} from "./direct-enhancement-audio";
import {
  createDirectEnhancementWebGpuRenderer,
  type DirectEnhancementWebGpuRenderer
} from "./direct-enhancement-webgpu";

const DECODE_AHEAD_SECONDS = 2;
const FIRST_FRAME_TIMEOUT_MS = 8_000;
const GPU_QUEUE_SAMPLE_INTERVAL_FRAMES = 30;
const MAX_BUFFERED_VIDEO_FRAMES = 48;
const MAX_PLAYBACK_RANGE_REQUESTS = 4_096;

interface QueuedVideoFrame {
  frame: VideoFrame;
  timestampSeconds: number;
}

export interface DirectEnhancementPlaybackDiagnostics {
  active: boolean;
  audioClock: "audio-context";
  hasAudioTrack: boolean;
  renderedFrames: number;
  droppedFrames: number;
  decoderQueueSize: number;
  droppedFrameRatio: number;
  frameBudgetMs: number;
  currentAvDriftMs: number;
  maximumAvDriftMs: number;
  requestedPreset: "balanced" | "clear";
  effectivePreset: "balanced" | "clear";
  rangeRequestCount: number;
  receivedRangeBytes: number;
  rangeRetryCount: number;
  recoveredRangeCount: number;
  networkFailureCount: number;
  gpuQueueP95Ms?: number;
  gpuEstimatedWorkingSetBytes?: number;
  gpuResourceBudgetBytes?: number;
  degradationReason?: string;
}

export interface DirectEnhancementPlaybackState {
  ended: boolean;
  muted: boolean;
  playbackRate: number;
  positionSeconds: number;
  running: boolean;
  volume: number;
}

export interface DirectEnhancementPlaybackOptions {
  canvas: HTMLCanvasElement;
  mediaElement: HTMLVideoElement;
  preset: "balanced" | "clear";
  startPositionSeconds?: number;
  streamUrl: string;
  fetchFn?: typeof fetch;
  signal?: AbortSignal;
  onDiagnostics?: (diagnostics: DirectEnhancementPlaybackDiagnostics) => void;
  onFailure?: (error: Error) => void;
}

export interface DirectEnhancementPlaybackController {
  subscribe(listener: (state: DirectEnhancementPlaybackState) => void): () => void;
  play(): Promise<void>;
  pause(): void;
  seek(positionSeconds: number): Promise<void>;
  setVolume(volume: number): void;
  setMuted(muted: boolean): void;
  setPlaybackRate(rate: number): void;
  setPreset(preset: "balanced" | "clear"): void;
  getDiagnostics(): DirectEnhancementPlaybackDiagnostics;
  dispose(): Promise<void>;
}

/** 创建由独立 AudioContext 提供主时钟的 WebCodecs/WebGPU 音视频增强循环。 */
export async function createDirectEnhancementPlayback(
  options: DirectEnhancementPlaybackOptions
): Promise<DirectEnhancementPlaybackController> {
  const restoreNativePlayback = !options.mediaElement.paused;
  const playback = new DirectEnhancementPlayback(options);
  try {
    await playback.initialize();
    return playback;
  } catch (error) {
    await playback.dispose();
    if (restoreNativePlayback && !options.signal?.aborted && options.mediaElement.isConnected) {
      void options.mediaElement.play().catch((restoreError) => {
        console.warn("[remote] F5-F 初始化失败后恢复原视频播放失败", restoreError);
      });
    }
    throw error;
  }
}

class DirectEnhancementPlayback implements DirectEnhancementPlaybackController {
  private readonly frameQueue = new DirectEnhancementFrameQueue<QueuedVideoFrame>(MAX_BUFFERED_VIDEO_FRAMES);
  private readonly performanceMonitor = new DirectEnhancementPerformanceMonitor();
  private readonly stateListeners = new Set<(state: DirectEnhancementPlaybackState) => void>();
  private readonly mediaInput;
  private readonly telemetry: DirectEnhancementRangeTelemetry;
  private decoder?: VideoDecoder;
  private decoderConfig?: VideoDecoderConfig;
  private renderer?: DirectEnhancementWebGpuRenderer;
  private audioPlayback?: DirectEnhancementAudioPlayback;
  private packetSink?: EncodedPacketSink;
  private nextPacket: EncodedPacket | null | undefined;
  private decodePump?: Promise<void>;
  private pendingVideoFrames = 0;
  private animationFrame?: number;
  private generation = 0;
  private requestedPreset: "balanced" | "clear";
  private effectivePreset: "balanced" | "clear";
  private renderedFrames = 0;
  private droppedFrames = 0;
  private droppedSincePresentation = 0;
  private disposed = false;
  private failed = false;
  private awaitingPresentedFrame = true;
  private awaitingPresentedSinceMs = performance.now();
  private lastPresentedAtMs?: number;
  private lastPresentedMediaTimeSeconds?: number;
  private gpuQueueSamplePending = false;
  private suppressMediaPlaybackEvent = false;
  private initiallyRunning = false;
  private volume = 0.7;
  private muted = false;
  private playbackRate = 1;
  private readySettled = false;
  private failureError?: Error;
  private automaticDegradationReason?: string;
  private lastDiagnosticsPublishedAt = 0;
  private readonly readyPromise: Promise<void>;
  private readonly resolveReady: () => void;

  constructor(private readonly options: DirectEnhancementPlaybackOptions) {
    let resolveReady!: () => void;
    this.readyPromise = new Promise<void>((resolve) => {
      resolveReady = resolve;
    });
    this.resolveReady = resolveReady;
    const handle = createDirectEnhancementMediaInput(options.streamUrl, {
      fetchFn: options.fetchFn,
      maximumRangeRequests: MAX_PLAYBACK_RANGE_REQUESTS
    });
    this.mediaInput = handle.input;
    this.telemetry = handle.telemetry;
    this.requestedPreset = options.preset;
    this.effectivePreset = options.preset;
  }

  async initialize(): Promise<void> {
    if (this.options.signal?.aborted) {
      throw new DOMException("F5-D 视频增强初始化已取消", "AbortError");
    }
    this.options.signal?.addEventListener("abort", this.handleAbort, { once: true });
    const input = this.mediaInput;
    const [format, videoTrack, audioTrack, durationSeconds] = await Promise.all([
      input.getFormat(),
      input.getPrimaryVideoTrack(),
      input.getPrimaryAudioTrack(),
      input.getDurationFromMetadata()
    ]);
    this.throwIfStopped();
    if (!videoTrack) throw new Error("F5-D 媒体源没有视频轨");
    const config = await videoTrack.getDecoderConfig();
    const support = evaluateDirectEnhancementMediaCandidate({
      container: format === MP4 ? "mp4" : "webm",
      videoCodec: config?.codec,
      durationSeconds
    });
    if (!support.supported || !config) {
      throw new Error(support.reason ?? "F5-D 视频配置不受支持");
    }
    const decoderSupport = await VideoDecoder.isConfigSupported(config);
    this.throwIfStopped();
    if (!decoderSupport.supported) {
      throw new Error(`F5-D 浏览器不能解码 ${config.codec}`);
    }

    const [width, height] = await Promise.all([
      videoTrack.getDisplayWidth(),
      videoTrack.getDisplayHeight()
    ]);
    this.throwIfStopped();
    this.options.canvas.width = Math.max(1, Math.round(width));
    this.options.canvas.height = Math.max(1, Math.round(height));
    const renderer = await createDirectEnhancementWebGpuRenderer(this.options.canvas, {
      maximumBufferedVideoFrames: MAX_BUFFERED_VIDEO_FRAMES
    });
    if (this.disposed) {
      renderer.dispose();
      this.throwIfStopped();
    }
    this.renderer = renderer;
    void renderer.deviceLost.then((info) => {
      if (this.disposed) return;
      this.fail(new Error(
        info.message
          ? `WebGPU 设备已丢失：${info.message}`
          : `WebGPU 设备已丢失：${info.reason}`
      ));
    });
    void renderer.resourcePressure.then((error) => {
      if (!this.disposed) this.fail(error);
    });
    this.packetSink = new EncodedPacketSink(videoTrack);
    this.decoderConfig = config;
    this.audioPlayback = await createDirectEnhancementAudioPlayback(audioTrack, {
      durationSeconds: durationSeconds ?? 0,
      initialPositionSeconds: this.options.startPositionSeconds ?? this.options.mediaElement.currentTime ?? 0,
      signal: this.options.signal,
      onError: (error) => this.fail(error),
      onPosition: (positionSeconds, running, ended) => {
        this.publishPlaybackState(positionSeconds, running, ended);
        this.scheduleAnimationFrame();
      }
    });
    this.volume = this.options.mediaElement.volume;
    this.muted = this.options.mediaElement.muted;
    this.playbackRate = this.options.mediaElement.playbackRate;
    this.audioPlayback.setVolume(this.volume);
    this.audioPlayback.setMuted(this.muted);
    this.audioPlayback.setPlaybackRate(this.playbackRate);
    this.initiallyRunning = !this.options.mediaElement.paused;
    this.suppressMediaPlaybackEvent = true;
    this.options.mediaElement.pause();
    this.suppressMediaPlaybackEvent = false;
    this.decoder = this.createDecoder(config);
    this.bindMediaEvents();
    await this.restartAt(this.options.startPositionSeconds ?? this.options.mediaElement.currentTime ?? 0);
    this.scheduleAnimationFrame();
    if (this.initiallyRunning) await this.play();
    await this.waitForFirstFrame();
    if (this.failureError) throw this.failureError;
    if (this.disposed) throw new DOMException("F5-D 视频增强初始化已取消", "AbortError");
    this.publishDiagnostics(undefined, true);
  }

  subscribe(listener: (state: DirectEnhancementPlaybackState) => void): () => void {
    this.stateListeners.add(listener);
    listener({
      ended: this.hasEnded(),
      muted: this.muted,
      playbackRate: this.playbackRate,
      positionSeconds: this.getPositionSeconds(),
      running: this.isRunning(),
      volume: this.volume
    });
    return () => this.stateListeners.delete(listener);
  }

  async play(): Promise<void> {
    this.throwIfStopped();
    if (this.audioPlayback) {
      this.suppressMediaPlaybackEvent = true;
      this.options.mediaElement.pause();
      this.suppressMediaPlaybackEvent = false;
      await this.audioPlayback.play();
      this.scheduleAnimationFrame();
      this.publishPlaybackState(this.getPositionSeconds(), true, false);
      return;
    }
    await this.options.mediaElement.play();
  }

  pause(): void {
    if (this.audioPlayback) {
      this.audioPlayback.pause();
      this.publishPlaybackState(this.getPositionSeconds(), false, this.hasEnded());
      return;
    }
    this.options.mediaElement.pause();
  }

  async seek(positionSeconds: number): Promise<void> {
    if (this.audioPlayback) {
      await this.audioPlayback.seek(positionSeconds);
      await this.restartAt(positionSeconds);
      this.publishPlaybackState(this.getPositionSeconds(), this.isRunning(), this.hasEnded());
      this.scheduleAnimationFrame();
      return;
    }
    this.options.mediaElement.currentTime = positionSeconds;
  }

  setVolume(volume: number): void {
    this.volume = volume;
    this.audioPlayback?.setVolume(volume);
    if (!this.audioPlayback) this.options.mediaElement.volume = volume;
    this.publishPlaybackState(this.getPositionSeconds(), this.isRunning(), this.hasEnded());
  }

  setMuted(muted: boolean): void {
    this.muted = muted;
    this.audioPlayback?.setMuted(muted);
    if (!this.audioPlayback) this.options.mediaElement.muted = muted;
    this.publishPlaybackState(this.getPositionSeconds(), this.isRunning(), this.hasEnded());
  }

  setPlaybackRate(rate: number): void {
    this.playbackRate = rate;
    this.audioPlayback?.setPlaybackRate(rate);
    if (!this.audioPlayback) this.options.mediaElement.playbackRate = rate;
    this.scheduleAnimationFrame();
    this.publishPlaybackState(this.getPositionSeconds(), this.isRunning(), this.hasEnded());
  }

  setPreset(preset: "balanced" | "clear"): void {
    this.requestedPreset = preset;
    this.effectivePreset = preset;
    this.automaticDegradationReason = undefined;
    this.performanceMonitor.resetLoadWindow();
    this.publishDiagnostics(undefined, true);
  }

  getDiagnostics(): DirectEnhancementPlaybackDiagnostics {
    const performanceSnapshot = this.performanceMonitor.snapshot(this.effectivePreset);
    return {
      active: !this.disposed && !this.failed,
      audioClock: "audio-context",
      hasAudioTrack: this.audioPlayback?.hasAudioTrack ?? false,
      renderedFrames: this.renderedFrames,
      droppedFrames: this.droppedFrames,
      decoderQueueSize: this.decoder?.decodeQueueSize ?? 0,
      droppedFrameRatio: performanceSnapshot.droppedFrameRatio,
      frameBudgetMs: performanceSnapshot.frameBudgetMs,
      currentAvDriftMs: performanceSnapshot.currentAvDriftMs,
      maximumAvDriftMs: performanceSnapshot.maximumAvDriftMs,
      requestedPreset: this.requestedPreset,
      effectivePreset: this.effectivePreset,
      rangeRequestCount: this.telemetry.rangeRequestCount,
      receivedRangeBytes: this.telemetry.receivedRangeBytes,
      rangeRetryCount: this.telemetry.retryCount,
      recoveredRangeCount: this.telemetry.recoveredRangeCount,
      networkFailureCount: this.telemetry.networkFailureCount,
      ...(performanceSnapshot.gpuQueueP95Ms === undefined
        ? {}
        : { gpuQueueP95Ms: performanceSnapshot.gpuQueueP95Ms }),
      ...(this.renderer ? {
        gpuEstimatedWorkingSetBytes: this.renderer.resourceBudget.estimatedWorkingSetBytes,
        gpuResourceBudgetBytes: this.renderer.resourceBudget.resourceBudgetBytes
      } : {}),
      ...(this.failureError || this.automaticDegradationReason ? {
        degradationReason: this.failureError?.message ?? this.automaticDegradationReason
      } : {})
    };
  }

  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    this.generation += 1;
    if (this.animationFrame !== undefined) cancelAnimationFrame(this.animationFrame);
    this.animationFrame = undefined;
    this.unbindMediaEvents();
    this.options.signal?.removeEventListener("abort", this.handleAbort);
    this.closeQueuedFrames();
    this.settleReady();
    await this.audioPlayback?.dispose();
    this.audioPlayback = undefined;
    try {
      await this.decoder?.flush();
    } catch {
      // A reset or device failure can reject the final flush; resources still need to be released.
    }
    this.decoder?.close();
    this.renderer?.dispose();
    this.mediaInput.dispose();
    this.publishDiagnostics(undefined, true);
    this.stateListeners.clear();
  }

  private createDecoder(config: VideoDecoderConfig): VideoDecoder {
    const decoder = new VideoDecoder({
      output: (frame) => {
        this.pendingVideoFrames = Math.max(0, this.pendingVideoFrames - 1);
        if (this.disposed || this.failed) {
          frame.close();
          return;
        }
        const discarded = this.frameQueue.push({
          frame,
          timestampSeconds: frame.timestamp / 1_000_000
        });
        this.droppedFrames += discarded.length;
        if (!this.awaitingPresentedFrame) this.droppedSincePresentation += discarded.length;
        discarded.forEach((item) => item.frame.close());
      },
      error: (error) => this.fail(error)
    });
    decoder.configure(config);
    return decoder;
  }

  private readonly handlePlay = (): void => {
    if (this.audioPlayback) {
      void this.play().catch((error) => this.fail(error));
      return;
    }
    this.scheduleAnimationFrame();
  };
  private readonly handlePause = (): void => {
    if (this.suppressMediaPlaybackEvent) return;
    this.audioPlayback?.pause();
    this.publishPlaybackState(this.getPositionSeconds(), false, this.hasEnded());
  };
  private readonly handleSeeking = (): void => {
    const positionSeconds = Math.max(0, this.options.mediaElement.currentTime || 0);
    void (this.audioPlayback
      ? this.audioPlayback.seek(positionSeconds)
      : Promise.resolve())
      .then(() => this.restartAt(positionSeconds))
      .then(() => this.scheduleAnimationFrame())
      .catch((error) => this.fail(error));
  };
  private readonly handleEnded = (): void => {
    if (this.animationFrame !== undefined) cancelAnimationFrame(this.animationFrame);
    this.animationFrame = undefined;
  };
  private readonly handleAbort = (): void => {
    void this.dispose();
  };

  private bindMediaEvents(): void {
    const media = this.options.mediaElement;
    media.addEventListener("play", this.handlePlay);
    media.addEventListener("pause", this.handlePause);
    media.addEventListener("seeking", this.handleSeeking);
    media.addEventListener("ended", this.handleEnded);
  }

  private unbindMediaEvents(): void {
    const media = this.options.mediaElement;
    media.removeEventListener("play", this.handlePlay);
    media.removeEventListener("pause", this.handlePause);
    media.removeEventListener("seeking", this.handleSeeking);
    media.removeEventListener("ended", this.handleEnded);
  }

  private async restartAt(positionSeconds: number): Promise<void> {
    if (this.disposed || this.failed || !this.packetSink || !this.decoder || !this.decoderConfig) return;
    const generation = ++this.generation;
    this.awaitingPresentedFrame = true;
    this.awaitingPresentedSinceMs = performance.now();
    this.lastPresentedAtMs = undefined;
    this.lastPresentedMediaTimeSeconds = undefined;
    this.droppedSincePresentation = 0;
    this.nextPacket = undefined;
    this.closeQueuedFrames();
    this.decoder.reset();
    this.pendingVideoFrames = 0;
    this.decoder.configure(this.decoderConfig);
    const packet = await this.packetSink.getKeyPacket(Math.max(0, positionSeconds));
    if (generation !== this.generation || this.disposed) return;
    if (!packet) throw new Error("F5-D 无法定位目标位置之前的视频关键帧");
    this.nextPacket = packet;
    await this.ensureDecodeWindow(generation);
  }

  private scheduleAnimationFrame(): void {
    if (this.disposed || this.failed || this.animationFrame !== undefined) return;
    this.animationFrame = requestAnimationFrame(this.handleAnimationFrame);
  }

  private readonly handleAnimationFrame = (): void => {
    this.animationFrame = undefined;
    if (this.disposed || this.failed) return;
    const positionSeconds = this.getPositionSeconds();
    const selection = this.frameQueue.take(positionSeconds);
    this.droppedFrames += selection.discarded.length;
    if (!this.awaitingPresentedFrame) {
      this.droppedSincePresentation += selection.discarded.length;
    }
    selection.discarded.forEach((item) => item.frame.close());
    const selectedFrameDriftMs = selection.frame
      ? Math.abs(positionSeconds - selection.frame.timestampSeconds) * 1_000
      : 0;
    if (selection.frame && selectedFrameDriftMs > 250) {
      this.droppedFrames += 1;
      if (!this.awaitingPresentedFrame) {
        this.droppedSincePresentation += 1;
        this.performanceMonitor.recordClockDrift(
          positionSeconds,
          selection.frame.timestampSeconds
        );
        this.applyPerformanceRecommendation();
      }
      selection.frame.frame.close();
    } else if (selection.frame && this.renderer) {
      try {
        this.renderer.render(
          selection.frame.frame,
          this.effectivePreset === "clear" ? 0.5 : 0.3
        );
        this.renderedFrames += 1;
        this.awaitingPresentedFrame = false;
        this.lastPresentedAtMs = performance.now();
        this.lastPresentedMediaTimeSeconds = positionSeconds;
        this.performanceMonitor.recordPresentation(
          positionSeconds,
          selection.frame.timestampSeconds,
          this.droppedSincePresentation
        );
        this.droppedSincePresentation = 0;
        this.settleReady();
        this.applyPerformanceRecommendation();
        if (this.renderedFrames % GPU_QUEUE_SAMPLE_INTERVAL_FRAMES === 0) {
          this.sampleGpuQueueLatency();
        }
      } catch (error) {
        this.fail(error);
      } finally {
        selection.frame.frame.close();
      }
    }
    void this.ensureDecodeWindow(this.generation);
    this.checkFrameStarvation(positionSeconds);
    this.publishDiagnostics();
    if (
      this.awaitingPresentedFrame
      || this.isRunning()
    ) {
      this.scheduleAnimationFrame();
    }
  };

  private async ensureDecodeWindow(generation: number): Promise<void> {
    if (this.decodePump || this.disposed || this.failed || !this.decoder || !this.packetSink) return;
    const pump = async (): Promise<void> => {
      let packet = this.nextPacket;
      const target = this.getPositionSeconds() + DECODE_AHEAD_SECONDS;
      while (
        packet
        && generation === this.generation
        && packet.timestamp <= target
        && this.decoder
        && hasDirectEnhancementDecodeCapacity({
          decodedFrameCount: this.frameQueue.size,
          pendingFrameCount: this.pendingVideoFrames,
          maximumBufferedFrames: MAX_BUFFERED_VIDEO_FRAMES
        })
      ) {
        this.pendingVideoFrames += 1;
        try {
          this.decoder.decode(packet.toEncodedVideoChunk());
        } catch (error) {
          this.pendingVideoFrames = Math.max(0, this.pendingVideoFrames - 1);
          throw error;
        }
        packet = await this.packetSink!.getNextPacket(packet);
        if (generation !== this.generation || this.disposed) return;
        this.nextPacket = packet;
      }
    };
    this.decodePump = pump()
      .catch((error) => this.fail(error))
      .finally(() => {
        this.decodePump = undefined;
      });
    await this.decodePump;
  }

  private closeQueuedFrames(): void {
    this.frameQueue.clear().forEach((item) => item.frame.close());
  }

  private sampleGpuQueueLatency(): void {
    const renderer = this.renderer;
    if (!renderer || this.gpuQueueSamplePending || this.disposed || this.failed) return;
    this.gpuQueueSamplePending = true;
    const startedAt = performance.now();
    void renderer.waitForSubmittedWork()
      .then(() => {
        if (this.disposed || this.failed) return;
        this.performanceMonitor.recordGpuQueueDuration(performance.now() - startedAt);
        this.applyPerformanceRecommendation();
        this.publishDiagnostics(undefined, true);
      })
      .catch((error) => this.fail(error))
      .finally(() => {
        this.gpuQueueSamplePending = false;
      });
  }

  private applyPerformanceRecommendation(): void {
    const recommendation = this.performanceMonitor.snapshot(this.effectivePreset);
    if (recommendation.action === "keep") return;
    if (recommendation.action === "degrade" && this.effectivePreset === "clear") {
      this.effectivePreset = "balanced";
      this.automaticDegradationReason = recommendation.reason ?? "清晰档超过实时帧预算";
      this.performanceMonitor.resetLoadWindow();
      this.publishDiagnostics(undefined, true);
      console.info("[remote] F5-E 直传增强已自动降为均衡档", {
        reason: this.automaticDegradationReason
      });
      return;
    }
    this.fail(new Error(recommendation.reason ?? "直传增强超过实时帧预算"));
  }

  private checkFrameStarvation(positionSeconds: number): void {
    if (this.disposed || this.failed) return;
    const now = performance.now();
    if (this.awaitingPresentedFrame) {
      if (now - this.awaitingPresentedSinceMs > FIRST_FRAME_TIMEOUT_MS) {
        this.fail(new Error("关键帧拖动后 8 秒内未恢复增强画面"));
      }
      return;
    }
    if (
      this.lastPresentedAtMs !== undefined
      && this.lastPresentedMediaTimeSeconds !== undefined
      && positionSeconds - this.lastPresentedMediaTimeSeconds > 0.5
      && now - this.lastPresentedAtMs > 2_000
    ) {
      this.fail(new Error(
        "媒体时钟持续前进但增强画面超过 2 秒没有更新"
        + `：decoded=${this.frameQueue.size}`
        + ` pending=${this.pendingVideoFrames}`
        + ` decoder=${this.decoder?.decodeQueueSize ?? 0}`
        + ` next=${this.nextPacket?.timestamp ?? "none"}`
      ));
    }
  }

  private getPositionSeconds(): number {
    return this.audioPlayback?.getPositionSeconds()
      ?? Math.max(0, this.options.mediaElement.currentTime || 0);
  }

  private isRunning(): boolean {
    return this.audioPlayback?.isRunning() ?? !this.options.mediaElement.paused;
  }

  private hasEnded(): boolean {
    const durationSeconds = this.audioPlayback?.durationSeconds ?? this.options.mediaElement.duration;
    return Number.isFinite(durationSeconds)
      && durationSeconds > 0
      && this.getPositionSeconds() >= durationSeconds;
  }

  private publishPlaybackState(positionSeconds: number, running: boolean, ended: boolean): void {
    const state = {
      ended,
      muted: this.muted,
      playbackRate: this.playbackRate,
      positionSeconds,
      running,
      volume: this.volume
    };
    for (const listener of this.stateListeners) listener(state);
  }

  private fail(caught: unknown): void {
    if (this.failed || this.disposed) return;
    this.failed = true;
    const error = caught instanceof Error ? caught : new Error("F5-D 视频增强循环失败");
    this.failureError = error;
    this.closeQueuedFrames();
    this.settleReady();
    this.publishDiagnostics(error.message, true);
    this.options.onFailure?.(error);
    void this.dispose();
  }

  private throwIfStopped(): void {
    if (this.disposed || this.options.signal?.aborted) {
      throw new DOMException("F5-D 视频增强初始化已取消", "AbortError");
    }
  }

  private async waitForFirstFrame(): Promise<void> {
    let timeout: number | undefined;
    const timeoutPromise = new Promise<never>((_resolve, reject) => {
      timeout = window.setTimeout(() => {
        reject(new Error(`F5-D 在 ${FIRST_FRAME_TIMEOUT_MS / 1_000} 秒内未输出增强首帧`));
      }, FIRST_FRAME_TIMEOUT_MS);
    });
    try {
      await Promise.race([this.readyPromise, timeoutPromise]);
    } finally {
      window.clearTimeout(timeout);
    }
  }

  private settleReady(): void {
    if (this.readySettled) return;
    this.readySettled = true;
    this.resolveReady();
  }

  private publishDiagnostics(degradationReason?: string, force = false): void {
    const now = performance.now();
    if (!force && !degradationReason && now - this.lastDiagnosticsPublishedAt < 1_000) return;
    this.lastDiagnosticsPublishedAt = now;
    const diagnostics = this.getDiagnostics();
    this.options.onDiagnostics?.({
      ...diagnostics,
      ...(degradationReason ? { degradationReason } : {})
    });
  }
}
