/// A task an agent posted for the user (`ftm ask` / `ftm flag`), as ftmd sends it in `Event::Tasks`.
export interface Task {
  id: number;
  workspace: string;
  /// "unknown" for a kind from a newer ftmd than this app knows.
  kind: "decision" | "flag" | "unknown";
  title: string;
  body?: string | null;
  options?: string[];
  created_at: number;
  state: "open" | "answered" | "dismissed" | "resolved" | "unknown";
  answer?: string | null;
  closed_at?: number | null;
}

export interface TaskGroup<W> {
  workspace: string;
  /// Missing while ftmd's tree has not caught up with its tasks.
  ws: W | undefined;
  tasks: Task[];
}

/// Open tasks grouped by the workspace that posted them, groups in `workspaces` order (the sidebar's), oldest task
/// first within each.
export function taskGroups<W extends { id: string }>(tasks: Task[], workspaces: W[]): TaskGroup<W>[] {
  const order = new Map(workspaces.map((w, i) => [w.id, i]));
  const groups = new Map<string, TaskGroup<W>>();
  for (const t of tasks) {
    if (t.state !== "open") continue;
    let group = groups.get(t.workspace);
    if (!group) {
      group = { workspace: t.workspace, ws: workspaces.find((w) => w.id === t.workspace), tasks: [] };
      groups.set(t.workspace, group);
    }
    group.tasks.push(t);
  }
  const rank = (g: TaskGroup<W>) => order.get(g.workspace) ?? Number.MAX_SAFE_INTEGER;
  return [...groups.values()]
    .sort((a, b) => rank(a) - rank(b))
    .map((g) => ({ ...g, tasks: g.tasks.sort((a, b) => a.created_at - b.created_at || a.id - b.id) }));
}

/// Ids restart from 1 if ftmd loses its state, so the creation time keeps a new task from passing for an old one.
const taskKey = (t: Task) => `${t.id}@${t.created_at}`;

/// Picks out tasks that appeared since the last update, for a notification. The first update (the replay on
/// subscribe) only primes it, and a reconnect's replay repeats tasks already seen.
export class TaskAlerts {
  private seen = new Set<string>();
  private primed = false;

  update(tasks: Task[]): Task[] {
    const fresh = this.primed ? tasks.filter((t) => t.state === "open" && !this.seen.has(taskKey(t))) : [];
    this.seen = new Set(tasks.map(taskKey));
    this.primed = true;
    return fresh;
  }
}

/// What a key in a reply box does: Enter sends, Shift+Enter (or Enter while an IME composes) is left to the textarea.
export const replyKey = (e: { key: string; shiftKey: boolean; isComposing: boolean }): "send" | null =>
  e.key === "Enter" && !e.shiftKey && !e.isComposing ? "send" : null;
