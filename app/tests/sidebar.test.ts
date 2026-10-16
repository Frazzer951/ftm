import assert from "node:assert/strict";
import { test } from "node:test";
import { clampSidebarWidth, SIDEBAR_DEFAULT, SIDEBAR_MIN } from "../src/sidebar.ts";

test("the sidebar width stays between the minimum and half the window", () => {
  assert.equal(clampSidebarWidth(300, 1600), 300);
  assert.equal(clampSidebarWidth(100, 1600), SIDEBAR_MIN);
  assert.equal(clampSidebarWidth(1000, 1600), 800);
  assert.equal(clampSidebarWidth(250.6, 1600), 251);
  assert.equal(clampSidebarWidth(300, 300), SIDEBAR_MIN, "the minimum wins in a narrow window");
  assert.equal(clampSidebarWidth(Number.NaN, 1600), SIDEBAR_DEFAULT);
});
