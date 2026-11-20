import assert from "node:assert/strict";
import { test } from "node:test";
import { checkStatus, compareVersions, versionNotice } from "../src/version.ts";

test("versions order by number, prereleases before their release", () => {
  assert.equal(compareVersions("0.1.8", "0.1.8"), 0);
  assert.ok(compareVersions("0.1.8", "0.1.10") < 0);
  assert.ok(compareVersions("0.2.0", "0.1.10") > 0);
  assert.ok(compareVersions("0.1.9-nightly.20261016.42", "0.1.9") < 0);
  assert.ok(compareVersions("0.1.9-nightly.20261016.42", "0.1.8") > 0);
  assert.ok(compareVersions("0.1.9-nightly.20261016.9", "0.1.9-nightly.20261016.42") < 0);
  assert.ok(compareVersions("0.1.9-nightly.20261017.1", "0.1.9-nightly.20261016.42") > 0);
});

test("a newer daemon asks for a relaunch; a newer app does not", () => {
  assert.equal(versionNotice("0.1.8", "0.1.8"), null);
  assert.match(versionNotice("0.1.9", "0.1.8") ?? "", /Relaunch FTM/);
  const older = versionNotice("0.1.7", "0.1.8") ?? "";
  assert.doesNotMatch(older, /Relaunch/);
  assert.match(older, /ftmd is still running 0\.1\.7/);
});

test("the update check status names the version, the result and when it ran", () => {
  const at = new Date(2026, 10, 20, 14, 5);
  const now = new Date(2026, 10, 20, 15, 0);
  const upToDate = checkStatus({ at, current: "0.4.1", update: null }, now);
  assert.match(upToDate, /^Up to date: this is 0\.4\.1\. Last checked /);
  assert.match(upToDate, /2:05|14:05/);
  assert.doesNotMatch(upToDate, /Nov/);
  const update = { version: "0.5.0", channel: "stable", switch: false };
  assert.match(checkStatus({ at, current: "0.4.1", update }, now), /^FTM 0\.5\.0 is available \(this is 0\.4\.1\)/);
  const nightly = { version: "0.5.0-nightly.20261120.1", channel: "nightly", switch: true };
  assert.match(checkStatus({ at, current: "0.4.1", update: nightly }, now), /^FTM Nightly 0\.5\.0-nightly\.20261120\.1 is available to switch to/);
  assert.match(checkStatus({ at, error: "curl: (6) Could not resolve host" }, now), /failed: curl: \(6\) Could not resolve host$/);
  assert.match(checkStatus({ at, current: "0.4.1", update: null }, new Date(2026, 10, 21, 9)), /Nov/);
});
