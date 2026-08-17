import { strict as assert } from "node:assert";
import { test } from "node:test";
import { resolvePlayerShortcut } from "../player-shortcuts";

test("空格在播放器非编辑态切换播放且忽略长按", () => {
  const target = { closest: () => null };
  assert.equal(resolvePlayerShortcut({ code: "Space", key: " ", target }), "space");
  assert.equal(resolvePlayerShortcut({ code: "Space", key: " ", repeat: true, target }), undefined);
});

test("输入控件和组合键保留自身的键盘行为", () => {
  const input = { closest: () => ({ tagName: "INPUT" }) };
  assert.equal(resolvePlayerShortcut({ code: "Space", key: " ", target: input }), undefined);
  assert.equal(resolvePlayerShortcut({ code: "Space", key: " ", ctrlKey: true }), undefined);
});

test("播放器其他既有快捷键仍可被统一解析", () => {
  assert.equal(resolvePlayerShortcut({ key: "ArrowLeft" }), "arrowleft");
  assert.equal(resolvePlayerShortcut({ key: "Escape" }), "escape");
  assert.equal(resolvePlayerShortcut({ key: "Escape", target: { closest: () => null } }), "escape");
  assert.equal(resolvePlayerShortcut({ key: "Escape", target: { closest: (selector: string) => selector.includes("[role='dialog']") ? {} : null } }), undefined);
  assert.equal(resolvePlayerShortcut({ key: "M" }), "m");
  assert.equal(resolvePlayerShortcut({ key: "Enter" }), undefined);
});
