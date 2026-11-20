import assert from "node:assert/strict";
import { test } from "node:test";
import { replyKey, type Task, TaskAlerts, taskGroups } from "../src/tasks.ts";

const task = (id: number, workspace: string, extra: Partial<Task> = {}): Task => ({
  id,
  workspace,
  kind: "decision",
  title: `t${id}`,
  created_at: id,
  state: "open",
  ...extra,
});

test("open tasks group by agent in sidebar order, oldest first", () => {
  const tasks = [
    task(1, "b"),
    task(2, "a", { created_at: 9 }),
    task(3, "a", { created_at: 4 }),
    task(4, "a", { state: "answered" }),
    task(5, "gone"),
  ];
  const groups = taskGroups(tasks, [{ id: "a" }, { id: "b" }]);
  assert.deepEqual(
    groups.map((g) => [g.workspace, g.tasks.map((t) => t.id)]),
    [
      ["a", [3, 2]],
      ["b", [1]],
      ["gone", [5]],
    ],
  );
  assert.equal(groups[2].ws, undefined);
  assert.deepEqual(taskGroups([task(1, "a", { state: "dismissed" })], [{ id: "a" }]), []);
});

test("alerts fire for new open tasks only, not for the replay on subscribe", () => {
  const alerts = new TaskAlerts();
  assert.deepEqual(alerts.update([task(1, "a")]), []);
  assert.deepEqual(alerts.update([task(1, "a")]), [], "a reconnect's replay");
  assert.deepEqual(
    alerts.update([task(1, "a"), task(2, "a"), task(3, "a", { state: "resolved" })]).map((t) => t.id),
    [2],
  );
  assert.deepEqual(alerts.update([task(1, "a", { created_at: 99 })]).map((t) => t.id), [1], "ids reused by a fresh ftmd");
});

test("Enter sends a reply, Shift+Enter and composing do not", () => {
  assert.equal(replyKey({ key: "Enter", shiftKey: false, isComposing: false }), "send");
  assert.equal(replyKey({ key: "Enter", shiftKey: true, isComposing: false }), null);
  assert.equal(replyKey({ key: "Enter", shiftKey: false, isComposing: true }), null);
  assert.equal(replyKey({ key: "a", shiftKey: false, isComposing: false }), null);
});
