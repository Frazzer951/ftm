import type { IDisposable, Terminal } from "@xterm/xterm";

/// How the wheel scrolls a full-screen program: a step per row of travel, or xterm.js's own steps.
export type AppScroll = "rows" | "xterm";

export interface AppWheel {
  /// After `term.reset()`, which does not go through the parser.
  reset(): void;
  dispose(): void;
}

type Encoding = "default" | "sgr" | "sgr-pixels";

const DOM_DELTA_LINE = 1;
const DOM_DELTA_PAGE = 2;
const ALT = 8;
const CTRL = 16;

/// Whole rows of wheel travel in `e`, negative upward; the fraction carries over to the next event that goes the
/// same way, so a slow trackpad swipe still adds up to rows.
export function wheelRows(
  carry: { rows: number },
  e: Pick<WheelEvent, "deltaY" | "deltaMode">,
  rowHeight: number,
  pageRows: number,
): number {
  const rows =
    e.deltaMode === DOM_DELTA_LINE ? e.deltaY : e.deltaMode === DOM_DELTA_PAGE ? e.deltaY * pageRows : e.deltaY / rowHeight;
  if (Math.sign(rows) !== Math.sign(carry.rows)) carry.rows = 0;
  carry.rows += rows;
  const whole = Math.trunc(carry.rows);
  carry.rows -= whole;
  return whole;
}

/// SGR wheel reports (buttons 64 up, 65 down) at a 1-based cell, one per row.
export const sgrWheel = (rows: number, col: number, row: number, mods: number) =>
  `\x1b[<${(rows < 0 ? 64 : 65) | mods};${col};${row}M`.repeat(Math.abs(rows));

export const arrowWheel = (rows: number, appCursor: boolean) =>
  `\x1b${appCursor ? "O" : "["}${rows < 0 ? "A" : "B"}`.repeat(Math.abs(rows));

const clamp = (n: number, max: number) => Math.min(Math.max(n, 1), max);

/// xterm.js 6.0 gives a program that reads the mouse, or runs on the alternate screen without it, at most one wheel
/// step per event, and damps events under 50px to 30% first, so a light trackpad swipe barely moves Claude Code's
/// fullscreen view and a mouse wheel crawls. Native terminals send a step per row scrolled. With `rows`, this does
/// the same: SGR reports when the program asked for them (other encodings keep xterm's), arrow keys on the
/// alternate screen otherwise. The main screen's own scrollback and Shift+wheel stay xterm's.
export function appWheel(
  term: Pick<Terminal, "parser" | "buffer" | "modes" | "cols" | "rows" | "element" | "attachCustomWheelEventHandler">,
  mode: () => AppScroll,
  send: (data: string) => void,
): AppWheel {
  let encoding: Encoding = "default";
  const carry = { rows: 0 };
  const decset = (set: boolean) =>
    term.parser.registerCsiHandler({ prefix: "?", final: set ? "h" : "l" }, (params) => {
      for (const p of params) {
        if (p === 1006) encoding = set ? "sgr" : "default";
        else if (p === 1016) encoding = set ? "sgr-pixels" : "default";
      }
      return false;
    });
  const disposables: IDisposable[] = [
    decset(true),
    decset(false),
    // RIS: xterm resets its own mouse modes too.
    term.parser.registerEscHandler({ final: "c" }, () => {
      encoding = "default";
      return false;
    }),
  ];
  term.attachCustomWheelEventHandler((e) => {
    if (mode() !== "rows" || e.deltaY === 0 || e.shiftKey) return true;
    const tracking = term.modes.mouseTrackingMode;
    const reports = tracking === "vt200" || tracking === "drag" || tracking === "any";
    if (reports ? encoding !== "sgr" : term.buffer.active.type !== "alternate") return true;
    const rect = term.element?.querySelector(".xterm-screen")?.getBoundingClientRect();
    if (!rect?.height) return true;
    const rows = wheelRows(carry, e, rect.height / term.rows, term.rows);
    if (rows && reports) {
      const col = clamp(Math.floor(((e.clientX - rect.left) * term.cols) / rect.width) + 1, term.cols);
      const row = clamp(Math.floor(((e.clientY - rect.top) * term.rows) / rect.height) + 1, term.rows);
      send(sgrWheel(rows, col, row, (e.altKey ? ALT : 0) | (e.ctrlKey ? CTRL : 0)));
    } else if (rows) {
      send(arrowWheel(rows, term.modes.applicationCursorKeysMode));
    }
    e.preventDefault();
    return false;
  });
  return {
    reset() {
      encoding = "default";
      carry.rows = 0;
    },
    dispose() {
      for (const d of disposables) d.dispose();
      term.attachCustomWheelEventHandler(() => true);
    },
  };
}
