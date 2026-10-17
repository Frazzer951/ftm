// Sidebar ordering, kept free of DOM use so it can be checked outside the app.

/// `items` with `item` moved to just before index `target` of `items` as they were (`items.length` for the end);
/// unchanged when `item` is missing.
export function moveItem<T>(items: T[], item: T, target: number): T[] {
  const from = items.indexOf(item);
  if (from < 0) return items;
  const out = items.filter((_, i) => i !== from);
  out.splice(target > from ? target - 1 : target, 0, item);
  return out;
}

/// `all` with the members of `group` put in `group`'s order, in the places they already held; everything else stays.
/// Members of `group` missing from `all` are ignored.
export function reorderWithin<T>(all: T[], group: T[]): T[] {
  const present = new Set(all);
  const kept = group.filter((x) => present.has(x));
  const members = new Set(kept);
  let next = 0;
  return all.map((x) => (members.has(x) ? kept[next++] : x));
}

export const sameOrder = <T>(a: T[], b: T[]): boolean => a.length === b.length && a.every((x, i) => x === b[i]);

/// How a sidebar section orders its rows: as dragged, by shown name, or by color.
export type SortMode = "manual" | "name" | "color";

/// A section's sort until the viewer picks one: Name for the workspace sections, Manual for Folders, whose dragged order
/// came first.
export const defaultSort = (section: string): SortMode => (section === "folders" ? "manual" : "name");

const SORT_MODES: readonly string[] = ["manual", "name", "color"] satisfies SortMode[];

/// The saved sorts in `raw` (JSON), keeping only known modes; anything else leaves a section on its default.
export function parseSectionSort(raw: string | null): Record<string, SortMode> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw ?? "{}");
  } catch {
    return {};
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return {};
  return Object.fromEntries(
    Object.entries(parsed).filter((e): e is [string, SortMode] => SORT_MODES.includes(e[1])),
  );
}

/// Palette colors in `palette` order, then other (hex) colors, then rows without one. Ties keep manual order.
function colorRank(color: string | null | undefined, palette: readonly string[]): [number, string] {
  if (!color) return [palette.length + 1, ""];
  const i = palette.indexOf(color);
  return i >= 0 ? [i, ""] : [palette.length, color];
}

/// `items` (in manual order) as `mode` shows them; the sort is stable, so equal rows keep their manual order.
export function sortRows<T>(
  items: T[],
  mode: SortMode,
  name: (item: T) => string,
  color: (item: T) => string | null | undefined,
  palette: readonly string[],
): T[] {
  if (mode === "name") {
    return [...items].sort((a, b) => name(a).localeCompare(name(b), undefined, { sensitivity: "base", numeric: true }));
  }
  if (mode === "color") {
    return [...items].sort((a, b) => {
      const [x, y] = [colorRank(color(a), palette), colorRank(color(b), palette)];
      return x[0] - y[0] || x[1].localeCompare(y[1]);
    });
  }
  return items;
}
