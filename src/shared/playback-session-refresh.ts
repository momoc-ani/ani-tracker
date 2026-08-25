export interface PlaybackSessionRefreshOptions<T> {
  intervalMs: number;
  refresh(): Promise<T>;
  onSession(session: T): void;
  onError(error: unknown): void;
  schedule(callback: () => void, intervalMs: number): number;
  cancel(timer: number): void;
}

/** 立即读取一次会话并按固定间隔刷新，清理后丢弃所有迟到结果。 */
export function startPlaybackSessionRefresh<T>(options: PlaybackSessionRefreshOptions<T>): () => void {
  let active = true;
  const refresh = (): void => {
    void options.refresh()
      .then((session) => {
        if (active) options.onSession(session);
      })
      .catch((error: unknown) => {
        if (active) options.onError(error);
      });
  };
  const timer = options.schedule(refresh, options.intervalMs);
  refresh();
  return () => {
    active = false;
    options.cancel(timer);
  };
}
