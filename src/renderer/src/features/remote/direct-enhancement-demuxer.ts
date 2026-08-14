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

const MAX_CACHE_BYTES = 32 * 1024 * 1024;
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

interface RangeTelemetry {
  requestCount: number;
  rangeRequestCount: number;
  receivedRangeBytes: number;
  contentRanges: string[];
}

export interface DirectEnhancementDemuxDiagnostics extends DirectEnhancementMediaSupport {
  videoDecoderSupported: boolean;
  audioDecoderSupported: boolean;
  keyFrameTimestampSeconds?: number;
  keyFrameDurationSeconds?: number;
  requestCount: number;
  rangeRequestCount: number;
  receivedRangeBytes: number;
  contentRanges: string[];
}

export interface DirectEnhancementDemuxProbeOptions {
  signal?: AbortSignal;
  startPositionSeconds?: number;
  fetchFn?: typeof fetch;
  globals?: BrowserDecoderGlobals;
}

/**
 * 使用受控 MP4/WebM demuxer 验证当前直传源的 codec、关键帧和 Range 路径。
 * 此函数只返回诊断，不创建解码循环，也不接管当前播放器。
 */
export async function probeDirectEnhancementMediaSource(
  streamUrl: string,
  options: DirectEnhancementDemuxProbeOptions = {}
): Promise<DirectEnhancementDemuxDiagnostics> {
  const telemetry: RangeTelemetry = {
    requestCount: 0,
    rangeRequestCount: 0,
    receivedRangeBytes: 0,
    contentRanges: []
  };
  const fetchFn = createBoundedRangeFetch(options.fetchFn ?? fetch, telemetry);
  const input = new Input({
    source: new UrlSource(streamUrl, {
      fetchFn,
      maxCacheSize: MAX_CACHE_BYTES,
      parallelism: 2,
      requestInit: {
        cache: "no-store",
        credentials: "same-origin"
      }
    }),
    formats: [MP4, WEBM]
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
      telemetry
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

async function probeInput(
  input: Input<UrlSource>,
  startPositionSeconds: number,
  globals: BrowserDecoderGlobals,
  telemetry: RangeTelemetry
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

  const keyFrame = await new EncodedPacketSink(videoTrack).getKeyPacket(startPositionSeconds, {
    metadataOnly: true
  });
  return {
    ...support,
    videoDecoderSupported,
    audioDecoderSupported,
    ...(keyFrame ? {
      keyFrameTimestampSeconds: keyFrame.timestamp,
      keyFrameDurationSeconds: keyFrame.duration
    } : {}),
    ...telemetryResult(telemetry)
  };
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

function createBoundedRangeFetch(baseFetch: typeof fetch, telemetry: RangeTelemetry): typeof fetch {
  return async (input, init) => {
    telemetry.requestCount += 1;
    const requestHeaders = new Headers(input instanceof Request ? input.headers : undefined);
    new Headers(init?.headers).forEach((value, key) => requestHeaders.set(key, value));
    const range = requestHeaders.get("range");
    if (range) {
      telemetry.rangeRequestCount += 1;
      if (telemetry.rangeRequestCount > MAX_RANGE_REQUESTS) {
        throw new Error(`F5-B Range 请求超过 ${MAX_RANGE_REQUESTS} 次上限`);
      }
    }

    const response = await baseFetch(input, init);
    if (!range) return response;
    if (response.status !== 206) {
      void response.body?.cancel();
      throw new Error(`F5-B Range 请求未返回 206，实际状态 ${response.status}`);
    }

    const contentRange = response.headers.get("content-range");
    if (!contentRange) {
      void response.body?.cancel();
      throw new Error("F5-B Range 响应缺少 Content-Range");
    }
    telemetry.contentRanges.push(contentRange);
    return monitorResponseBody(response, telemetry);
  };
}

function monitorResponseBody(response: Response, telemetry: RangeTelemetry): Response {
  if (!response.body) return response;
  const reader = response.body.getReader();
  const body = new ReadableStream<Uint8Array>({
    async pull(controller) {
      const result = await reader.read();
      if (result.done) {
        controller.close();
        return;
      }
      telemetry.receivedRangeBytes += result.value.byteLength;
      if (telemetry.receivedRangeBytes > MAX_RANGE_BYTES) {
        await reader.cancel();
        controller.error(new Error(`F5-B Range 实际读取超过 ${MAX_RANGE_BYTES} 字节上限`));
        return;
      }
      controller.enqueue(result.value);
    },
    cancel(reason) {
      return reader.cancel(reason);
    }
  });
  const monitored = new Response(body, {
    status: response.status,
    statusText: response.statusText,
    headers: response.headers
  });
  Object.defineProperties(monitored, {
    redirected: { value: response.redirected },
    url: { value: response.url }
  });
  return monitored;
}

function unsupportedDiagnostics(
  reason: string,
  telemetry: RangeTelemetry
): DirectEnhancementDemuxDiagnostics {
  return {
    supported: false,
    reason,
    videoDecoderSupported: false,
    audioDecoderSupported: false,
    ...telemetryResult(telemetry)
  };
}

function telemetryResult(telemetry: RangeTelemetry): Pick<
  DirectEnhancementDemuxDiagnostics,
  "requestCount" | "rangeRequestCount" | "receivedRangeBytes" | "contentRanges"
> {
  return {
    requestCount: telemetry.requestCount,
    rangeRequestCount: telemetry.rangeRequestCount,
    receivedRangeBytes: telemetry.receivedRangeBytes,
    contentRanges: [...telemetry.contentRanges]
  };
}

function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) throw new DOMException("F5-B 媒体源探测已取消", "AbortError");
}
