import DOMPurify from "dompurify";
import { marked } from "marked";
import { type ArtifactKind, artifactKind, HTML_CSP, markdownCsp, mimeType, resolvePath, withCsp } from "./artifacts.ts";

type Id = string;

export interface Stat {
  path: string;
  size: number;
  modified: number;
}

export interface ViewerHost {
  read(path: string): Promise<Uint8Array>;
  stat(path: string): Promise<Stat>;
  openInEditor(path: string): void;
  /// A link clicked in a rendered document: an http(s) URL, or a local path to route like a Cmd+click.
  openLink(url: string): void;
  openPath(path: string): void;
}

/// The panel's Tasks tab: every agent's open tasks, the same whichever workspace is shown.
export interface ViewerTasks {
  el: HTMLElement;
  counts(): { open: number; decisions: number };
  /// Off in Settings: no Tasks tab, and a workspace without a file has no panel.
  enabled(): boolean;
}

type PanelTab = "file" | "tasks";

/// What a workspace's panel shows, kept across restarts.
interface Entry {
  path: string | null;
  collapsed: boolean;
  /// Opened while the workspace was not on screen, or while the panel was collapsed.
  unread: boolean;
  recent: string[];
  /// Which tab shows while the workspace has a file; without one the panel only has Tasks.
  tab?: PanelTab;
}

const STATE_KEY = "ftm.viewer";
const WIDTH_KEY = "ftm.viewerWidth";
/// Whether the panel is collapsed in workspaces without a file, where it only shows Tasks. One flag for all of them,
/// since the tasks are the same everywhere.
const TASKS_COLLAPSED_KEY = "ftm.tasksCollapsed";
const WIDTH_DEFAULT = 480;
const RECENT_MAX = 10;
/// Images inlined into a markdown document, each and in all.
const INLINE_IMAGE_MAX = 20 << 20;
const INLINE_TOTAL_MAX = 60 << 20;
const POLL_MS = 1000;

/// Runs inside a rendered markdown frame, the only script there: WebKit runs no listener, even the panel's, in a frame
/// without `allow-scripts`, so links are reported to the panel rather than navigating the frame, and so is the scroll
/// position, which a reload restores.
const DOC_SCRIPT = `
addEventListener("load", () => {
  scrollTo(0, Number(document.documentElement.dataset.scroll) || 0);
  parent.postMessage({ ftmViewer: "scroll", y: scrollY }, "*");
});
document.addEventListener("click", (e) => {
  const a = e.target instanceof Element ? e.target.closest("a") : null;
  const href = a ? a.getAttribute("href") || "" : "";
  if (!a || href.startsWith("#")) return;
  e.preventDefault();
  parent.postMessage({ ftmViewer: "link", href }, "*");
});
let pending;
addEventListener("scroll", () => {
  clearTimeout(pending);
  pending = setTimeout(() => parent.postMessage({ ftmViewer: "scroll", y: scrollY }, "*"), 100);
});
`;

const nonce = () => Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) => b.toString(16).padStart(2, "0")).join("");

const basename = (path: string) => path.split("/").pop() || path;
const dirname = (path: string) => path.replace(/\/[^/]*$/, "") || "/";

function dataUrl(bytes: Uint8Array, mime: string): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return `data:${mime};base64,${btoa(bin)}`;
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, props: Record<string, unknown> = {}, ...children: (Node | string)[]): HTMLElementTagNameMap[K] {
  const e: HTMLElementTagNameMap[K] = document.createElement(tag);
  Object.assign(e, props);
  e.append(...children);
  return e;
}

