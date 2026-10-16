const SAFE = /^[A-Za-z0-9_@%+=:,./-]+$/;

/// `path` as one POSIX shell word: as is when nothing in it is special, otherwise single-quoted.
export function shellQuote(path: string): string {
  if (SAFE.test(path)) return path;
  return `'${path.replaceAll("'", `'\\''`)}'`;
}

/// What dropping `paths` on a terminal types: each path a shell word, with a space after the last.
export function dropText(paths: string[]): string {
  return paths.length ? `${paths.map(shellQuote).join(" ")} ` : "";
}
