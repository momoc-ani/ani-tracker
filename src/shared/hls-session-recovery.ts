export const MAX_AUTOMATIC_HLS_SESSION_RECOVERIES = 2;

export interface HlsSessionRecoveryInput {
  activeSessionId?: string;
  failedSessionId: string;
  positionSeconds: number;
  durationSeconds?: number;
  attempts: number;
}

export interface HlsSessionRecoveryPlan {
  positionSeconds: number;
  nextAttempts: number;
}

/** 为可恢复的 HLS 中断生成有界重建计划，拒绝旧会话和无效媒体时间。 */
export function planHlsSessionRecovery(
  input: HlsSessionRecoveryInput
): HlsSessionRecoveryPlan | undefined {
  if (
    input.activeSessionId !== input.failedSessionId
    || input.attempts >= MAX_AUTOMATIC_HLS_SESSION_RECOVERIES
  ) return undefined;
  const duration = Number.isFinite(input.durationSeconds) && (input.durationSeconds ?? 0) > 0
    ? input.durationSeconds
    : undefined;
  const positionSeconds = Number.isFinite(input.positionSeconds)
    ? Math.max(0, duration === undefined ? input.positionSeconds : Math.min(input.positionSeconds, duration))
    : 0;
  return { positionSeconds, nextAttempts: input.attempts + 1 };
}
