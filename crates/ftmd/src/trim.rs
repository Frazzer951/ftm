//! Where to cut a pane's output buffer when it outgrows its cap. The buffer is replayed into a fresh terminal, so it
//! must not start inside an escape sequence: the rest of an image (sixel, iTerm2, kitty), a title or a link would show
//! up as text.

/// The last `cap` bytes of a pane's output, cut only between escape sequences (`cut`). It trims once it is `slack` past
/// the cap, or past where an image still being sent kept it last time, so a long one is not rescanned on every read.
pub struct Tail {
    bytes: Vec<u8>,
    cap: usize,
    slack: usize,
    trim_at: usize,
}

impl Tail {
    pub fn new(cap: usize, slack: usize) -> Self {
        Tail {
            bytes: Vec::new(),
            cap,
            slack,
            trim_at: cap + slack,
        }
    }

    pub fn push(&mut self, more: &[u8]) {
        self.bytes.extend_from_slice(more);
        if self.bytes.len() > self.trim_at {
            let at = cut(&self.bytes, self.bytes.len() - self.cap);
            self.bytes.drain(..at);
            self.trim_at = (self.cap + self.slack).max(self.bytes.len() + self.slack.max(self.bytes.len() / 4));
        }
    }

    pub fn clear(&mut self) {
        self.bytes.clear();
        self.trim_at = self.cap + self.slack;
    }
}

impl std::ops::Deref for Tail {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

impl AsRef<[u8]> for Tail {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

/// How long an escape string still open at the end of the buffer may keep the buffer over its cap, the size of the
/// largest image the app's terminal takes. A program that never ends its string is cut where it would have been.
pub const MAX_STRING: usize = 32 << 20;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// The index to drop `buf[..index]` at: `excess` (the bytes over the cap), moved forward past the escape sequence it
/// falls in, if any, or past the rest of a UTF-8 character. A sequence that has not ended yet is kept whole, so the cut
/// moves back to its start.
pub fn cut(buf: &[u8], excess: usize) -> usize {
    let mut excess = excess.min(buf.len());
    while buf.get(excess).is_some_and(|b| (0x80..0xc0).contains(b)) {
        excess += 1;
    }
    // The last ESC before the cut starts the only sequence it can fall in: strings hold no ESC but their terminator,
    // and that terminator is itself the start of a complete two-byte sequence.
    let Some(start) = buf[..excess].iter().rposition(|&b| b == ESC) else {
        return excess;
    };
    match sequence_end(&buf[start..]) {
        Some(len) if start + len > excess => start + len,
        Some(_) => excess,
        None if buf.len() - start <= MAX_STRING => start,
        None => excess,
    }
}

/// The length of the escape sequence at the start of `seq` (which starts with ESC), or `None` if it is cut short.
fn sequence_end(seq: &[u8]) -> Option<usize> {
    match *seq.get(1)? {
        // CSI: parameters and intermediates, then a final byte.
        b'[' => seq[2..].iter().position(|b| (0x40..=0x7e).contains(b)).map(|i| i + 3),
        // OSC ends with BEL or ST; DCS, APC, PM and SOS with ST.
        kind @ (b']' | b'P' | b'_' | b'^' | b'X') => {
            let body = &seq[2..];
            body.iter().enumerate().find_map(|(i, &b)| match b {
                BEL if kind == b']' => Some(i + 3),
                ESC if body.get(i + 1) == Some(&b'\\') => Some(i + 4),
                _ => None,
            })
        },
        // Intermediates, then a final byte (ESC ( B, ESC 7, ESC \).
        _ => seq[1..].iter().position(|b| (0x30..=0x7e).contains(b)).map(|i| i + 2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tail_keeps_the_last_cap_bytes() {
        let mut tail = Tail::new(10, 5);
        tail.push(b"0123456789abcd");
        assert_eq!(&*tail, b"0123456789abcd", "within the slack");
        tail.push(b"ef");
        assert_eq!(&*tail, b"6789abcdef");
        tail.clear();
        assert!(tail.is_empty());
    }

    #[test]
    fn a_tail_waits_for_an_open_image() {
        let mut tail = Tail::new(10, 5);
        tail.push(b"x\x1b]1337;File=:AAAAAAAAAAAA");
        assert_eq!(tail[0], b'\x1b', "kept whole while open");
        tail.push(b"\x07after");
        tail.push(b"0123456789");
        assert!(!tail.contains(&b'\x1b'));
        assert_eq!(&tail[tail.len() - 10..], b"0123456789");
    }

    #[test]
    fn plain_text_cuts_where_asked() {
        assert_eq!(cut(b"hello world", 5), 5);
        assert_eq!(cut(b"\x1b[1mhello\x1b[0m world", 8), 8);
        assert_eq!(cut(b"abc", 10), 3);
    }

    #[test]
    fn a_cut_inside_a_sequence_moves_past_it() {
        let csi = b"ab\x1b[38;5;196mtext";
        assert_eq!(&csi[cut(csi, 5)..], b"text");
        let osc = b"x\x1b]1337;File=inline=1:AAAABBBB\x07after";
        assert_eq!(&osc[cut(osc, 20)..], b"after");
        let sixel = b"x\x1bPq#0;2;100;0;0#0~~~~-~~~~\x1b\\after";
        assert_eq!(&sixel[cut(sixel, 10)..], b"after");
        let kitty = b"\x1b_Ga=T,f=100;AAAA\x1b\\\x1b_Gm=0;BBBB\x1b\\after";
        assert_eq!(&kitty[cut(kitty, 22)..], b"after");
    }

    #[test]
    fn a_cut_after_a_finished_sequence_stays() {
        let osc = b"\x1b]0;title\x07plain text here";
        assert_eq!(cut(osc, 15), 15);
        let st = b"\x1bPq~~\x1b\\plain text";
        assert_eq!(cut(st, 9), 9);
    }

    #[test]
    fn an_unfinished_sequence_is_kept_whole() {
        let open = b"text\x1b]1337;File=inline=1:AAAA";
        assert_eq!(cut(open, 10), 4);
        assert_eq!(cut(b"ab\x1b[38;5", 3), 2);
    }

    #[test]
    fn a_cut_inside_a_character_moves_past_it() {
        let text = "aé€b".as_bytes();
        assert_eq!(cut(text, 2), 3);
        assert_eq!(cut(text, 4), 6);
    }

    #[test]
    fn a_runaway_string_is_cut_anyway() {
        let mut buf = b"\x1b]".to_vec();
        buf.resize(MAX_STRING + 10, b'A');
        assert_eq!(cut(&buf, 100), 100);
    }
}
