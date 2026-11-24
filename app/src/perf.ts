/// Page-side numbers for ftm-app.log, where an installed app's lag leaves a trace (its stderr goes nowhere):
/// main-thread stalls (a timer that fires late while the window is visible), how long xterm takes to parse a write to
/// a pane on screen, sidebar rebuilds, and WebGL context losses. `take` returns a summary once per `window` ms while
/// anything happened, or at once for a stall of `slow` ms or more.
export class PerfMeter {
  private since: number;
  private lastSlow = Number.NEGATIVE_INFINITY;
  private stalls = 0;
  private maxStall = 0;
  private writes = 0;
  private maxWrite = 0;
  private shownBytes = 0;
  private hiddenBytes = 0;
  private sidebars = 0;
  private sidebarMs = 0;
  private maxSidebar = 0;
  private workspaceEvents = 0;
  private glLost = 0;
  private readonly window: number;
  private readonly slow: number;
  private readonly stallFloor: number;

  constructor(now: number, window = 600_000, slow = 1_000, stallFloor = 100) {
    this.since = now;
    this.window = window;
    this.slow = slow;
    this.stallFloor = stallFloor;
  }

  stall(ms: number) {
    if (ms < this.stallFloor) return;
    this.stalls++;
    this.maxStall = Math.max(this.maxStall, ms);
  }

  write(bytes: number, shown: boolean, parseMs?: number) {
    if (shown) this.shownBytes += bytes;
    else this.hiddenBytes += bytes;
    if (parseMs === undefined) return;
    this.writes++;
    this.maxWrite = Math.max(this.maxWrite, parseMs);
  }

  sidebar(ms: number) {
    this.sidebars++;
    this.sidebarMs += ms;
    this.maxSidebar = Math.max(this.maxSidebar, ms);
  }

  workspaces() {
    this.workspaceEvents++;
  }

  contextLost() {
    this.glLost++;
  }

  take(now: number): string | null {
    const slow = this.maxStall >= this.slow && now - this.lastSlow >= this.window;
    if (!slow && now - this.since < this.window) return null;
    const busy = this.stalls || this.writes || this.hiddenBytes || this.sidebars || this.glLost;
    const line = busy
      ? `perf ${slow ? "slow " : ""}${Math.round((now - this.since) / 1000)}s: ` +
        `stalls ${this.stalls} (max ${Math.round(this.maxStall)} ms), ` +
        `parse max ${Math.round(this.maxWrite)} ms over ${this.writes} writes, ` +
        `output ${kb(this.shownBytes)} shown / ${kb(this.hiddenBytes)} hidden, ` +
        `sidebar ${this.sidebars}× (${Math.round(this.sidebarMs)} ms, max ${Math.round(this.maxSidebar)} ms) ` +
        `for ${this.workspaceEvents} workspaces events, ` +
        `${this.glLost} webgl contexts lost`
      : null;
    if (slow) this.lastSlow = now;
    this.reset(now);
    return line;
  }

  private reset(now: number) {
    this.since = now;
    this.stalls = this.maxStall = this.writes = this.maxWrite = this.shownBytes = this.hiddenBytes = 0;
    this.sidebars = this.sidebarMs = this.maxSidebar = this.workspaceEvents = this.glLost = 0;
  }
}

const kb = (bytes: number) => `${Math.round(bytes / 1024)} KB`;
