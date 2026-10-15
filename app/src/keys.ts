import type { KeyEvent, KeyProtocol } from "./kitty.ts";

/// iTerm2's "Natural Text Editing": what shells' emacs keymaps understand, unlike xterm's `\e[1;3D` and friends.
const textEditKeys: Record<string, { alt?: string; meta?: string }> = {
  ArrowLeft: { alt: "\x1bb", meta: "\x01" },
  ArrowRight: { alt: "\x1bf", meta: "\x05" },
  Backspace: { alt: "\x17", meta: "\x15" },
  Delete: { alt: "\x1bd" },
};

export interface KeyContext {
  mac: boolean;
  /// Whether the Option held for this event is Meta rather than a key that composes characters (macOS only).
  altIsMeta: boolean;
  kitty: Pick<KeyProtocol, "flags" | "encode">;
}

type PaneKeyEvent = KeyEvent & Pick<KeyboardEvent, "type">;

const only = (e: PaneKeyEvent, mod: "shiftKey" | "altKey" | "ctrlKey" | "metaKey") =>
  e[mod] && ["shiftKey", "altKey", "ctrlKey", "metaKey"].every((m) => m === mod || !e[m as typeof mod]);

/// Linux terminals' copy and paste keys; macOS has Cmd+C/V in its Edit menu.
export function clipboardKey(e: PaneKeyEvent, mac: boolean): "copy" | "paste" | undefined {
  if (mac || e.type !== "keydown") return;
  if (e.ctrlKey && e.shiftKey && !e.altKey && !e.metaKey) {
    const key = e.key.toLowerCase();
    return key === "c" ? "copy" : key === "v" ? "paste" : undefined;
  }
  if (e.key === "Insert" && only(e, "shiftKey")) return "paste";
}

/// The bytes to send instead of xterm's own encoding of a key, if any.
export function paneKey(e: PaneKeyEvent, ctx: KeyContext): string | undefined {
  if (e.type !== "keydown" || e.isComposing) return;
  if (ctx.mac && (only(e, "altKey") || only(e, "metaKey"))) {
    const seq = textEditKeys[e.key]?.[e.altKey ? "alt" : "meta"];
    if (seq !== undefined) return seq;
  }
  // Cmd belongs to the app and the Edit menu on macOS, so it is never reported as Super.
  if (ctx.kitty.flags) return ctx.mac && e.metaKey ? undefined : ctx.kitty.encode(e, ctx.altIsMeta);
  // What Claude Code's /terminal-setup makes iTerm2 and VS Code send; Alt+Enter already sends it too.
  if (e.key === "Enter" && only(e, "shiftKey")) return "\x1b\r";
}
