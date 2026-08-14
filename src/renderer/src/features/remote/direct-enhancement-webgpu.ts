/// <reference types="@webgpu/types" />

export const DIRECT_ENHANCEMENT_WGSL = `
struct VertexOutput {
  @builtin(position) position: vec4<f32>,
  @location(0) uv: vec2<f32>,
}

struct SharpenParams {
  texelSize: vec2<f32>,
  strength: f32,
  padding: f32,
}

@group(0) @binding(0) var source: texture_external;
@group(0) @binding(1) var linearSampler: sampler;
@group(0) @binding(2) var<uniform> params: SharpenParams;

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> VertexOutput {
  var positions = array<vec2<f32>, 6>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(1.0, -1.0),
    vec2<f32>(-1.0, 1.0),
    vec2<f32>(-1.0, 1.0),
    vec2<f32>(1.0, -1.0),
    vec2<f32>(1.0, 1.0),
  );
  let position = positions[index];
  var output: VertexOutput;
  output.position = vec4<f32>(position, 0.0, 1.0);
  output.uv = vec2<f32>(position.x * 0.5 + 0.5, 0.5 - position.y * 0.5);
  return output;
}

fn sample(uv: vec2<f32>) -> vec4<f32> {
  return textureSampleBaseClampToEdge(source, linearSampler, uv);
}

@fragment
fn fragment(input: VertexOutput) -> @location(0) vec4<f32> {
  let center = sample(input.uv);
  let cross = (
    sample(input.uv + vec2<f32>(-params.texelSize.x, 0.0))
    + sample(input.uv + vec2<f32>(params.texelSize.x, 0.0))
    + sample(input.uv + vec2<f32>(0.0, -params.texelSize.y))
    + sample(input.uv + vec2<f32>(0.0, params.texelSize.y))
  ) * 0.25;
  let enhanced = clamp(center.rgb + (center.rgb - cross.rgb) * params.strength, vec3<f32>(0.0), vec3<f32>(1.0));
  return vec4<f32>(enhanced, center.a);
}
`;

const DIRECT_ENHANCEMENT_WGSL_SHA256 = "2d01d34bcf4bd5958b0e25d4146b451ff7546ed3dc0f0899fa1bed8d7a48a957";

export interface DirectEnhancementWebGpuRenderer {
  readonly deviceLost: Promise<GPUDeviceLostInfo>;
  render(frame: VideoFrame, strength?: number): void;
  waitForSubmittedWork(): Promise<void>;
  dispose(): void;
}

/** 校验应用内置 shader，避免远端内容变成可执行 GPU 代码。 */
export async function verifyDirectEnhancementShader(): Promise<boolean> {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) return false;
  const digest = await subtle.digest(
    "SHA-256",
    new TextEncoder().encode(DIRECT_ENHANCEMENT_WGSL)
  );
  const actual = [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  return actual === DIRECT_ENHANCEMENT_WGSL_SHA256;
}

/** 创建 F5-C 的外部 VideoFrame shader 表面；调用方负责管理 frame 生命周期。 */
export async function createDirectEnhancementWebGpuRenderer(
  canvas: HTMLCanvasElement | OffscreenCanvas
): Promise<DirectEnhancementWebGpuRenderer> {
  if (!await verifyDirectEnhancementShader()) {
    throw new Error("F5-C 内置 WGSL shader 摘要校验失败");
  }
  const gpu = navigator.gpu;
  if (!gpu) throw new Error("当前浏览器未提供 WebGPU");
  const adapter = await gpu.requestAdapter();
  if (!adapter) throw new Error("WebGPU adapter 不可用");
  const device = await adapter.requestDevice();
  const context = canvas.getContext("webgpu") as GPUCanvasContext | null;
  if (!context) {
    device.destroy();
    throw new Error("WebGPU canvas context 不可用");
  }
  const format = gpu.getPreferredCanvasFormat();
  context.configure({ device, format, alphaMode: "opaque" });
  const pipeline = device.createRenderPipeline({
    layout: "auto",
    vertex: { module: device.createShaderModule({ code: DIRECT_ENHANCEMENT_WGSL }), entryPoint: "vertex" },
    fragment: {
      module: device.createShaderModule({ code: DIRECT_ENHANCEMENT_WGSL }),
      entryPoint: "fragment",
      targets: [{ format }]
    },
    primitive: { topology: "triangle-list" }
  });
  const sampler = device.createSampler({ magFilter: "linear", minFilter: "linear" });
  const params = device.createBuffer({
    size: 16,
    usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST
  });
  const layout = pipeline.getBindGroupLayout(0);

  return {
    deviceLost: device.lost,
    render(frame, strength = 0.35) {
      const width = Math.max(1, frame.displayWidth || frame.codedWidth);
      const height = Math.max(1, frame.displayHeight || frame.codedHeight);
      device.queue.writeBuffer(params, 0, new Float32Array([
        1 / width,
        1 / height,
        Math.min(0.75, Math.max(0, strength)),
        0
      ]));
      const externalTexture = device.importExternalTexture({ source: frame });
      const bindGroup = device.createBindGroup({
        layout,
        entries: [
          { binding: 0, resource: externalTexture },
          { binding: 1, resource: sampler },
          { binding: 2, resource: { buffer: params } }
        ]
      });
      const encoder = device.createCommandEncoder();
      const pass = encoder.beginRenderPass({
        colorAttachments: [{
          view: context.getCurrentTexture().createView(),
          clearValue: { r: 0, g: 0, b: 0, a: 1 },
          loadOp: "clear",
          storeOp: "store"
        }]
      });
      pass.setPipeline(pipeline);
      pass.setBindGroup(0, bindGroup);
      pass.draw(6);
      pass.end();
      device.queue.submit([encoder.finish()]);
    },
    waitForSubmittedWork() {
      return device.queue.onSubmittedWorkDone();
    },
    dispose() {
      context.unconfigure?.();
      params.destroy();
      device.destroy();
    }
  };
}
