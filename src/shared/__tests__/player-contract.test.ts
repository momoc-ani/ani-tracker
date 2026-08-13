import { strict as assert } from "node:assert";
import { test } from "node:test";
import {
  acceptPlayerSnapshot,
  createInitialPlayerSnapshot,
  createUnavailablePlayerCapabilities,
  isPlaybackCompleted,
  rejectUnsupportedPlayerCommand,
  type PlayerCapabilities
} from "../player-contract";

const capabilities: PlayerCapabilities = {
  backend: "libvlc",
  platform: "tauri-desktop",
  availability: "available",
  canSeek: true,
  canSetVolume: true,
  canMute: true,
  playbackRates: [0.5, 1, 1.5, 2],
  supportsAudioTracks: true,
  supportsSubtitleTracks: true,
  supportsSubtitleScale: true,
  supportsVideoEnhancement: true,
  supportsFrameInterpolation: false,
  supportsModelEnhancement: false,
  supportsAspectRatio: true,
  supportsFullscreen: true,
  supportsPictureInPicture: false,
  supportsPlaylistNavigation: true,
  supportsDirectPlayback: true,
  supportsTranscodingFallback: true,
  supportsHdr: true
};

test("createInitialPlayerSnapshot 创建稳定的空闲状态", () => {
  const snapshot = createInitialPlayerSnapshot({ sessionId: "session-a", capabilities });

  assert.equal(snapshot.sessionId, "session-a");
  assert.equal(snapshot.sequence, 0);
  assert.equal(snapshot.status, "idle");
  assert.equal(snapshot.volume, 1);
  assert.equal(snapshot.subtitleScale, 100);
  assert.equal(snapshot.videoEnhancement, "off");
  assert.equal(snapshot.frameInterpolation, "off");
  assert.equal(snapshot.enhancementDiagnostics.pipeline, "none");
  assert.deepEqual(snapshot.playlist.items, []);
});

test("createUnavailablePlayerCapabilities 默认关闭所有原生能力", () => {
  const unavailable = createUnavailablePlayerCapabilities("libvlc", "tauri-desktop", "运行时缺失");

  assert.equal(unavailable.availability, "unavailable");
  assert.equal(unavailable.canSeek, false);
  assert.equal(unavailable.supportsDirectPlayback, false);
  assert.equal(unavailable.supportsSubtitleScale, false);
  assert.equal(unavailable.supportsVideoEnhancement, false);
  assert.equal(unavailable.supportsFrameInterpolation, false);
  assert.equal(unavailable.supportsModelEnhancement, false);
  assert.deepEqual(unavailable.playbackRates, [1]);
  assert.equal(unavailable.unavailableReason, "运行时缺失");
});

test("acceptPlayerSnapshot 丢弃旧会话和乱序事件", () => {
  const current = { ...createInitialPlayerSnapshot({ sessionId: "session-a", capabilities }), sequence: 4 };
  const stale = { ...current, sequence: 3, status: "playing" as const };
  const oldSession = { ...current, sessionId: "session-old", sequence: 20 };
  const next = { ...current, sequence: 5, status: "playing" as const };

  assert.equal(acceptPlayerSnapshot("session-a", current, stale), current);
  assert.equal(acceptPlayerSnapshot("session-a", current, oldSession), current);
  assert.equal(acceptPlayerSnapshot("session-a", current, next), next);
});

test("acceptPlayerSnapshot 为旧原生快照补齐增强字段", () => {
  const legacy = createInitialPlayerSnapshot({ sessionId: "session-a", capabilities });
  const payload = { ...legacy, sequence: 1 } as Record<string, unknown>;
  delete payload.frameInterpolation;
  delete payload.enhancementDiagnostics;
  const legacyCapabilities = { ...(payload.capabilities as Record<string, unknown>) };
  delete legacyCapabilities.supportsFrameInterpolation;
  delete legacyCapabilities.supportsModelEnhancement;
  payload.capabilities = legacyCapabilities;

  const accepted = acceptPlayerSnapshot("session-a", undefined, payload as unknown as typeof legacy);
  assert.equal(accepted?.frameInterpolation, "off");
  assert.deepEqual(accepted?.enhancementDiagnostics, { pipeline: "none", droppedFrames: 0 });
  assert.equal(accepted?.capabilities.supportsFrameInterpolation, false);
  assert.equal(accepted?.capabilities.supportsModelEnhancement, false);
});

test("isPlaybackCompleted 在 90% 边界或自然结束时判定完成", () => {
  assert.equal(isPlaybackCompleted("playing", 89.99, 100), false);
  assert.equal(isPlaybackCompleted("playing", 90, 100), true);
  assert.equal(isPlaybackCompleted("ended", 0, 0), true);
  assert.equal(isPlaybackCompleted("playing", Number.NaN, 100), false);
});

test("rejectUnsupportedPlayerCommand 返回可展示的结构化错误", () => {
  assert.deepEqual(rejectUnsupportedPlayerCommand("command-a", "当前平台不支持画中画"), {
    commandId: "command-a",
    accepted: false,
    error: {
      code: "unsupported",
      message: "当前平台不支持画中画",
      recoverable: false,
      recoveryActions: []
    }
  });
});
