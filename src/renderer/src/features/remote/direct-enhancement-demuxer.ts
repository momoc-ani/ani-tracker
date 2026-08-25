import {
  EncodedPacketSink,
  Input,
  MP4,
  UrlSource,
  WEBM,
  type InputAudioTrack,
  type InputVideoTrack
} from "mediabunny";
import {
  evaluateDirectEnhancementMediaCandidate,
  type DirectEnhancementContainer,
  type DirectEnhancementMediaSupport
} from "@shared/direct-enhancement-media";
import {
  createDirectEnhancementRangeFetch,
  createDirectEnhancementRangeTelemetry,
  type DirectEnhancementRangeOptions,
  type DirectEnhancementRangeTelemetry
} from "@shared/direct-enhancement-range";
import { createDirectEnhancementWebGpuRenderer } from "./direct-enhancement-webgpu";

export type { DirectEnhancementRangeTelemetry } from "@shared/direct-enhancement-range";

export const DIRECT_ENHANCEMENT_CACHE_BYTES = 32 * 1024 * 1024;
const MAX_RANGE_BYTES = 64 * 1024 * 1024;
const MAX_RANGE_REQUESTS = 24;
const PROBE_TIMEOUT_MS = 8_000;

interface DecoderSupportResult {
  supported?: boolean;
}

interface BrowserDecoderConstructor<Config> {
  isConfigSupported(config: Config): Promise<DecoderSupportResult>;
}

interface BrowserDecoderGlobals {
  VideoDecoder?: BrowserDecoderConstructor<VideoDecoderConfig>;
  AudioDecoder?: BrowserDecoderConstructor<AudioDecoderConfig>;
}

export interface DirectEnhancementMediaInputOptions extends DirectEnhancementRangeOptions {
  fetchFn?: typeof fetch;
}

export interface DirectEnhancementMediaInputHandle {
  input: Input<UrlSource>;
  telemetry: DirectEnhancementRangeTelemetry;
}

export interface DirectEnhancementDemuxDiagnostics extends DirectEnhancementMediaSupport {
  videoDecoderSupported: boolean;
  audioDecoderSupported: boolean;
  keyFrameTimestampSeconds?: number;
  keyFrameDurationSeconds?: number;
  audioSampleTimestampSeconds?: number;
  firstFrameRendered?: boolean;
  requestCount: number;
  rangeRequestCount: number;
  receivedRangeBytes: number;
  contentRanges: string[];
  retryCount: number;
  recoveredRangeCount: number;
  networkFailureCount: number;
  lastNetworkError?: string;
}

export interface DirectEnhancementDemuxProbeOptions {
  signal?: AbortSignal;
  startPositionSeconds?: number;
  fetchFn?: typeof fetch;
  globals?: BrowserDecoderGlobals;
  renderCanvas?: HTMLCanvasElement | OffscreenCanvas;
}

/**
 * 使用受控 MP4/WebM demuxer 验证当前直传源的 codec、关键帧和 Range 路径。
 * 此函数只返回诊断，不创建解码循环，也不接管当前播放器。
 */
export async function probeDirectEnhancementMediaSource(
  streamUrl: string,
  options: DirectEnhancementDemuxProbeOptions = {}
): Promise<DirectEnhancementDemuxDiagnostics> {
  const { input, telemetry } = createDirectEnhancementMediaInput(streamUrl, {
    fetchFn: options.fetchFn,
    maximumRangeRequests: MAX_RANGE_REQUESTS,
    maximumReceivedBytes: MAX_RANGE_BYTES
  });
  const abort = (): void => input.dispose();
  options.signal?.addEventListener("abort", abort, { once: true });
  const timeout = window.setTimeout(abort, PROBE_TIMEOUT_MS);

  try {
    throwIfAborted(options.signal);
    const result = await probeInput(
      input,
      Math.max(0, options.startPositionSeconds ?? 0),
      options.globals ?? globalThis as unknown as BrowserDecoderGlobals,
      telemetry,
      options.renderCanvas
    );
    throwIfAborted(options.signal);
    return result;
  } catch (error) {
    if (options.signal?.aborted) {
      throw new DOMException("F5-B 媒体源探测已取消", "AbortError");
    }
    throw error;
  } finally {
    window.clearTimeout(timeout);
    options.signal?.removeEventListener("abort", abort);
    input.dispose();
  }
}

/** 创建共享的严格 Range 媒体输入；探测和连续播放分别提供自己的预算。 */
export function createDirectEnhancementMediaInput(
  streamUrl: string,
  options: DirectEnhancementMediaInputOptions = {}
): DirectEnhancementMediaInputHandle {
  const telemetry = createDirectEnhancementRangeTelemetry();
  const fetchFn = createDirectEnhancementRangeFetch(
    options.fetchFn ?? fetch,
    telemetry,
    options
  );
  return {
    input: new Input({
      source: new UrlSource(streamUrl, {
        fetchFn,
        maxCacheSize: DIRECT_ENHANCEMENT_CACHE_BYTES,
        parallelism: 2,
        requestInit: {
          cache: "no-store",
          credentials: "same-origin"
        }
      }),
      formats: [MP4, WEBM]
    }),
    telemetry
  };
}

