import { strict as assert } from "node:assert";
import { test } from "node:test";
import { evaluateDirectEnhancementCapabilities } from "../direct-enhancement";

test("F5 只有 WebCodecs、WebGPU、画布和 codec 全部通过才可用", () => {
  const result = evaluateDirectEnhancementCapabilities({
    videoDecoderAvailable: true,
    videoFrameAvailable: true,
    webGpuAvailable: true,
    gpuDeviceAvailable: true,
    offscreenCanvasAvailable: true,
    mediaCapabilitiesAvailable: true,
    supportedCodecs: ["avc1.640028", "avc1.640028"],
    smoothCodecs: ["avc1.640028"],
    powerEfficientCodecs: ["avc1.640028"]
  });

  assert.equal(result.supported, true);
  assert.deepEqual(result.supportedCodecs, ["avc1.640028"]);
  assert.deepEqual(result.powerEfficientCodecs, ["avc1.640028"]);
  assert.equal(result.reason, undefined);
});

test("缺少 WebGPU 或 codec 时必须保持增强关闭并返回原因", () => {
  const result = evaluateDirectEnhancementCapabilities({
    videoDecoderAvailable: true,
    videoFrameAvailable: true,
    webGpuAvailable: true,
    gpuDeviceAvailable: false,
    offscreenCanvasAvailable: true,
    mediaCapabilitiesAvailable: true,
    supportedCodecs: [],
    smoothCodecs: [],
    powerEfficientCodecs: []
  });

  assert.equal(result.supported, false);
  assert.equal(result.webGpu, false);
  assert.match(result.reason ?? "", /WebGPU/);
});

test("没有 WebCodecs 时不允许误报终端增强", () => {
  const result = evaluateDirectEnhancementCapabilities({
    videoDecoderAvailable: false,
    videoFrameAvailable: false,
    webGpuAvailable: true,
    gpuDeviceAvailable: true,
    offscreenCanvasAvailable: true,
    mediaCapabilitiesAvailable: true,
    supportedCodecs: ["avc1.640028"],
    smoothCodecs: ["avc1.640028"],
    powerEfficientCodecs: ["avc1.640028"]
  });

  assert.equal(result.supported, false);
  assert.equal(result.webCodecs, false);
  assert.match(result.reason ?? "", /WebCodecs/);
});

test("WebCodecs 支持但媒体能力不流畅时保持关闭", () => {
  const result = evaluateDirectEnhancementCapabilities({
    videoDecoderAvailable: true,
    videoFrameAvailable: true,
    webGpuAvailable: true,
    gpuDeviceAvailable: true,
    offscreenCanvasAvailable: true,
    mediaCapabilitiesAvailable: true,
    supportedCodecs: ["avc1.640028"],
    smoothCodecs: [],
    powerEfficientCodecs: []
  });

  assert.equal(result.supported, false);
  assert.match(result.reason ?? "", /流畅度/);
});