/// The side panel that shows a workspace's artifact (markdown, HTML, images and video) and, in a second tab, every
/// agent's open tasks. One panel, its contents swapped as the selected workspace changes; each workspace remembers its
/// file, its tab, whether the panel is collapsed, and its recent files. A workspace without a file shows the panel
/// only while there are open tasks, collapsed or not by one shared flag.
export class Viewer {
  private entries = new Map<Id, Entry>();
  private shown: Id | null = null;
  /// What the body currently renders, so a re-render of the same file and version is skipped.
  private rendered: { ws: Id; path: string; version: string } | null = null;
  private blobUrl: string | null = null;
  /// The rendered markdown's folder and last reported scroll position.
  private doc: { dir: string; scroll: number } | null = null;
  private poll: number | undefined;
  /// Bumped per load, so a slower earlier load never replaces a later one.
  private seq = 0;
  private busy = false;
  private readonly root: HTMLElement;
  private readonly body: HTMLElement;
  private readonly title: HTMLElement;
  private readonly recent: HTMLSelectElement;
  private readonly strip: HTMLButtonElement;
  private readonly stripLabel: HTMLElement;
  private readonly stripBadge: HTMLElement;
  private readonly fileTab: HTMLButtonElement;
  private readonly tasksTab: HTMLButtonElement;
  private readonly tasksBadge: HTMLElement;
  private readonly fileControls: HTMLElement;
  private tasksCollapsed: boolean;

  constructor(
    root: HTMLElement,
    private readonly host: ViewerHost,
    private readonly home: () => string,
    shortcut: string,
    private readonly tasks: ViewerTasks,
  ) {
    this.root = root;
    this.loadState();
    this.tasksCollapsed = (() => {
      try {
        return localStorage.getItem(TASKS_COLLAPSED_KEY) !== "false";
      } catch {
        return true;
      }
    })();
    this.title = el("span", { className: "title" });
    this.recent = el("select", { className: "recent", title: "Recent files" });
    this.recent.onchange = () => {
      if (this.shown && this.recent.value) this.open(this.shown, this.recent.value);
    };
    const editor = el("button", { type: "button", textContent: "VS Code", title: "Open in VS Code" });
    editor.onclick = () => {
      const path = this.current()?.path;
      if (path) this.host.openInEditor(path);
    };
    const collapse = el("button", { type: "button", textContent: "›", title: `Collapse (${shortcut})`, className: "collapse" });
    collapse.onclick = () => this.setCollapsed(true);
    const close = el("button", { type: "button", textContent: "×", title: "Close" });
    close.onclick = () => this.close();
    this.stripLabel = el("span", { className: "label" });
    this.stripBadge = el("span", { className: "strip-badge" });
    this.strip = el("button", { type: "button", className: "strip", title: `Expand (${shortcut})` }, this.stripBadge, this.stripLabel);
    this.strip.onclick = () => this.setCollapsed(false);
    this.stripBadge.onclick = (e) => {
      e.stopPropagation();
      this.setTab("tasks");
      this.setCollapsed(false);
    };
    this.body = el("div", { className: "body" });
    this.fileTab = el("button", { type: "button", className: "tab file-tab" }, this.title);
    this.fileTab.onclick = () => this.setTab("file");
    this.tasksBadge = el("span", { className: "tab-badge" });
    this.tasksTab = el("button", { type: "button", className: "tab", title: "Every agent's open tasks" }, "Tasks", this.tasksBadge);
    this.tasksTab.onclick = () => this.setTab("tasks");
    this.fileControls = el("span", { className: "file-controls" }, this.recent, editor, close);
    tasks.el.classList.add("tasks-body");
    const resize = el("div", { className: "resize", title: "Drag to resize, double-click to reset" });
    this.setupResize(resize);
    root.replaceChildren(
      resize,
      this.strip,
      el("header", {}, collapse, el("span", { className: "tabs" }, this.fileTab, this.tasksTab), this.fileControls),
      this.body,
      tasks.el,
    );
    document.addEventListener("visibilitychange", () => this.schedule());
    window.addEventListener("message", (e) => this.onFrameMessage(e));
  }

  /// Shows `path` in the workspace's panel. `focus` is false for files pushed from elsewhere (`ftm show`): the panel
  /// opens for when the workspace is next looked at, without taking over the screen now.
  open(ws: Id, path: string, opts: { focus?: boolean } = {}) {
    const entry = this.entry(ws);
    entry.path = path;
    entry.collapsed = false;
    entry.tab = "file";
    entry.recent = [path, ...entry.recent.filter((p) => p !== path)].slice(0, RECENT_MAX);
    entry.unread = opts.focus === false && ws !== this.shown;
    this.save();
    if (ws === this.shown) this.render();
  }

