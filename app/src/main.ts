import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { homeDir } from "@tauri-apps/api/path";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { open } from "@tauri-apps/plugin-dialog";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { FitAddon } from "@xterm/addon-fit";
import { LigaturesAddon } from "@xterm/addon-ligatures";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { chromeColors, WORKSPACE_COLORS, WORKSPACE_PALETTE } from "./colors.ts";
import { dropText } from "./drop.ts";
import { StatusSounds, statusDot, stillFinished } from "./finish.ts";
import { clipboardKey, type OptionAsMeta, optionIsMeta, paneKey } from "./keys.ts";
import { KeyLayout, type KeyProtocol, kittyKeyboard } from "./kitty.ts";
import {
  defaultSort,
  moveItem,
  parseSectionSort,
  reorderWithin,
  type SortMode,
  sameOrder,
  sortRows,
} from "./order.ts";
import { replyFilter } from "./replies.ts";
import { clampSidebarWidth, SIDEBAR_DEFAULT } from "./sidebar.ts";
import { loadSounds, play, type SoundEntry, type SoundSettings } from "./sounds.ts";
import "./style.css";
import { Tabs } from "./tabs.ts";
import { compareVersions, versionNotice } from "./version.ts";
import { type AppScroll, type AppWheel, appWheel } from "./wheel.ts";

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
  auto_title: string | null;
  pinned: boolean;
  /// A name from `WORKSPACE_COLORS` or `#rrggbb`; missing from an ftmd that predates colors.
  color?: string | null;
  /// "unknown" for a kind or status from a newer ftmd than this app knows.
  kind: "terminal" | "agent" | "unknown";
  cwd: string;
  worktree: { repo: string; branch: string } | null;
  status: string;
  unseen: boolean;
  background_agents: number;
  tabs: Tab[];
}
interface Folder {
  path: string;
  last_used: number;
}
type FtmEvent =
  | { event: "workspaces"; workspaces: Workspace[] }
  | { event: "folders"; folders: Folder[]; order?: string[] }
  | { event: "output"; pane: Id; data: string; reset: boolean }
  | { event: "update_blocked"; version: string | null; reason: string };

interface PaneView {
  term: Terminal;
  fit: FitAddon;
  ligatures?: LigaturesAddon;
  kitty: KeyProtocol;
  wheel: AppWheel;
  /// Replays of the daemon's buffer in flight; queries in them (DA, DSR, kitty's `CSI ? u`) were answered long ago,
  /// so nothing xterm sends meanwhile reaches the pane.
  replaying: number;
  el: HTMLElement;
  cols: number;
  rows: number;
}

const isMac = navigator.userAgent.includes("Mac");

const report = (message: string) => {
  invoke("log", { message }).catch(() => {});
};
const consoleError = console.error.bind(console);
console.error = (...args: unknown[]) => {
  consoleError(...args);
  report(args.map((a) => (a instanceof Error ? `${a.message}\n${a.stack}` : String(a))).join(" "));
};
window.addEventListener("error", (e) => report(`${e.message} at ${e.filename}:${e.lineno}`));
window.addEventListener("unhandledrejection", (e) => report(`unhandled rejection: ${e.reason?.stack ?? e.reason}`));
const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

let workspaces: Workspace[] = [];
/// Most recently used first.
let folders: Folder[] = [];
/// The sidebar's order, rearranged by dragging; empty from an ftmd that predates it.
let folderOrder: string[] = [];
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

// biome-ignore lint/suspicious/noExplicitAny: ftmd responses are untyped JSON whose shape depends on the op
const request = (req: object): Promise<any> => invoke("request", { request: req });
const send = (req: object) => {
  invoke("send", { request: req }).catch(console.error);
};

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
    fontFamily: cssFamily(settings.font_family),
    fontSize: settings.font_size,
    lineHeight: settings.line_height,
    scrollback: 10000,
    theme: xtermTheme,
    allowProposedApi: true,
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(el);
  // WebKitGTK without its DMA-BUF renderer (forced on in main.rs) leaves a WebGL canvas unpainted until something
  // else repaints the page, so output shows up one keystroke late; the DOM renderer has no such lag.
  if (!navigator.userAgent.includes("Linux")) {
    try {
      const gl = new WebglAddon();
      gl.onContextLoss(() => gl.dispose());
      term.loadAddon(gl);
    } catch (e) {
      console.warn("webgl unavailable, using the DOM renderer", e);
    }
  }
  const input = (data: string) => send({ op: "input", pane, data });
  const replies = replyFilter(() => v.replaying > 0);
  term.onData((d) => {
    if (replies.keep(d)) input(encode(d));
  });
  term.onBinary((d) => input(btoa(d)));
  const kitty = kittyKeyboard(term, (d) => {
    if (!v.replaying) input(encode(d));
  });
  const wheel = appWheel(term, () => settings.app_scroll, (d) => input(encode(d)));
  term.attachCustomKeyEventHandler((e) => {
    replies.key();
    const clip = clipboardKey(e, isMac);
    if (clip) {
      e.preventDefault();
      if (clip === "paste") readText().then((text) => text && term.paste(text), console.error);
      else if (term.hasSelection()) writeText(term.getSelection()).catch(console.error);
      return false;
    }
    const altIsMeta = !isMac || optionIsMeta(settings.option_as_meta, optionsDown);
    if (isMac && e.type === "keydown") term.options.macOptionIsMeta = altIsMeta;
    const seq = paneKey(e, { mac: isMac, altIsMeta, kitty, layout });
    if (seq === undefined) return true;
    e.preventDefault();
    term.input(seq);
    return false;
  });
  // xterm already turns off autocorrect, autocapitalize and spellcheck; macOS inline predictions also need these.
  term.textarea?.setAttribute("writingsuggestions", "false");
  term.textarea?.setAttribute("autocomplete", "off");
  term.textarea?.addEventListener("focus", () => setFocus(pane));
  const v: PaneView = { term, fit, kitty, wheel, replaying: 0, el, cols: 0, rows: 0 };
  setLigatures(v);
  views.set(pane, v);
  return v;
}

interface ThemeColors {
  name: string;
  background: string;
  foreground: string;
  cursor: string;
  cursor_text: string;
  selection_background: string;
  selection_foreground: string | null;
  palette: string[];
}

interface ThemeSettings {
  name: string;
  follow_system: boolean;
  dark: string;
  light: string;
}

let themeSettings: ThemeSettings = { name: "Supacode Dark", follow_system: false, dark: "Supacode Dark", light: "Supacode Light" };
let themes: Promise<(ThemeColors & { source: string })[]> | null = null;
const loadThemes = () => (themes ??= invoke<(ThemeColors & { source: string })[]>("list_themes"));
let xtermTheme: Record<string, string> = { background: "#1e1e1e", foreground: "#ffffff" };

const systemDark = window.matchMedia("(prefers-color-scheme: dark)");
/// The theme name the settings call for right now, following the system appearance if asked to.
const wantedTheme = (t: ThemeSettings) => (t.follow_system ? (systemDark.matches ? t.dark : t.light) : t.name);

/// Terminal colors from the theme; the app's chrome derived from its background, foreground and palette.
function applyTheme(t: ThemeColors) {
  const p = t.palette;
  xtermTheme = {
    background: t.background,
    foreground: t.foreground,
    cursor: t.cursor,
    cursorAccent: t.cursor_text,
    selectionBackground: t.selection_background,
    ...(t.selection_foreground ? { selectionForeground: t.selection_foreground } : {}),
    black: p[0], red: p[1], green: p[2], yellow: p[3], blue: p[4], magenta: p[5], cyan: p[6], white: p[7],
    brightBlack: p[8], brightRed: p[9], brightGreen: p[10], brightYellow: p[11],
    brightBlue: p[12], brightMagenta: p[13], brightCyan: p[14], brightWhite: p[15],
  };
  for (const v of views.values()) v.term.options.theme = xtermTheme;

  const { vars, dark } = chromeColors(t.background, t.foreground, p);
  for (const [k, v] of Object.entries(vars)) document.documentElement.style.setProperty(k, v);
  document.documentElement.style.colorScheme = dark ? "dark" : "light";
}

