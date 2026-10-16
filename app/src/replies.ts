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