  /// Called whenever the selected workspace may have changed.
  show(ws: Id | null) {
    if (ws !== this.shown) {
      this.shown = ws;
      const entry = ws ? this.entries.get(ws) : undefined;
      if (entry?.unread) {
        entry.unread = false;
        this.save();
      }
    }
    this.render();
  }

  toggle() {
    if (!this.root.hidden) this.setCollapsed(!this.collapsed());
  }

  /// The open tasks changed.
  tasksChanged() {
    this.render();
  }

  /// Shows the Tasks tab, expanded, in `ws`: picking an agent from the tasks keeps them on screen.
  showTasks(ws: Id) {
    const entry = this.entries.get(ws);
    if (entry?.path) {
      entry.tab = "tasks";
      entry.collapsed = false;
      this.save();
    } else {
      this.setTasksCollapsed(false);
    }
    if (ws === this.shown) this.render();
  }

  /// Forgets workspaces that are gone.
  prune(live: Set<Id>) {
    let changed = false;
    for (const id of this.entries.keys()) {
      if (!live.has(id)) {
        this.entries.delete(id);
        changed = true;
      }
    }
    if (changed) this.save();
  }

  /// Whether the workspace has a file it was sent but nobody has looked at yet.
  unread(ws: Id): boolean {
    return this.entries.get(ws)?.unread ?? false;
  }

  private current(): Entry | undefined {
    return this.shown ? this.entries.get(this.shown) : undefined;
  }

  private entry(ws: Id): Entry {
    let entry = this.entries.get(ws);
    if (!entry) {
      entry = { path: null, collapsed: false, unread: false, recent: [] };
      this.entries.set(ws, entry);
    }
    return entry;
  }

  private collapsed(): boolean {
    const entry = this.current();
    return entry?.path ? entry.collapsed : this.tasksCollapsed;
  }

  private tab(): PanelTab {
    const entry = this.current();
    if (!entry?.path) return "tasks";
    return this.tasks.enabled() ? (entry.tab ?? "file") : "file";
  }

  private setCollapsed(collapsed: boolean) {
    const entry = this.current();
    if (entry?.path) {
      entry.collapsed = collapsed;
      this.save();
    } else {
      this.setTasksCollapsed(collapsed);
    }
    this.render();
  }

  private setTasksCollapsed(collapsed: boolean) {
    this.tasksCollapsed = collapsed;
    try {
      localStorage.setItem(TASKS_COLLAPSED_KEY, String(collapsed));
    } catch {
      // Only the collapsed state is lost.
    }
  }

  private setTab(tab: PanelTab) {
    const entry = this.current();
    if (!entry?.path) return;
    entry.tab = tab;
    this.save();
    this.render();
  }

  private close() {
    const entry = this.current();
    if (!entry) return;
    entry.path = null;
    this.save();
    this.render();
  }

  private render() {
    const entry = this.current();
    const path = entry?.path ?? null;
    const { open, decisions } = this.tasks.counts();
    const collapsed = this.collapsed();
    const tab = this.tab();
    this.root.hidden = !this.shown || (!path && open === 0);
    this.root.classList.toggle("collapsed", collapsed);
    this.root.classList.toggle("show-tasks", tab === "tasks");
    this.root.classList.toggle("has-file", !!path);
    const count = open ? `${open}` : "";
    const summary = `${open} open task${open === 1 ? "" : "s"}, ${decisions} waiting on a decision`;
    for (const badge of [this.stripBadge, this.tasksBadge]) {
      badge.textContent = count;
      badge.hidden = !open;
      badge.classList.toggle("urgent", decisions > 0);
      badge.title = summary;
    }
    this.fileTab.classList.toggle("active", tab === "file");
    this.tasksTab.classList.toggle("active", tab === "tasks");
    this.fileTab.hidden = !path;
    this.tasksTab.hidden = !this.tasks.enabled();
    this.fileControls.hidden = tab !== "file";
    this.stripLabel.textContent = path ? basename(path) : "Tasks";
    if (!path || !entry || !this.shown) {
      this.clear();
      this.schedule();
      return;
    }
    this.title.textContent = basename(path);
    this.fileTab.title = path;
    this.recent.hidden = entry.recent.length < 2;
    this.recent.replaceChildren(
      ...entry.recent.map((p) => el("option", { value: p, textContent: basename(p), title: p, selected: p === path })),
    );
    if (collapsed || tab === "tasks") {
      // Collapsing keeps a document but not a video, which would keep the media keys and keep playing.
      if (artifactKind(path) === "video") this.clear();
    } else if (this.rendered?.ws !== this.shown || this.rendered.path !== path) {
      this.renderFile(this.shown, path, true).catch(console.error);
    }
    this.schedule();
  }

