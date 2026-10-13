import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import "./style.css";

type Id = string;
type Layout =
  | { type: "pane"; id: Id }
  | { type: "split"; dir: "row" | "column"; ratio: number; first: Layout; second: Layout };
interface Tab {
  id: Id;
  layout: Layout;
}
interface Workspace {
  id: Id;
  parent: Id | null;
  name: string;
  title: string | null;
  kind: "terminal" | "agent";
  cwd: string;
  worktree: { repo: string; branch: string } | null;
  status: string;
  tabs: Tab[];
}
type FtmEvent =
  | { event: "workspaces"; workspaces: Workspace[] }
  | { event: "output"; pane: Id; data: string; reset: boolean };

interface PaneView {
  term: Terminal;
  fit: FitAddon;
  el: HTMLElement;
  cols: number;
  rows: number;
}

const isMac = navigator.userAgent.includes("Mac");
const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

let workspaces: Workspace[] = [];
let selected: Id | null = null;
/// Restored from the last run once the daemon's tree arrives; ftmd keeps the sessions, this keeps where you were.
let restoredUi = false;
let focused: Id | null = null;
let pendingFocus: Id | null = null;
let shownLayout = "";
/// The workspace whose name is being edited; the sidebar is not rebuilt meanwhile, so the input survives tree updates.
let editing: Id | null = null;
const activeTab = new Map<Id, Id>();
const views = new Map<Id, PaneView>();

const request = (req: object): Promise<any> => invoke("request", { request: req });
const send = (req: object) => invoke("send", { request: req }).catch(console.error);

function encode(text: string): string {
  let bin = "";
  for (const b of new TextEncoder().encode(text)) bin += String.fromCharCode(b);
  return btoa(bin);
}

