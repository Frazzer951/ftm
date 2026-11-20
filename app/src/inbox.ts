import DOMPurify from "dompurify";
import { marked } from "marked";
import { openableUrl } from "./links.ts";
import { replyKey, type Task, taskGroups } from "./tasks.ts";

export interface InboxHost<W> {
  workspaces(): W[];
  name(ws: W): string;
  /// CSS color of the workspace's stripe, if it has one.
  color(ws: W): string | null;
  dot(ws: W): { className: string; title: string };
  select(id: string): void;
  request(req: object): Promise<unknown>;
  openUrl(url: string): void;
  /// The open tasks changed: their count, for the badges that show it.
  changed(): void;
}

/// Bodies are agent-written markdown shown in the app's own DOM, unlike the viewer's sandboxed frame, so only text
/// formatting survives: no images, media, forms, styles or attributes beyond a link's target.
const BODY_TAGS = [
  "p", "br", "strong", "em", "b", "i", "del", "s", "code", "pre", "blockquote", "ul", "ol", "li", "a", "hr",
  "h1", "h2", "h3", "h4", "h5", "h6", "table", "thead", "tbody", "tr", "th", "td",
];

function bodyHtml(markdown: string): string {
  const html = marked.parse(markdown, { gfm: true, async: false });
  return DOMPurify.sanitize(html, { ALLOWED_TAGS: BODY_TAGS, ALLOWED_ATTR: ["href", "title"] });
}

const el = <K extends keyof HTMLElementTagNameMap>(tag: K, className = "", text = ""): HTMLElementTagNameMap[K] => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text) node.textContent = text;
  return node;
};

/// Every agent's open tasks, in the viewer panel's Tasks tab. It redraws only when what it shows changes, so a reply
/// being typed keeps its focus and caret through the tree updates every status change brings.
export class Inbox<W extends { id: string }> {
  private tasks: Task[] = [];
  private drafts = new Map<number, string>();
  private expanded = new Set<number>();
  private pending = new Set<number>();
  private errors = new Map<number, string>();
  private drawn = "";

  constructor(
    readonly root: HTMLElement,
    private host: InboxHost<W>,
  ) {}

  setTasks(tasks: Task[]) {
    const before = this.openIds();
    this.tasks = tasks;
    const live = new Set(tasks.filter((t) => t.state === "open").map((t) => t.id));
    for (const set of [this.drafts, this.errors, this.expanded]) {
      for (const id of set.keys()) if (!live.has(id)) set.delete(id);
    }
    this.render();
    if (this.openIds() !== before) this.host.changed();
  }

  private openIds = () =>
    this.tasks
      .filter((t) => t.state === "open")
      .map((t) => t.id)
      .join();

  /// Open tasks, and how many of them are decisions.
  counts(): { open: number; decisions: number } {
    const open = this.tasks.filter((t) => t.state === "open");
    return { open: open.length, decisions: open.filter((t) => t.kind === "decision").length };
  }

  /// Open tasks posted from `ws`.
  openFor(ws: string): number {
    return this.tasks.filter((t) => t.state === "open" && t.workspace === ws).length;
  }

  render() {
    const groups = taskGroups(this.tasks, this.host.workspaces());
    const key = JSON.stringify([
      groups.map((g) => [
        g.workspace,
        g.ws && [this.host.name(g.ws), this.host.color(g.ws), this.host.dot(g.ws)],
        g.tasks.map((t) => t.id),
      ]),
      [...this.expanded],
      [...this.pending],
      [...this.errors],
    ]);
    if (key === this.drawn) return;
    this.drawn = key;

    const active = document.activeElement;
    const focused =
      active instanceof HTMLTextAreaElement && this.root.contains(active)
        ? { id: active.dataset.task, start: active.selectionStart, end: active.selectionEnd }
        : null;

    if (!groups.length) {
      this.root.replaceChildren(
        el("p", "message", "No open tasks. Agents post them with ftm ask (decisions) and ftm flag (FYI)."),
      );
      return;
    }
    this.root.replaceChildren(...groups.map((g) => this.group(g.workspace, g.ws, g.tasks)));

    if (focused) {
      const box = this.root.querySelector<HTMLTextAreaElement>(`textarea[data-task="${focused.id}"]`);
      if (box) {
        box.focus();
        box.setSelectionRange(focused.start, focused.end);
      }
    }
  }

