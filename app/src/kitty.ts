import type { IDisposable, Terminal } from "@xterm/xterm";

/// The kitty keyboard protocol (https://sw.kovidgoyal.net/kitty/keyboard-protocol/), which xterm.js only gains in
/// 6.1 (`vtExtensions.kittyKeyboard`). Programs push the flags they want; while any are set, keys the legacy
/// encoding cannot tell apart (Shift+Enter, Ctrl+Tab, Ctrl+Shift+letter, Esc) are sent as `CSI code;mods u`.
/// Only "disambiguate" and "report alternate keys" are implemented, and a query reports only those, so a program
/// never waits for release events or text-as-escape-codes it will not get.
export interface KeyProtocol {
  readonly flags: number;
  /// The bytes for a keydown, or undefined when xterm's own encoding is already right for it.
  encode(e: KeyEvent, altIsMeta: boolean): string | undefined;
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

const numpad: Record<string, number> = {
  Decimal: 57409,
  Divide: 57410,
  Multiply: 57411,
  Subtract: 57412,
  Add: 57413,
  Enter: 57414,
  Equal: 57415,
};

function numpadCode(code: string): number | undefined {
  if (!code.startsWith("Numpad")) return;
  const rest = code.slice(6);
  return /^[0-9]$/.test(rest) ? 57399 + Number(rest) : numpad[rest];
}

/// The unshifted key, from the physical key when Shift or a Meta Option has changed what `key` reports.
function baseCode(e: KeyEvent, altMod: boolean): number | undefined {
  if ((e.shiftKey || altMod) && /^(Key[A-Z]|Digit[0-9])$/.test(e.code)) return e.code.slice(-1).toLowerCase().charCodeAt(0);
  const chars = [...e.key];
  if (chars.length !== 1) return;
  const cp = chars[0].codePointAt(0) as number;
  return cp >= 65 && cp <= 90 ? cp + 32 : cp;
}

/// `altIsMeta` is false for a macOS Option that composes characters, which then is not a modifier at all.
export function encodeKey(e: KeyEvent, flags: number, altIsMeta: boolean): string | undefined {
  if (!(flags & DISAMBIGUATE) || e.isComposing) return;
  const altMod = e.altKey && altIsMeta;
  const bits = (e.shiftKey ? SHIFT : 0) | (altMod ? ALT : 0) | (e.ctrlKey ? CTRL : 0) | (e.metaKey ? SUPER : 0);
  const pad = numpadCode(e.code);
  const func = pad ?? functional[e.key];
  const code = func ?? baseCode(e, altMod);
  if (code === undefined) return;
  // Enter, Tab and Backspace keep their legacy bytes unmodified; other functional keys always get CSI u.
  const legacyWhenBare = code === 13 || code === 9 || code === 127;
  const csiU = (func !== undefined && !legacyWhenBare) || (bits !== 0 && func !== undefined) || (bits & ~SHIFT) !== 0;
  if (!csiU) return;
  let key = String(code);
  if (flags & ALTERNATE_KEYS && e.shiftKey && func === undefined) {
    const shifted = [...e.key].length === 1 ? (e.key.codePointAt(0) as number) : undefined;
    if (shifted !== undefined && shifted !== code) key += `:${shifted}`;
  }
  return bits ? `\x1b[${key};${bits + 1}u` : `\x1b[${key}u`;
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
    encode: (e, altIsMeta) => encodeKey(e, screen().flags, altIsMeta),
    reset() {
      main = fresh();
      alt = fresh();
    },
    dispose() {
      for (const d of disposables) d.dispose();
    },
  };
}
