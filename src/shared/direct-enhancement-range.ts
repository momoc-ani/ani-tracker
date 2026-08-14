import {
  isDirectEnhancementRetryableStatus,
  parseDirectEnhancementContentRange,
  type DirectEnhancementContentRange
} from "./direct-enhancement-media";

const DEFAULT_MAXIMUM_RANGE_RETRIES = 2;
const DEFAULT_RANGE_RETRY_BASE_DELAY_MS = 150;

export interface DirectEnhancementRangeTelemetry {
  requestCount: number;
  rangeRequestCount: number;
  receivedRangeBytes: number;
  contentRanges: string[];
  retryCount: number;
  recoveredRangeCount: number;
  networkFailureCount: number;
  lastNetworkError?: string;
}

export interface DirectEnhancementRangeOptions {
  maximumRangeRequests?: number;
  maximumReceivedBytes?: number;
  maximumRangeRetries?: number;
  rangeRetryBaseDelayMs?: number;
}

export function createDirectEnhancementRangeTelemetry(): DirectEnhancementRangeTelemetry {
  return {
    requestCount: 0,
    rangeRequestCount: 0,
    receivedRangeBytes: 0,
    contentRanges: [],
    retryCount: 0,
    recoveredRangeCount: 0,
    networkFailureCount: 0
  };
}

/** 为 Mediabunny Range 请求增加严格协议校验、预算和断点续传。 */
export function createDirectEnhancementRangeFetch(
  baseFetch: typeof fetch,
  telemetry: DirectEnhancementRangeTelemetry,
  limits: DirectEnhancementRangeOptions
): typeof fetch {
  return async (input, init) => {
    const requestHeaders = new Headers(input instanceof Request ? input.headers : undefined);
    new Headers(init?.headers).forEach((value, key) => requestHeaders.set(key, value));
    const range = requestHeaders.get("range");
    if (!range) {
      telemetry.requestCount += 1;
      return baseFetch(input, init);
    }

    const maximumRetries = normalizeRangeRetryCount(limits.maximumRangeRetries);
    const signal = init?.signal ?? (input instanceof Request ? input.signal : undefined);
    const requestedRange = parseRequestedByteRange(range);
    const result = await fetchRangeResponse({
      baseFetch,
      input,
      init,
      requestHeaders,
      telemetry,
      limits,
      signal,
      maximumRetries,
      retriesUsed: 0,
      expectedStartByte: requestedRange?.startByte
    });
    const targetEndByte = requestedRange?.endByte === undefined
      ? result.contentRange.endByte
      : Math.min(
        requestedRange.endByte,
        result.contentRange.totalBytes === undefined
          ? requestedRange.endByte
          : result.contentRange.totalBytes - 1
      );
    return monitorResponseBody(result.response, telemetry, limits, {
      baseFetch,
      input,
      init,
      requestHeaders,
      signal,
      maximumRetries,
      retriesUsed: result.retriesUsed,
      initialStartByte: result.contentRange.startByte,
      targetEndByte
    });
  };
}

