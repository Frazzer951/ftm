import assert from "node:assert/strict";
import { test } from "node:test";
import { compareVersions, versionNotice } from "../src/version.ts";

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