/// Applies the named theme, falling back to the default when it is missing (e.g. a removed user file).
async function applyThemeNamed(name: string) {
  const list = await loadThemes();
  const theme =
    list.find((t) => t.name.toLowerCase() === name.toLowerCase()) ??
    list.find((t) => t.name === "Supacode Dark") ??
    list[0];
  if (theme) applyTheme(theme);
}

systemDark.addEventListener("change", () => {
  if (themeSettings.follow_system) applyThemeNamed(wantedTheme(themeSettings)).catch(console.error);
});

interface TerminalSettings {
  font_family: string;
  font_size: number;
  ligatures: boolean;
  line_height: number;
  option_as_meta: OptionAsMeta;
  app_scroll: AppScroll;
}

let settings: TerminalSettings = {
  font_family: 'Menlo, "DejaVu Sans Mono", monospace',
  font_size: 13,
  ligatures: false,
  line_height: 1,
  option_as_meta: "both",
  app_scroll: "rows",
};

/// Which Option keys are down, for Option as Meta on one side only; key events only say that some Alt is held.
const optionsDown = new Set<string>();
/// Learned from every key typed anywhere in the window.
const layout = new KeyLayout();
for (const type of ["keydown", "keyup"] as const)
  window.addEventListener(
    type,
    (e) => {
      if (e.code === "AltLeft" || e.code === "AltRight") optionsDown[type === "keydown" ? "add" : "delete"](e.code);
      layout.learn(e);
    },
    true,
  );
window.addEventListener("blur", () => optionsDown.clear());

