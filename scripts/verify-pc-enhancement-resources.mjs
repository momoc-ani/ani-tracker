#!/usr/bin/env node
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import { join, resolve } from "node:path";
import process from "node:process";

const SHADER_ROOT = [
  ["Anime4K_Clamp_Highlights.glsl", "f3cdf83652328c04c09bb5bc41a733cfe03198c6be66e14b1bebe5c6a9523986"],
  ["Anime4K_Upscale_Original_x2.glsl", "842feff5fc800c2eb2d504aaf7e864d232a86072cd88edf360e6e93127e0eedc"],
  ["FSRCNNX_x2_8-0-4-1.glsl", "c831d602e28b2bd880e3ffa61f80f9537ce88dcd4ea3ea6ce35a49f4607f969b"],
  ["ArtCNN_C4F16.glsl", "1706bddf4350643b34815c1baa72d26bfebd30e1f0473cf5352507c312757dfd"],
  ["ArtCNN_C4F32.glsl", "b4181db4baecab6669d69d3618f3ade554ffbba5210ba437fe947387e4acf487"]
];

const MODEL_ROOT = [
  {
    directory: "rife-v4.6",
    modelId: "rife-v4.6",
    files: [
      ["flownet.bin", "f334ed2260149ce0188a6dcf049844e8b0cdd912e01cbcfb63553157d2508958"],
      ["flownet.param", "724569596bcd1e7b9fa50455c604777ebed99746d2ef40aa86e31b5725f1053c"]
    ]
  },
  {
    directory: "realesr-animevideov3-x2",
    modelId: "realesr-animevideov3-x2",
    files: [
      ["realesr-animevideov3-x2.bin", "548a36f9c3f4ab8da56cd3b13badf23968bee207b396dad14d04b830e5f2ab2d"],
      ["realesr-animevideov3-x2.param", "b88ff4f00ebf019a7fdac17fdd45a7fd3665d37509efc5baf2e4da2e24420a04"]
    ]
  }
];

if (process.argv[1] && resolve(process.argv[1]) === resolve(new URL(import.meta.url).pathname)) {
  await verifyPcEnhancementResources(resolve("resources"));
  console.log("[pc-enhancement-resources] verified");
}

/** 校验桌面 shader、模型权重、manifest 和来源文件。 */
export async function verifyPcEnhancementResources(resourcesRoot) {
  const shaderDirectory = join(resourcesRoot, "shaders", "anime4k");
  const shaderSource = JSON.parse(await readFile(join(shaderDirectory, "SOURCE.json"), "utf8"));
  for (const [name, expected] of SHADER_ROOT) {
    await verifyFile(join(shaderDirectory, name), expected);
    if (!shaderSource.resources.some((entry) => entry.file === name && entry.sha256 === expected)) {
      throw new Error(`[pc-enhancement-resources] shader manifest mismatch: ${name}`);
    }
  }

  for (const model of MODEL_ROOT) {
    const directory = join(resourcesRoot, "models", model.directory);
    const manifest = JSON.parse(await readFile(join(directory, "manifest.json"), "utf8"));
    if (manifest.model?.modelId !== model.modelId || !Array.isArray(manifest.files)) {
      throw new Error(`[pc-enhancement-resources] invalid model manifest: ${model.modelId}`);
    }
    for (const [name, expected] of model.files) {
      const declared = manifest.files.find((file) => file.path === name);
      if (!declared || declared.sha256 !== expected) {
        throw new Error(`[pc-enhancement-resources] model manifest mismatch: ${model.modelId}/${name}`);
      }
      await verifyFile(join(directory, name), expected);
    }
    await stat(join(directory, "SOURCE.json"));
  }
}

async function verifyFile(path, expected) {
  const info = await stat(path);
  if (!info.isFile() || info.size === 0) throw new Error(`[pc-enhancement-resources] missing file: ${path}`);
  const digest = createHash("sha256");
  for await (const chunk of createReadStream(path)) digest.update(chunk);
  const actual = digest.digest("hex");
  if (actual !== expected) {
    throw new Error(`[pc-enhancement-resources] SHA-256 mismatch: ${path}`);
  }
}
