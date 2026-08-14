import { strict as assert } from "node:assert";
import { test } from "node:test";
import {
  createDirectEnhancementRangeFetch,
  createDirectEnhancementRangeTelemetry
} from "../direct-enhancement-range";

test("F5-H 响应体断流后从下一字节续传并合并为原响应", async () => {
  const requestedRanges: string[] = [];
  let attempt = 0;
  const baseFetch = (async (_input, init) => {
    requestedRanges.push(new Headers(init?.headers).get("range") ?? "");
    attempt += 1;
    if (attempt === 1) {
      let emitted = false;
      return new Response(new ReadableStream<Uint8Array>({
        pull(controller) {
          if (!emitted) {
            emitted = true;
            controller.enqueue(Uint8Array.from([0, 1]));
            return;
          }
          controller.error(new TypeError("socket reset"));
        }
      }), {
        status: 206,
        headers: { "content-range": "bytes 0-5/6" }
      });
    }
    return rangeResponse([2, 3, 4, 5], "bytes 2-5/6");
  }) as typeof fetch;
  const telemetry = createDirectEnhancementRangeTelemetry();
  const rangeFetch = createDirectEnhancementRangeFetch(baseFetch, telemetry, {
    maximumRangeRequests: 4,
    maximumReceivedBytes: 32,
    rangeRetryBaseDelayMs: 0
  });

  const response = await rangeFetch("https://example.test/video.mp4", {
    headers: { range: "bytes=0-5" }
  });
  assert.deepEqual([...new Uint8Array(await response.arrayBuffer())], [0, 1, 2, 3, 4, 5]);
  assert.deepEqual(requestedRanges, ["bytes=0-5", "bytes=2-5"]);
  assert.equal(telemetry.rangeRequestCount, 2);
  assert.equal(telemetry.retryCount, 1);
  assert.equal(telemetry.recoveredRangeCount, 1);
  assert.equal(telemetry.networkFailureCount, 1);
  assert.equal(telemetry.receivedRangeBytes, 6);
});

test("F5-H 可重试状态恢复且 416 不进入重试", async () => {
  let retryableAttempts = 0;
  const retryableFetch = (async () => {
    retryableAttempts += 1;
    return retryableAttempts === 1
      ? new Response(null, { status: 503 })
      : rangeResponse([7, 8], "bytes 0-1/2");
  }) as typeof fetch;
  const retryableTelemetry = createDirectEnhancementRangeTelemetry();
  const rangeFetch = createDirectEnhancementRangeFetch(retryableFetch, retryableTelemetry, {
    maximumRangeRequests: 3,
    rangeRetryBaseDelayMs: 0
  });
  const response = await rangeFetch("https://example.test/video.mp4", {
    headers: { range: "bytes=0-1" }
  });

  assert.deepEqual([...new Uint8Array(await response.arrayBuffer())], [7, 8]);
  assert.equal(retryableTelemetry.retryCount, 1);
  assert.equal(retryableTelemetry.recoveredRangeCount, 1);

  let rangeErrorAttempts = 0;
  const rangeErrorTelemetry = createDirectEnhancementRangeTelemetry();
  const rangeErrorFetch = createDirectEnhancementRangeFetch((async () => {
    rangeErrorAttempts += 1;
    return new Response(null, { status: 416 });
  }) as typeof fetch, rangeErrorTelemetry, { rangeRetryBaseDelayMs: 0 });
  await assert.rejects(
    rangeErrorFetch("https://example.test/video.mp4", { headers: { range: "bytes=0-1" } }),
    /实际状态 416/
  );
  assert.equal(rangeErrorAttempts, 1);
  assert.equal(rangeErrorTelemetry.retryCount, 0);
});

function rangeResponse(bytes: number[], contentRange: string): Response {
  return new Response(Uint8Array.from(bytes), {
    status: 206,
    headers: { "content-range": contentRange }
  });
}
