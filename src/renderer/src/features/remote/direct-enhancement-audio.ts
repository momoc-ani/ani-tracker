import { AudioBufferSink, type InputAudioTrack, type WrappedAudioBuffer } from "mediabunny";
import { DirectEnhancementMediaClock } from "@shared/direct-enhancement-media";

const AUDIO_SCHEDULE_AHEAD_SECONDS = 1.5;
const AUDIO_SCHEDULE_INTERVAL_MS = 120;
const AUDIO_MAX_SCHEDULED_BUFFERS = 96;
const AUDIO_CONTEXT_RESUME_TIMEOUT_MS = 2_000;

export interface DirectEnhancementAudioPlaybackOptions {
  durationSeconds: number;
  initialPositionSeconds: number;
  signal?: AbortSignal;
  onError?: (error: Error) => void;
  onPosition?: (positionSeconds: number, running: boolean, ended: boolean) => void;
}

export interface DirectEnhancementAudioPlayback {
  readonly durationSeconds: number;
  readonly hasAudioTrack: boolean;
  getPositionSeconds(): number;
  isRunning(): boolean;
  play(): Promise<void>;
  pause(): void;
  seek(positionSeconds: number): Promise<void>;
  setPlaybackRate(rate: number): void;
  setVolume(volume: number): void;
  setMuted(muted: boolean): void;
  dispose(): Promise<void>;
}

/** 使用 AudioDecoder 解码独立音轨，并由 AudioContext 作为增强链主时钟。 */
export async function createDirectEnhancementAudioPlayback(
  audioTrack: InputAudioTrack | null,
  options: DirectEnhancementAudioPlaybackOptions
): Promise<DirectEnhancementAudioPlayback> {
  const playback = new DirectEnhancementAudioPlaybackImpl(audioTrack, options);
  await playback.initialize();
  return playback;
}

class DirectEnhancementAudioPlaybackImpl implements DirectEnhancementAudioPlayback {
  readonly durationSeconds: number;
  readonly hasAudioTrack: boolean;

  private readonly context: AudioContext;
  private readonly gain: GainNode;
  private readonly audioTrack: InputAudioTrack | null;
  private readonly sink?: AudioBufferSink;
  private readonly clock: DirectEnhancementMediaClock;
  private readonly scheduledSources = new Set<AudioBufferSourceNode>();
  private readonly options: DirectEnhancementAudioPlaybackOptions;
  private iterator?: AsyncGenerator<WrappedAudioBuffer>;
  private scheduleTimer?: number;
  private generation = 0;
  private scheduleThroughSeconds = 0;
  private running = false;
  private disposed = false;
  private scheduling?: Promise<void>;
  private playbackRate = 1;
  private volume = 1;
  private muted = false;

  constructor(
    audioTrack: InputAudioTrack | null,
    options: DirectEnhancementAudioPlaybackOptions
  ) {
    this.options = options;
    this.audioTrack = audioTrack;
    this.durationSeconds = Math.max(0, options.durationSeconds);
    this.hasAudioTrack = audioTrack !== null;
    this.context = new AudioContext({ latencyHint: "interactive" });
    this.gain = this.context.createGain();
    this.gain.connect(this.context.destination);
    this.sink = audioTrack ? new AudioBufferSink(audioTrack) : undefined;
    this.clock = new DirectEnhancementMediaClock(() => this.context.currentTime);
    this.clock.seek(normalizePosition(options.initialPositionSeconds, this.durationSeconds));
    this.gain.gain.value = 0;
    options.signal?.addEventListener("abort", this.handleAbort, { once: true });
  }

  async initialize(): Promise<void> {
    if (this.options.signal?.aborted) throw abortError();
    if (this.sink) {
      // AudioBufferSink uses the same WebCodecs AudioDecoder path, but this explicit
      // check keeps the capability contract visible before a decoder is allocated.
      const config = await this.audioTrack!.getDecoderConfig();
      if (!config || typeof AudioDecoder === "undefined") {
        throw new Error("当前浏览器没有可用的 AudioDecoder 音频配置");
      }
      const support = await AudioDecoder.isConfigSupported(config);
      if (!support.supported) {
        throw new Error(`当前浏览器不能解码媒体源音频配置 ${config.codec}`);
      }
    }
    this.setGain();
  }

