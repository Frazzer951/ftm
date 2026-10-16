import assert from "node:assert/strict";
import { test } from "node:test";
import { clipboardKey, optionIsMeta, paneKey } from "../src/keys.ts";
import { DISAMBIGUATE, encodeKey, KeyLayout, kittyKeyboard, type KeyEvent } from "../src/kitty.ts";

type Handler = (params: (number | number[])[]) => boolean;

/// Just enough of xterm's parser and buffers to drive the protocol's handlers.
function fakeTerm() {
  const csi = new Map<string, Handler>();
  let ris = () => false;
  let onChange: (b: { type: string }) => void = () => {};
  const active = { type: "normal" };
  const term = {
    parser: {
      registerCsiHandler: (id: { prefix: string; final: string }, h: Handler) => {
        csi.set(id.prefix + id.final, h);
        return { dispose: () => csi.delete(id.prefix + id.final) };
      },
      registerEscHandler: (_id: unknown, h: () => boolean) => {
        ris = h;
        return { dispose() {} };
      },
    },
    buffer: {
      active,
      onBufferChange: (h: typeof onChange) => {
        onChange = h;
        return { dispose() {} };
      },
    },
  };
  const replies: string[] = [];
  // biome-ignore lint/suspicious/noExplicitAny: a structural stand-in for Terminal
  const kitty = kittyKeyboard(term as any, (d) => replies.push(d));
  return {
    kitty,
    replies,
    seq: (prefix: string, ...params: number[]) => csi.get(`${prefix}u`)?.(params),
    ris: () => ris(),
    screen: (type: "normal" | "alternate") => {
      active.type = type;
      onChange({ type });
    },
  };
}

const key = (k: string, code: string, mods: Partial<KeyEvent> = {}): KeyEvent => ({
  key: k,
  code,
  shiftKey: false,
  altKey: false,
  ctrlKey: false,
  metaKey: false,
  isComposing: false,
  ...mods,
});

test("push, pop, set and query", () => {
  const t = fakeTerm();
  t.seq("?");
  t.seq(">", 5);
  t.seq("?");
  t.seq(">", 1);
  t.seq("?");
  t.seq("<");
  t.seq("?");
  t.seq("<");
  t.seq("?");
  assert.deepEqual(t.replies, ["\x1b[?0u", "\x1b[?5u", "\x1b[?1u", "\x1b[?5u", "\x1b[?0u"]);
});

test("unsupported flags are not reported back", () => {
  const t = fakeTerm();
  t.seq(">", 31);
  assert.equal(t.kitty.flags, 5);
});

test("set modes replace, add and remove flags", () => {
  const t = fakeTerm();
  t.seq("=", 1);
  assert.equal(t.kitty.flags, 1);
  t.seq("=", 4, 2);
  assert.equal(t.kitty.flags, 5);
  t.seq("=", 1, 3);
  assert.equal(t.kitty.flags, 4);
});

test("popping more than was pushed clears the flags", () => {
  const t = fakeTerm();
  t.seq(">", 1);
  t.seq(">", 5);
  t.seq("<", 5);
  assert.equal(t.kitty.flags, 0);
});

test("the stack is capped", () => {
  const t = fakeTerm();
  for (let i = 0; i < 40; i++) t.seq(">", 1);
  t.seq("<", 15);
  assert.equal(t.kitty.flags, 1);
  t.seq("<");
  assert.equal(t.kitty.flags, 0);
});

test("each screen has its own flags, and the alternate one starts clean", () => {
  const t = fakeTerm();
  t.seq(">", 1);
  t.screen("alternate");
  assert.equal(t.kitty.flags, 0);
  t.seq(">", 5);
  t.screen("normal");
  assert.equal(t.kitty.flags, 1);
  t.screen("alternate");
  assert.equal(t.kitty.flags, 0);
});

test("RIS and reset clear the flags and let xterm reset too", () => {
  const t = fakeTerm();
  t.seq(">", 1);
  assert.equal(t.ris(), false);
  assert.equal(t.kitty.flags, 0);
  t.seq(">", 1);
  t.kitty.reset();
  assert.equal(t.kitty.flags, 0);
});

