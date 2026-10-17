// Theme color math, kept free of DOM use so it can be checked outside the app.

export function rgb(hex: string): [number, number, number] {
  const n = parseInt(hex.slice(1), 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

export function mix(a: string, b: string, t: number): string {
  const [x, y] = [rgb(a), rgb(b)];
  return "#" + x.map((v, i) => Math.round(v + (y[i] - v) * t).toString(16).padStart(2, "0")).join("");
}

export function luminance(hex: string): number {
  const [r, g, b] = rgb(hex).map((v) => {
    const c = v / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

export const contrast = (a: string, b: string) => {
  const [l1, l2] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (l1 + 0.05) / (l2 + 0.05);
};

/// Moves `color` toward `toward` until it reads against `bg`, for status dots on themes with dim palettes.
export function readable(color: string, bg: string, toward: string, min = 2.5): string {
  for (let t = 0; t <= 1; t += 0.1) {
    const c = mix(color, toward, t);
    if (contrast(c, bg) >= min) return c;
  }
  return toward;
}

/// The named colors a workspace's sidebar row can take, as ftmd accepts them, in sort order. Fixed rather than taken
/// from the theme: a theme's blue, magenta and their mixes can come out nearly the same, and each name should look like
/// itself on every theme.
export const WORKSPACE_PALETTE: Record<string, string> = {
  red: "#ff4245",
  orange: "#ff8c00",
  yellow: "#ffd600",
  green: "#30d158",
  cyan: "#40c8e0",
  blue: "#0091ff",
  purple: "#bf5af2",
  pink: "#ff6482",
  gray: "#98989d",
};
export const WORKSPACE_COLORS = Object.keys(WORKSPACE_PALETTE);

/// The app chrome's CSS variables for a theme: sidebar a little darker than the terminal, muted text blended from the
/// foreground, accent from blue (palette 4), status dots from green/red/grey/blue, plus yellow for a finished turn not yet
/// looked at, each nudged until readable.
export function chromeColors(bg: string, fg: string, p: string[]): { vars: Record<string, string>; dark: boolean } {
  const dark = luminance(bg) < 0.4;
  const sidebar = mix(bg, "#000000", dark ? 0.18 : 0.05);
  const accent = readable(p[4], sidebar, fg, 2);
  const vars: Record<string, string> = {
    "--bg": bg,
    "--sidebar": sidebar,
    "--surface": mix(bg, fg, dark ? 0.06 : 0.03),
    "--hover": mix(sidebar, fg, 0.07),
    "--selected": mix(sidebar, accent, dark ? 0.28 : 0.2),
    "--border": mix(bg, fg, 0.12),
    "--border-strong": mix(bg, fg, 0.22),
    "--text": mix(fg, bg, 0.1),
    "--text-strong": fg,
    "--muted": readable(mix(fg, bg, 0.42), sidebar, fg, 3),
    "--accent": accent,
    "--danger": readable(p[1], bg, fg),
    "--warning": readable(p[3], bg, fg),
    "--status-starting": readable(p[4], sidebar, fg),
    "--status-working": readable(p[2], sidebar, fg),
    "--status-needs-input": readable(p[1], sidebar, fg),
    "--status-idle": readable(p[8], sidebar, fg, 2),
    "--status-done": readable(p[8], sidebar, fg, 2),
    "--status-unseen": readable(p[3], sidebar, fg),
  };
  return { vars, dark };
}
