import assert from "node:assert/strict";
import { test } from "node:test";
import { PerfMeter } from "../src/perf.ts";

test("a summary goes out once per window, and only when something happened", () => {
  const perf = new PerfMeter(0, 1000, 500);
  perf.stall(50);
  assert.equal(perf.take(999), null);
  assert.equal(perf.take(1000), null, "a stall under the floor is not news");
  perf.stall(150);
  perf.write(2048, true, 12);
  perf.write(4096, false);
  perf.sidebar(3);
  perf.sidebar(5);
  perf.workspaces();
  perf.contextLost();
  assert.equal(
    perf.take(2000),
    "perf 1s: stalls 1 (max 150 ms), parse max 12 ms over 1 writes, output 2 KB shown / 4 KB hidden, " +
      "sidebar 2× (8 ms, max 5 ms) for 1 workspaces events, 1 webgl contexts lost",
  );
  assert.equal(perf.take(3000), null, "the counters start over");
});

test("a long stall is reported at once, then not again within the window", () => {
  const perf = new PerfMeter(0, 10_000, 1000);
  perf.stall(1500);
  assert.match(perf.take(100) ?? "", /^perf slow 0s: stalls 1 \(max 1500 ms\)/);
  perf.stall(2000);
  assert.equal(perf.take(200), null);
  assert.match(perf.take(10_100) ?? "", /^perf slow 10s: stalls 1 \(max 2000 ms\)/);
});