  /// Polls the shown file while the panel is open and the window visible, for live reload.
  private schedule() {
    const entry = this.current();
    const want = !!entry?.path && !entry.collapsed && this.tab() === "file" && document.visibilityState === "visible";
    if (want && this.poll === undefined) {
      this.poll = window.setInterval(() => this.check().catch(console.error), POLL_MS);
    } else if (!want && this.poll !== undefined) {
      clearInterval(this.poll);
      this.poll = undefined;
    }
  }

  private async check() {
    const ws = this.shown;
    const path = this.current()?.path;
    if (!ws || !path || this.busy || this.rendered?.ws !== ws || this.rendered.path !== path) return;
    const version = await this.version(path);
    if (version !== this.rendered.version && this.current()?.path === path) await this.renderFile(ws, path, false);
  }

  private async version(path: string): Promise<string> {
    try {
      const s = await this.host.stat(path);
      return `${s.size}:${s.modified}`;
    } catch {
      return "missing";
    }
  }

  private clear() {
    for (const media of this.body.querySelectorAll("video")) {
      media.pause();
      media.removeAttribute("src");
      media.load();
    }
    this.body.replaceChildren();
    if (this.blobUrl) URL.revokeObjectURL(this.blobUrl);
    this.blobUrl = null;
    this.rendered = null;
    this.doc = null;
  }

  /// Reads and renders `path`; `fresh` is a newly opened file, otherwise a reload that keeps the scroll position.
  private async renderFile(ws: Id, path: string, fresh: boolean) {
    const kind = artifactKind(path);
    const token = ++this.seq;
    this.busy = true;
    const docScroll = fresh ? 0 : (this.doc?.scroll ?? 0);
    try {
      const version = await this.version(path);
      let node: HTMLElement;
      let url: string | null = null;
      if (version === "missing") {
        node = el("p", { className: "message", textContent: `${path} not found. It shows up here once it exists.` });
      } else if (!kind) {
        node = el("p", { className: "message", textContent: `FTM does not render ${basename(path)}.` });
      } else {
        const bytes = await this.host.read(path);
        if (token !== this.seq) return;
        ({ node, url } = await this.build(kind, path, bytes, docScroll));
      }
      if (token !== this.seq || this.shown !== ws || this.current()?.path !== path) {
        if (url) URL.revokeObjectURL(url);
        return;
      }
      const scroll = fresh ? 0 : this.body.scrollTop;
      this.clear();
      this.blobUrl = url;
      this.body.append(node);
      this.rendered = { ws, path, version };
      this.body.scrollTop = scroll;
      if (node.dataset.dir !== undefined) this.doc = { dir: node.dataset.dir, scroll: docScroll };
    } catch (e) {
      if (token !== this.seq) return;
      this.clear();
      this.body.append(el("p", { className: "message error", textContent: String(e) }));
      this.rendered = { ws, path, version: "error" };
    } finally {
      if (token === this.seq) this.busy = false;
    }
  }

