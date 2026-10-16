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
export function reorderWithin<T>(all: T[], group: T[]): T[] {
  const members = new Set(group);
  let next = 0;
  return all.map((x) => (members.has(x) ? group[next++] : x));
}
