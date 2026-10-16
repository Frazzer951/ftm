import assert from "node:assert/strict";
import { test } from "node:test";
import { type PanelEl, type TabEl, Tabs, tabTarget } from "../src/tabs.ts";

test("arrows wrap, Home and End jump, other keys are not navigation", () => {
  const all = [true, true, true, true];
  assert.equal(tabTarget("ArrowRight", 0, all), 1);
  assert.equal(tabTarget("ArrowRight", 3, all), 0);
  assert.equal(tabTarget("ArrowLeft", 0, all), 3);
  assert.equal(tabTarget("Home", 2, all), 0);
  assert.equal(tabTarget("End", 1, all), 3);
  assert.equal(tabTarget("Enter", 1, all), null);
  assert.equal(tabTarget("ArrowDown", 1, all), null);
});

test("navigation skips hidden tabs", () => {
  const visible = [false, true, false, true];
  assert.equal(tabTarget("ArrowRight", 1, visible), 3);
  assert.equal(tabTarget("ArrowRight", 3, visible), 1);
  assert.equal(tabTarget("ArrowLeft", 1, visible), 3);
  assert.equal(tabTarget("Home", 3, visible), 1);
  assert.equal(tabTarget("End", 1, [true, true, false]), 1);
  assert.equal(tabTarget("ArrowRight", 0, [true, false]), 0, "a lone tab stays put");
});

function fake(ids: string[]) {
  const focused: string[] = [];
  const tabs = ids.map((id) => {
    const attrs: Record<string, string> = {};
    const tab: TabEl & { attrs: Record<string, string> } = {
      id,
      hidden: false,
      tabIndex: 0,
      attrs,
      setAttribute: (name, value) => {
        attrs[name] = value;
      },
      focus: () => focused.push(id),
    };
    return tab;
  });
  const panels: PanelEl[] = ids.map(() => ({ hidden: false }));
  const selected: string[] = [];
  const t = new Tabs(tabs, panels, (id) => selected.push(id));
  const state = () =>
    tabs.map((tab, i) => [tab.attrs["aria-selected"] === "true", tab.tabIndex === 0, !panels[i].hidden]);
  return { t, tabs, panels, focused, selected, state };
}

test("one tab is selected, tabbable and shown at a time", () => {
  const { t, state, selected } = fake(["a", "b", "c"]);
  assert.equal(t.current, "a");
  assert.deepEqual(state(), [
    [true, true, true],
    [false, false, false],
    [false, false, false],
  ]);
  t.select("c");
  assert.equal(t.current, "c");
  assert.deepEqual(state()[2], [true, true, true]);
  assert.deepEqual(state()[0], [false, false, false]);
  t.select("nope");
  assert.equal(t.current, "c");
  assert.deepEqual(selected, ["a", "c"]);
});

test("keys move the selection and focus with it", () => {
  const { t, focused } = fake(["a", "b", "c"]);
  assert.equal(t.key("ArrowLeft"), true);
  assert.equal(t.current, "c");
  assert.equal(t.key("ArrowRight"), true);
  assert.equal(t.current, "a");
  assert.equal(t.key("x"), false);
  assert.deepEqual(focused, ["c", "a"]);
});

test("hiding the open tab moves to the first visible one, and hidden tabs cannot be picked", () => {
  const { t, panels } = fake(["a", "b", "c"]);
  t.select("b");
  t.setHidden("b", true);
  assert.equal(t.current, "a");
  t.select("b");
  assert.equal(t.current, "a");
  t.key("ArrowRight");
  assert.equal(t.current, "c");
  assert.equal(t.tabOf(panels[2]), "c");
  t.setHidden("b", false);
  t.select("b");
  assert.equal(t.current, "b");
});