test("encoding under disambiguate", () => {
  const f = DISAMBIGUATE;
  const enc = (e: KeyEvent) => encodeKey(e, f, true);
  assert.equal(enc(key("Enter", "Enter", { shiftKey: true })), "\x1b[13;2u");
  assert.equal(enc(key("Enter", "Enter", { ctrlKey: true })), "\x1b[13;5u");
  assert.equal(enc(key("Enter", "Enter")), undefined);
  assert.equal(enc(key("Tab", "Tab", { ctrlKey: true })), "\x1b[9;5u");
  assert.equal(enc(key("Tab", "Tab", { shiftKey: true })), "\x1b[9;2u");
  assert.equal(enc(key("Backspace", "Backspace")), undefined);
  assert.equal(enc(key("Backspace", "Backspace", { ctrlKey: true })), "\x1b[127;5u");
  assert.equal(enc(key("Backspace", "Backspace", { altKey: true })), "\x1b[127;3u");
  assert.equal(enc(key("Escape", "Escape")), "\x1b[27u");
  assert.equal(enc(key("c", "KeyC", { ctrlKey: true })), "\x1b[99;5u");
  assert.equal(enc(key("C", "KeyC", { ctrlKey: true, shiftKey: true })), "\x1b[99;6u");
  assert.equal(enc(key("b", "KeyB", { altKey: true })), "\x1b[98;3u");
  assert.equal(enc(key(" ", "Space", { ctrlKey: true })), "\x1b[32;5u");
  assert.equal(enc(key("F13", "F13")), "\x1b[57376u");
});

test("text and keys xterm already encodes unambiguously are left to xterm", () => {
  const enc = (e: KeyEvent) => encodeKey(e, DISAMBIGUATE, true);
  assert.equal(enc(key("a", "KeyA")), undefined);
  assert.equal(enc(key("A", "KeyA", { shiftKey: true })), undefined);
  assert.equal(enc(key(" ", "Space", { shiftKey: true })), undefined);
  assert.equal(enc(key("ArrowLeft", "ArrowLeft", { ctrlKey: true })), undefined);
  assert.equal(enc(key("Delete", "Delete", { ctrlKey: true })), undefined);
  assert.equal(enc(key("F5", "F5")), undefined);
  assert.equal(enc(key("Shift", "ShiftLeft", { shiftKey: true })), undefined);
  assert.equal(enc(key("a", "KeyA", { ctrlKey: true, isComposing: true })), undefined);
  assert.equal(encodeKey(key("Enter", "Enter", { shiftKey: true }), 0, true), undefined);
});

test("alternate keys add the shifted key", () => {
  const enc = (e: KeyEvent) => encodeKey(e, 0b101, true);
  assert.equal(enc(key("A", "KeyA", { ctrlKey: true, shiftKey: true })), "\x1b[97:65;6u");
  assert.equal(enc(key("!", "Digit1", { ctrlKey: true, shiftKey: true })), "\x1b[49:33;6u");
  assert.equal(enc(key("Enter", "Enter", { shiftKey: true })), "\x1b[13;2u");
});

test("a composing Option is not a modifier, a Meta one is unwound to the physical key", () => {
  assert.equal(encodeKey(key("∫", "KeyB", { altKey: true }), DISAMBIGUATE, false, true), undefined);
  assert.equal(encodeKey(key("∫", "KeyB", { altKey: true }), DISAMBIGUATE, true, true), "\x1b[98;3u");
});

test("a Meta Option unwinds punctuation and Space to the US key, with its shifted form as the alternate", () => {
  const enc = (e: KeyEvent, flags = DISAMBIGUATE) => encodeKey(e, flags, true, true);
  assert.equal(enc(key("≥", "Period", { altKey: true })), "\x1b[46;3u");
  assert.equal(enc(key("\u00a0", "Space", { altKey: true })), "\x1b[32;3u");
  assert.equal(enc(key("÷", "Slash", { altKey: true })), "\x1b[47;3u");
  assert.equal(enc(key("˘", "Period", { altKey: true, shiftKey: true }), 0b101), "\x1b[46:62;4u");
  assert.equal(enc(key("⁄", "Digit1", { altKey: true, shiftKey: true }), 0b101), "\x1b[49:33;4u");
});

