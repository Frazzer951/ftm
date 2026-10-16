export interface Tracked {
  id: string;
  kind: string;
  status: string;
  unseen: boolean;
  background_agents: number;
}

interface Snapshot {
  status: string;
  unseen: boolean;
  background: number;
}

export type Decision<T> =
  | { sound: "needs_input"; waiting: T[] }
  | { sound: "completed" }
  | { hold: T[] }
  | null;

/// Turns each workspace tree into the sound it calls for. A transition into needs_input plays its sound. A turn
/// finishing (ftmd marking the workspace unseen) plays the other one if `counts` says so, which it does not for the
/// workspace on screen. With `waitForSubagents`, a turn that ends with background subagents still running stays
/// quiet, and the finish is held (see `stillFinished`) for the workspace to stay finished. The last subagent handing
/// back can end a turn that never left idle, so ftmd does not mark it unseen again; its background count dropping to
/// zero while it is still unseen is a finish too. The first tree only records state.
export class StatusSounds<T extends Tracked> {
  private last = new Map<string, Snapshot>();
  private started = false;

  update(list: T[], waitForSubagents: boolean, counts: (w: T) => boolean): Decision<T> {
    const prev = this.last;
    const waiting = list.filter(
      (w) => w.kind === "agent" && w.status === "needs_input" && prev.get(w.id)?.status !== "needs_input",
    );
    const newlyUnseen = list.filter((w) => w.unseen && !prev.get(w.id)?.unseen);
    const handedBack = list.filter((w) => {
      const p = prev.get(w.id);
      return w.unseen && p?.unseen && p.background > 0 && !w.background_agents;
    });
    this.last = new Map(list.map((w) => [w.id, { status: w.status, unseen: w.unseen, background: w.background_agents }]));
    if (!this.started) {
      this.started = true;
      return null;
    }
    if (waiting.length) return { sound: "needs_input", waiting };
    if (!waitForSubagents) return newlyUnseen.some(counts) ? { sound: "completed" } : null;
    const done = [...newlyUnseen, ...handedBack].filter((w) => !w.background_agents);
    return done.length ? { hold: done } : null;
  }
}

/// Whether a held finish still stands when the hold ends: the workspace is still unseen, idle, and has no subagent
/// back at work.
export const stillFinished = <T extends Tracked>(w: T, counts: (w: T) => boolean) =>
  w.unseen && !w.background_agents && w.status !== "working" && counts(w);
