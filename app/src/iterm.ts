/// iTerm2 3.5's multipart inline images (`OSC 1337;MultipartFile=<args>`, then `FilePart=<base64>` chunks, then
/// `FileEnd`), which current `imgcat` and Claude Code send, joined into the single `OSC 1337;File=<args>:<base64>` the
/// image addon reads. Everything else passes through untouched, split across chunks however it arrives.

const INTRO = [0x1b, 0x5d, 0x31, 0x33, 0x33, 0x37, 0x3b]; // ESC ] 1337 ;
const ESC = 0x1b;
const BEL = 0x07;
/// The addon's own limit on a single image sequence; a bigger one is dropped rather than held.
const MAX_IMAGE = 32 << 20;

const latin1 = new TextDecoder("latin1");
const encoder = new TextEncoder();

function concat(parts: Uint8Array[]): Uint8Array {
  if (parts.length === 1) return parts[0];
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

/// The index of the next `ESC ] 1337 ;` at or after `from`, or of its start cut off by the end of `data` (a read
/// that split a sequence; the rest follows at once); `-1` when there is none.
function findIntro(data: Uint8Array, from: number): number {
  for (let i = data.indexOf(ESC, from); i !== -1; i = data.indexOf(ESC, i + 1)) {
    let k = 1;
    while (k < INTRO.length && i + k < data.length && data[i + k] === INTRO[k]) k++;
    if (k === INTRO.length || i + k === data.length) return i;
  }
  return -1;
}

/// The sequence's end (past its BEL or ST) from `body`, or `-1` when it has not ended yet.
function findEnd(data: Uint8Array, body: number): number {
  for (let i = body; i < data.length; i++) {
    if (data[i] === BEL) return i + 1;
    if (data[i] === ESC && i + 1 < data.length && data[i + 1] === 0x5c) return i + 2;
  }
  return -1;
}

export class MultipartImages {
  private carry: Uint8Array | null = null;
  private image: { args: string; parts: string[]; size: number } | null = null;

  /// The bytes to write to the terminal for this chunk of pane output.
  feed(chunk: Uint8Array): Uint8Array {
    const data = this.carry ? concat([this.carry, chunk]) : chunk;
    this.carry = null;
    if (!this.image && findIntro(data, 0) === -1) return data;
    const out: Uint8Array[] = [];
    let i = 0;
    while (i < data.length) {
      const start = findIntro(data, i);
      if (start === -1) {
        out.push(data.subarray(i));
        break;
      }
      out.push(data.subarray(i, start));
      const end = start + INTRO.length <= data.length ? findEnd(data, start + INTRO.length) : -1;
      if (end === -1) {
        // Wait for the rest, unless it is a single image too big to show anyway.
        if (data.length - start <= MAX_IMAGE) this.carry = data.slice(start);
        else out.push(data.subarray(start));
        break;
      }
      const body = latin1.decode(data.subarray(start + INTRO.length, end - (data[end - 1] === BEL ? 1 : 2)));
      out.push(this.sequence(body) ?? data.subarray(start, end));
      i = end;
    }
    return concat(out);
  }

  /// A replay starts over.
  reset() {
    this.carry = null;
    this.image = null;
  }

  /// What replaces one `OSC 1337` sequence, or `undefined` to keep it as it came.
  private sequence(body: string): Uint8Array | undefined {
    const none = new Uint8Array(0);
    if (body.startsWith("MultipartFile=")) {
      this.image = { args: body.slice("MultipartFile=".length), parts: [], size: 0 };
      return none;
    }
    if (body.startsWith("FilePart=") && this.image) {
      const part = body.slice("FilePart=".length);
      this.image.size += part.length;
      if (this.image.size > MAX_IMAGE) this.image = null;
      else this.image.parts.push(part);
      return none;
    }
    if (body === "FileEnd" && this.image) {
      const { args, parts } = this.image;
      this.image = null;
      return encoder.encode(`\x1b]1337;File=${args}:${parts.join("")}\x07`);
    }
    return undefined;
  }
}
