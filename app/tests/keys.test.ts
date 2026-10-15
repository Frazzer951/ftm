import assert from "node:assert/strict";
import { test } from "node:test";
import { clipboardKey, paneKey } from "../src/keys.ts";
import { DISAMBIGUATE, encodeKey, kittyKeyboard, type KeyEvent } from "../src/kitty.ts";

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
  assert.equal(enc(key("1", "Numpad1")), "\x1b[57400u");
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
  assert.equal(encodeKey(key("∫", "KeyB", { altKey: true }), DISAMBIGUATE, false), undefined);
  assert.equal(encodeKey(key("∫", "KeyB", { altKey: true }), DISAMBIGUATE, true), "\x1b[98;3u");
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