  getPositionSeconds(): number {
    return normalizePosition(this.clock.snapshot().positionSeconds, this.durationSeconds);
  }

  isRunning(): boolean {
    return this.running;
  }

  async play(): Promise<void> {
    this.throwIfDisposed();
    if (this.running) return;
    if (this.options.signal?.aborted) throw abortError();
    await resumeAudioContext(this.context);
    this.throwIfDisposed();
    this.clock.play();
    this.running = true;
    this.emitPosition();
    await this.scheduleAudio();
    this.scheduleNext();
  }

  pause(): void {
    if (this.disposed || !this.running) return;
    this.clock.pause();
    this.running = false;
    this.clearScheduleTimer();
    this.stopScheduledSources();
    void this.context.suspend().catch((error) => this.report(error));
    this.emitPosition();
  }

  async seek(positionSeconds: number): Promise<void> {
    this.throwIfDisposed();
    const target = normalizePosition(positionSeconds, this.durationSeconds);
    const wasRunning = this.running;
    this.clock.seek(target);
    this.generation += 1;
    this.clearScheduleTimer();
    this.stopScheduledSources();
    await this.resetIterator();
    this.scheduleThroughSeconds = target;
    this.emitPosition();
    if (wasRunning) await this.scheduleAudio();
    if (wasRunning) this.scheduleNext();
  }

  setPlaybackRate(rate: number): void {
    this.throwIfDisposed();
    if (!Number.isFinite(rate) || rate < 0.25 || rate > 4) {
      throw new RangeError("直传增强音频倍速必须在 0.25 到 4 之间");
    }
    if (rate === this.playbackRate) return;
    const position = this.getPositionSeconds();
    const wasRunning = this.running;
    this.playbackRate = rate;
    this.clock.setPlaybackRate(rate);
    this.clock.seek(position);
    this.generation += 1;
    this.clearScheduleTimer();
    this.stopScheduledSources();
    void this.resetIterator();
    this.scheduleThroughSeconds = position;
    if (wasRunning) {
      void this.scheduleAudio().then(() => this.scheduleNext()).catch((error) => this.report(error));
    }
    this.emitPosition();
  }

  setVolume(volume: number): void {
    if (!Number.isFinite(volume) || volume < 0 || volume > 1) {
      throw new RangeError("直传增强音量必须在 0 到 1 之间");
    }
    this.volume = volume;
    this.setGain();
  }

