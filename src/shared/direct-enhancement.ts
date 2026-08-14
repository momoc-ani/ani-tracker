/** F5 直传终端增强首批探测的候选视频编码。 */
export const DIRECT_ENHANCEMENT_CODEC_CANDIDATES = [
  { codec: "avc1.640028", contentType: "video/mp4; codecs=\"avc1.640028\"", label: "H.264 High" },
  { codec: "vp09.00.10.08", contentType: "video/webm; codecs=\"vp09.00.10.08\"", label: "VP9" },
  { codec: "av01.0.08M.08", contentType: "video/mp4; codecs=\"av01.0.08M.08\"", label: "AV1" }
] as const;

export interface DirectEnhancementCapabilityInput {
  videoDecoderAvailable: boolean;
  videoFrameAvailable: boolean;
  audioDecoderAvailable: boolean;
  audioDataAvailable: boolean;
  webGpuAvailable: boolean;
  gpuDeviceAvailable: boolean;
  offscreenCanvasAvailable: boolean;
  mediaCapabilitiesAvailable: boolean;
  supportedCodecs: readonly string[];
  smoothCodecs: readonly string[];
  powerEfficientCodecs: readonly string[];
}

export interface DirectEnhancementCapabilities {
  supported: boolean;
  webCodecs: boolean;
  audioWebCodecs: boolean;
  webGpu: boolean;
  offscreenCanvas: boolean;
  mediaCapabilities: boolean;
  supportedCodecs: string[];
  smoothCodecs: string[];
  powerEfficientCodecs: string[];
  reason?: string;
}

/** 只根据已验证的运行时事实决定 F5 是否具备启动条件。 */
export function evaluateDirectEnhancementCapabilities(
  input: DirectEnhancementCapabilityInput
): DirectEnhancementCapabilities {
  const videoWebCodecs = input.videoDecoderAvailable && input.videoFrameAvailable;
  const audioWebCodecs = input.audioDecoderAvailable && input.audioDataAvailable;
  const webCodecs = videoWebCodecs && audioWebCodecs;
  const webGpu = input.webGpuAvailable && input.gpuDeviceAvailable;
  const supportedCodecs = [...new Set(input.supportedCodecs)];
  const smoothCodecs = [...new Set(input.smoothCodecs)];
  const powerEfficientCodecs = [...new Set(input.powerEfficientCodecs)];
  const supported = webCodecs
    && webGpu
    && input.offscreenCanvasAvailable
    && input.mediaCapabilitiesAvailable
    && supportedCodecs.some((codec) => smoothCodecs.includes(codec));

  let reason: string | undefined;
  if (!videoWebCodecs) {
    reason = "当前浏览器未提供可用的 WebCodecs VideoDecoder/VideoFrame";
  } else if (!audioWebCodecs) {
    reason = "当前浏览器未提供可用的 WebCodecs AudioDecoder/AudioData";
  } else if (!webGpu) {
    reason = "当前浏览器未提供可用的 WebGPU adapter/device";
  } else if (!input.offscreenCanvasAvailable) {
    reason = "当前浏览器未提供 OffscreenCanvas";
  } else if (!input.mediaCapabilitiesAvailable) {
    reason = "当前浏览器未提供 MediaCapabilities 解码诊断";
  } else if (supportedCodecs.length === 0) {
    reason = "当前浏览器没有通过探测的直传增强视频编码";
  } else if (!supportedCodecs.some((codec) => smoothCodecs.includes(codec))) {
    reason = "当前浏览器没有通过流畅度探测的直传增强视频编码";
  }

  return {
    supported,
    webCodecs,
    audioWebCodecs,
    webGpu,
    offscreenCanvas: input.offscreenCanvasAvailable,
    mediaCapabilities: input.mediaCapabilitiesAvailable,
    supportedCodecs,
    smoothCodecs,
    powerEfficientCodecs,
    ...(reason ? { reason } : {})
  };
}
