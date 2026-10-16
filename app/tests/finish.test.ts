import assert from "node:assert/strict";
import { test } from "node:test";
import { StatusSounds, stillFinished, type Tracked } from "../src/finish.ts";

const ws = (id: string, status: string, unseen = false, background_agents = 0): Tracked => ({
  id,
  kind: "agent",
  status,
  unseen,
  background_agents,
});
const always = () => true;

function started(...list: Tracked[]) {
  const sounds = new StatusSounds<Tracked>();
  assert.equal(sounds.update(list, true, always), null);
  return sounds;
}

test("the first tree only records state", () => {
  const sounds = new StatusSounds<Tracked>();
  assert.equal(sounds.update([ws("a", "needs_input"), ws("b", "idle", true)], false, always), null);
  assert.equal(sounds.update([ws("a", "needs_input"), ws("b", "idle", true)], false, always), null);
});

test("a finished turn plays at once, or is held while waiting for subagents", () => {
  const plain = new StatusSounds<Tracked>();
  plain.update([ws("a", "working")], false, always);
  assert.deepEqual(plain.update([ws("a", "idle", true)], false, always), { sound: "completed" });
  assert.equal(plain.update([ws("b", "idle", true)], false, () => false), null);

  const sounds = started(ws("a", "working"));
  const done = ws("a", "idle", true);
  assert.deepEqual(sounds.update([done], true, always), { hold: [done] });
});

test("a turn ending with subagents running is quiet; the last one handing back from idle is the finish", () => {
  const sounds = started(ws("a", "working"));
  assert.equal(sounds.update([ws("a", "idle", true, 1)], true, always), null);
  // The hand-back turn ran without a working phase: idle to idle, still unseen.
  const back = ws("a", "idle", true, 0);
  assert.deepEqual(sounds.update([back], true, always), { hold: [back] });
  // Nothing new after that.
  assert.equal(sounds.update([back], true, always), null);
});

test("a hand-back the user already saw does not play", () => {
  const sounds = started(ws("a", "idle", true, 1));
  assert.equal(sounds.update([ws("a", "idle", false, 1)], true, always), null);
  assert.equal(sounds.update([ws("a", "idle", false, 0)], true, always), null);
});

test("needs_input wins over a finish in the same tree", () => {
  const sounds = started(ws("a", "working"), ws("b", "working"));
  const asking = ws("a", "needs_input");
  assert.deepEqual(sounds.update([asking, ws("b", "idle", true)], true, always), {
    sound: "needs_input",
    waiting: [asking],
  });
  assert.equal(sounds.update([asking, ws("b", "idle", true)], true, always), null);
});

test("unknown statuses from a newer ftmd make no sound", () => {
  const sounds = started(ws("a", "working"));
  assert.equal(sounds.update([ws("a", "unknown")], true, always), null);
});

test("a held finish is cancelled by a hand-back starting another turn within the hold", () => {
  assert.equal(stillFinished(ws("a", "idle", true), always), true);
  assert.equal(stillFinished(ws("a", "working", true), always), false);
  assert.equal(stillFinished(ws("a", "idle", true, 1), always), false);
  assert.equal(stillFinished(ws("a", "idle", false), always), false);
  assert.equal(stillFinished(ws("a", "idle", true), () => false), false);
});