async function probeInput(
  input: Input<UrlSource>,
  startPositionSeconds: number,
  globals: BrowserDecoderGlobals,
  telemetry: DirectEnhancementRangeTelemetry,
  renderCanvas?: HTMLCanvasElement | OffscreenCanvas
): Promise<DirectEnhancementDemuxDiagnostics> {
  const format = await input.getFormat();
  const container: DirectEnhancementContainer = format === MP4 ? "mp4" : "webm";
  const [videoTrack, audioTrack, durationSeconds] = await Promise.all([
    input.getPrimaryVideoTrack(),
    input.getPrimaryAudioTrack(),
    input.getDurationFromMetadata()
  ]);
  if (!videoTrack) {
    return unsupportedDiagnostics("媒体源没有可用视频轨", telemetry);
  }

  const [videoConfig, audioConfig] = await Promise.all([
    videoTrack.getDecoderConfig(),
    audioTrack?.getDecoderConfig() ?? null
  ]);
  const support = evaluateDirectEnhancementMediaCandidate({
    container,
    videoCodec: videoConfig?.codec,
    audioCodec: audioConfig?.codec,
    durationSeconds
  });
  if (!support.supported || !videoConfig) {
    return {
      ...support,
      videoDecoderSupported: false,
      audioDecoderSupported: audioTrack === null,
      ...telemetryResult(telemetry)
    };
  }

  const [videoDecoderSupported, audioDecoderSupported] = await Promise.all([
    decoderSupports(globals.VideoDecoder, videoConfig),
    audioTrack ? decoderSupports(globals.AudioDecoder, audioConfig) : Promise.resolve(true)
  ]);
  if (!videoDecoderSupported || !audioDecoderSupported) {
    return {
      ...support,
      supported: false,
      reason: !videoDecoderSupported
        ? `当前浏览器不能解码媒体源视频配置 ${videoConfig.codec}`
        : `当前浏览器不能解码媒体源音频配置 ${audioConfig?.codec ?? "unknown"}`,
      videoDecoderSupported,
      audioDecoderSupported,
      ...telemetryResult(telemetry)
    };
  }

  const [keyFrame, audioSample] = await Promise.all([
    new EncodedPacketSink(videoTrack).getKeyPacket(startPositionSeconds, {
      metadataOnly: !renderCanvas
    }),
    audioTrack
      ? new EncodedPacketSink(audioTrack).getFirstPacket({ metadataOnly: true })
      : Promise.resolve(null)
  ]);
  return {
    ...support,
    videoDecoderSupported,
    audioDecoderSupported,
    ...(keyFrame ? {
      keyFrameTimestampSeconds: keyFrame.timestamp,
      keyFrameDurationSeconds: keyFrame.duration
    } : {}),
    ...(audioSample ? { audioSampleTimestampSeconds: audioSample.timestamp } : {}),
    ...(renderCanvas && keyFrame ? {
      firstFrameRendered: await decodeAndRenderFirstFrame(keyFrame, videoConfig, renderCanvas)
    } : {}),
    ...telemetryResult(telemetry)
  };
}

async function decodeAndRenderFirstFrame(
  packet: import("mediabunny").EncodedPacket,
  config: VideoDecoderConfig,
  canvas: HTMLCanvasElement | OffscreenCanvas
): Promise<boolean> {
  if (packet.isMetadataOnly) return false;
  const renderer = await createDirectEnhancementWebGpuRenderer(canvas);
  let rendered = false;
  const decoder = new VideoDecoder({
    output(frame) {
      try {
        renderer.render(frame);
        rendered = true;
      } finally {
        frame.close();
      }
    },
    error(error) {
      console.info("[remote] F5-C 首帧 WebCodecs 解码失败", { error });
    }
  });
  try {
    decoder.configure(config);
    decoder.decode(packet.toEncodedVideoChunk());
    await decoder.flush();
    return rendered;
  } finally {
    decoder.close();
    renderer.dispose();
  }
}

async function decoderSupports<Config>(
  decoder: BrowserDecoderConstructor<Config> | undefined,
  config: Config | null
): Promise<boolean> {
  if (!decoder?.isConfigSupported || !config) return false;
  try {
    return Boolean((await decoder.isConfigSupported(config)).supported);
  } catch {
    return false;
  }
}

function unsupportedDiagnostics(
  reason: string,
  telemetry: DirectEnhancementRangeTelemetry
): DirectEnhancementDemuxDiagnostics {
  return {
    supported: false,
    reason,
    videoDecoderSupported: false,
    audioDecoderSupported: false,
    ...telemetryResult(telemetry)
  };
}

function telemetryResult(telemetry: DirectEnhancementRangeTelemetry): Pick<
  DirectEnhancementDemuxDiagnostics,
  "requestCount" | "rangeRequestCount" | "receivedRangeBytes" | "contentRanges"
  | "retryCount" | "recoveredRangeCount" | "networkFailureCount" | "lastNetworkError"
> {
  return {
    requestCount: telemetry.requestCount,
    rangeRequestCount: telemetry.rangeRequestCount,
    receivedRangeBytes: telemetry.receivedRangeBytes,
    contentRanges: [...telemetry.contentRanges],
    retryCount: telemetry.retryCount,
    recoveredRangeCount: telemetry.recoveredRangeCount,
    networkFailureCount: telemetry.networkFailureCount,
    ...(telemetry.lastNetworkError ? { lastNetworkError: telemetry.lastNetworkError } : {})
  };
}

function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) throw new DOMException("F5-B 媒体源探测已取消", "AbortError");
}
