import assert from "node:assert/strict";
import { test } from "node:test";
import { defaultSort, moveItem, reorderWithin, sortRows } from "../src/order.ts";

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

test("sections sort by name or color, keeping manual order for ties", () => {
  const palette = ["red", "green", "blue"];
  const rows = [
    { name: "beta", color: "blue" },
    { name: "Alpha", color: null },
    { name: "gamma", color: "#123456" },
    { name: "ws10", color: "red" },
    { name: "ws9", color: "blue" },
  ];
  const sort = (mode: "manual" | "name" | "color") =>
    sortRows(rows, mode, (r) => r.name, (r) => r.color, palette).map((r) => r.name);
  assert.deepEqual(sort("manual"), ["beta", "Alpha", "gamma", "ws10", "ws9"]);
  assert.deepEqual(sort("name"), ["Alpha", "beta", "gamma", "ws9", "ws10"], "case-insensitive, numbers by value");
  assert.deepEqual(sort("color"), ["ws10", "beta", "ws9", "gamma", "Alpha"], "palette, then hex, then none");
});

test("workspace sections start sorted by name, Folders in its dragged order", () => {
  for (const section of ["pinned", "terminals", "agents"]) assert.equal(defaultSort(section), "name");
  assert.equal(defaultSort("folders"), "manual");
});
