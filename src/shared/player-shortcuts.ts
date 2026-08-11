export type PlayerShortcutKey =
  | "space"
  | "arrowleft"
  | "arrowright"
  | "arrowup"
  | "arrowdown"
  | "m"
  | "f"
  | "l"
  | "p"
  | "n"
  | "c";

export interface PlayerShortcutEvent {
  key: string;
  code?: string;
  target?: unknown;
  repeat?: boolean;
  isComposing?: boolean;
  defaultPrevented?: boolean;
  altKey?: boolean;
  ctrlKey?: boolean;
  metaKey?: boolean;
}

interface ClosestTarget {
  closest: (selector: string) => unknown;
}

const PLAYER_SHORTCUT_KEYS = new Set<PlayerShortcutKey>([
  "arrowleft",
  "arrowright",
  "arrowup",
  "arrowdown",
  "m",
  "f",
  "l",
  "p",
  "n",
  "c"
]);

const PLAYER_SHORTCUT_BLOCK_SELECTOR = [
  "input",
  "textarea",
  "select",
  "button",
  "a[href]",
  "[contenteditable='true']",
  "[role='button']",
  "[role='dialog']",
  "[role='menu']",
  "[role='slider']",
  "[data-player-shortcut-ignore]"
].join(",");

/** 将键盘事件解析为播放器快捷键，并排除编辑态和控件原生交互。 */
export function resolvePlayerShortcut(event: PlayerShortcutEvent): PlayerShortcutKey | undefined {
  if (
    event.defaultPrevented
    || event.isComposing
    || event.altKey
    || event.ctrlKey
    || event.metaKey
    || isPlayerShortcutBlockedTarget(event.target)
  ) {
    return undefined;
  }

  if (event.code === "Space" || event.key === " " || event.key.toLowerCase() === "spacebar") {
    return event.repeat ? undefined : "space";
  }

  const key = event.key.toLowerCase() as PlayerShortcutKey;
  return PLAYER_SHORTCUT_KEYS.has(key) ? key : undefined;
}

/** 判断事件目标是否需要保留输入、按钮或弹层自身的键盘行为。 */
function isPlayerShortcutBlockedTarget(target: unknown): boolean {
  if (!target || typeof (target as Partial<ClosestTarget>).closest !== "function") {
    return false;
  }
  return Boolean((target as ClosestTarget).closest(PLAYER_SHORTCUT_BLOCK_SELECTOR));
}
