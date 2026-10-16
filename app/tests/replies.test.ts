import assert from "node:assert/strict";
import { test } from "node:test";
import { isQueryReply, replyFilter } from "../src/replies.ts";

test("xterm's query replies are recognised", () => {
  assert.ok(isQueryReply("\x1b[?1;2c"));
  assert.ok(isQueryReply("\x1b[>0;276;0c"));
  assert.ok(isQueryReply("\x1b[0n"));
  assert.ok(isQueryReply("\x1b[24;80R"));
  assert.ok(isQueryReply("\x1b[?2004;1$y"));
  assert.ok(isQueryReply("\x1b[8;24;80t"));
  assert.ok(isQueryReply("\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\"));
  assert.ok(isQueryReply("\x1b]10;rgb:ffff/ffff/ffff\x07"));
  assert.ok(isQueryReply("\x1bP>|xterm.js(6.0.0)\x1b\\"));
  assert.ok(isQueryReply("\x1b[?1;2c\x1b[24;80R"));
});

test("what the user types or pastes is not a reply", () => {
  for (const data of ["", "a", "\x03", "\r", "\x1b", "\x1b]", "\x1b[A", "\x1b[13;2u", "\x1b[200~c\x1b[201~", "ls\r", "\x1b[0nx"]) {
    assert.equal(isQueryReply(data), false, JSON.stringify(data));
  }
});

test("during a replay, replies are dropped but keys shaped like one are not", async () => {
  let replaying = true;
  const replies = replyFilter(() => replaying);
  assert.equal(replies.keep("\x1b[24;80R"), false);
  assert.equal(replies.keep("a"), true);
  // Shift+F3 in xterm's encoding.
  replies.key();
  assert.equal(replies.keep("\x1b[1;2R"), true);
  await Promise.resolve();
  assert.equal(replies.keep("\x1b[1;2R"), false);
  replaying = false;
  assert.equal(replies.keep("\x1b[24;80R"), true);
});
