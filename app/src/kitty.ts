import type { IDisposable, Terminal } from "@xterm/xterm";

/// The kitty keyboard protocol (https://sw.kovidgoyal.net/kitty/keyboard-protocol/), which xterm.js only gains in
/// 6.1 (`vtExtensions.kittyKeyboard`). Programs push the flags they want; while any are set, keys the legacy
/// encoding cannot tell apart (Shift+Enter, Ctrl+Tab, Ctrl+Shift+letter, Esc) are sent as `CSI code;mods u`.
/// Only "disambiguate" and "report alternate keys" are implemented, and a query reports only those, so a program
/// never waits for release events or text-as-escape-codes it will not get.
export interface KeyProtocol {
  readonly flags: number;
  /// The bytes for a keydown, or undefined when xterm's own encoding is already right for it.
  encode(e: KeyEvent, altIsMeta: boolean, composedAlt?: boolean, layout?: KeyLayout): string | undefined;
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

/// The US layout's unshifted and shifted character for each text key, which a key the active layout is not yet known
/// for is taken from.
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

/// `c` in the given case, unless that takes more than one character (`İ`), or `c` has no case.
function cased(c: string, upper: boolean): string {
  const to = upper ? c.toUpperCase() : c.toLowerCase();
  return [...to].length === 1 ? to : c;
}

const hasCase = (c: string) => c.toLowerCase() !== c.toUpperCase();

/// The active layout's unshifted and shifted character for each key, learned from the keys typed with at most Shift
/// held. WebKit has no `navigator.keyboard.getLayoutMap()`, and a macOS Option composes even as Meta, so its `key`
/// does not say which key it was on. Until a key is seen, the US layout stands in for it.
export class KeyLayout {
  private keys = new Map<string, [string | undefined, string | undefined]>();

  learn(e: KeyEvent & Partial<Pick<KeyboardEvent, "type" | "getModifierState">>) {
    if (e.type !== undefined && e.type !== "keydown") return;
    if (e.ctrlKey || e.altKey || e.metaKey || e.isComposing || e.getModifierState?.("AltGraph")) return;
    const chars = [...e.key];
    if (chars.length !== 1 || !e.code || e.code.startsWith("Numpad")) return;
    const ch = cased(chars[0], e.shiftKey);
    const slot = e.shiftKey ? 1 : 0;
    const pair = this.keys.get(e.code);
    if (pair?.[slot] === ch) return;
    // A changed key means a changed layout, which makes the other half stale too.
    const fresh: [string | undefined, string | undefined] = [undefined, undefined];
    fresh[slot] = ch;
    if (pair && pair[slot] === undefined) fresh[1 - slot] = pair[1 - slot];
    this.keys.set(e.code, fresh);
  }

  /// The key's unshifted and shifted character, either of which may not be known yet.
  key(code: string): [string | undefined, string | undefined] | undefined {
    return this.keys.get(code);
  }
}

const usLayout = new KeyLayout();

/// The key's unshifted character, and the shifted one when Shift is held. Taken from `key`, so it follows the active
/// layout, except where `key` cannot say: a composed Option, or a shifted symbol, whose unshifted form comes from the
/// layout.
function textKey(e: KeyEvent, composedAlt: boolean, layout: KeyLayout): { code: number; shifted?: number } | undefined {
  const known = layout.key(e.code);
  const us = usKeys[e.code];
  // An Option that left a letter alone did not compose.
  if (composedAlt && !/^[a-z]$/i.test(e.key)) {
    const [base, shifted] = known?.[0] !== undefined ? known : (us ?? []);
    if (base !== undefined) {
      const alternate = shifted ?? (hasCase(base) ? cased(base, true) : undefined);
      return { code: codePoint(base), shifted: e.shiftKey && alternate ? codePoint(alternate) : undefined };
    }
  }
  const chars = [...e.key];
  if (chars.length !== 1) return;
  const ch = chars[0];
  let base = cased(ch, false);
  if (e.shiftKey && !hasCase(ch)) {
    // A shifted character other than the one learned for the key means it was not typed in this layout (AltGr).
    if (known?.[0] !== undefined) {
      if (known[1] === undefined || known[1] === ch) base = known[0];
    } else if (us && (us[1] === ch || e.code.startsWith("Digit"))) base = us[0];
  }
  return { code: codePoint(base), shifted: e.shiftKey ? codePoint(ch) : undefined };
}

/// `altIsMeta` is false for a macOS Option that composes characters, which then is not a modifier at all.
/// `composedAlt` says Alt still composes `key` when it is Meta, as a macOS Option does.
/// `layout` is the active keyboard layout as far as it is known.
export function encodeKey(
  e: KeyEvent,
  flags: number,
  altIsMeta: boolean,
  composedAlt = false,
  layout = usLayout,
): string | undefined {
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
  const text = textKey(e, altMod && composedAlt, layout);
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
    encode: (e, altIsMeta, composedAlt, layout) => encodeKey(e, screen().flags, altIsMeta, composedAlt, layout),
    reset() {
      main = fresh();
      alt = fresh();
    },
    dispose() {
      for (const d of disposables) d.dispose();
    },
  };
}
