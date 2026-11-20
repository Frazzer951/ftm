import assert from "node:assert/strict";
import { test } from "node:test";
import { artifactKind, fileUrlPath, findPaths, mimeType, resolvePath, splitLine, withCsp } from "../src/artifacts.ts";

test("rendered kinds go to the viewer, the rest do not", () => {
  assert.equal(artifactKind("/a/PLAN.MD"), "markdown");
  assert.equal(artifactKind("report.html"), "html");
  assert.equal(artifactKind("shot.png"), "image");
  assert.equal(artifactKind("logo.svg"), "image");
  assert.equal(artifactKind("demo.mov"), "video");
  for (const p of ["main.rs", "Makefile", "a.md/notes", "x.tar.gz"]) assert.equal(artifactKind(p), undefined, p);
  assert.equal(mimeType("logo.svg"), "image/svg+xml");
  assert.equal(mimeType("a.bin"), "application/octet-stream");
});

test("file URLs name local paths only", () => {
  assert.equal(fileUrlPath("file:///Users/me/a%20b/plan.md"), "/Users/me/a b/plan.md");
  assert.equal(fileUrlPath("file://localhost/tmp/x.png"), "/tmp/x.png");
  assert.equal(fileUrlPath("file://server/share/x.png"), undefined);
  assert.equal(fileUrlPath("https://example.com/x.png"), undefined);
  assert.equal(fileUrlPath("/tmp/x.png"), undefined);
});

test("line suffixes split off", () => {
  assert.deepEqual(splitLine("src/a.rs:12:3"), { path: "src/a.rs", line: 12 });
  assert.deepEqual(splitLine("src/a.rs:7"), { path: "src/a.rs", line: 7 });
  assert.deepEqual(splitLine("src/a.rs"), { path: "src/a.rs" });
});

test("paths resolve against the workspace folder and home", () => {
  assert.equal(resolvePath("docs/plan.md", "/w/repo", "/home/me"), "/w/repo/docs/plan.md");
  assert.equal(resolvePath("./a/../b.md", "/w/repo/", "/home/me"), "/w/repo/b.md");
  assert.equal(resolvePath("~/notes/x.md", "/w", "/home/me"), "/home/me/notes/x.md");
  assert.equal(resolvePath("/etc/../tmp/x.png", "/w", "/home/me"), "/tmp/x.png");
  assert.equal(resolvePath("../../../../x.md", "/w/repo", "/h"), "/x.md");
});

test("paths are found in pane text, URLs and versions are not", () => {
  const texts = (line: string) => findPaths(line).map((p) => p.text);
  assert.deepEqual(texts("Wrote docs/plan.md."), ["docs/plan.md"]);
  assert.deepEqual(texts("error at src/main.rs:12:5: oops"), ["src/main.rs:12:5"]);
  assert.deepEqual(texts("see ./out/shot.png and ~/x/report.html"), ["./out/shot.png", "~/x/report.html"]);
  assert.deepEqual(texts("open /tmp/a b.md"), ["b.md"]);
  assert.deepEqual(texts("https://example.com/docs/plan.md"), []);
  assert.deepEqual(texts("version 0.1.20 released"), []);
  assert.deepEqual(texts("README.md"), ["README.md"]);
  const [p] = findPaths("  ⏺ Read(app/src/main.ts)");
  assert.equal("  ⏺ Read(app/src/main.ts)".slice(p.start, p.end), "app/src/main.ts");
});

test("the CSP meta comes before everything but the doctype", () => {
  const meta = `<meta http-equiv="Content-Security-Policy" content="x">`;
  assert.equal(withCsp("<!DOCTYPE html><html><head><title>t</title></head></html>", "x"), `<!DOCTYPE html>${meta}<html><head><title>t</title></head></html>`);
  assert.equal(withCsp("<!-- <head> --><p>b</p>", "x"), `${meta}<!-- <head> --><p>b</p>`);
  assert.equal(withCsp("<p>b</p>", "x"), `${meta}<p>b</p>`);
});