function decode(data: string): Uint8Array {
  const bin = atob(data);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function panesOf(layout: Layout): Id[] {
  return layout.type === "pane" ? [layout.id] : [...panesOf(layout.first), ...panesOf(layout.second)];
}

/// Terminals live in a hidden parking element while their tab is not shown, so they keep scrollback and state.
function view(pane: Id): PaneView {
  const existing = views.get(pane);
  if (existing) return existing;
  const el = document.createElement("div");
  el.className = "term";
  $("parking").appendChild(el);
  const term = new Terminal({
    fontFamily: 'Menlo, "DejaVu Sans Mono", monospace',
    fontSize: 13,
    scrollback: 10000,
    theme: { background: "#16181d" },
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(el);
  try {
    const gl = new WebglAddon();
    gl.onContextLoss(() => gl.dispose());
    term.loadAddon(gl);
  } catch (e) {
    console.warn("webgl unavailable, using the DOM renderer", e);
  }
  term.onData((d) => send({ op: "input", pane, data: encode(d) }));
  term.onBinary((d) => send({ op: "input", pane, data: btoa(d) }));
  term.textarea?.addEventListener("focus", () => setFocus(pane));
  const v = { term, fit, el, cols: 0, rows: 0 };
  views.set(pane, v);
  return v;
}

function selectedWorkspace(): Workspace | undefined {
  return workspaces.find((w) => w.id === selected);
}

function currentTab(): Tab | undefined {
  const ws = selectedWorkspace();
  if (!ws) return undefined;
  return ws.tabs.find((t) => t.id === activeTab.get(ws.id)) ?? ws.tabs[0];
}

function setFocus(pane: Id) {
  focused = pane;
  document.querySelectorAll(".slot").forEach((s) => s.classList.toggle("focused", (s as HTMLElement).dataset.pane === pane));
}

let seenTree = false;

function onWorkspaces(list: Workspace[]) {
  notifyNeedsInput(list, !seenTree).catch(console.error);
  seenTree = true;
  workspaces = list;
  const live = new Set(list.flatMap((w) => w.tabs.flatMap((t) => panesOf(t.layout))));
  for (const [id, v] of views) {
    if (!live.has(id)) {
      v.term.dispose();
      v.el.remove();
      views.delete(id);
    }
  }
  if (!restoredUi) {
    restoredUi = true;
    restoreUi(list);
  }
  if (!list.some((w) => w.id === selected)) selected = list[0]?.id ?? null;
  claimPendingFocus();
  render();
}

const UI_KEY = "ftm.ui";

function saveUi() {
  try {
    localStorage.setItem(UI_KEY, JSON.stringify({ selected, activeTab: [...activeTab] }));
  } catch {
    // Storage is a convenience; losing it only loses the selection.
  }
}

function restoreUi(list: Workspace[]) {
  try {
    const saved = JSON.parse(localStorage.getItem(UI_KEY) ?? "null");
    if (!saved) return;
    if (list.some((w) => w.id === saved.selected)) selected = saved.selected;
    for (const [ws, tab] of saved.activeTab ?? []) activeTab.set(ws, tab);
  } catch {
    // Ignore a corrupt or missing entry.
  }
}

/// A new tab or split's pane can show up in the tree before or after its request returns; whichever is second focuses it.
function claimPendingFocus() {
  if (!pendingFocus) return;
  for (const ws of workspaces) {
    const tab = ws.tabs.find((t) => panesOf(t.layout).includes(pendingFocus!));
    if (tab) {
      selected = ws.id;
      activeTab.set(ws.id, tab.id);
      focused = pendingFocus;
      pendingFocus = null;
      return;
    }
  }
}

function render() {
  saveUi();
  renderSidebar();
  renderTabs();
  renderPanes();
}

const displayName = (ws: Workspace) => ws.title ?? ws.name;

function renderSidebar() {
  if (editing && workspaces.some((w) => w.id === editing)) return;
  editing = null;
  const ids = new Set(workspaces.map((w) => w.id));
  const build = (parent: Id | null): HTMLUListElement => {
    const ul = document.createElement("ul");
    const children = workspaces.filter((w) => (parent === null ? !w.parent || !ids.has(w.parent) : w.parent === parent));
    for (const ws of children) {
      const li = document.createElement("li");
      const row = document.createElement("div");
      row.className = "ws" + (ws.id === selected ? " selected" : "");
      row.dataset.id = ws.id;
      row.title = ws.title ? `${ws.name} · ${ws.cwd}` : ws.cwd;
      const dot = document.createElement("span");
      dot.className = ws.kind === "agent" ? `dot ${ws.status}` : "dot none";
      dot.title = ws.kind === "agent" ? ws.status.replace("_", " ") : "";
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = (ws.kind === "agent" ? "✻ " : "") + displayName(ws);
      name.ondblclick = (e) => {
        e.stopPropagation();
        startRename(ws.id);
      };
      const branch = document.createElement("span");
      branch.className = "branch";
      branch.textContent = ws.worktree?.branch ?? "";
      const close = document.createElement("button");
      close.className = "close";
      close.textContent = "×";
      close.title = "Close workspace";
      close.onclick = (e) => {
        e.stopPropagation();
        request({ op: "close", id: ws.id, remove_worktree: false }).catch(console.error);
      };
      row.append(dot, name, branch);
      const waiting = descendantsNeedingInput(ws.id);
      if (waiting > 0) {
        const badge = document.createElement("span");
        badge.className = "badge";
        badge.textContent = `${waiting}`;
        badge.title = `${waiting} agent${waiting === 1 ? "" : "s"} below need input`;
        row.append(badge);
      }
      row.append(close);
      row.onclick = () => {
        selected = ws.id;
        render();
      };
      row.oncontextmenu = (e) => {
        e.preventDefault();
        showMenu(e.clientX, e.clientY, [["Rename", () => startRename(ws.id)]]);
      };
      li.append(row);
      if (workspaces.some((w) => w.parent === ws.id)) li.append(build(ws.id));
      ul.append(li);
    }
    return ul;
  };
  $("workspaces").replaceChildren(...build(null).children);
}

function startRename(id: Id) {
  closeMenu();
  const ws = workspaces.find((w) => w.id === id);
  const label = document.querySelector(`.ws[data-id="${id}"] .name`);
  if (!ws || !label || editing) return;
  editing = id;
  const input = document.createElement("input");
  input.className = "rename";
  input.value = displayName(ws);
  let done = false;
  const finish = () => {
    done = true;
    editing = null;
    render();
  };
  const save = async () => {
    if (done) return;
    done = true;
    const value = input.value.trim();
    if (value === displayName(ws)) return finish();
    try {
      await request({ op: "rename", id: ws.id, name: value });
      finish();
    } catch (err) {
      done = false;
      input.classList.add("invalid");
      input.title = String(err);
    }
  };
  input.onkeydown = (e) => {
    e.stopPropagation();
    if (e.key === "Enter") save();
    else if (e.key === "Escape" && !done) finish();
    else input.classList.remove("invalid");
  };
  input.onblur = save;
  for (const ev of ["click", "dblclick", "mousedown", "contextmenu"] as const) input.addEventListener(ev, (e) => e.stopPropagation());
  label.replaceWith(input);
  input.focus();
  input.select();
}

function showMenu(x: number, y: number, items: [string, () => void][]) {
  closeMenu();
  const menu = document.createElement("div");
  menu.id = "menu";
  for (const [label, action] of items) {
    const item = document.createElement("button");
    item.textContent = label;
    item.onclick = () => {
      closeMenu();
      action();
    };
    menu.append(item);
  }
  document.body.append(menu);
  menu.style.left = `${Math.min(x, innerWidth - menu.offsetWidth - 4)}px`;
  menu.style.top = `${Math.min(y, innerHeight - menu.offsetHeight - 4)}px`;
}

function closeMenu() {
  document.getElementById("menu")?.remove();
}

function descendantsNeedingInput(id: Id): number {
  return workspaces
    .filter((w) => w.parent === id)
    .reduce((n, w) => n + (w.kind === "agent" && w.status === "needs_input" ? 1 : 0) + descendantsNeedingInput(w.id), 0);
}

const lastStatus = new Map<Id, string>();

/// One desktop notification per transition into needs_input; the first tree after startup only records statuses.
async function notifyNeedsInput(list: Workspace[], initial: boolean) {
  const fresh = list.filter((w) => w.kind === "agent" && w.status === "needs_input" && lastStatus.get(w.id) !== "needs_input");
  lastStatus.clear();
  for (const w of list) lastStatus.set(w.id, w.status);
  if (initial || fresh.length === 0) return;
  let granted = await isPermissionGranted();
  if (!granted) granted = (await requestPermission()) === "granted";
  if (!granted) return;
  for (const w of fresh) sendNotification({ title: displayName(w), body: "needs input" });
}

function renderTabs() {
  const nav = $("tabs");
  nav.replaceChildren();
  const ws = selectedWorkspace();
  if (!ws) return;
  const current = currentTab();
  ws.tabs.forEach((tab, i) => {
    const b = document.createElement("button");
    b.textContent = `${i + 1}`;
    b.className = tab.id === current?.id ? "active" : "";
    b.onclick = () => {
      activeTab.set(ws.id, tab.id);
      render();
    };
    nav.append(b);
  });
  const add = document.createElement("button");
  add.textContent = "+";
  add.title = "New tab";
  add.onclick = newTab;
  nav.append(add);
}

function renderPanes() {
  const root = $("panes");
  const tab = currentTab();
  const key = tab ? tab.id + JSON.stringify(tab.layout) : "";
  if (key !== shownLayout) {
    shownLayout = key;
    for (const v of views.values()) $("parking").appendChild(v.el);
    if (!tab) {
      const empty = document.createElement("div");
      empty.className = "empty";
      empty.textContent = selectedWorkspace() ? "No tabs. Press + to open one." : "No workspaces. Press + in the sidebar.";
      root.replaceChildren(empty);
    } else {
      root.replaceChildren(build(tab.layout));
    }
  }
  if (!tab) return;
  const panes = panesOf(tab.layout);
  if (!focused || !panes.includes(focused)) focused = panes[0];
  setFocus(focused);
  requestAnimationFrame(() => {
    fitVisible();
    if (!editing) views.get(focused!)?.term.focus();
  });
}

function build(layout: Layout): HTMLElement {
  if (layout.type === "pane") {
    const slot = document.createElement("div");
    slot.className = "slot";
    slot.dataset.pane = layout.id;
    slot.append(view(layout.id).el);
    slot.onmousedown = () => setFocus(layout.id);
    return slot;
  }
  const el = document.createElement("div");
  el.className = `split ${layout.dir}`;
  const a = build(layout.first);
  const b = build(layout.second);
  a.style.flex = `${layout.ratio} 1 0`;
  b.style.flex = `${1 - layout.ratio} 1 0`;
  el.append(a, b);
  return el;
}

function fitVisible() {
  const tab = currentTab();
  if (!tab) return;
  for (const pane of panesOf(tab.layout)) {
    const v = views.get(pane);
    if (!v) continue;
    v.fit.fit();
    if (v.term.cols !== v.cols || v.term.rows !== v.rows) {
      v.cols = v.term.cols;
      v.rows = v.term.rows;
      send({ op: "resize", pane, cols: v.cols, rows: v.rows });
    }
  }
}

function targetPane(): Id | undefined {
  const tab = currentTab();
  if (!tab) return undefined;
  const panes = panesOf(tab.layout);
  return focused && panes.includes(focused) ? focused : panes[0];
}

async function newTab() {
  if (!selected) return;
  const r = await request({ op: "new_tab", workspace: selected }).catch(console.error);
  if (r?.pane) focusWhenShown(r.pane);
}

async function split(dir: "row" | "column") {
  const pane = targetPane();
  if (!pane) return newTab();
  const r = await request({ op: "split", pane, dir }).catch(console.error);
  if (r?.pane) focusWhenShown(r.pane);
}

function focusWhenShown(pane: Id) {
  pendingFocus = pane;
  claimPendingFocus();
  if (!pendingFocus) render();
}

function closePane() {
  const pane = targetPane();
  if (pane) request({ op: "close_pane", pane }).catch(console.error);
}

/// Cmd+T/D/Shift+D/W on macOS; Ctrl+Shift+T/D/E/W on Linux (Ctrl+Shift+D can't also carry a second Shift).
function shortcut(e: KeyboardEvent) {
  if (e.key === "Escape") closeMenu();
  if (editing) return;
  const mod = isMac ? e.metaKey && !e.ctrlKey && !e.altKey : e.ctrlKey && e.shiftKey && !e.metaKey && !e.altKey;
  if (!mod) return;
  const key = e.key.toLowerCase();
  const action =
    key === "t"
      ? newTab
      : key === "d"
        ? () => split(isMac && e.shiftKey ? "column" : "row")
        : key === "e" && !isMac
          ? () => split("column")
          : key === "w"
            ? closePane
            : null;
  if (!action) return;
  e.preventDefault();
  e.stopPropagation();
  if (!e.repeat) action();
}

function setupSpawnDialog() {
  const dialog = $<HTMLDialogElement>("spawn");
  const form = $<HTMLFormElement>("spawn-form");
  const error = $("spawn-error");
  const create = $<HTMLButtonElement>("create");
  let folder: string | null = null;

  $("add").onclick = () => {
    form.reset();
    folder = null;
    $("folder").textContent = "none";
    $("git").hidden = true;
    error.textContent = "";
    dialog.showModal();
  };

  $("pick").onclick = async () => {
    const dir = await open({ directory: true, defaultPath: folder ?? undefined });
    if (typeof dir !== "string") return;
    folder = dir;
    $("folder").textContent = dir;
    $("folder").title = dir;
    const root = await invoke<string | null>("git_root", { path: dir });
    $("git").hidden = !root;
  };

  create.onclick = async (e) => {
    e.preventDefault();
    if (!folder) {
      error.textContent = "Pick a folder first.";
      return;
    }
    const data = new FormData(form);
    const text = (k: string) => ((data.get(k) as string | null) ?? "").trim() || null;
    const worktree = !$("git").hidden && data.get("worktree") === "on";
    if (worktree && !text("branch")) {
      error.textContent = "A worktree needs a branch name.";
      return;
    }
    create.disabled = true;
    error.textContent = worktree ? "Creating worktree…" : "";
    try {
      const r = await request({
        op: "spawn",
        kind: data.get("kind"),
        name: text("name"),
        parent: null,
        cwd: folder,
        worktree_branch: worktree ? text("branch") : null,
        base: worktree ? text("base") : null,
        copy_changes: worktree && data.get("copy") === "on",
        brief: null,
      });
      selected = r.workspace.id;
      dialog.close();
      render();
    } catch (err) {
      error.textContent = String(err);
    } finally {
      create.disabled = false;
    }
  };
}

invoke("ensure_on_screen").catch(console.error);
window.addEventListener("keydown", shortcut, true);
window.addEventListener("mousedown", (e) => {
  if (!(e.target as HTMLElement).closest("#menu")) closeMenu();
});
window.addEventListener("blur", closeMenu);
new ResizeObserver(() => requestAnimationFrame(fitVisible)).observe($("panes"));
setupSpawnDialog();
render();

listen<FtmEvent>("ftm-event", ({ payload }) => {
  if (payload.event === "workspaces") {
    onWorkspaces(payload.workspaces);
  } else {
    const v = view(payload.pane);
    if (payload.reset) v.term.reset();
    v.term.write(decode(payload.data));
  }
}).then(() => invoke("subscribe"));