test("Alt and Shift follow the layout, not the physical key", () => {
  const enc = (e: KeyEvent, flags = DISAMBIGUATE) => encodeKey(e, flags, true);
  // Dvorak: o sits on QWERTY's S key.
  assert.equal(enc(key("o", "KeyS", { altKey: true })), "\x1b[111;3u");
  assert.equal(enc(key("O", "KeyS", { altKey: true, shiftKey: true }), 0b101), "\x1b[111:79;4u");
  assert.equal(enc(key("O", "KeyS", { ctrlKey: true, shiftKey: true })), "\x1b[111;6u");
  // AZERTY: a sits on QWERTY's Q key.
  assert.equal(enc(key("a", "KeyQ", { ctrlKey: true })), "\x1b[97;5u");
  assert.equal(enc(key("!", "Digit1", { altKey: true, shiftKey: true }), 0b101), "\x1b[49:33;4u");
  assert.equal(enc(key("?", "Slash", { ctrlKey: true, shiftKey: true }), 0b101), "\x1b[47:63;6u");
});

/// A layout learned from typing each `[code, unshifted, shifted]` with and without Shift.
function learned(keys: [string, string, string][]): KeyLayout {
  const layout = new KeyLayout();
  for (const [code, plain, shifted] of keys) {
    layout.learn(key(plain, code));
    layout.learn(key(shifted, code, { shiftKey: true }));
  }
  return layout;
}

const dvorak = learned([
  ["KeyS", "o", "O"],
  ["KeyF", "u", "U"],
  ["Minus", "[", "{"],
]);
const qwertz = learned([
  ["KeyY", "z", "Z"],
  ["KeyZ", "y", "Y"],
  ["Digit5", "5", "%"],
  ["Minus", "ß", "?"],
]);

test("a Meta Option is unwound to the key in the active layout, once it has been typed", () => {
  const enc = (e: KeyEvent, layout: KeyLayout, flags = DISAMBIGUATE) => encodeKey(e, flags, true, true, layout);
  assert.equal(enc(key("ø", "KeyS", { altKey: true }), dvorak), "\x1b[111;3u");
  assert.equal(enc(key("Ø", "KeyS", { altKey: true, shiftKey: true }), dvorak, 0b101), "\x1b[111:79;4u");
  assert.equal(enc(key("“", "Minus", { altKey: true }), dvorak), "\x1b[91;3u");
  assert.equal(enc(key("¥", "KeyZ", { altKey: true }), qwertz), "\x1b[121;3u");
  // German Option+5 composes an ASCII "[", which is still not the key's own character.
  assert.equal(enc(key("[", "Digit5", { altKey: true }), qwertz), "\x1b[53;3u");
  // A key not typed yet falls back to the US layout.
  assert.equal(enc(key("∂", "KeyD", { altKey: true }), dvorak), "\x1b[100;3u");
});

test("a Meta Option that leaves a letter alone did not compose, so the letter counts", () => {
  const enc = (e: KeyEvent, flags = DISAMBIGUATE) => encodeKey(e, flags, true, true, new KeyLayout());
  assert.equal(enc(key("o", "KeyS", { altKey: true })), "\x1b[111;3u");
  assert.equal(enc(key("O", "KeyS", { altKey: true, shiftKey: true }), 0b101), "\x1b[111:79;4u");
});

test("a shifted symbol reports the unshifted key of the active layout as its base", () => {
  const enc = (e: KeyEvent, layout: KeyLayout) => encodeKey(e, 0b101, true, false, layout);
  assert.equal(enc(key("?", "Minus", { ctrlKey: true, shiftKey: true }), qwertz), "\x1b[223:63;6u");
  assert.equal(enc(key("%", "Digit5", { altKey: true, shiftKey: true }), qwertz), "\x1b[53:37;4u");
  assert.equal(enc(key("{", "Minus", { ctrlKey: true, shiftKey: true }), dvorak), "\x1b[91:123;6u");
  // AZERTY: the digit row types symbols unshifted.
  const azerty = learned([["Digit1", "&", "1"]]);
  assert.equal(enc(key("1", "Digit1", { ctrlKey: true, shiftKey: true }), azerty), "\x1b[38:49;6u");
  // Only the unshifted half known yet.
  const half = new KeyLayout();
  half.learn(key("ß", "Minus"));
  assert.equal(enc(key("?", "Minus", { ctrlKey: true, shiftKey: true }), half), "\x1b[223:63;6u");
  // A character the key does not type with Shift in this layout is its own base.
  assert.equal(enc(key("\\", "Minus", { ctrlKey: true, shiftKey: true }), qwertz), "\x1b[92;6u");
});