  private group(id: string, ws: W | undefined, tasks: Task[]): HTMLElement {
    const group = el("div", "task-group");
    const agent = el("div", "ws task-agent");
    const color = ws && this.host.color(ws);
    if (color) {
      agent.classList.add("colored");
      agent.style.setProperty("--ws-color", color);
    }
    const dot = el("span");
    if (ws) Object.assign(dot, this.host.dot(ws));
    const name = el("span", "name", ws ? this.host.name(ws) : "(closed workspace)");
    agent.append(dot, name);
    agent.title = "Show this agent";
    agent.onclick = () => this.host.select(id);
    group.append(agent, ...tasks.map((t) => this.item(t)));
    return group;
  }

  private item(t: Task): HTMLElement {
    const busy = this.pending.has(t.id);
    const item = el("div", `task ${t.kind}`);
    item.dataset.id = `${t.id}`;
    const head = el("div", "task-head");
    const icon = el("span", "task-icon", t.kind === "flag" ? "⚑" : "?");
    icon.title = t.kind === "flag" ? "Flag (FYI)" : "Decision";
    const title = el("span", "task-title", t.title);
    title.title = `#${t.id} ${t.title}`;
    head.append(icon, title);
    if (t.body) {
      const open = this.expanded.has(t.id);
      const more = el("button", "task-more", open ? "▾" : "▸");
      more.title = open ? "Hide details" : "Show details";
      head.append(more);
      head.classList.add("expandable");
      head.onclick = () => {
        if (!this.expanded.delete(t.id)) this.expanded.add(t.id);
        this.render();
      };
    }
    if (t.kind === "decision") {
      const close = el("button", "close", "×");
      close.title = "Dismiss without answering";
      close.disabled = busy;
      close.onclick = (e) => {
        e.stopPropagation();
        void this.act(t, { op: "dismiss_task", id: t.id });
      };
      head.append(close);
    }
    item.append(head);
    if (t.body && this.expanded.has(t.id)) {
      const body = el("div", "task-body");
      body.innerHTML = bodyHtml(t.body);
      body.onclick = (e) => {
        const link = (e.target as HTMLElement).closest("a");
        if (!link) return;
        e.preventDefault();
        const url = openableUrl(link.getAttribute("href") ?? "");
        if (url) this.host.openUrl(url);
      };
      item.append(body);
    }
    if (t.options?.length) {
      const options = el("div", "task-options");
      for (const option of t.options) {
        const b = el("button", "", option);
        b.title = `Answer "${option}"`;
        b.disabled = busy;
        b.onclick = () => this.answer(t, option);
        options.append(b);
      }
      item.append(options);
    }
    const reply = el("textarea", "task-reply");
    reply.dataset.task = `${t.id}`;
    reply.rows = 1;
    reply.placeholder = t.kind === "flag" ? "Reply (optional)…" : "Answer…";
    reply.title = "Enter sends, Shift+Enter adds a line";
    reply.value = this.drafts.get(t.id) ?? "";
    reply.disabled = busy;
    const fit = () => {
      reply.style.height = "auto";
      reply.style.height = `${Math.min(reply.scrollHeight, 160)}px`;
    };
    reply.oninput = () => {
      this.drafts.set(t.id, reply.value);
      fit();
    };
    reply.onkeydown = (e) => {
      if (replyKey(e) !== "send") return;
      e.preventDefault();
      if (reply.value.trim()) this.answer(t, reply.value);
    };
    requestAnimationFrame(fit);
    item.append(reply);
    const error = this.errors.get(t.id);
    if (error) item.append(el("div", "task-error", error));
    if (t.kind !== "decision") {
      const actions = el("div", "task-actions");
      const dismiss = el("button", "", "Dismiss");
      dismiss.disabled = busy;
      dismiss.onclick = () => this.act(t, { op: "dismiss_task", id: t.id });
      actions.append(dismiss);
      item.append(actions);
    }
    return item;
  }

  private answer(t: Task, answer: string) {
    void this.act(t, { op: "answer_task", id: t.id, answer }).then((ok) => {
      if (ok) this.drafts.delete(t.id);
    });
  }

  /// Leaves the task in place with the reason when ftmd refuses (the agent is busy at a prompt, its pane is gone,
  /// it withdrew the task meanwhile).
  private async act(t: Task, req: object): Promise<boolean> {
    this.pending.add(t.id);
    this.errors.delete(t.id);
    this.render();
    try {
      await this.host.request(req);
      return true;
    } catch (e) {
      this.errors.set(t.id, String(e));
      return false;
    } finally {
      this.pending.delete(t.id);
      this.render();
    }
  }
}
