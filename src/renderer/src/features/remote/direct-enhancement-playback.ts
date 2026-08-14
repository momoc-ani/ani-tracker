import { EncodedPacketSink, MP4, type EncodedPacket } from "mediabunny";
import {
  DirectEnhancementFrameQueue,
  evaluateDirectEnhancementMediaCandidate
} from "@shared/direct-enhancement-media";
import {
  createDirectEnhancementMediaInput,
  type DirectEnhancementRangeTelemetry
} from "./direct-enhancement-demuxer";
import {
  createDirectEnhancementWebGpuRenderer,
  type DirectEnhancementWebGpuRenderer
} from "./direct-enhancement-webgpu";

const DECODE_AHEAD_SECONDS = 2;
const FIRST_FRAME_TIMEOUT_MS = 8_000;
const MAX_DECODER_QUEUE_SIZE = 8;
const MAX_PLAYBACK_RANGE_REQUESTS = 4_096;

interface QueuedVideoFrame {
  frame: VideoFrame;
  timestampSeconds: number;
}

export interface DirectEnhancementPlaybackDiagnostics {
  active: boolean;
  renderedFrames: number;
  droppedFrames: number;
  decoderQueueSize: number;
  rangeRequestCount: number;
  receivedRangeBytes: number;
  degradationReason?: string;
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
  setPreset(preset: "balanced" | "clear"): void;
  getDiagnostics(): DirectEnhancementPlaybackDiagnostics;
  dispose(): Promise<void>;
}

/** 创建由现有媒体元素提供音频主时钟的 WebCodecs/WebGPU 视频增强循环。 */
export async function createDirectEnhancementPlayback(
  options: DirectEnhancementPlaybackOptions
): Promise<DirectEnhancementPlaybackController> {
  const playback = new DirectEnhancementPlayback(options);
  try {
    await playback.initialize();
    return playback;
  } catch (error) {
    await playback.dispose();
    throw error;
  }
}

