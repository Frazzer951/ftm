/// Files the viewer panel renders; anything else opens in VS Code.
export type ArtifactKind = "markdown" | "html" | "image" | "video";

const KINDS: Record<string, [ArtifactKind, string]> = {
  md: ["markdown", "text/markdown"],
  markdown: ["markdown", "text/markdown"],
  html: ["html", "text/html"],
  htm: ["html", "text/html"],
  png: ["image", "image/png"],
  jpg: ["image", "image/jpeg"],
  jpeg: ["image", "image/jpeg"],
  gif: ["image", "image/gif"],
  webp: ["image", "image/webp"],
  avif: ["image", "image/avif"],
  bmp: ["image", "image/bmp"],
  ico: ["image", "image/x-icon"],
  svg: ["image", "image/svg+xml"],
  mp4: ["video", "video/mp4"],
  m4v: ["video", "video/mp4"],
  mov: ["video", "video/quicktime"],
  webm: ["video", "video/webm"],
  ogv: ["video", "video/ogg"],
};

const extension = (path: string) => /\.([^./]+)$/.exec(path)?.[1]?.toLowerCase() ?? "";

export const artifactKind = (path: string): ArtifactKind | undefined => KINDS[extension(path)]?.[0];

export const mimeType = (path: string): string => KINDS[extension(path)]?.[1] ?? "application/octet-stream";

/// The local path a `file:` URL names. Only this machine's files: no host, or `localhost`.
export function fileUrlPath(text: string): string | undefined {
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return undefined;
  }
  if (url.protocol !== "file:" || (url.host !== "" && url.host !== "localhost")) return undefined;
  try {
    return decodeURIComponent(url.pathname);
  } catch {
    return undefined;
  }
}

/// `src/a.rs:12:3` → the path and line, as compilers and grep print them.
export function splitLine(text: string): { path: string; line?: number } {
  const m = /^(.*?):(\d+)(?::\d+)?$/.exec(text);
  return m ? { path: m[1], line: Number(m[2]) } : { path: text };
}

/// An absolute path for `path` as printed in a pane whose files live under `cwd`, with `.` and `..` folded.
export function resolvePath(path: string, cwd: string, home: string): string {
  const abs = path.startsWith("/")
    ? path
    : path === "~" || path.startsWith("~/")
      ? home + path.slice(1)
      : `${cwd.replace(/\/$/, "")}/${path}`;
  const parts: string[] = [];
  for (const part of abs.split("/")) {
    if (part === "" || part === ".") continue;
    if (part === "..") parts.pop();
    else parts.push(part);
  }
  return `/${parts.join("/")}`;
}

/// A path printed in a pane: something with a slash or a file extension, and an optional `:line[:col]`.
const PATH = /(?:~|\.{1,2})?\/?(?:[\w@%+=,.-]+\/)*[\w@%+=,-][\w@%+=,.-]*\.[A-Za-z][\w]{0,9}(?::\d+(?::\d+)?)?/g;

/// Path-like runs in one line of pane text, with their [start, end) offsets. A run inside a URL or glued to other
/// word characters is skipped; the URL is the web links addon's.
export function findPaths(line: string): { start: number; end: number; text: string }[] {
  const found: { start: number; end: number; text: string }[] = [];
  for (const m of line.matchAll(PATH)) {
    const start = m.index ?? 0;
    let text = m[0];
    // A sentence's full stop or comma is not part of the path.
    text = text.replace(/[.,]+$/, "");
    const end = start + text.length;
    const before = line.slice(0, start);
    if (/[\w:/@~-]$/.test(before) || /:\/\/\S*$/.test(before)) continue;
    if (/^[\w-]/.test(line.slice(end)) || !/\.[A-Za-z]/.test(text)) continue;
    found.push({ start, end, text });
  }
  return found;
}

/// For rendered markdown: only the viewer's own script (carrying `nonce`) runs, and only inlined images load.
export const markdownCsp = (nonce: string) =>
  `default-src 'none'; script-src 'nonce-${nonce}'; img-src data:; style-src 'unsafe-inline'; font-src data:`;

/// For an HTML artifact: its own inline scripts and styles run in an opaque-origin frame, and nothing is fetched, so
/// a page cannot report what it was shown.
export const HTML_CSP =
  "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; media-src data: blob:; font-src data:";

const cspMeta = (csp: string) => `<meta http-equiv="Content-Security-Policy" content="${csp}">`;

/// `html` with a CSP `<meta>` before anything but its doctype, so the parser puts it first in the head whatever the page
/// holds (a `<head>` inside a comment or script cannot move it). A page can add policies of its own, which only narrow it.
export function withCsp(html: string, csp: string, extra = ""): string {
  const doctype = /^\uFEFF?\s*<!doctype[^>]*>/i.exec(html);
  const at = doctype ? doctype[0].length : 0;
  return html.slice(0, at) + cspMeta(csp) + extra + html.slice(at);
}