  /// The node to show, and the Blob URL it holds (revoked once the node is replaced).
  private async build(kind: ArtifactKind, path: string, bytes: Uint8Array, scroll: number): Promise<{ node: HTMLElement; url: string | null }> {
    if (kind === "image" || kind === "video") {
      const url = URL.createObjectURL(new Blob([bytes as BlobPart], { type: mimeType(path) }));
      const media = kind === "image" ? el("img", { alt: basename(path) }) : el("video", { controls: true, preload: "metadata" });
      const box = el("div", { className: `media ${kind}` }, media);
      media.addEventListener("error", () => {
        const err = media instanceof HTMLVideoElement ? media.error : null;
        const why = err ? `${err.code} ${err.message}`.trim() : "unknown";
        console.error(`viewer: cannot show ${path}: ${why}`);
        box.replaceChildren(el("p", { className: "message error", textContent: `WebKit cannot show ${basename(path)} (${why}).` }));
      });
      media.src = url;
      return { node: box, url };
    }
    const text = new TextDecoder().decode(bytes);
    const frame = el("iframe", { className: "doc", title: basename(path) });
    if (kind === "html") {
      // Scripts run, but in an opaque origin: no IPC (Tauri injects it into the main frame only and refuses a null
      // Origin), no access to the app's DOM, and the CSP blocks every fetch.
      frame.setAttribute("sandbox", "allow-scripts");
      // Links open in a new window, which the sandbox (no `allow-popups`) refuses, rather than navigating the frame.
      frame.srcdoc = withCsp(text, HTML_CSP, '<base target="_blank">');
      return { node: frame, url: null };
    }
    // Sanitized, and the CSP lets only DOC_SCRIPT run, in an opaque origin like HTML artifacts.
    frame.setAttribute("sandbox", "allow-scripts");
    frame.classList.add("markdown");
    frame.srcdoc = await this.markdownDocument(text, dirname(path), scroll);
    frame.dataset.dir = dirname(path);
    return { node: frame, url: null };
  }