class DirectEnhancementPlayback implements DirectEnhancementPlaybackController {
  private readonly frameQueue = new DirectEnhancementFrameQueue<QueuedVideoFrame>(8);
  private readonly mediaInput;
  private readonly telemetry: DirectEnhancementRangeTelemetry;
  private decoder?: VideoDecoder;
  private decoderConfig?: VideoDecoderConfig;
  private renderer?: DirectEnhancementWebGpuRenderer;
  private packetSink?: EncodedPacketSink;
  private nextPacket: EncodedPacket | null | undefined;
  private decodePump?: Promise<void>;
  private animationFrame?: number;
  private generation = 0;
  private preset: "balanced" | "clear";
  private renderedFrames = 0;
  private droppedFrames = 0;
  private disposed = false;
  private failed = false;
  private awaitingPresentedFrame = true;
  private readySettled = false;
  private failureError?: Error;
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
    this.preset = options.preset;
  }

  async initialize(): Promise<void> {
    if (this.options.signal?.aborted) {
      throw new DOMException("F5-D 视频增强初始化已取消", "AbortError");
    }
    this.options.signal?.addEventListener("abort", this.handleAbort, { once: true });
    const input = this.mediaInput;
    const [format, videoTrack, durationSeconds] = await Promise.all([
      input.getFormat(),
      input.getPrimaryVideoTrack(),
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
    const renderer = await createDirectEnhancementWebGpuRenderer(this.options.canvas);
    if (this.disposed) {
      renderer.dispose();
      this.throwIfStopped();
    }
    this.renderer = renderer;
    this.packetSink = new EncodedPacketSink(videoTrack);
    this.decoderConfig = config;
    this.decoder = this.createDecoder(config);
    this.bindMediaEvents();
    await this.restartAt(this.options.startPositionSeconds ?? this.options.mediaElement.currentTime ?? 0);
    this.scheduleAnimationFrame();
    await this.waitForFirstFrame();
    if (this.failureError) throw this.failureError;
    if (this.disposed) throw new DOMException("F5-D 视频增强初始化已取消", "AbortError");
    this.publishDiagnostics(undefined, true);
  }

  setPreset(preset: "balanced" | "clear"): void {
    this.preset = preset;
  }

  getDiagnostics(): DirectEnhancementPlaybackDiagnostics {
    return {
      active: !this.disposed && !this.failed,
      renderedFrames: this.renderedFrames,
      droppedFrames: this.droppedFrames,
      decoderQueueSize: this.decoder?.decodeQueueSize ?? 0,
      rangeRequestCount: this.telemetry.rangeRequestCount,
      receivedRangeBytes: this.telemetry.receivedRangeBytes,
      ...(this.failed ? {
        degradationReason: this.failureError?.message ?? "WebCodecs/WebGPU 视频循环已失败"
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
    try {
      await this.decoder?.flush();
    } catch {
      // A reset or device failure can reject the final flush; resources still need to be released.
    }
    this.decoder?.close();
    this.renderer?.dispose();
    this.mediaInput.dispose();
    this.publishDiagnostics(undefined, true);
  }

  private createDecoder(config: VideoDecoderConfig): VideoDecoder {
    const decoder = new VideoDecoder({
      output: (frame) => {
        if (this.disposed || this.failed) {
          frame.close();
          return;
        }
        const discarded = this.frameQueue.push({
          frame,
          timestampSeconds: frame.timestamp / 1_000_000
        });
        this.droppedFrames += discarded.length;
        discarded.forEach((item) => item.frame.close());
      },
      error: (error) => this.fail(error)
    });
    decoder.configure(config);
    return decoder;
  }

  private readonly handlePlay = (): void => this.scheduleAnimationFrame();
  private readonly handleSeeking = (): void => {
    void this.restartAt(this.options.mediaElement.currentTime)
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
    media.addEventListener("seeking", this.handleSeeking);
    media.addEventListener("ended", this.handleEnded);
  }

  private unbindMediaEvents(): void {
    const media = this.options.mediaElement;
    media.removeEventListener("play", this.handlePlay);
    media.removeEventListener("seeking", this.handleSeeking);
    media.removeEventListener("ended", this.handleEnded);
  }

  private async restartAt(positionSeconds: number): Promise<void> {
    if (this.disposed || this.failed || !this.packetSink || !this.decoder || !this.decoderConfig) return;
    const generation = ++this.generation;
    this.awaitingPresentedFrame = true;
    this.nextPacket = undefined;
    this.closeQueuedFrames();
    this.decoder.reset();
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
    const positionSeconds = Math.max(0, this.options.mediaElement.currentTime || 0);
    const selection = this.frameQueue.take(positionSeconds);
    this.droppedFrames += selection.discarded.length;
    selection.discarded.forEach((item) => item.frame.close());
    if (selection.frame && this.renderer) {
      try {
        this.renderer.render(selection.frame.frame, this.preset === "clear" ? 0.5 : 0.3);
        this.renderedFrames += 1;
        this.awaitingPresentedFrame = false;
        this.settleReady();
      } catch (error) {
        this.fail(error);
      } finally {
        selection.frame.frame.close();
      }
    }
    void this.ensureDecodeWindow(this.generation);
    this.publishDiagnostics();
    if (
      this.awaitingPresentedFrame
      || (!this.options.mediaElement.paused && !this.options.mediaElement.ended)
    ) {
      this.scheduleAnimationFrame();
    }
  };

  private async ensureDecodeWindow(generation: number): Promise<void> {
    if (this.decodePump || this.disposed || this.failed || !this.decoder || !this.packetSink) return;
    const pump = async (): Promise<void> => {
      let packet = this.nextPacket;
      const target = Math.max(0, this.options.mediaElement.currentTime || 0) + DECODE_AHEAD_SECONDS;
      while (
        packet
        && generation === this.generation
        && packet.timestamp <= target
        && this.decoder
        && this.decoder.decodeQueueSize < MAX_DECODER_QUEUE_SIZE
      ) {
        this.decoder.decode(packet.toEncodedVideoChunk());
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
