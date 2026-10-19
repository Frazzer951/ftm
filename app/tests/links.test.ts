import assert from "node:assert/strict";
import { test } from "node:test";
import { openableUrl, opensLink } from "../src/links.ts";

test("web and mail links open", () => {
  assert.equal(openableUrl("https://example.com/a?b=c#d"), "https://example.com/a?b=c#d");
  assert.equal(openableUrl("http://localhost:3000"), "http://localhost:3000/");
  assert.equal(openableUrl("HTTPS://Example.com"), "https://example.com/");
  assert.equal(openableUrl("mailto:me@example.com"), "mailto:me@example.com");
});

test("other schemes and non-URLs never open", () => {
  for (const text of [
    "file:///etc/passwd",
    "javascript:alert(1)",
    "data:text/html,<b>x</b>",
    "vscode://file/tmp/x",
    "ssh://host",
    "smb://host/share",
    "/tmp/x",
    "example.com",
    "",
  ]) {
    assert.equal(openableUrl(text), undefined, text);
  }
});

test("the platform's modifier opens a link", () => {
  assert.equal(opensLink({ metaKey: true, ctrlKey: false }, true), true);
  assert.equal(opensLink({ metaKey: false, ctrlKey: false }, true), false);
  assert.equal(opensLink({ metaKey: false, ctrlKey: true }, true), false);
  assert.equal(opensLink({ metaKey: false, ctrlKey: true }, false), true);
  assert.equal(opensLink({ metaKey: true, ctrlKey: false }, false), false);
  assert.equal(opensLink({ metaKey: false, ctrlKey: false }, false), false);
});