  private async markdownDocument(text: string, dir: string, scroll: number): Promise<string> {
    const html = DOMPurify.sanitize(await marked.parse(text, { gfm: true }), { FORBID_TAGS: ["style", "form"] });
    const doc = new DOMParser().parseFromString(html, "text/html");
    let total = 0;
    for (const img of doc.querySelectorAll("img")) {
      const src = img.getAttribute("src") ?? "";
      if (/^[a-z][\w+.-]*:/i.test(src) || src.startsWith("//")) continue;
      try {
        const path = resolvePath(decodeURI(src.split(/[?#]/)[0]), dir, this.home());
        const bytes = await this.host.read(path);
        if (bytes.length > INLINE_IMAGE_MAX || total + bytes.length > INLINE_TOTAL_MAX) continue;
        total += bytes.length;
        img.setAttribute("src", dataUrl(bytes, mimeType(path)));
      } catch {
        // Left as is: the CSP blocks it and the alt text shows.
      }
    }
    const css = getComputedStyle(document.documentElement);
    const v = (name: string) => css.getPropertyValue(name).trim();
    const style = `
      :root { color-scheme: dark light; }
      body { margin: 0; padding: 16px 20px 40px; background: ${v("--bg")}; color: ${v("--text")};
        font: 14px/1.6 -apple-system, BlinkMacSystemFont, "Segoe UI", Inter, sans-serif; overflow-wrap: break-word; }
      a { color: ${v("--accent")}; }
      h1, h2 { border-bottom: 1px solid ${v("--border")}; padding-bottom: .3em; }
      h1, h2, h3, h4 { color: ${v("--text-strong")}; line-height: 1.25; margin: 1.4em 0 .6em; }
      code, pre { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12.5px; }
      code { background: ${v("--surface")}; padding: .15em .35em; border-radius: 4px; }
      pre { background: ${v("--surface")}; padding: 10px 12px; border-radius: 6px; overflow-x: auto; }
      pre code { background: none; padding: 0; }
      blockquote { margin: 0; padding: 0 1em; color: ${v("--muted")}; border-left: 3px solid ${v("--border-strong")}; }
      table { border-collapse: collapse; display: block; overflow-x: auto; }
      th, td { border: 1px solid ${v("--border")}; padding: 4px 10px; }
      img { max-width: 100%; }
      hr { border: none; border-top: 1px solid ${v("--border")}; }
    `;
    const n = nonce();
    return `<!doctype html><html data-scroll="${Math.round(scroll)}"><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${markdownCsp(n)}"><style>${style}</style><script nonce="${n}">${DOC_SCRIPT}</script></head><body>${doc.body.innerHTML}</body></html>`;
  }

  /// Messages from the shown markdown frame's DOC_SCRIPT; anything else posting messages is ignored.
  private onFrameMessage(e: MessageEvent) {
    const frame = this.body.querySelector<HTMLIFrameElement>("iframe.markdown");
    const doc = this.doc;
    if (!frame || !doc || e.source !== frame.contentWindow) return;
    const data = e.data as { ftmViewer?: string; y?: unknown; href?: unknown };
    if (data?.ftmViewer === "scroll" && typeof data.y === "number") doc.scroll = data.y;
    if (data?.ftmViewer !== "link" || typeof data.href !== "string") return;
    const href = data.href;
    if (/^[a-z][\w+.-]*:/i.test(href)) {
      this.host.openLink(href);
      return;
    }
    try {
      this.host.openPath(resolvePath(decodeURI(href.split(/[?#]/)[0]), doc.dir, this.home()));
    } catch {
      // A malformed escape: nothing to open.
    }
  }

  private setupResize(handle: HTMLElement) {
    const set = (w: number) => document.documentElement.style.setProperty("--viewer-width", `${w}px`);
    const clamp = (w: number) => Math.round(Math.min(Math.max(w, 240), window.innerWidth * 0.7));
    try {
      const saved = Number(localStorage.getItem(WIDTH_KEY));
      if (saved) set(clamp(saved));
    } catch {
      // Keep the default.
    }
    const store = (w: number) => {
      try {
        if (w === WIDTH_DEFAULT) localStorage.removeItem(WIDTH_KEY);
        else localStorage.setItem(WIDTH_KEY, String(w));
      } catch {
        // Only the width is lost.
      }
    };
    let drag: { x: number; width: number; last: number } | null = null;
    handle.onpointerdown = (e) => {
      if (e.button !== 0) return;
      const width = this.root.getBoundingClientRect().width;
      drag = { x: e.clientX, width, last: width };
      handle.setPointerCapture(e.pointerId);
      // The frame under the pointer would otherwise take the moves.
      document.body.classList.add("resizing-viewer");
      e.preventDefault();
    };
    handle.onpointermove = (e) => {
      if (!drag) return;
      drag.last = clamp(drag.width - (e.clientX - drag.x));
      set(drag.last);
    };
    const end = () => {
      if (!drag) return;
      store(drag.last);
      drag = null;
      document.body.classList.remove("resizing-viewer");
    };
    handle.onpointerup = end;
    handle.onpointercancel = end;
    handle.ondblclick = () => {
      set(WIDTH_DEFAULT);
      store(WIDTH_DEFAULT);
    };
  }

  private loadState() {
    try {
      const saved = JSON.parse(localStorage.getItem(STATE_KEY) ?? "{}");
      for (const [id, e] of Object.entries(saved as Record<string, Partial<Entry>>)) {
        if (typeof e?.path !== "string" && e?.path !== null) continue;
        this.entries.set(id, {
          path: e.path ?? null,
          collapsed: !!e.collapsed,
          unread: !!e.unread,
          recent: Array.isArray(e.recent) ? e.recent.filter((p): p is string => typeof p === "string").slice(0, RECENT_MAX) : [],
          tab: e.tab === "tasks" ? "tasks" : undefined,
        });
      }
    } catch {
      // A corrupt entry only loses the panels.
    }
  }

  private save() {
    try {
      localStorage.setItem(STATE_KEY, JSON.stringify(Object.fromEntries(this.entries)));
    } catch {
      // Storage is a convenience.
    }
  }
}