test("only keys typed with at most Shift held teach the layout, and a changed key drops what was known of it", () => {
  const layout = new KeyLayout();
  layout.learn(key("s", "KeyS", { ctrlKey: true }));
  layout.learn(key("ß", "KeyS", { altKey: true }));
  layout.learn({ ...key("@", "KeyQ"), getModifierState: (m: string) => m === "AltGraph" });
  layout.learn(key("Dead", "Quote"));
  layout.learn(key("1", "Numpad1"));
  layout.learn({ ...key("x", "KeyX"), type: "keyup" });
  for (const code of ["KeyS", "KeyQ", "Quote", "Numpad1", "KeyX"]) assert.equal(layout.key(code), undefined, code);
  layout.learn(key("A", "KeyA"));
  assert.deepEqual(layout.key("KeyA"), ["a", undefined]);
  layout.learn(key("a", "KeyA", { shiftKey: true }));
  assert.deepEqual(layout.key("KeyA"), ["a", "A"]);
  layout.learn(key("q", "KeyA"));
  assert.deepEqual(layout.key("KeyA"), ["q", undefined]);
});

test("keypad keys that type text stay text unless a modifier other than Shift is held", () => {
  const enc = (e: KeyEvent) => encodeKey(e, DISAMBIGUATE, true);
  // NumLock on.
  assert.equal(enc(key("1", "Numpad1")), undefined);
  assert.equal(enc(key("+", "NumpadAdd")), undefined);
  assert.equal(enc(key(".", "NumpadDecimal")), undefined);
  assert.equal(enc(key("*", "NumpadMultiply", { shiftKey: true })), undefined);
  assert.equal(enc(key("1", "Numpad1", { ctrlKey: true })), "\x1b[57400;5u");
  assert.equal(enc(key("/", "NumpadDivide", { altKey: true })), "\x1b[57410;3u");
  assert.equal(enc(key("Enter", "NumpadEnter")), undefined);
  assert.equal(enc(key("Enter", "NumpadEnter", { shiftKey: true })), "\x1b[57414;2u");
  // NumLock off: the keypad's own navigation keys.
  assert.equal(enc(key("End", "Numpad1")), "\x1b[57424u");
  assert.equal(enc(key("ArrowUp", "Numpad8")), "\x1b[57419u");
  assert.equal(enc(key("Delete", "NumpadDecimal")), "\x1b[57426u");
  assert.equal(enc(key("Home", "Numpad7", { ctrlKey: true })), "\x1b[57423;5u");
  assert.equal(enc(key("Clear", "Numpad5")), "\x1b[E");
  assert.equal(enc(key("Clear", "Numpad5", { shiftKey: true })), "\x1b[1;2E");
});

const noKitty = { flags: 0, encode: () => undefined };
const down = (k: string, code: string, mods: Partial<KeyEvent> = {}) => ({ type: "keydown", ...key(k, code, mods) });

test("Shift+Enter falls back to ESC CR without the protocol", () => {
  const ctx = { mac: false, altIsMeta: true, kitty: noKitty };
  assert.equal(paneKey(down("Enter", "Enter", { shiftKey: true }), ctx), "\x1b\r");
  assert.equal(paneKey(down("Enter", "Enter"), ctx), undefined);
  assert.equal(paneKey(down("Enter", "Enter", { shiftKey: true, ctrlKey: true }), ctx), undefined);
  assert.equal(paneKey({ ...down("Enter", "Enter", { shiftKey: true }), type: "keyup" }, ctx), undefined);
});

test("with the protocol on, keys go through it", () => {
  const t = fakeTerm();
  t.seq(">", 1);
  const ctx = { mac: false, altIsMeta: true, kitty: t.kitty };
  assert.equal(paneKey(down("Enter", "Enter", { shiftKey: true }), ctx), "\x1b[13;2u");
  assert.equal(paneKey(down("Enter", "Enter"), ctx), undefined);
});

test("macOS text editing keys are unchanged, with or without the protocol", () => {
  const t = fakeTerm();
  for (const kitty of [noKitty, t.kitty]) {
    const ctx = { mac: true, altIsMeta: false, kitty };
    assert.equal(paneKey(down("ArrowLeft", "ArrowLeft", { altKey: true }), ctx), "\x1bb");
    assert.equal(paneKey(down("ArrowRight", "ArrowRight", { metaKey: true }), ctx), "\x05");
    assert.equal(paneKey(down("Backspace", "Backspace", { altKey: true }), ctx), "\x17");
    assert.equal(paneKey(down("Backspace", "Backspace", { metaKey: true }), ctx), "\x15");
    assert.equal(paneKey(down("Delete", "Delete", { altKey: true }), ctx), "\x1bd");
    assert.equal(paneKey(down("ArrowLeft", "ArrowLeft", { altKey: true, shiftKey: true }), ctx), undefined);
    t.seq(">", 1);
  }
});