function monitorResponseBody(
  response: Response,
  telemetry: DirectEnhancementRangeTelemetry,
  limits: DirectEnhancementRangeOptions,
  recovery: RangeRecoveryContext
): Response {
  if (!response.body) return response;
  let reader: ReadableStreamDefaultReader<Uint8Array<ArrayBufferLike>> = response.body.getReader();
  let deliveredBytes = 0;
  let retriesUsed = recovery.retriesUsed;
  const expectedBytes = recovery.targetEndByte - recovery.initialStartByte + 1;
  const body = new ReadableStream<Uint8Array>({
    async pull(controller) {
      while (true) {
        try {
          const result = await reader.read();
          if (result.done) {
            if (deliveredBytes >= expectedBytes) {
              controller.close();
              return;
            }
            throw new Error(`Range 响应提前结束，缺少 ${expectedBytes - deliveredBytes} 字节`);
          }
          if (deliveredBytes + result.value.byteLength > expectedBytes) {
            await reader.cancel();
            controller.error(new Error("F5-H Range 恢复响应超过目标字节范围"));
            return;
          }
          telemetry.receivedRangeBytes += result.value.byteLength;
          if (
            limits.maximumReceivedBytes !== undefined
            && telemetry.receivedRangeBytes > limits.maximumReceivedBytes
          ) {
            await reader.cancel();
            controller.error(new Error(`F5-B Range 实际读取超过 ${limits.maximumReceivedBytes} 字节上限`));
            return;
          }
          deliveredBytes += result.value.byteLength;
          controller.enqueue(result.value);
          return;
        } catch (error) {
          if (isAbortFailure(error, recovery.signal)) {
            controller.error(error);
            return;
          }
          try {
            const recovered = await recoverRangeReader({
              ...recovery,
              telemetry,
              limits,
              retriesUsed,
              nextStartByte: recovery.initialStartByte + deliveredBytes
            }, error);
            reader = recovered.reader;
            retriesUsed = recovered.retriesUsed;
          } catch (recoveryError) {
            controller.error(recoveryError);
            return;
          }
        }
      }
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

interface RequestedByteRange {
  startByte: number;
  endByte?: number;
}

interface RangeFetchContext {
  baseFetch: typeof fetch;
  input: Parameters<typeof fetch>[0];
  init?: Parameters<typeof fetch>[1];
  requestHeaders: Headers;
  telemetry: DirectEnhancementRangeTelemetry;
  limits: DirectEnhancementRangeOptions;
  signal?: AbortSignal | null;
  maximumRetries: number;
  retriesUsed: number;
  expectedStartByte?: number;
  initialError?: unknown;
}

interface RangeFetchResult {
  response: Response;
  contentRange: DirectEnhancementContentRange;
  retriesUsed: number;
}

interface RangeRecoveryContext extends Omit<RangeFetchContext, "telemetry" | "limits" | "expectedStartByte"> {
  initialStartByte: number;
  targetEndByte: number;
}

async function fetchRangeResponse(context: RangeFetchContext): Promise<RangeFetchResult> {
  let retriesUsed = context.retriesUsed;
  let previousError = context.initialError;
  while (true) {
    if (previousError !== undefined) {
      recordNetworkFailure(context.telemetry, previousError);
      if (retriesUsed >= context.maximumRetries) {
        throw createRangeRetryError(previousError, retriesUsed);
      }
      retriesUsed += 1;
      context.telemetry.retryCount += 1;
      await waitForRangeRetry(context.limits, retriesUsed, context.signal);
    }

    let response: Response;
    try {
      response = await performRangeFetch(context, context.requestHeaders);
    } catch (error) {
      if (isAbortFailure(error, context.signal)) throw error;
      if (error instanceof DirectEnhancementRangeLimitError) throw error;
      previousError = error;
      continue;
    }
    if (isDirectEnhancementRetryableStatus(response.status)) {
      void response.body?.cancel();
      previousError = new Error(`Range 请求返回可重试状态 ${response.status}`);
      continue;
    }
    const contentRange = validateRangeResponse(response, context.expectedStartByte);
    context.telemetry.contentRanges.push(response.headers.get("content-range")!);
    if (retriesUsed > context.retriesUsed) context.telemetry.recoveredRangeCount += 1;
    return { response, contentRange, retriesUsed };
  }
}

async function recoverRangeReader(
  context: RangeRecoveryContext & {
    telemetry: DirectEnhancementRangeTelemetry;
    limits: DirectEnhancementRangeOptions;
    nextStartByte: number;
  },
  error: unknown
): Promise<{
  reader: ReadableStreamDefaultReader<Uint8Array<ArrayBufferLike>>;
  retriesUsed: number;
}> {
  if (context.nextStartByte > context.targetEndByte) {
    throw new Error("F5-H Range 恢复位置超过目标范围");
  }
  const headers = new Headers(context.requestHeaders);
  headers.set("range", `bytes=${context.nextStartByte}-${context.targetEndByte}`);
  const result = await fetchRangeResponse({
    ...context,
    requestHeaders: headers,
    expectedStartByte: context.nextStartByte,
    retriesUsed: context.retriesUsed,
    initialError: error
  });
  if (!result.response.body) {
    throw new Error("F5-H Range 恢复响应没有可读数据");
  }
  return { reader: result.response.body.getReader(), retriesUsed: result.retriesUsed };
}

async function performRangeFetch(context: RangeFetchContext, headers: Headers): Promise<Response> {
  context.telemetry.requestCount += 1;
  context.telemetry.rangeRequestCount += 1;
  if (
    context.limits.maximumRangeRequests !== undefined
    && context.telemetry.rangeRequestCount > context.limits.maximumRangeRequests
  ) {
    throw new DirectEnhancementRangeLimitError(
      `F5-B Range 请求超过 ${context.limits.maximumRangeRequests} 次上限`
    );
  }
  return context.baseFetch(context.input, { ...context.init, headers });
}

function validateRangeResponse(response: Response, expectedStartByte?: number): DirectEnhancementContentRange {
  if (response.status !== 206) {
    void response.body?.cancel();
    throw new Error(`F5-B Range 请求未返回 206，实际状态 ${response.status}`);
  }
  const rawContentRange = response.headers.get("content-range");
  const contentRange = parseDirectEnhancementContentRange(rawContentRange);
  if (!contentRange) {
    void response.body?.cancel();
    throw new Error("F5-B Range 响应缺少有效 Content-Range");
  }
  if (expectedStartByte !== undefined && contentRange.startByte !== expectedStartByte) {
    void response.body?.cancel();
    throw new Error(`F5-H Range 恢复起点不一致，期望 ${expectedStartByte}，实际 ${contentRange.startByte}`);
  }
  return contentRange;
}

function parseRequestedByteRange(value: string): RequestedByteRange | undefined {
  const match = /^bytes=(\d+)-(\d*)$/i.exec(value.trim());
  if (!match) return undefined;
  const startByte = Number(match[1]);
  const endByte = match[2] ? Number(match[2]) : undefined;
  if (
    !Number.isSafeInteger(startByte)
    || startByte < 0
    || (endByte !== undefined && (!Number.isSafeInteger(endByte) || endByte < startByte))
  ) return undefined;
  return { startByte, ...(endByte === undefined ? {} : { endByte }) };
}

function normalizeRangeRetryCount(value: number | undefined): number {
  return Number.isSafeInteger(value)
    ? Math.min(5, Math.max(0, Number(value)))
    : DEFAULT_MAXIMUM_RANGE_RETRIES;
}

async function waitForRangeRetry(
  limits: DirectEnhancementRangeOptions,
  retryNumber: number,
  signal?: AbortSignal | null
): Promise<void> {
  const baseDelayMs = Number.isFinite(limits.rangeRetryBaseDelayMs)
    ? Math.min(2_000, Math.max(0, Number(limits.rangeRetryBaseDelayMs)))
    : DEFAULT_RANGE_RETRY_BASE_DELAY_MS;
  const delayMs = baseDelayMs * 2 ** Math.max(0, retryNumber - 1);
  if (delayMs <= 0) return;
  await new Promise<void>((resolve, reject) => {
    const abort = (): void => {
      clearTimeout(timer);
      reject(new DOMException("F5-H Range 恢复已取消", "AbortError"));
    };
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", abort);
      resolve();
    }, delayMs);
    if (signal?.aborted) abort();
    else signal?.addEventListener("abort", abort, { once: true });
  });
}

function recordNetworkFailure(telemetry: DirectEnhancementRangeTelemetry, error: unknown): void {
  telemetry.networkFailureCount += 1;
  telemetry.lastNetworkError = error instanceof Error ? error.message : "Range 网络请求失败";
}

function createRangeRetryError(error: unknown, retriesUsed: number): Error {
  const detail = error instanceof Error ? error.message : "Range 网络请求失败";
  return new Error(`F5-H Range 网络恢复在 ${retriesUsed} 次重试后失败：${detail}`);
}

function isAbortFailure(error: unknown, signal?: AbortSignal | null): boolean {
  return Boolean(signal?.aborted)
    || (error instanceof DOMException && error.name === "AbortError");
}

class DirectEnhancementRangeLimitError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "DirectEnhancementRangeLimitError";
  }
}