/// A bare family name gets quoted and a monospace fallback; a CSS list is used as written.
const cssFamily = (family: string) => (/[,"']/.test(family) ? family : `"${family.trim()}", Menlo, monospace`);

/// WebKit has no font access API, so the addon cannot read the font's own ligature table; it joins a list of common
/// sequences (=>, !=, ->, === ...) instead, and the renderer then draws each joined run with the font's glyphs.
function setLigatures(v: PaneView) {
  if (settings.ligatures && !v.ligatures) {
    try {
      const addon = new LigaturesAddon();
      v.term.loadAddon(addon);
      v.ligatures = addon;
    } catch (e) {
      console.error("ligatures unavailable", e);
    }
  } else if (!settings.ligatures && v.ligatures) {
    v.ligatures.dispose();
    v.ligatures = undefined;
  }
}

async function applySettings() {
  await document.fonts.load(`${settings.font_size}px ${cssFamily(settings.font_family)}`).catch(() => {});
  for (const v of views.values()) {
    v.term.options.fontFamily = cssFamily(settings.font_family);
    v.term.options.fontSize = settings.font_size;
    v.term.options.lineHeight = settings.line_height;
    setLigatures(v);
    v.term.clearTextureAtlas();
    v.cols = 0;
  }
  requestAnimationFrame(fitVisible);
}


interface FontFamily {
  name: string;
  monospace: boolean;
}

let fontList: Promise<FontFamily[]> | null = null;

let soundSettings: SoundSettings = { needs_input: "Clink", completed: "Ding", volume: 0.7, wait_for_subagents: false };

type Channel = "stable" | "nightly";
/// Unset (`null`) means automatic: check only on a pacman install, on this build's own channel.
interface UpdateSettings {
  check: boolean | null;
  channel: Channel | null;
}
let updateSettings: UpdateSettings = { check: null, channel: null };

const playSound = (name: string, volume = soundSettings.volume) => {
  play(name, volume).catch((err) => console.error(`playing ${name}`, err));
};

/// A searchable list of installed families (monospace first, the rest behind "Show all fonts"), each drawn in its own
/// font, with "Custom…" for a CSS font stack, and a preview line that shows ligatures on or off.
function setupSettingsDialog() {
  const dialog = $<HTMLDialogElement>("settings");
  const form = $<HTMLFormElement>("settings-form");
  const field = (name: string) => form.elements.namedItem(name) as HTMLInputElement;
  const custom = $<HTMLInputElement>("font-custom");
  const search = $<HTMLInputElement>("font-search");
  const showAll = $<HTMLInputElement>("font-all");
  let fonts: FontFamily[] = [];
  let family = settings.font_family;
  let customMode = false;

  // The open tab outlives the dialog, so settings reopens where it was left.
  const tabButtons = [...document.querySelectorAll<HTMLButtonElement>("#settings-tabs [role=tab]")];
  const panels = tabButtons.map((t) => $(t.getAttribute("aria-controls") ?? ""));
  // Hidden lists cannot scroll to their pick, so it is done when the tab shows.
  const revealChosen = (id: string) =>
    panels[tabButtons.findIndex((t) => t.id === id)]
      ?.querySelector("#font-list .chosen:not(.custom), #theme-list .chosen")
      ?.scrollIntoView({ block: "nearest" });
  const tabs = new Tabs(tabButtons, panels, revealChosen);
  for (const t of tabButtons) t.onclick = () => tabs.select(t.id);
  $("settings-tabs").onkeydown = (e) => {
    if (tabs.key(e.key)) e.preventDefault();
  };

  const isStack = (f: string) => /[,"']/.test(f);
  const chosen = () => (customMode ? custom.value.trim() : family);

  const preview = () => {
    const p = $("font-preview");
    p.style.fontFamily = cssFamily(chosen() || settings.font_family);
    p.style.fontSize = `${Number(field("font_size").value) || settings.font_size}px`;
    p.style.lineHeight = String(Number(field("line_height").value) || 1);
    p.style.fontVariantLigatures = field("ligatures").checked ? "normal" : "none";
    p.style.fontFeatureSettings = field("ligatures").checked ? '"calt" 1, "liga" 1' : '"calt" 0, "liga" 0';
    $("line-height-value").textContent = Number(field("line_height").value).toFixed(2);
    // Only a custom stack can name a font that is not installed: picks come from list_fonts. Its first family is
    // checked against that list, since WebKit's own font checks are unreliable for system fonts.
    const first = chosen().split(",")[0].trim().replace(/^["']|["']$/g, "");
    const generic = /^(monospace|serif|sans-serif|ui-monospace|system-ui|cursive|fantasy)$/i.test(first);
    const known = fonts.some((f) => f.name.toLowerCase() === first.toLowerCase());
    $("font-hint").textContent =
      customMode && first && !generic && fonts.length && !known ? `"${first}" is not an installed font; the next one in the stack will be used.` : "";
  };

  const renderList = () => {
    const query = search.value.trim().toLowerCase();
    const matches = fonts.filter((f) => !query || f.name.toLowerCase().includes(query));
    const mono = matches.filter((f) => f.monospace);
    const rest = showAll.checked || query ? matches.filter((f) => !f.monospace) : [];
    const option = (f: FontFamily) => {
      const b = document.createElement("button");
      b.type = "button";
      b.textContent = f.name;
      b.title = f.name;
      b.style.fontFamily = `"${f.name}", monospace`;
      b.className = !customMode && f.name === family ? "chosen" : "";
      b.onclick = () => {
        customMode = false;
        family = f.name;
        custom.hidden = true;
        renderList();
        preview();
      };
      return b;
    };
    const group = (label: string) => {
      const g = document.createElement("div");
      g.className = "group";
      g.textContent = label;
      return g;
    };
    const customOption = document.createElement("button");
    customOption.type = "button";
    customOption.className = "custom" + (customMode ? " chosen" : "");
    customOption.textContent = "Custom… (a CSS font stack)";
    customOption.onclick = () => {
      customMode = true;
      custom.hidden = false;
      if (!custom.value) custom.value = family;
      custom.focus();
      renderList();
      preview();
    };
    const items: HTMLElement[] = [customOption];
    if (mono.length) items.push(group("Monospace"), ...mono.map(option));
    if (rest.length) items.push(group("Other fonts"), ...rest.map(option));
    if (!mono.length && !rest.length) {
      const none = document.createElement("p");
      none.className = "hint";
      none.textContent = fonts.length ? "No matching fonts" : "No fonts found";
      items.push(none);
    }
    $("font-list").replaceChildren(...items);
    $("font-list").querySelector(".chosen:not(.custom)")?.scrollIntoView({ block: "nearest" });
  };

  search.oninput = renderList;
  showAll.onchange = renderList;
  custom.oninput = preview;
  for (const name of ["font_size", "line_height", "ligatures"]) field(name).addEventListener("input", preview);
  form.querySelectorAll<HTMLButtonElement>(".stepper button").forEach((b) => {
    b.onclick = () => {
      const input = field("font_size");
      const next = (Number(input.value) || settings.font_size) + Number(b.dataset.step);
      input.value = String(Math.min(Number(input.max), Math.max(Number(input.min), next)));
      preview();
    };
  });

  // Theme: a draft that previews live, saved with the rest or reverted on cancel.
  let draft: ThemeSettings = { ...themeSettings };
  let slot: "dark" | "light" = "dark";
  let saved = false;
  let themeList: (ThemeColors & { source: string })[] = [];
  const themeSearch = $<HTMLInputElement>("theme-search");
  const follow = $<HTMLInputElement>("theme-follow");
  const target = () => (draft.follow_system ? draft[slot] : draft.name);
  const swatches = (t: ThemeColors) => {
    const s = document.createElement("span");
    s.className = "swatches";
    for (const c of t.palette.slice(0, 8)) {
      const i = document.createElement("i");
      i.style.background = c;
      s.append(i);
    }
    return s;
  };
  // What Save will apply, drawn in the theme's own colors once the list has loaded.
  const chip = (name: string) => {
    const c = document.createElement("span");
    c.className = "theme-chip";
    const label = document.createElement("span");
    label.textContent = name;
    c.append(label);
    const t = themeList.find((x) => x.name === name);
    if (t) {
      c.style.background = t.background;
      c.style.color = t.foreground;
      c.append(swatches(t));
    }
    return c;
  };
  const renderThemes = () => {
    $("theme-current").hidden = draft.follow_system;
    $("theme-slots").hidden = !draft.follow_system;
    $("theme-current-name").replaceChildren(chip(draft.name));
    $("theme-dark").replaceChildren(chip(draft.dark));
    $("theme-light").replaceChildren(chip(draft.light));
    document.querySelectorAll<HTMLButtonElement>("#theme-slots button").forEach((b) => {
      b.classList.toggle("active", b.dataset.slot === slot);
    });
    const query = themeSearch.value.trim().toLowerCase();
    const shown = themeList.filter((t) => !query || t.name.toLowerCase().includes(query));
    $("theme-count").textContent = themeList.length ? `${shown.length} of ${themeList.length}` : "";
    const items: HTMLElement[] = [];
    const labels: Record<string, string> = { builtin: "Built-in", user: "Yours (~/.config/ftm/themes)", system: "From other apps" };
    let group = "";
    for (const t of shown) {
      if (t.source !== group) {
        group = t.source;
        const g = document.createElement("div");
        g.className = "group";
        g.textContent = labels[group] ?? group;
        items.push(g);
      }
      const b = document.createElement("button");
      b.type = "button";
      b.className = t.name === target() ? "chosen" : "";
      b.style.background = t.background;
      b.style.color = t.foreground;
      const name = document.createElement("span");
      name.className = "theme-name";
      name.textContent = t.name;
      b.append(name, swatches(t));
      b.onclick = () => {
        if (draft.follow_system) draft[slot] = t.name;
        else draft.name = t.name;
        applyTheme(t);
        renderThemes();
      };
      items.push(b);
    }
    if (!items.length) {
      const none = document.createElement("p");
      none.className = "hint";
      none.textContent = themeList.length ? "No matching themes" : "Loading themes…";
      items.push(none);
    }
    $("theme-list").replaceChildren(...items);
  };
  themeSearch.oninput = renderThemes;
  follow.onchange = () => {
    draft.follow_system = follow.checked;
    slot = systemDark.matches ? "dark" : "light";
    applyThemeNamed(wantedTheme(draft)).catch(console.error);
    renderThemes();
  };
  document.querySelectorAll<HTMLButtonElement>("#theme-slots button").forEach((b) => {
    b.onclick = () => {
      slot = b.dataset.slot as "dark" | "light";
      applyThemeNamed(draft[slot]).catch(console.error);
      renderThemes();
    };
  });
  dialog.addEventListener("close", () => {
    if (!saved) applyThemeNamed(wantedTheme(themeSettings)).catch(console.error);
  });

  // Sounds: a draft saved with the rest; picking one plays it.
  let soundDraft: SoundSettings = { ...soundSettings };
  let soundList: SoundEntry[] = [];
  const events = ["needs_input", "completed"] as const;
  const volume = $<HTMLInputElement>("sound-volume");
  const renderSounds = () => {
    const labels: Record<string, string> = { builtin: "Built-in", user: "Yours (~/.config/ftm/sounds)", system: "System" };
    for (const event of events) {
      const select = $<HTMLSelectElement>(`sound-${event}`);
      const option = (value: string, label: string) => {
        const o = document.createElement("option");
        o.value = value;
        o.textContent = label;
        return o;
      };
      const items: HTMLElement[] = [option("", "None")];
      for (const source of ["builtin", "user", "system"]) {
        const sounds = soundList.filter((s) => s.source === source);
        if (!sounds.length) continue;
        const group = document.createElement("optgroup");
        group.label = labels[source];
        group.append(...sounds.map((s) => option(s.name, s.name)));
        items.push(group);
      }
      const current = soundDraft[event];
      if (current && !soundList.some((s) => s.name.toLowerCase() === current.toLowerCase())) {
        items.push(option(current, soundList.length ? `${current} (not found)` : current));
      }
      select.replaceChildren(...items);
      select.value = soundList.find((s) => s.name.toLowerCase() === current.toLowerCase())?.name ?? current;
    }
    volume.value = String(soundDraft.volume);
    $<HTMLInputElement>("sound-wait-subagents").checked = soundDraft.wait_for_subagents;
    $("sound-volume-value").textContent = `${Math.round(soundDraft.volume * 100)}%`;
  };
  for (const event of events) {
    $<HTMLSelectElement>(`sound-${event}`).onchange = (e) => {
      soundDraft[event] = (e.target as HTMLSelectElement).value;
      playSound(soundDraft[event], soundDraft.volume);
    };
  }
  form.querySelectorAll<HTMLButtonElement>("[data-preview]").forEach((b) => {
    b.onclick = () => playSound(soundDraft[b.dataset.preview as (typeof events)[number]], soundDraft.volume);
  });
  volume.oninput = () => {
    soundDraft.volume = Number(volume.value);
    $("sound-volume-value").textContent = `${Math.round(soundDraft.volume * 100)}%`;
  };
  $<HTMLInputElement>("sound-wait-subagents").onchange = (e) => {
    soundDraft.wait_for_subagents = (e.target as HTMLInputElement).checked;
  };

  $("open-settings").onclick = () => {
    saved = false;
    draft = { ...themeSettings };
    slot = systemDark.matches ? "dark" : "light";
    follow.checked = draft.follow_system;
    themeSearch.value = "";
    renderThemes();
    loadThemes()
      .then((list) => {
        themeList = list;
        renderThemes();
        $("theme-list").querySelector(".chosen")?.scrollIntoView({ block: "center" });
      })
      .catch((err) => console.error("listing themes", err));
    soundDraft = { ...soundSettings };
    renderSounds();
    loadSounds()
      .then((list) => {
        soundList = list;
        renderSounds();
      })
      .catch((err) => console.error("listing sounds", err));
    family = settings.font_family;
    customMode = isStack(family);
    custom.hidden = !customMode;
    custom.value = customMode ? family : "";
    search.value = "";
    field("font_size").value = String(settings.font_size);
    field("line_height").value = String(settings.line_height);
    field("ligatures").checked = settings.ligatures;
    $("keyboard-settings").hidden = !isMac;
    panels.forEach((p, i) => {
      tabs.setHidden(tabButtons[i].id, !p.querySelector(":scope > :not([hidden])"));
    });
    tabs.select(tabs.current);
    $<HTMLSelectElement>("option-as-meta").value = settings.option_as_meta;
    $<HTMLSelectElement>("app-scroll").value = settings.app_scroll;
    $<HTMLSelectElement>("update-check").value = updateSettings.check === null ? "" : String(updateSettings.check);
    $<HTMLSelectElement>("update-channel").value = updateSettings.channel ?? "";
    $("settings-error").textContent = "";
    preview();
    dialog.showModal();
    fontList ??= invoke<FontFamily[]>("list_fonts");
    fontList
      .then((list) => {
        fonts = list;
        preview();
        // A saved family that is not monospace (or not found) would otherwise be hidden.
        if (!customMode && !list.some((f) => f.name === family && f.monospace)) showAll.checked = true;
        renderList();
      })
      .catch((err) => {
        console.error("listing fonts", err);
        $("font-list").textContent = "Could not list fonts; use Custom…";
      });
  };

  $("save-settings").onclick = async (e) => {
    e.preventDefault();
    // Save skips the form's own validation, which would also refuse sizes off the stepper's half-point grid.
    const invalid = [...form.elements].find(
      (el): el is HTMLInputElement => el instanceof HTMLInputElement && !el.validity.valid && !el.validity.stepMismatch,
    );
    if (invalid) {
      const tab = tabs.tabOf(invalid.closest<HTMLElement>("[role=tabpanel]") as HTMLElement);
      if (tab) tabs.select(tab);
      const label = invalid.closest(".field")?.querySelector(":scope > span")?.textContent;
      $("settings-error").textContent = label ? `${label}: ${invalid.validationMessage}` : invalid.validationMessage;
      invalid.focus();
      return;
    }
    const next: TerminalSettings = {
      font_family: chosen() || settings.font_family,
      font_size: Number(field("font_size").value) || settings.font_size,
      line_height: Number(field("line_height").value) || settings.line_height,
      ligatures: field("ligatures").checked,
      option_as_meta: $<HTMLSelectElement>("option-as-meta").value as OptionAsMeta,
      app_scroll: $<HTMLSelectElement>("app-scroll").value as AppScroll,
    };
    const check = $<HTMLSelectElement>("update-check").value;
    const updates: UpdateSettings = {
      check: check === "" ? null : check === "true",
      channel: ($<HTMLSelectElement>("update-channel").value || null) as Channel | null,
    };
    try {
      await invoke("save_settings", { settings: { terminal: next, theme: draft, sounds: soundDraft, updates } });
      const recheck = updates.check !== updateSettings.check || updates.channel !== updateSettings.channel;
      updateSettings = updates;
      if (recheck) checkForRelease().catch(console.error);
      settings = next;
      themeSettings = { ...draft };
      soundSettings = { ...soundDraft };
      saved = true;
      dialog.close();
      await applySettings();
    } catch (err) {
      $("settings-error").textContent = String(err);
    }
  };
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
  document.querySelectorAll(".slot").forEach((s) => {
    s.classList.toggle("focused", (s as HTMLElement).dataset.pane === pane);
  });
}

/// Panes in some layout. Output can trail a pane's removal, and a view made for it then would never be disposed.
let live = new Set<Id>();

function onWorkspaces(list: Workspace[]) {
  onStatusChanges(list);
  workspaces = list;
  live = new Set(list.flatMap((w) => w.tabs.flatMap((t) => panesOf(t.layout))));
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
  refreshGit(list.map((w) => w.cwd).filter((cwd) => !gitInfo.has(cwd))).catch(console.error);
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
  const target = pendingFocus;
  if (!target) return;
  for (const ws of workspaces) {
    const tab = ws.tabs.find((t) => panesOf(t.layout).includes(target));
    if (tab) {
      selected = ws.id;
      activeTab.set(ws.id, tab.id);
      focused = target;
      pendingFocus = null;
      return;
    }
  }
}

/// Looking at a workspace (selected, window focused) clears its finished-but-unseen mark.
const seenPending = new Set<Id>();
function markSelectedSeen() {
  for (const id of seenPending) if (!workspaces.some((w) => w.id === id && w.unseen)) seenPending.delete(id);
  const ws = selectedWorkspace();
  if (!ws?.unseen || !document.hasFocus() || seenPending.has(ws.id)) return;
  seenPending.add(ws.id);
  request({ op: "mark_seen", id: ws.id }).catch((err) => {
    seenPending.delete(ws.id);
    console.error(err);
  });
}

function render() {
  saveUi();
  markSelectedSeen();
  renderSidebar();
  renderTabs();
  renderPanes();
}

const displayName = (ws: Workspace) => ws.title ?? ws.auto_title ?? ws.name;

const COLLAPSED_KEY = "ftm.collapsed";
const collapsed = new Set<string>(
  (() => {
    try {
      return JSON.parse(localStorage.getItem(COLLAPSED_KEY) ?? "[]");
    } catch {
      return [];
    }
  })(),
);

function toggleSection(key: string) {
  if (!collapsed.delete(key)) collapsed.add(key);
  try {
    localStorage.setItem(COLLAPSED_KEY, JSON.stringify([...collapsed]));
  } catch {
    // Only the collapsed state is lost.
  }
  renderSidebar();
}

const SORT_KEY = "ftm.sectionSort";
/// Each section's sort, by section key; a section that is not listed has its `defaultSort`.
const sectionSort: Record<string, SortMode> = (() => {
  try {
    return parseSectionSort(localStorage.getItem(SORT_KEY));
  } catch {
    return {};
  }
})();
const sortOf = (key: string): SortMode => sectionSort[key] ?? defaultSort(key);

function setSectionSort(key: string, mode: SortMode) {
  if (mode === defaultSort(key)) delete sectionSort[key];
  else sectionSort[key] = mode;
  try {
    localStorage.setItem(SORT_KEY, JSON.stringify(sectionSort));
  } catch {
    // Only the sort is lost.
  }
  renderSidebar();
}

const SORT_LABELS: Record<SortMode, [string, string]> = {
  manual: ["⇅", "Manual (drag to rearrange)"],
  name: ["A–Z", "Name"],
  color: ["●", "Color"],
};

/// Section header's sort button: hidden until hovered while the section has its default sort.
function sortButton(key: string, modes: SortMode[]): HTMLButtonElement {
  const mode = sortOf(key);
  const button = document.createElement("button");
  button.className = "section-sort" + (mode === defaultSort(key) ? "" : " active");
  button.textContent = SORT_LABELS[mode][0];
  button.title = `Sort: ${SORT_LABELS[mode][1]}`;
  button.onclick = () => {
    const r = button.getBoundingClientRect();
    showMenu(
      r.left,
      r.bottom,
      modes.map((m) => [`${SORT_LABELS[m][1]}${m === mode ? " ✓" : ""}`, () => setSectionSort(key, m)]),
    );
  };
  return button;
}

const sortWorkspaces = (list: Workspace[], mode: SortMode) =>
  sortRows(list, mode, displayName, (w) => w.color, WORKSPACE_COLORS);

const SIDEBAR_WIDTH_KEY = "ftm.sidebarWidth";

function setSidebarWidth(width: number) {
  document.documentElement.style.setProperty("--sidebar-width", `${width}px`);
}

function saveSidebarWidth(width: number) {
  try {
    if (width === SIDEBAR_DEFAULT) localStorage.removeItem(SIDEBAR_WIDTH_KEY);
    else localStorage.setItem(SIDEBAR_WIDTH_KEY, String(width));
  } catch {
    // Only the width is lost; the next start uses the default.
  }
}

/// Drag the sidebar's right edge to resize it, double-click it to go back to the default. The panes refit through
/// their ResizeObserver.
function setupSidebarResize() {
  try {
    const saved = localStorage.getItem(SIDEBAR_WIDTH_KEY);
    if (saved) setSidebarWidth(clampSidebarWidth(Number(saved), window.innerWidth));
  } catch {
    // Keep the default.
  }
  const handle = $("sidebar-resize");
  let drag: { x: number; width: number; last: number } | null = null;
  handle.onpointerdown = (e) => {
    if (e.button !== 0) return;
    const width = $("sidebar").getBoundingClientRect().width;
    drag = { x: e.clientX, width, last: width };
    handle.setPointerCapture(e.pointerId);
    document.body.classList.add("resizing-sidebar");
    e.preventDefault();
  };
  handle.onpointermove = (e) => {
    if (!drag) return;
    drag.last = clampSidebarWidth(drag.width + e.clientX - drag.x, window.innerWidth);
    setSidebarWidth(drag.last);
  };
  const end = () => {
    if (!drag) return;
    saveSidebarWidth(drag.last);
    drag = null;
    document.body.classList.remove("resizing-sidebar");
  };
  handle.onpointerup = end;
  handle.onpointercancel = end;
  handle.ondblclick = () => {
    setSidebarWidth(SIDEBAR_DEFAULT);
    saveSidebarWidth(SIDEBAR_DEFAULT);
  };
}

/// Top to bottom: Pinned, Active Terminals, Active Agents, Folders. Children stay under their parent, wherever the
/// parent is, sorted the way the parent's section is.
function renderSidebar() {
  if (editing && workspaces.some((w) => w.id === editing)) return;
  editing = null;
  if (rowDrag?.moved) return;
  const ids = new Set(workspaces.map((w) => w.id));
  const top = workspaces.filter((w) => !w.parent || !ids.has(w.parent));
  const trees = (key: string, list: Workspace[]) =>
    sortWorkspaces(list, sortOf(key)).map((w) => workspaceTree(w, sortOf(key)));
  const folderList = sidebarFolders();
  const sections: [string, string, HTMLElement[], SortMode[]][] = [
    ["pinned", "Pinned", trees("pinned", top.filter((w) => w.pinned)), ["manual", "name", "color"]],
    [
      "terminals",
      "Active Terminals",
      trees("terminals", top.filter((w) => !w.pinned && w.kind !== "agent")),
      ["manual", "name", "color"],
    ],
    [
      "agents",
      "Active Agents",
      trees("agents", top.filter((w) => !w.pinned && w.kind === "agent")),
      ["manual", "name", "color"],
    ],
    [
      "folders",
      "Folders",
      sortRows(folderList, sortOf("folders") === "name" ? "name" : "manual", (f) => basename(f.path), () => null, []).map(
        (f) => folderRow(f, sortOf("folders") !== "name"),
      ),
      ["manual", "name"],
    ],
  ];
  $("sections").replaceChildren(
    ...sections
      .filter(([key, , rows]) => rows.length > 0 || key === "folders")
      .map(([key, title, rows, modes]) => {
        const section = document.createElement("section");
        const head = document.createElement("button");
        head.className = "section-head";
        head.textContent = `${collapsed.has(key) ? "▸" : "▾"} ${title}`;
        const count = document.createElement("span");
        count.className = "count";
        count.textContent = rows.length ? `${rows.length}` : "";
        head.append(count);
        head.onclick = () => toggleSection(key);
        const bar = document.createElement("div");
        bar.className = "section-bar";
        bar.append(head, sortButton(key, modes));
        section.append(bar);
        if (!collapsed.has(key)) {
          const ul = document.createElement("ul");
          ul.append(...rows);
          section.append(ul);
        }
        return section;
      }),
  );
}

function workspaceTree(ws: Workspace, sort: SortMode): HTMLLIElement {
  const li = document.createElement("li");
  li.append(workspaceRow(ws, sort === "manual"));
  const children = sortWorkspaces(
    workspaces.filter((w) => w.parent === ws.id),
    sort,
  );
  if (children.length) {
    const ul = document.createElement("ul");
    ul.append(...children.map((w) => workspaceTree(w, sort)));
    li.append(ul);
  }
  return li;
}

/// `draggable` while its section is in manual order.
function workspaceRow(ws: Workspace, draggable: boolean): HTMLElement {
  const row = document.createElement("div");
  row.className = "ws" + (ws.id === selected ? " selected" : "");
  row.dataset.id = ws.id;
  if (ws.color) {
    row.classList.add("colored");
    row.style.setProperty("--ws-color", workspaceColor(ws.color));
  }
  row.title = [displayName(ws) === ws.name ? ws.name : `${displayName(ws)} (${ws.name})`, ws.kind, ws.cwd].join("\n");
  const dot = document.createElement("span");
  Object.assign(dot, statusDot(ws));
  const text = document.createElement("div");
  text.className = "text";
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = displayName(ws);
  name.ondblclick = (e) => {
    e.stopPropagation();
    startRename(ws.id);
  };
  const sub = document.createElement("span");
  sub.className = "sub";
  sub.textContent = location(ws);
  text.append(name, sub);
  row.append(dot, text);
  const waiting = descendantsNeedingInput(ws.id);
  if (waiting > 0) {
    const badge = document.createElement("span");
    badge.className = "badge";
    badge.textContent = `${waiting}`;
    badge.title = `${waiting} agent${waiting === 1 ? "" : "s"} below need input`;
    row.append(badge);
  }
  const close = document.createElement("button");
  close.className = "close";
  close.textContent = "×";
  close.title = "Close workspace";
  close.onclick = (e) => {
    e.stopPropagation();
    closeWorkspace(ws.id);
  };
  row.append(close);
  if (draggable) {
    startRowDrag(
      row,
      ws.id,
      () => siblingRows(row),
      (target) => dropWorkspace(row, ws.id, target),
    );
  }
  row.onclick = () => {
    if (swallowRowClick) return;
    selected = ws.id;
    render();
  };
  row.oncontextmenu = (e) => {
    e.preventDefault();
    const items: [string, () => void][] = [["Rename", () => startRename(ws.id)]];
    if (!ws.parent || !workspaces.some((w) => w.id === ws.parent)) {
      items.push([ws.pinned ? "Unpin" : "Pin", () => request({ op: "pin", id: ws.id, pinned: !ws.pinned }).catch(console.error)]);
    }
    items.push(["Color…", () => showColorMenu(e.clientX, e.clientY, ws)]);
    items.push(...openInItems(ws.cwd), ["Close", () => closeWorkspace(ws.id)]);
    showMenu(e.clientX, e.clientY, items);
  };
  return row;
}

/// The CSS color for a workspace's `color`: a palette name's fixed color, or the hex as given.
const workspaceColor = (color: string) => WORKSPACE_PALETTE[color] ?? color;

function setColor(ws: Workspace, color: string | null) {
  request({ op: "set_color", id: ws.id, color }).catch((err) => {
    console.error(err);
    notice("color", `Could not color ${displayName(ws)}: ${err}`, "error");
  });
}

/// The palette, plus None; `ftm color '#rrggbb'` sets any other color.
function showColorMenu(x: number, y: number, ws: Workspace) {
  showMenu(x, y, [
    ...WORKSPACE_COLORS.map((c): MenuItem => [
      c[0].toUpperCase() + c.slice(1) + (ws.color === c ? " ✓" : ""),
      () => setColor(ws, c),
      workspaceColor(c),
    ]),
    ...(ws.color?.startsWith("#") ? [[`${ws.color} ✓`, () => {}, ws.color] as MenuItem] : []),
    [ws.color ? "None" : "None ✓", () => setColor(ws, null)],
  ]);
}

interface OpenTarget {
  id: string;
  name: string;
  label: string;
}

/// Installed apps a folder can be opened in, detected once by the backend.
let openTargets: OpenTarget[] = [];
invoke<OpenTarget[]>("open_targets")
  .then((list) => {
    openTargets = list;
    renderTabs();
  })
  .catch(console.error);

function openIn(target: string, path: string) {
  invoke("open_in", { target, path })
    .then(() => notice("open-in", null))
    .catch((err) => {
      console.error(err);
      notice("open-in", `Could not open ${tilde(path)}: ${err}`, "error");
    });
}

const openInItems = (path: string): [string, () => void][] => openTargets.map((t) => [t.label, () => openIn(t.id, path)]);

function closeWorkspace(id: Id) {
  request({ op: "close", id, remove_worktree: false }).catch(console.error);
}

function sidebarFolders(): Folder[] {
  const byName = (a: Folder, b: Folder) => basename(a.path).localeCompare(basename(b.path));
  const byPath = new Map(folders.map((f) => [f.path, f]));
  const ranked = folderOrder.flatMap((p) => byPath.get(p) ?? []);
  const listed = new Set(folderOrder);
  return [...ranked, ...folders.filter((f) => !listed.has(f.path)).sort(byName)];
}

/// A sidebar row being dragged to a new place among its siblings: a folder in the Folders section, or a workspace
/// among the rows under the same parent in its section. Pointer events rather than HTML5 drag and drop, which Tauri's
/// native file-drop handler intercepts.
let rowDrag: { key: string; x: number; y: number; moved: boolean; target: number } | null = null;
/// Set between the end of a drag and the click that follows it, so dropping a row does not also click it.
let swallowRowClick = false;

/// Index among `rows` the dragged one would be dropped before, and the indicator shown there.
function markDrop(rows: HTMLElement[], y: number): number {
  let target = rows.findIndex((r) => {
    const box = r.getBoundingClientRect();
    return y < box.top + box.height / 2;
  });
  if (target < 0) target = rows.length;
  rows.forEach((r, i) => {
    r.classList.toggle("drop-before", i === target);
    r.classList.toggle("drop-after", target === rows.length && i === rows.length - 1);
  });
  return target;
}

/// Lets `row` be dragged among `siblings()` (which include it); a drop calls `drop` with the index among them to put
/// it before.
function startRowDrag(row: HTMLElement, key: string, siblings: () => HTMLElement[], drop: (target: number) => void) {
  row.onpointerdown = (e) => {
    if (e.button !== 0 || (e.target as HTMLElement).closest("button, input")) return;
    const drag = { key, x: e.clientX, y: e.clientY, moved: false, target: -1 };
    rowDrag = drag;
    // Until the drag starts nothing is captured, so a release elsewhere never reaches the row.
    const abandon = () => {
      window.removeEventListener("pointerup", abandon);
      window.removeEventListener("pointercancel", abandon);
      if (rowDrag === drag && !drag.moved) rowDrag = null;
    };
    window.addEventListener("pointerup", abandon);
    window.addEventListener("pointercancel", abandon);
  };
  row.onpointermove = (e) => {
    if (!rowDrag || rowDrag.key !== key) return;
    if (!rowDrag.moved) {
      if (e.buttons === 0) {
        rowDrag = null;
        return;
      }
      if (Math.hypot(e.clientX - rowDrag.x, e.clientY - rowDrag.y) < 4) return;
      // Captured only once it is a drag, so clicks and double-clicks (rename) still reach what was under the pointer.
      rowDrag.moved = true;
      row.setPointerCapture(e.pointerId);
      row.classList.add("dragging");
    }
    rowDrag.target = markDrop(siblings(), e.clientY);
  };
  const end = (dropped: boolean) => {
    const drag = rowDrag;
    if (!drag || drag.key !== key) return;
    rowDrag = null;
    if (!drag.moved) return;
    if (dropped) drop(drag.target);
    swallowRowClick = true;
    setTimeout(() => {
      swallowRowClick = false;
      renderSidebar();
    });
  };
  row.onpointerup = () => end(true);
  row.onpointercancel = () => end(false);
}

/// The workspace rows directly under the same list as `row`: its siblings in the sidebar.
function siblingRows(row: HTMLElement): HTMLElement[] {
  const ul = row.closest("ul");
  return ul ? [...ul.querySelectorAll<HTMLElement>(":scope > li > .ws")] : [row];
}

function dropWorkspace(row: HTMLElement, id: Id, target: number) {
  // The rows were not re-rendered during the drag, so they can list workspaces that have closed since.
  const group = moveItem(
    siblingRows(row).map((r) => r.dataset.id ?? ""),
    id,
    target,
  );
  const current = workspaces.map((w) => w.id);
  const order = reorderWithin(current, group);
  if (sameOrder(order, current)) return;
  const byId = new Map(workspaces.map((w) => [w.id, w]));
  const before = workspaces;
  const reordered = order.flatMap((i) => byId.get(i) ?? []);
  workspaces = reordered;
  request({ op: "reorder_workspaces", ids: order }).catch((err) => {
    console.error(err);
    notice("reorder", `Could not save the new order: ${err}`, "error");
    // Undone unless a newer tree from ftmd has replaced it meanwhile.
    if (workspaces !== reordered) return;
    workspaces = before;
    renderSidebar();
  });
}

/// `draggable` while the Folders section is in manual order.
function folderRow(folder: Folder, draggable: boolean): HTMLLIElement {
  const li = document.createElement("li");
  const row = document.createElement("div");
  row.className = "ws folder";
  if (draggable) startRowDrag(
    row,
    folder.path,
    () => [...document.querySelectorAll<HTMLElement>("#sections .ws.folder")],
    (target) => {
      const before = folderOrder;
      const current = sidebarFolders().map((f) => f.path);
      const order = moveItem(current, folder.path, target);
      if (sameOrder(order, current)) return;
      folderOrder = order;
      request({ op: "reorder_folders", paths: order }).catch((err) => {
        console.error(err);
        if (folderOrder !== order) return;
        folderOrder = before;
        renderSidebar();
      });
    },
  );
  row.title = folder.path;
  const icon = document.createElement("span");
  icon.className = "folder-icon";
  icon.textContent = "▢";
  const text = document.createElement("div");
  text.className = "text";
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = basename(folder.path);
  const sub = document.createElement("span");
  sub.className = "sub";
  const git = gitInfo.get(folder.path);
  sub.textContent = git ? `${git.branch === "HEAD" ? "detached" : git.branch}` : tilde(folder.path);
  text.append(name, sub);
  const add = document.createElement("button");
  add.className = "close";
  add.textContent = "+";
  add.title = "New workspace here";
  const menu = (x: number, y: number) => {
    const items: [string, () => void][] = [
      ["Agent", () => spawnAt("agent", folder.path)],
      ["Terminal", () => spawnAt("terminal", folder.path)],
    ];
    if (git) items.push(["Worktree…", () => openSpawn({ folder: folder.path, worktree: true })]);
    showMenu(x, y, items);
  };
  add.onclick = (e) => {
    e.stopPropagation();
    const r = add.getBoundingClientRect();
    menu(r.left, r.bottom);
  };
  row.onclick = (e) => {
    if (swallowRowClick) return;
    menu(e.clientX, e.clientY);
  };
  row.oncontextmenu = (e) => {
    e.preventDefault();
    showMenu(e.clientX, e.clientY, [
      ...openInItems(folder.path),
      ["Remove from list", () => request({ op: "remove_folder", path: folder.path }).catch(console.error)],
    ]);
  };
  row.append(icon, text, add);
  li.append(row);
  return li;
}

async function spawnAt(kind: "agent" | "terminal", path: string) {
  try {
    const r = await request({
      op: "spawn",
      kind,
      name: null,
      parent: null,
      cwd: path,
      worktree_branch: null,
      base: null,
      copy_changes: false,
      brief: null,
    });
    selected = r.workspace.id;
    render();
  } catch (err) {
    console.error(err);
  }
}

let home = "";
homeDir().then((h) => (home = h.replace(/\/$/, ""))).catch(() => {});

const tilde = (path: string) => (home && (path === home || path.startsWith(home + "/")) ? "~" + path.slice(home.length) : path);
const basename = (path: string) => path.replace(/\/$/, "").split("/").pop() || path;

/// Git root and branch per folder, refreshed when the window gains focus and every few seconds; `null` means not a repo.
type GitInfo = { root: string; repo: string; branch: string };
const gitInfo = new Map<string, GitInfo | null>();

async function refreshGit(paths: Iterable<string>) {
  let changed = false;
  await Promise.all(
    [...new Set(paths)].map(async (path) => {
      const info = await invoke<GitInfo | null>("git_info", { path }).catch(() => null);
      if (JSON.stringify(info) !== JSON.stringify(gitInfo.get(path))) {
        gitInfo.set(path, info);
        changed = true;
      }
    }),
  );
  if (changed) renderSidebar();
}

const refreshAllGit = () => refreshGit([...workspaces.map((w) => w.cwd), ...folders.map((f) => f.path)]);

/// Second sidebar line: `repo · branch` in a git repo (`repo/sub · branch` below its root), else the folder.
function location(ws: Workspace): string {
  const git = gitInfo.get(ws.cwd);
  if (ws.worktree) return `${git?.repo ?? basename(ws.worktree.repo)} · ${git?.branch ?? ws.worktree.branch}`;
  if (!git) return tilde(ws.cwd);
  const below = ws.cwd.startsWith(git.root + "/") ? ws.cwd.slice(git.root.length) : "";
  const branch = git.branch === "HEAD" ? "detached" : git.branch;
  return `${git.repo}${below} · ${branch}`;
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
    if (e.key === "Enter") save().catch(console.error);
    else if (e.key === "Escape" && !done) finish();
    else input.classList.remove("invalid");
  };
  input.onblur = save;
  for (const ev of ["click", "dblclick", "mousedown", "contextmenu"] as const) input.addEventListener(ev, (e) => e.stopPropagation());
  label.replaceWith(input);
  input.focus();
  input.select();
}

/// A menu entry: its label, what it does, and optionally a color swatch shown before the label.
type MenuItem = [string, () => void, string?];

function showMenu(x: number, y: number, items: MenuItem[]) {
  closeMenu();
  const menu = document.createElement("div");
  menu.id = "menu";
  for (const [label, action, swatch] of items) {
    const item = document.createElement("button");
    item.textContent = label;
    if (swatch) {
      const chip = document.createElement("span");
      chip.className = "swatch";
      chip.style.background = swatch;
      item.prepend(chip);
    }
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
    .reduce((n, w) => n + (w.status === "needs_input" ? 1 : 0) + descendantsNeedingInput(w.id), 0);
}

const statusSounds = new StatusSounds<Workspace>();
const heldFinish = new Set<Id>();
let heldTimer: ReturnType<typeof setTimeout> | undefined;

/// How long `wait_for_subagents` holds the finished sound: two subagents finishing together can leave the second
/// hand-back queued behind a turn that ended with none running.
const FINISH_HOLD_MS = 1500;

const finishCounts = (w: Workspace) => w.id !== selected || !document.hasFocus();

/// Plays the sound a new tree calls for (see `StatusSounds`); needs_input also sends a desktop notification.
function onStatusChanges(list: Workspace[]) {
  const decision = statusSounds.update(list, soundSettings.wait_for_subagents, finishCounts);
  if (!decision) return;
  if ("hold" in decision) {
    for (const w of decision.hold) heldFinish.add(w.id);
    clearTimeout(heldTimer);
    heldTimer = setTimeout(() => {
      if (workspaces.some((w) => heldFinish.has(w.id) && stillFinished(w, finishCounts))) playSound(soundSettings.completed);
      heldFinish.clear();
    }, FINISH_HOLD_MS);
  } else if (decision.sound === "needs_input") {
    playSound(soundSettings.needs_input);
    notifyNeedsInput(decision.waiting).catch(console.error);
  } else {
    playSound(soundSettings.completed);
  }
}

async function notifyNeedsInput(fresh: Workspace[]) {
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
  const control = openInControl(ws.cwd);
  if (control) nav.append(control);
}

const OPEN_ICON =
  '<svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true"><path d="M9 2.5h4.5V7M13.5 2.5 7.5 8.5M12 9.5v3a1 1 0 0 1-1 1H3.5a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1h3" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/></svg>';

/// The main click opens VS Code (or the first installed target without it); the chevron lists every target.
function openInControl(path: string): HTMLElement | null {
  const main = openTargets.find((t) => t.id === "vscode") ?? openTargets[0];
  if (!main) return null;
  const group = document.createElement("div");
  group.className = "open-in";
  const button = document.createElement("button");
  button.innerHTML = OPEN_ICON;
  button.append(main.name);
  button.title = `Open ${tilde(path)} in ${main.name}`;
  button.onclick = () => openIn(main.id, path);
  const more = document.createElement("button");
  more.className = "chevron";
  more.textContent = "▾";
  more.title = "Open in…";
  more.onclick = () => {
    const r = group.getBoundingClientRect();
    showMenu(r.left, r.bottom + 2, openInItems(path));
  };
  group.append(button, more);
  return group;
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
    if (!editing && focused) views.get(focused)?.term.focus();
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

/**
 * A dismissable message in the corner; showing one under a key that is already up replaces it, `null` removes it.
 * Each action is a button that also closes the notice; `onClose` runs when × closes it.
 */
function notice(
  key: string,
  message: string | null,
  kind: "warning" | "error" | "info" = "warning",
  actions: [string, () => void][] = [],
  onClose?: () => void,
) {
  const box = $("notices");
  box.querySelector(`[data-key="${key}"]`)?.remove();
  if (message === null) return;
  const el = document.createElement("div");
  el.className = `notice ${kind}`;
  el.dataset.key = key;
  const body = document.createElement("div");
  body.className = "notice-body";
  const text = document.createElement("span");
  text.textContent = message;
  body.append(text);
  if (actions.length) {
    const row = document.createElement("div");
    row.className = "notice-actions";
    for (const [label, run] of actions) {
      const button = document.createElement("button");
      button.textContent = label;
      button.onclick = () => {
        el.remove();
        run();
      };
      row.append(button);
    }
    body.append(row);
  }
  const close = document.createElement("button");
  close.className = "notice-close";
  close.textContent = "×";
  close.title = "Dismiss";
  close.onclick = () => {
    el.remove();
    onClose?.();
  };
  el.append(body, close);
  box.append(el);
}

function paneFailed(err: unknown) {
  console.error(err);
  notice("pane", `Could not open a pane: ${err}`, "error");
}

async function newTab() {
  if (!selected) return;
  const r = await request({ op: "new_tab", workspace: selected }).catch(paneFailed);
  if (r?.pane) focusWhenShown(r.pane);
}

async function split(dir: "row" | "column") {
  const pane = targetPane();
  if (!pane) return newTab();
  const r = await request({ op: "split", pane, dir }).catch(paneFailed);
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
  if (editing || document.querySelector("dialog[open]")) return;
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
  if (!e.repeat) Promise.resolve(action()).catch(console.error);
}

let openSpawn: (preset?: { folder?: string; worktree?: boolean }) => void = () => {};

function setupSpawnDialog() {
  const dialog = $<HTMLDialogElement>("spawn");
  const form = $<HTMLFormElement>("spawn-form");
  const error = $("spawn-error");
  const create = $<HTMLButtonElement>("create");
  let folder: string | null = null;

  const choose = async (dir: string) => {
    folder = dir;
    $("folder").textContent = tilde(dir);
    $("folder").title = dir;
    const root = await invoke<string | null>("git_root", { path: dir });
    $("git").hidden = !root;
  };

  /// One-click picks from the folders ftmd remembers, most recently used first.
  const showRecent = () => {
    $("recent").replaceChildren(
      ...folders.slice(0, 10).map((f) => {
        const b = document.createElement("button");
        b.type = "button";
        b.className = "recent" + (f.path === folder ? " chosen" : "");
        b.textContent = tilde(f.path);
        b.title = f.path;
        b.onclick = () => choose(f.path).then(showRecent).catch(console.error);
        return b;
      }),
    );
  };

  openSpawn = (preset = {}) => {
    form.reset();
    folder = null;
    $("folder").textContent = "none";
    $("git").hidden = true;
    error.textContent = "";
    showRecent();
    if (preset.folder) {
      choose(preset.folder)
        .then(() => {
          showRecent();
          (form.elements.namedItem("worktree") as HTMLInputElement).checked = !!preset.worktree;
          if (preset.worktree) (form.elements.namedItem("branch") as HTMLInputElement).focus();
        })
        .catch(console.error);
    }
    dialog.showModal();
  };
  $("add").onclick = () => openSpawn();

  $("pick").onclick = async () => {
    try {
      const dir = await open({ directory: true, defaultPath: folder ?? undefined });
      if (typeof dir === "string") await choose(dir);
      showRecent();
    } catch (err) {
      console.error(err);
      error.textContent = String(err);
    }
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
window.addEventListener("focus", () => {
  markSelectedSeen();
  refreshAllGit().catch(console.error);
});
setInterval(() => refreshAllGit().catch(console.error), 10_000);
new ResizeObserver(() => requestAnimationFrame(fitVisible)).observe($("panes"));

/// The pane slot under a file drag. wry reports GTK widget and AppKit view coordinates, which are CSS pixels despite
/// the event's `PhysicalPosition` type.
function dropSlot(x: number, y: number): HTMLElement | null {
  return document.elementFromPoint(x, y)?.closest<HTMLElement>(".slot[data-pane]") ?? null;
}

function markDropSlot(slot: HTMLElement | null) {
  document.querySelectorAll(".slot.drop-target").forEach((s) => {
    if (s !== slot) s.classList.remove("drop-target");
  });
  slot?.classList.add("drop-target");
}

getCurrentWebview()
  .onDragDropEvent(({ payload }) => {
    if (payload.type === "leave") return markDropSlot(null);
    const slot = dropSlot(payload.position.x, payload.position.y);
    if (payload.type !== "drop") return markDropSlot(slot);
    markDropSlot(null);
    const pane = slot?.dataset.pane;
    const v = pane && views.get(pane);
    if (!pane || !v) return;
    setFocus(pane);
    v.term.paste(dropText(payload.paths));
    v.term.focus();
  })
  .catch(console.error);
setupSidebarResize();
setupSpawnDialog();
setupSettingsDialog();
invoke<{ terminal: TerminalSettings; theme: ThemeSettings; sounds: SoundSettings; updates: Partial<UpdateSettings> }>(
  "get_settings",
)
  .then((loaded) => {
    updateSettings = { check: loaded.updates.check ?? null, channel: loaded.updates.channel ?? null };
    settings = loaded.terminal;
    themeSettings = loaded.theme;
    soundSettings = loaded.sounds;
    applyThemeNamed(wantedTheme(themeSettings)).catch(console.error);
    return applySettings();
  })
  .catch(console.error);
render();

const events = listen<FtmEvent>("ftm-event", ({ payload }) => {
  if (payload.event === "workspaces") {
    onWorkspaces(payload.workspaces);
  } else if (payload.event === "folders") {
    folders = payload.folders;
    folderOrder = payload.order ?? [];
    refreshGit(folders.map((f) => f.path).filter((p) => !gitInfo.has(p))).catch(console.error);
    renderSidebar();
  } else if (payload.event === "update_blocked") {
    const build = payload.version ? `FTM ${payload.version}` : "A new FTM build";
    notice(
      "update",
      `${build} is installed, but ftmd cannot switch to it without closing your sessions (${payload.reason}). Restarting ftmd finishes the update and closes every pane.`,
    );
  } else if (payload.event === "output" && live.has(payload.pane)) {
    const v = view(payload.pane);
    if (payload.reset) {
      v.term.reset();
      v.kitty.reset();
      v.wheel.reset();
      v.replaying++;
      v.term.write(decode(payload.data), () => v.replaying--);
    } else {
      v.term.write(decode(payload.data));
    }
  }
});

/// Whether the daemon, as of the last (re)connect, is at least as new as the app; an older one may not know how to
/// run a command.
let daemonCurrent = false;

// Each (re)connect of the event stream reports the daemon's version, then replays any update_blocked.
const daemon = listen<{ version: string; app_version: string }>("ftm-daemon", ({ payload }) => {
  daemonCurrent = compareVersions(payload.version, payload.app_version) >= 0;
  notice("subscribe", null);
  notice("update", null);
  notice("version", versionNotice(payload.version, payload.app_version));
});
// The stream keeps retrying in the background; without it the sidebar stays empty, so say why.
const daemonError = listen<string>("ftm-daemon-error", ({ payload }) => {
  console.error("ftmd event stream", payload);
  notice("subscribe", `Could not connect to ftmd: ${payload}\n\nRetrying…`, "error");
});
Promise.all([events, daemon, daemonError])
  .then(() => invoke("subscribe"))
  .catch((err) => console.error("subscribing to ftmd events", err));

interface Release {
  version: string;
  channel: Channel;
  switch: boolean;
  script: string | null;
}

const releaseName = (r: Release) => `${r.channel === "nightly" ? "FTM Nightly" : "FTM"} ${r.version}`;

const DISMISSED_RELEASE = "ftm.dismissedRelease";

function dismissedRelease(): string | null {
  try {
    return localStorage.getItem(DISMISSED_RELEASE);
  } catch {
    return null;
  }
}

async function checkForRelease() {
  const release = await invoke<Release | null>("check_update");
  if (!release) {
    notice("release", null);
    return;
  }
  if (release.version === dismissedRelease()) return;
  const dismiss = () => {
    try {
      localStorage.setItem(DISMISSED_RELEASE, release.version);
    } catch {}
  };
  const name = releaseName(release);
  const fresh = release.switch
    ? " Switching channels starts a fresh daemon: open panes and sessions stay with this one and don't carry over."
    : "";
  if (!release.script) {
    const how = release.switch ? "Install it" : "Upgrade it";
    notice("release", `${name} is available. ${how} with your package manager.${fresh}`, "info", [], dismiss);
    return;
  }
  notice(
    "release",
    release.switch ? `Switch to ${name}?${fresh}` : `${name} is available.`,
    "info",
    [
      [release.switch ? "Switch" : "Update", () => installRelease(release, release.script as string)],
      ["Dismiss", dismiss],
    ],
    dismiss,
  );
}

/// Runs the install in a pane of its own, where makepkg shows its progress and sudo can ask for the password.
async function installRelease(release: Release, script: string) {
  // An ftmd older than this app ignores the command and would open a bare shell.
  if (!daemonCurrent) {
    notice("release", `To install ${releaseName(release)}, run this in a terminal:\n\n${script}`, "info");
    return;
  }
  try {
    const r = await request({
      op: "spawn",
      kind: "terminal",
      name: `${releaseName(release)} ${release.switch ? "switch" : "update"}`,
      parent: null,
      cwd: home || "/",
      worktree_branch: null,
      base: null,
      copy_changes: false,
      brief: null,
      command: script,
    });
    selected = r.workspace.id;
    render();
  } catch (err) {
    console.error(err);
    notice("release", `Could not start installing ${releaseName(release)}: ${err}`, "error");
  }
}

setTimeout(() => checkForRelease().catch(console.error), 10_000);
setInterval(() => checkForRelease().catch(console.error), 6 * 60 * 60 * 1000);
