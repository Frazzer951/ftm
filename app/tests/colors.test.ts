import assert from "node:assert/strict";
import { test } from "node:test";
import { chromeColors, contrast, luminance, mix, readable, rgb, WORKSPACE_COLORS, WORKSPACE_PALETTE } from "../src/colors.ts";

const near = (actual: number, expected: number, eps = 0.01) =>
  assert.ok(Math.abs(actual - expected) < eps, `${actual} is not close to ${expected}`);

test("hex colors parse and mix", () => {
  assert.deepEqual(rgb("#1e90ff"), [30, 144, 255]);
  assert.equal(mix("#000000", "#ffffff", 0), "#000000");
  assert.equal(mix("#000000", "#ffffff", 1), "#ffffff");
  assert.equal(mix("#000000", "#ffffff", 0.5), "#808080");
  assert.equal(mix("#ff0000", "#0000ff", 0.25), "#bf0040");
});

test("luminance and contrast follow WCAG", () => {
  near(luminance("#000000"), 0);
  near(luminance("#ffffff"), 1);
  near(contrast("#000000", "#ffffff"), 21);
  near(contrast("#ffffff", "#000000"), 21);
  near(contrast("#777777", "#777777"), 1);
  near(contrast("#767676", "#ffffff"), 4.54);
});

test("a readable color is kept, a dim one is moved toward the fallback until it reads", () => {
  assert.equal(readable("#ff0000", "#000000", "#ffffff"), "#ff0000");
  const dim = readable("#202020", "#1a1a1a", "#ffffff");
  assert.notEqual(dim, "#202020");
  assert.ok(contrast(dim, "#1a1a1a") >= 2.5);
  // When even the fallback is too close, the fallback is what is left.
  assert.equal(readable("#101010", "#111111", "#121212"), "#121212");
});

const palette = [
  "#000000", "#cc0000", "#4e9a06", "#c4a000", "#3465a4", "#75507b", "#06989a", "#d3d7cf",
  "#555753", "#ef2929", "#8ae234", "#fce94f", "#729fcf", "#ad7fa8", "#34e2e2", "#eeeeec",
];

test("chrome colors for a dark and a light theme", () => {
  const dark = chromeColors("#1e1e1e", "#d4d4d4", palette);
  assert.equal(dark.dark, true);
  assert.equal(dark.vars["--bg"], "#1e1e1e");
  assert.equal(dark.vars["--text-strong"], "#d4d4d4");
  assert.ok(luminance(dark.vars["--sidebar"]) < luminance("#1e1e1e"));

  const light = chromeColors("#ffffff", "#1e1e1e", palette);
  assert.equal(light.dark, false);
  assert.ok(luminance(light.vars["--sidebar"]) < 1);
});

test("status dots read against the sidebar even on a dim palette", () => {
  const dim = palette.map(() => "#262626");
  const { vars } = chromeColors("#1e1e1e", "#d4d4d4", dim);
  for (const [name, min] of [
    ["--status-starting", 2.5],
    ["--status-working", 2.5],
    ["--status-needs-input", 2.5],
    ["--status-unseen", 2.5],
    ["--status-idle", 2],
    ["--accent", 2],
    ["--muted", 3],
  ] as const) {
    assert.ok(contrast(vars[name], vars["--sidebar"]) >= min, `${name} ${vars[name]} on ${vars["--sidebar"]}`);
  }
});

test("workspace colors are the names ftmd accepts, each clearly apart from the others", () => {
  assert.deepEqual(WORKSPACE_COLORS, ["red", "orange", "yellow", "green", "cyan", "blue", "purple", "pink", "gray"]);
  const distance = (a: string, b: string) => Math.hypot(...rgb(a).map((v, i) => v - rgb(b)[i]));
  for (const [i, a] of WORKSPACE_COLORS.entries()) {
    for (const b of WORKSPACE_COLORS.slice(i + 1)) {
      const d = distance(WORKSPACE_PALETTE[a], WORKSPACE_PALETTE[b]);
      assert.ok(d >= 60, `${a} ${WORKSPACE_PALETTE[a]} and ${b} ${WORKSPACE_PALETTE[b]} are only ${d.toFixed(0)} apart`);
    }
  }
});
