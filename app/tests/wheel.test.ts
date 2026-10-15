import assert from "node:assert/strict";
import { test } from "node:test";
import { type AppScroll, appWheel, arrowWheel, sgrWheel, wheelRows } from "../src/wheel.ts";

const PIXEL = 0;
const LINE = 1;
const PAGE = 2;

test("wheel travel turns into whole rows, carrying the rest", () => {
  const carry = { rows: 0 };
  const px = (deltaY: number) => wheelRows(carry, { deltaY, deltaMode: PIXEL }, 16, 40);
  assert.deepEqual([px(5), px(5), px(5), px(5)], [0, 0, 0, 1]);
  assert.equal(px(-40), -2, "turning around drops what was carried the other way");
  assert.equal(px(-8), -1);
  assert.equal(px(200), 12);
  assert.equal(wheelRows({ rows: 0 }, { deltaY: -3, deltaMode: LINE }, 16, 40), -3);
  assert.equal(wheelRows({ rows: 0 }, { deltaY: 1, deltaMode: PAGE }, 16, 40), 40);
});

test("wheel steps are SGR reports or arrow keys, one per row", () => {
  assert.equal(sgrWheel(-2, 5, 7, 0), "\x1b[<64;5;7M\x1b[<64;5;7M");
  assert.equal(sgrWheel(1, 1, 1, 8 | 16), "\x1b[<89;1;1M");
  assert.equal(arrowWheel(-2, false), "\x1b[A\x1b[A");
  assert.equal(arrowWheel(1, true), "\x1bOB");
});

type Handler = (params: (number | number[])[]) => boolean;

/// Just enough of xterm for the wheel handler: its parser, modes, active buffer and a 16px-row screen at (100, 50).
function fakeTerm(scroll: AppScroll = "rows") {
  const csi = new Map<string, Handler>();
  let ris = () => false;
  let wheel: (e: WheelEvent) => boolean = () => true;
  const modes = { mouseTrackingMode: "none", applicationCursorKeysMode: false };
  const active = { type: "normal" };
  const rect = { left: 100, top: 50, width: 800, height: 640 };
  const term = {
    cols: 100,
    rows: 40,
    modes,
    buffer: { active },
    element: { querySelector: () => ({ getBoundingClientRect: () => rect }) },
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
    attachCustomWheelEventHandler: (h: typeof wheel) => {
      wheel = h;
    },
  };
  const sent: string[] = [];
  const setting = { scroll };
  // biome-ignore lint/suspicious/noExplicitAny: a structural stand-in for Terminal
  const handle = appWheel(term as any, () => setting.scroll, (d) => sent.push(d));
  let prevented = 0;
  const roll = (deltaY: number, extra: Partial<WheelEvent> = {}) =>
    wheel({
      deltaY,
      deltaMode: PIXEL,
      clientX: 100 + 8 * 9 + 1,
      clientY: 50 + 16 * 3 + 1,
      shiftKey: false,
      altKey: false,
      ctrlKey: false,
      preventDefault: () => prevented++,
      ...extra,
    } as WheelEvent);
  return {
    handle,
    sent,
    setting,
    modes,
    active,
    roll,
    prevented: () => prevented,
    decset: (set: boolean, ...params: number[]) => csi.get(`?${set ? "h" : "l"}`)?.(params),
    ris: () => ris(),
  };
}

test("a program reading SGR mouse reports gets one wheel report per row", () => {
  const t = fakeTerm();
  t.modes.mouseTrackingMode = "any";
  assert.equal(t.decset(true, 1000, 1006), false, "xterm still handles the mode too");
  assert.equal(t.roll(-40), false);
  assert.deepEqual(t.sent, ["\x1b[<64;10;4M\x1b[<64;10;4M"]);
  assert.equal(t.prevented(), 1);
  assert.equal(t.roll(-4), false, "less than a row sends nothing but is still ours");
  assert.deepEqual(t.sent.length, 1);
  t.roll(30, { altKey: true });
  assert.equal(t.sent[1], "\x1b[<73;10;4M", "the turn drops the carried 0.75 row, and Alt is reported");
});

test("mouse reports in other encodings, Shift+wheel and the xterm setting stay with xterm", () => {
  const t = fakeTerm();
  t.modes.mouseTrackingMode = "vt200";
  assert.equal(t.roll(-40), true, "default encoding");
  t.decset(true, 1016);
  assert.equal(t.roll(-40), true, "SGR pixels");
  t.decset(true, 1006);
  assert.equal(t.roll(-40, { shiftKey: true }), true);
  t.setting.scroll = "xterm";
  assert.equal(t.roll(-40), true);
  t.setting.scroll = "rows";
  assert.equal(t.roll(-40), false);
  t.decset(false, 1006);
  assert.equal(t.roll(-40), true, "DECRST 1006 goes back to the default encoding");
  assert.deepEqual(t.sent.length, 1);
});

test("RIS and a replay's reset forget the encoding", () => {
  const t = fakeTerm();
  t.modes.mouseTrackingMode = "drag";
  t.decset(true, 1006);
  assert.equal(t.ris(), false, "xterm resets too");
  assert.equal(t.roll(-40), true);
  t.decset(true, 1006);
  t.handle.reset();
  assert.equal(t.roll(-40), true);
});

test("the alternate screen without mouse tracking gets arrow keys; the main screen keeps xterm's scrollback", () => {
  const t = fakeTerm();
  assert.equal(t.roll(-40), true);
  t.active.type = "alternate";
  assert.equal(t.roll(-40), false);
  t.modes.applicationCursorKeysMode = true;
  t.roll(48);
  assert.deepEqual(t.sent, ["\x1b[A\x1b[A", "\x1bOB\x1bOB\x1bOB"]);
  t.modes.mouseTrackingMode = "x10";
  t.roll(-16);
  assert.equal(t.sent[2], "\x1bOA", "X10 reports no wheel, so it scrolls like no tracking");
});

test("reports land on the cell under the pointer, clamped to the screen", () => {
  const t = fakeTerm();
  t.modes.mouseTrackingMode = "any";
  t.decset(true, 1006);
  t.roll(-16, { clientX: 0, clientY: 10_000 });
  assert.deepEqual(t.sent, ["\x1b[<64;1;40M"]);
});
