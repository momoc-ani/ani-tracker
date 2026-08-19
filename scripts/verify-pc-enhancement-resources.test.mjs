import test from "node:test";
import { resolve } from "node:path";
import { verifyPcEnhancementResources } from "./verify-pc-enhancement-resources.mjs";

test("验证 PC 画质增强 shader 和模型资源清单", async () => {
  await verifyPcEnhancementResources(resolve("resources"));
});
