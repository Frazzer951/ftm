import assert from "node:assert/strict";
import { test } from "node:test";
import { MultipartImages } from "../src/iterm.ts";

const enc = (s: string) => new TextEncoder().encode(s);
const dec = (b: Uint8Array) => new TextDecoder().decode(b);
const multipart = "\x1b]1337;MultipartFile=inline=1;size=6;width=320px\x07\x1b]1337;FilePart=AAAA\x07\x1b]1337;FilePart=BBBB\x1b\\\x1b]1337;FileEnd\x07";
const joined = "\x1b]1337;File=inline=1;size=6;width=320px:AAAABBBB\x07";

test("multipart images become one File= sequence", () => {
  const m = new MultipartImages();
  assert.equal(dec(m.feed(enc(`before\r\n${multipart}\nafter`))), `before\r\n${joined}\nafter`);
});

test("any split across chunks gives the same bytes", () => {
  const all = `x${multipart}y\x1b]0;title\x07z`;
  const want = `x${joined}y\x1b]0;title\x07z`;
  for (let cut = 0; cut <= all.length; cut++) {
    const m = new MultipartImages();
    const got = dec(m.feed(enc(all.slice(0, cut)))) + dec(m.feed(enc(all.slice(cut))));
    assert.equal(got, want, `cut at ${cut}`);
  }
});

test("other output passes through as is", () => {
  const m = new MultipartImages();
  const bytes = enc("plain \x1b[1mbold\x1b[0m \x1b]1337;File=inline=1:AAAA\x07 \x1b]1337;SetMark\x07 é");
  assert.deepEqual(m.feed(bytes), bytes);
  assert.equal(dec(m.feed(enc("a split \x1b"))), "a split ");
  assert.equal(dec(m.feed(enc("[1mrest"))), "\x1b[1mrest");
});

test("parts without a header, and a reset mid-image, are dropped or kept as they came", () => {
  const m = new MultipartImages();
  assert.equal(dec(m.feed(enc("\x1b]1337;FilePart=AAAA\x07"))), "\x1b]1337;FilePart=AAAA\x07");
  m.feed(enc("\x1b]1337;MultipartFile=inline=1\x07\x1b]1337;FilePart=AAAA\x07"));
  m.reset();
  assert.equal(dec(m.feed(enc("\x1b]1337;FileEnd\x07"))), "\x1b]1337;FileEnd\x07");
});
