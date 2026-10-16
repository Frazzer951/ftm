/// Orders two semver versions, including prereleases such as nightlies' `0.1.9-nightly.20261016.42`: negative when
/// `a` is older.
export function compareVersions(a: string, b: string): number {
  const [coreA, preA] = split(a);
  const [coreB, preB] = split(b);
  const core = compareIdentifiers(coreA, coreB);
  if (core !== 0) return core;
  if (preA.length === 0 || preB.length === 0) return preB.length - preA.length;
  return compareIdentifiers(preA, preB);
}

function split(version: string): [string[], string[]] {
  const plain = version.split("+")[0];
  const dash = plain.indexOf("-");
  if (dash < 0) return [plain.split("."), []];
  return [plain.slice(0, dash).split("."), plain.slice(dash + 1).split(".")];
}

function compareIdentifiers(a: string[], b: string[]): number {
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    if (a[i] === undefined || b[i] === undefined) return a[i] === undefined ? -1 : 1;
    const numeric = /^\d+$/.test(a[i]) && /^\d+$/.test(b[i]);
    const order = numeric ? Number(a[i]) - Number(b[i]) : a[i] < b[i] ? -1 : a[i] > b[i] ? 1 : 0;
    if (order !== 0) return Math.sign(order);
  }
  return 0;
}

/// What to tell the user when the app and the daemon it talks to are different builds, or null when they match.
export function versionNotice(daemon: string, app: string): string | null {
  const order = compareVersions(daemon, app);
  if (order === 0) return null;
  if (order > 0) {
    return `FTM was updated (ftmd ${daemon}, app ${app}). Relaunch FTM to finish; your sessions stay open.`;
  }
  return `ftmd is still running ${daemon}, older than this app (${app}), so some features may not work until it moves to the new build.`;
}
