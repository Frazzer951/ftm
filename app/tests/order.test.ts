import assert from "node:assert/strict";
import { test } from "node:test";
import { moveItem, reorderWithin } from "../src/order.ts";

test("a dragged row lands before the row it was dropped on", () => {
  const rows = ["a", "b", "c", "d"];
  assert.deepEqual(moveItem(rows, "a", 3), ["b", "c", "a", "d"]);
  assert.deepEqual(moveItem(rows, "d", 0), ["d", "a", "b", "c"]);
  assert.deepEqual(moveItem(rows, "b", 4), ["a", "c", "d", "b"], "past the last row");
  assert.deepEqual(moveItem(rows, "b", 1), rows, "onto itself");
  assert.deepEqual(moveItem(rows, "b", 2), rows, "just below itself");
  assert.deepEqual(moveItem(rows, "x", 0), rows);
});

test("reordering one section's rows leaves the others where they were", () => {
  // Pinned p1, p2 sit between the terminals t1, t2, t3 in the full order.
  const all = ["t1", "p1", "t2", "p2", "t3"];
  assert.deepEqual(reorderWithin(all, ["t3", "t1", "t2"]), ["t3", "p1", "t1", "p2", "t2"]);
  assert.deepEqual(reorderWithin(all, ["p2", "p1"]), ["t1", "p2", "t2", "p1", "t3"]);
});
