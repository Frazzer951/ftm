/// What a clicked link may open; `file:`, `javascript:` and other apps' schemes are never handed to the OS.
const OPENABLE = new Set(["http:", "https:", "mailto:"]);

/// The URL to open for a link's text, if it is one FTM opens.
export function openableUrl(text: string): string | undefined {
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return undefined;
  }
  return OPENABLE.has(url.protocol) ? url.href : undefined;
}

type ClickModifiers = Pick<MouseEvent, "metaKey" | "ctrlKey">;

/// Cmd+click on macOS and Ctrl+click elsewhere, as in Ghostty and VS Code: a plain click still selects text or
/// reaches a TUI that tracks the mouse.
export function opensLink(e: ClickModifiers, mac: boolean): boolean {
  return mac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
}
