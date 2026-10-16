import type { IDisposable, Terminal } from "@xterm/xterm";

/// The kitty keyboard protocol (https://sw.kovidgoyal.net/kitty/keyboard-protocol/), which xterm.js only gains in
/// 6.1 (`vtExtensions.kittyKeyboard`). Programs push the flags they want; while any are set, keys the legacy
/// encoding cannot tell apart (Shift+Enter, Ctrl+Tab, Ctrl+Shift+letter, Esc) are sent as `CSI code;mods u`.
/// Only "disambiguate" and "report alternate keys" are implemented, and a query reports only those, so a program
/// never waits for release events or text-as-escape-codes it will not get.
export interface KeyProtocol {
  readonly flags: number;
  /// The bytes for a keydown, or undefined when xterm's own encoding is already right for it.
  encode(e: KeyEvent, altIsMeta: boolean, composedAlt?: boolean): string | undefined;
  /// After `term.reset()`, which does not go through the parser.
  reset(): void;
  dispose(): void;
}

export type KeyEvent = Pick<
  KeyboardEvent,
  "key" | "code" | "shiftKey" | "altKey" | "ctrlKey" | "metaKey" | "isComposing"
>;

export const DISAMBIGUATE = 0b1;
export const ALTERNATE_KEYS = 0b100;
const SUPPORTED = DISAMBIGUATE | ALTERNATE_KEYS;
const STACK_LIMIT = 16;

const SHIFT = 1;
const ALT = 2;
const CTRL = 4;
const SUPER = 8;

const functional: Record<string, number> = {
  Escape: 27,
  Enter: 13,
  Tab: 9,
  Backspace: 127,
  PrintScreen: 57361,
  Pause: 57362,
  ContextMenu: 57363,
  ...Object.fromEntries(Array.from({ length: 23 }, (_, i) => [`F${i + 13}`, 57376 + i])),
  MediaPlayPause: 57430,
  MediaStop: 57432,
  MediaTrackNext: 57435,
  MediaTrackPrevious: 57436,
  AudioVolumeDown: 57438,
  AudioVolumeUp: 57439,
  AudioVolumeMute: 57440,
};

/// Keypad keys that type text while NumLock is on: plain text unless a modifier other than Shift is held.
const keypadText: Record<string, number> = {
  Decimal: 57409,
  Divide: 57410,
  Multiply: 57411,
  Subtract: 57412,
  Add: 57413,
  Equal: 57415,
  Comma: 57416,
};

/// What the keypad's digit keys report with NumLock off.
const keypadNav: Record<string, number> = {
  ArrowLeft: 57417,
  ArrowRight: 57418,
  ArrowUp: 57419,
  ArrowDown: 57420,
  PageUp: 57421,
  PageDown: 57422,
  Home: 57423,
  End: 57424,
  Insert: 57425,
  Delete: 57426,
};

type Keypad = { code: number; text: boolean } | "enter" | "begin";

function keypad(e: KeyEvent): Keypad | undefined {
  if (!e.code.startsWith("Numpad")) return;
  if (e.code === "NumpadEnter") return "enter";
  if (e.key in keypadNav) return { code: keypadNav[e.key], text: false };
  if (e.key === "Clear") return "begin";
  const rest = e.code.slice(6);
  if (/^[0-9]$/.test(rest)) return { code: 57399 + Number(rest), text: true };
  return rest in keypadText ? { code: keypadText[rest], text: true } : undefined;
}

/// The US layout's unshifted and shifted character for each text key, which a macOS Meta Option is unwound to: it
/// still composes, so `key` holds the composed character rather than the one on the key.
const usKeys: Record<string, [string, string]> = {
  Backquote: ["`", "~"],
  Minus: ["-", "_"],
  Equal: ["=", "+"],
  BracketLeft: ["[", "{"],
  BracketRight: ["]", "}"],
  Backslash: ["\\", "|"],
  Semicolon: [";", ":"],
  Quote: ["'", '"'],
  Comma: [",", "<"],
  Period: [".", ">"],
  Slash: ["/", "?"],
  Space: [" ", " "],
  ...Object.fromEntries([..."0123456789"].map((d, i) => [`Digit${d}`, [d, ")!@#$%^&*("[i]]])),
  ...Object.fromEntries([..."abcdefghijklmnopqrstuvwxyz"].map((c) => [`Key${c.toUpperCase()}`, [c, c.toUpperCase()]])),
};

const codePoint = (c: string) => c.codePointAt(0) as number;

