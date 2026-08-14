import {
  DIRECT_ENHANCEMENT_CODEC_CANDIDATES,
  evaluateDirectEnhancementCapabilities,
  type DirectEnhancementCapabilities
} from "@shared/direct-enhancement";

interface BrowserVideoDecoder {
  isConfigSupported(config: {
    codec: string;
    codedWidth: number;
    codedHeight: number;
    hardwareAcceleration: "prefer-hardware";
  }): Promise<{
    supported?: boolean;
  }>;
}

interface BrowserMediaCapabilities {
  decodingInfo(config: {
    type: "file";
    video: {
      contentType: string;
      width: number;
      height: number;
      bitrate: number;
      framerate: number;
    };
  }): Promise<{ supported?: boolean; smooth?: boolean; powerEfficient?: boolean }>;
}

interface BrowserGpuDevice {
  destroy?: () => void;
}

interface BrowserGpuAdapter {
  requestDevice?: () => Promise<BrowserGpuDevice>;
}

interface BrowserGpu {
  requestAdapter?: () => Promise<BrowserGpuAdapter | null>;
}

interface BrowserCapabilityGlobals {
  VideoDecoder?: BrowserVideoDecoder;
  VideoFrame?: unknown;
  OffscreenCanvas?: unknown;
  navigator?: Navigator & { gpu?: BrowserGpu; mediaCapabilities?: BrowserMediaCapabilities };
}

/** 探测 F5 直传增强运行条件，但不创建播放器或修改当前播放路径。 */
export async function probeDirectEnhancementCapabilities(
  globals: BrowserCapabilityGlobals = globalThis as unknown as BrowserCapabilityGlobals
): Promise<DirectEnhancementCapabilities> {
  const videoDecoder = globals.VideoDecoder;
  const webGpu = globals.navigator?.gpu;
  const mediaCapabilities = globals.navigator?.mediaCapabilities;
  const input = {
    videoDecoderAvailable: Boolean(videoDecoder?.isConfigSupported),
    videoFrameAvailable: globals.VideoFrame !== undefined,
    webGpuAvailable: Boolean(webGpu?.requestAdapter),
    gpuDeviceAvailable: false,
    offscreenCanvasAvailable: globals.OffscreenCanvas !== undefined,
    mediaCapabilitiesAvailable: Boolean(mediaCapabilities?.decodingInfo),
    supportedCodecs: [] as string[],
    smoothCodecs: [] as string[],
    powerEfficientCodecs: [] as string[]
  };

  if (webGpu?.requestAdapter) {
    try {
      const adapter = await webGpu.requestAdapter();
      if (adapter?.requestDevice) {
        const device = await adapter.requestDevice();
        input.gpuDeviceAvailable = true;
        device.destroy?.();
      }
    } catch (error) {
      console.info("[remote] WebGPU 直传增强设备探测失败", { error });
    }
  }

  if (videoDecoder?.isConfigSupported) {
    for (const candidate of DIRECT_ENHANCEMENT_CODEC_CANDIDATES) {
      try {
        const result = await videoDecoder.isConfigSupported({
          codec: candidate.codec,
          codedWidth: 1_920,
          codedHeight: 1_080,
          hardwareAcceleration: "prefer-hardware"
        });
        if (!result.supported) continue;
        input.supportedCodecs.push(candidate.codec);
        if (mediaCapabilities?.decodingInfo) {
          const mediaResult = await mediaCapabilities.decodingInfo({
            type: "file",
            video: {
              contentType: candidate.contentType,
              width: 1_920,
              height: 1_080,
              bitrate: 12_000_000,
              framerate: 60
            }
          });
          if (mediaResult.supported && mediaResult.smooth) input.smoothCodecs.push(candidate.codec);
          if (mediaResult.powerEfficient) input.powerEfficientCodecs.push(candidate.codec);
        }
      } catch (error) {
        console.info("[remote] WebCodecs codec 探测失败", { codec: candidate.codec, error });
      }
    }
  }

  return evaluateDirectEnhancementCapabilities(input);
}