test("Cmd is never sent as Super on macOS", () => {
  const t = fakeTerm();
  t.seq(">", 1);
  assert.equal(paneKey(down("c", "KeyC", { metaKey: true }), { mac: true, altIsMeta: false, kitty: t.kitty }), undefined);
  assert.equal(paneKey(down("c", "KeyC", { metaKey: true }), { mac: false, altIsMeta: true, kitty: t.kitty }), "\x1b[99;9u");
});

test("Linux copy and paste keys", () => {
  assert.equal(clipboardKey(down("C", "KeyC", { ctrlKey: true, shiftKey: true }), false), "copy");
  assert.equal(clipboardKey(down("V", "KeyV", { ctrlKey: true, shiftKey: true }), false), "paste");
  assert.equal(clipboardKey(down("Insert", "Insert", { shiftKey: true }), false), "paste");
  assert.equal(clipboardKey(down("c", "KeyC", { ctrlKey: true }), false), undefined);
  assert.equal(clipboardKey(down("v", "KeyV", { ctrlKey: true }), false), undefined);
  assert.equal(clipboardKey(down("Insert", "Insert", { shiftKey: true, ctrlKey: true }), false), undefined);
  assert.equal(clipboardKey({ ...down("C", "KeyC", { ctrlKey: true, shiftKey: true }), type: "keyup" }, false), undefined);
  assert.equal(clipboardKey(down("C", "KeyC", { ctrlKey: true, shiftKey: true }), true), undefined);
});

test("Linux word editing keys, and the protocol's encoding when it is on", () => {
  const ctx = { mac: false, altIsMeta: true, kitty: noKitty };
  assert.equal(paneKey(down("Backspace", "Backspace", { ctrlKey: true }), ctx), "\x17");
  assert.equal(paneKey(down("Delete", "Delete", { ctrlKey: true }), ctx), "\x1bd");
  assert.equal(paneKey(down("ArrowLeft", "ArrowLeft", { ctrlKey: true }), ctx), undefined);
  assert.equal(paneKey(down("Backspace", "Backspace", { altKey: true }), ctx), undefined);
  assert.equal(paneKey(down("Backspace", "Backspace", { ctrlKey: true, shiftKey: true }), ctx), undefined);
  assert.equal(paneKey(down("Backspace", "Backspace", { ctrlKey: true }), { ...ctx, mac: true, altIsMeta: false }), undefined);
  const t = fakeTerm();
  t.seq(">", 1);
  const kctx = { ...ctx, kitty: t.kitty };
  assert.equal(paneKey(down("Backspace", "Backspace", { ctrlKey: true }), kctx), "\x1b[127;5u");
  assert.equal(paneKey(down("Delete", "Delete", { ctrlKey: true }), kctx), undefined);
});

test("Option as Meta by side", () => {
  const left = new Set(["AltLeft"]);
  const right = new Set(["AltRight"]);
  assert.equal(optionIsMeta("both", left), true);
  assert.equal(optionIsMeta("both", new Set()), true);
  assert.equal(optionIsMeta("off", left), false);
  assert.equal(optionIsMeta("left", left), true);
  assert.equal(optionIsMeta("left", right), false);
  assert.equal(optionIsMeta("right", right), true);
  assert.equal(optionIsMeta("right", left), false);
});

test("a Meta Option goes through the protocol as Alt on macOS, a composing one types its character", () => {
  const t = fakeTerm();
  t.seq(">", 1);
  const b = down("∫", "KeyB", { altKey: true });
  assert.equal(paneKey(b, { mac: true, altIsMeta: true, kitty: t.kitty }), "\x1b[98;3u");
  assert.equal(paneKey(down("≥", "Period", { altKey: true }), { mac: true, altIsMeta: true, kitty: t.kitty }), "\x1b[46;3u");
  // Linux Alt does not compose, so the layout's key counts.
  assert.equal(paneKey(down("o", "KeyS", { altKey: true }), { mac: false, altIsMeta: true, kitty: t.kitty }), "\x1b[111;3u");
  assert.equal(paneKey(b, { mac: true, altIsMeta: false, kitty: t.kitty }), undefined);
  assert.equal(paneKey(down("ø", "KeyS", { altKey: true }), { mac: true, altIsMeta: true, kitty: t.kitty, layout: dvorak }), "\x1b[111;3u");
});