  setMuted(muted: boolean): void {
    this.muted = muted;
    this.setGain();
  }

  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    this.generation += 1;
    this.clearScheduleTimer();
    this.stopScheduledSources();
    await this.resetIterator();
    this.options.signal?.removeEventListener("abort", this.handleAbort);
    try {
      await this.context.close();
    } catch {
      // The context may already be closed after a browser-side device failure.
    }
  }

  private readonly handleAbort = (): void => {
    void this.dispose();
  };

  private async scheduleAudio(): Promise<void> {
    if (this.scheduling) return this.scheduling;
    const generation = this.generation;
    this.scheduling = this.scheduleAudioWindow(generation)
      .catch((error) => this.report(error))
      .finally(() => {
        this.scheduling = undefined;
      });
    return this.scheduling;
  }

  private async scheduleAudioWindow(generation: number): Promise<void> {
    if (!this.running || this.disposed || !this.sink) return;
    const position = this.getPositionSeconds();
    const target = Math.min(this.durationSeconds || Infinity, position + AUDIO_SCHEDULE_AHEAD_SECONDS);
    const contextNow = this.context.currentTime;
    while (
      generation === this.generation
      && this.running
      && !this.disposed
      && this.scheduleThroughSeconds < target
      && this.scheduledSources.size < AUDIO_MAX_SCHEDULED_BUFFERS
    ) {
      if (!this.iterator) {
        this.iterator = this.sink.buffers(this.scheduleThroughSeconds, this.durationSeconds || undefined);
      }
      const next = await this.iterator.next();
      if (generation !== this.generation || !this.running || this.disposed) return;
      if (next.done) {
        this.scheduleThroughSeconds = target;
        break;
      }
      const buffer = next.value;
      const bufferStart = Math.max(0, buffer.timestamp);
      const bufferEnd = Math.max(bufferStart, buffer.timestamp + buffer.duration);
      const startPosition = Math.max(position, bufferStart);
      const offsetSeconds = Math.max(0, startPosition - bufferStart);
      const remainingDuration = Math.max(0, bufferEnd - startPosition);
      if (remainingDuration <= 0) {
        this.scheduleThroughSeconds = Math.max(this.scheduleThroughSeconds, bufferEnd);
        continue;
      }
      const source = this.context.createBufferSource();
      source.buffer = buffer.buffer;
      source.playbackRate.value = this.playbackRate;
      source.connect(this.gain);
      const when = contextNow + Math.max(0, (startPosition - position) / this.playbackRate);
      source.onended = () => this.scheduledSources.delete(source);
      source.start(when, offsetSeconds, remainingDuration);
      this.scheduledSources.add(source);
      this.scheduleThroughSeconds = Math.max(this.scheduleThroughSeconds, bufferEnd);
    }
  }

  private scheduleNext(): void {
    this.clearScheduleTimer();
    if (!this.running || this.disposed) return;
    this.scheduleTimer = window.setTimeout(() => {
      void this.scheduleAudio().then(() => {
        if (this.durationSeconds > 0 && this.getPositionSeconds() >= this.durationSeconds) {
          this.clock.pause();
          this.running = false;
          this.stopScheduledSources();
          this.emitPosition();
          return;
        }
        this.emitPosition();
        this.scheduleNext();
      });
    }, AUDIO_SCHEDULE_INTERVAL_MS);
  }

  private clearScheduleTimer(): void {
    if (this.scheduleTimer !== undefined) window.clearTimeout(this.scheduleTimer);
    this.scheduleTimer = undefined;
  }

  private stopScheduledSources(): void {
    for (const source of this.scheduledSources) {
      source.onended = null;
      try {
        source.stop();
      } catch {
        // A source that has already ended is safe to discard.
      }
      source.disconnect();
    }
    this.scheduledSources.clear();
  }

  private async resetIterator(): Promise<void> {
    const iterator = this.iterator;
    this.iterator = undefined;
    if (!iterator?.return) return;
    try {
      await iterator.return(undefined);
    } catch {
      // Input disposal or a generation change may already have canceled decoding.
    }
  }

  private setGain(): void {
    this.gain.gain.value = this.muted ? 0 : this.volume;
  }

  private emitPosition(): void {
    const positionSeconds = this.getPositionSeconds();
    const ended = this.durationSeconds > 0 && positionSeconds >= this.durationSeconds;
    this.options.onPosition?.(positionSeconds, this.running && !ended, ended);
  }

  private report(caught: unknown): void {
    const error = caught instanceof Error ? caught : new Error("直传增强音频调度失败");
    this.options.onError?.(error);
  }

  private throwIfDisposed(): void {
    if (this.disposed) throw new Error("直传增强音频已经关闭");
  }
}

function normalizePosition(positionSeconds: number, durationSeconds: number): number {
  if (!Number.isFinite(positionSeconds) || positionSeconds < 0) return 0;
  return durationSeconds > 0 ? Math.min(positionSeconds, durationSeconds) : positionSeconds;
}

function abortError(): DOMException {
  return new DOMException("直传增强音频初始化已取消", "AbortError");
}

async function resumeAudioContext(context: AudioContext): Promise<void> {
  if (context.state === "running") return;
  let timeout: number | undefined;
  const timeoutPromise = new Promise<never>((_resolve, reject) => {
    timeout = window.setTimeout(() => {
      reject(new Error("浏览器阻止了直传增强音频自动播放，请手动开始播放或关闭增强"));
    }, AUDIO_CONTEXT_RESUME_TIMEOUT_MS);
  });
  try {
    await Promise.race([context.resume(), timeoutPromise]);
  } finally {
    window.clearTimeout(timeout);
  }
  if (readAudioContextState(context) !== "running") {
    throw new Error("浏览器没有启动直传增强 AudioContext 音频输出");
  }
}

function readAudioContextState(context: AudioContext): AudioContextState {
  return context.state;
}