/// The key's unshifted character, and the shifted one when Shift is held. Taken from `key`, so it follows the active
/// layout, except where `key` cannot say: a composed Option, or a shifted symbol whose unshifted form only the
/// physical key gives.
function textKey(e: KeyEvent, composedAlt: boolean): { code: number; shifted?: number } | undefined {
  const us = usKeys[e.code];
  if (composedAlt && us) {
    return { code: codePoint(us[0]), shifted: e.shiftKey ? codePoint(us[1]) : undefined };
  }
  const chars = [...e.key];
  if (chars.length !== 1) return;
  const ch = chars[0];
  const lower = ch.toLowerCase();
  const cased = lower !== ch.toUpperCase();
  let base = cased && [...lower].length === 1 ? lower : ch;
  if (e.shiftKey && !cased && us && (us[1] === ch || e.code.startsWith("Digit"))) base = us[0];
  return { code: codePoint(base), shifted: e.shiftKey ? codePoint(ch) : undefined };
}

/// `altIsMeta` is false for a macOS Option that composes characters, which then is not a modifier at all.
/// `composedAlt` says Alt still composes `key` when it is Meta, as a macOS Option does.
export function encodeKey(e: KeyEvent, flags: number, altIsMeta: boolean, composedAlt = false): string | undefined {
  if (!(flags & DISAMBIGUATE) || e.isComposing) return;
  const altMod = e.altKey && altIsMeta;
  const bits = (e.shiftKey ? SHIFT : 0) | (altMod ? ALT : 0) | (e.ctrlKey ? CTRL : 0) | (e.metaKey ? SUPER : 0);
  const csi = (key: string | number) => (bits ? `\x1b[${key};${bits + 1}u` : `\x1b[${key}u`);
  const pad = keypad(e);
  if (pad === "begin") return bits ? `\x1b[1;${bits + 1}E` : "\x1b[E";
  if (pad === "enter") return bits ? csi(57414) : undefined;
  if (pad) return pad.text && (bits & ~SHIFT) === 0 ? undefined : csi(pad.code);
  const func = functional[e.key];
  if (func !== undefined) {
    // Enter, Tab and Backspace keep their legacy bytes unmodified; other functional keys always get CSI u.
    const legacyWhenBare = func === 13 || func === 9 || func === 127;
    return legacyWhenBare && !bits ? undefined : csi(func);
  }
  if ((bits & ~SHIFT) === 0) return;
  const text = textKey(e, altMod && composedAlt);
  if (text === undefined) return;
  let key = String(text.code);
  if (flags & ALTERNATE_KEYS && text.shifted !== undefined && text.shifted !== text.code) key += `:${text.shifted}`;
  return csi(key);
}

interface Screen {
  flags: number;
  stack: number[];
}

const fresh = (): Screen => ({ flags: 0, stack: [] });
const first = (p: number | number[] | undefined) => (Array.isArray(p) ? p[0] : p) ?? 0;

/// Each screen keeps its own flags and stack, as the spec asks; the alternate one starts clean every time it is
/// entered, so a full-screen program that died without popping does not leave its flags to the next one.
export function kittyKeyboard(term: Pick<Terminal, "parser" | "buffer">, reply: (data: string) => void): KeyProtocol {
  let main = fresh();
  let alt = fresh();
  const screen = () => (term.buffer.active.type === "alternate" ? alt : main);
  const csi = (prefix: string, handler: (params: (number | number[])[]) => void) =>
    term.parser.registerCsiHandler({ prefix, final: "u" }, (params) => {
      handler(params);
      return true;
    });
  const disposables: IDisposable[] = [
    csi("?", () => reply(`\x1b[?${screen().flags}u`)),
    csi(">", (params) => {
      const s = screen();
      s.stack.push(s.flags);
      if (s.stack.length > STACK_LIMIT) s.stack.shift();
      s.flags = first(params[0]) & SUPPORTED;
    }),
    csi("<", (params) => {
      const s = screen();
      const n = Math.max(1, first(params[0]));
      for (let i = 0; i < n && s.stack.length; i++) s.flags = s.stack.pop() as number;
      if (!s.stack.length) s.flags = 0;
    }),
    csi("=", (params) => {
      const s = screen();
      const flags = first(params[0]) & SUPPORTED;
      const mode = params.length > 1 ? first(params[1]) || 1 : 1;
      if (mode === 1) s.flags = flags;
      else if (mode === 2) s.flags |= flags;
      else if (mode === 3) s.flags &= ~flags;
    }),
    // RIS: let xterm reset everything else too.
    term.parser.registerEscHandler({ final: "c" }, () => {
      main = fresh();
      alt = fresh();
      return false;
    }),
    term.buffer.onBufferChange((b) => {
      if (b.type === "alternate") alt = fresh();
    }),
  ];
  return {
    get flags() {
      return screen().flags;
    },
    encode: (e, altIsMeta, composedAlt) => encodeKey(e, screen().flags, altIsMeta, composedAlt),
    reset() {
      main = fresh();
      alt = fresh();
    },
    dispose() {
      for (const d of disposables) d.dispose();
    },
  };
}
