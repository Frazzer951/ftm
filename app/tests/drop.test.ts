import assert from "node:assert/strict";
import { test } from "node:test";
import { dropText, shellQuote } from "../src/drop.ts";

test("plain paths stay as they are", () => {
  assert.equal(shellQuote("/home/me/notes.md"), "/home/me/notes.md");
  assert.equal(shellQuote("./a-b_c+d@e%f=g:h,i"), "./a-b_c+d@e%f=g:h,i");
});

test("anything a shell would read specially is single-quoted", () => {
  assert.equal(shellQuote("/tmp/my file.txt"), "'/tmp/my file.txt'");
  assert.equal(shellQuote("/tmp/$HOME*?[x]"), "'/tmp/$HOME*?[x]'");
  assert.equal(shellQuote("/tmp/it's"), `'/tmp/it'\\''s'`);
  assert.equal(shellQuote("~/x"), "'~/x'");
  assert.equal(shellQuote("/tmp/café"), "'/tmp/café'");
  assert.equal(shellQuote(""), "''");
});

test("dropped paths are joined with spaces and end in one", () => {
  assert.equal(dropText(["/a", "/b c"]), "/a '/b c' ");
  assert.equal(dropText([]), "");
});
