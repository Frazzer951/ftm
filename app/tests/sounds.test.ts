import assert from "node:assert/strict";
import { test } from "node:test";
import { gainFor } from "../src/sounds.ts";

const db = (gain: number) => 20 * Math.log10(gain);

test("volume ends map to silence and full gain", () => {
  assert.equal(gainFor(0), 0);
  assert.equal(gainFor(1), 1);
});

test("out-of-range volumes clamp", () => {
  assert.equal(gainFor(-0.5), 0);
  assert.equal(gainFor(2), 1);
});

test("the taper is perceptual, not linear", () => {
  assert.ok(Math.abs(db(gainFor(0.5)) - -18.06) < 0.01);
  assert.ok(Math.abs(db(gainFor(0.7)) - -9.29) < 0.01);
  assert.ok(Math.abs(db(gainFor(0.1)) - -60) < 0.01);
});

test("gain rises monotonically with the slider", () => {
  for (let v = 0; v < 1; v += 0.05) assert.ok(gainFor(v + 0.05) > gainFor(v));
});
