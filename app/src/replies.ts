// Device attributes, status and cursor position reports, window reports and mode reports; OSC and DCS replies.
// biome-ignore lint/suspicious/noControlCharactersInRegex: escape sequences are what it matches
const REPLY = /\x1b\[[?>=]?[\d;]*(?:c|R|n|t|\$y)|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1bP[^\x1b]*\x1b\\/y;

/// Whether `data` is nothing but xterm's answers to terminal queries. Replaying a pane's buffered output re-runs
/// the queries in it, whose askers are long gone, so those answers are dropped; anything the user typed is not.
export function isQueryReply(data: string): boolean {
  if (!data) return false;
  REPLY.lastIndex = 0;
  while (REPLY.lastIndex < data.length) {
    if (!REPLY.test(data)) return false;
  }
  return true;
}

/// What to send of xterm's `onData` for a pane whose buffer is `replaying()`. xterm turns a key into input
/// synchronously in its event, while a replay is parsed in tasks of its own, so whatever arrives before the key
/// event's microtasks run is the user's even when it looks like a reply: Shift+F3 is `CSI 1;2R`, a cursor position.
export function replyFilter(replaying: () => boolean) {
  let typing = false;
  return {
    key() {
      if (typing) return;
      typing = true;
      queueMicrotask(() => {
        typing = false;
      });
    },
    keep: (data: string) => typing || !replaying() || !isQueryReply(data),
  };
}
