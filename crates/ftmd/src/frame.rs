//! Length-prefixed frames between ftmd and a pane holder: one tag byte, a big-endian u32 length, then the payload.

use std::io::{self, Read, Write};

/// Bumped whenever a frame changes meaning, so a new ftmd refuses holders it cannot talk to.
pub const VERSION: u8 = 1;

#[derive(Debug, PartialEq)]
pub enum Frame {
    /// Holder to ftmd, first on every connection.
    Hello {
        version: u8,
        rows: u16,
        cols: u16,
    },
    /// Holder to ftmd: PTY output. The first one after `Hello` is the holder's whole buffer.
    Output(Vec<u8>),
    /// ftmd to holder: bytes for the PTY.
    Input(Vec<u8>),
    Resize {
        rows: u16,
        cols: u16,
    },
    /// ftmd to holder: terminate everything on the terminal, then exit.
    Kill,
}

const HELLO: u8 = 1;
const OUTPUT: u8 = 2;
const INPUT: u8 = 3;
const RESIZE: u8 = 4;
const KILL: u8 = 5;

/// Well above the largest frame either side sends (a holder's whole buffer), so a corrupt header cannot make the reader
/// allocate gigabytes.
pub const MAX_PAYLOAD: usize = 16 << 20;

pub fn write(w: &mut impl Write, frame: &Frame) -> io::Result<()> {
    let (tag, payload): (u8, Vec<u8>) = match frame {
        Frame::Hello { version, rows, cols } => {
            let mut p = vec![*version];
            p.extend(rows.to_be_bytes());
            p.extend(cols.to_be_bytes());
            (HELLO, p)
        },
        Frame::Output(bytes) => (OUTPUT, bytes.clone()),
        Frame::Input(bytes) => (INPUT, bytes.clone()),
        Frame::Resize { rows, cols } => (RESIZE, [rows.to_be_bytes(), cols.to_be_bytes()].concat()),
        Frame::Kill => (KILL, Vec::new()),
    };
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(tag);
    let len = u32::try_from(payload.len()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    out.extend(len.to_be_bytes());
    out.extend(payload);
    w.write_all(&out)?;
    w.flush()
}

/// `None` on a clean end of stream between frames.
pub fn read(r: &mut impl Read) -> io::Result<Option<Frame>> {
    let mut head = [0u8; 5];
    match r.read_exact(&mut head[..1]) {
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        other => other?,
    }
    r.read_exact(&mut head[1..])?;
    let len = u32::from_be_bytes(head[1..].try_into().expect("4 bytes")) as usize;
    if len > MAX_PAYLOAD {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("frame of {len} bytes")));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    let u16_at = |i: usize| -> io::Result<u16> {
        payload
            .get(i..i + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "short frame"))
    };
    Ok(Some(match head[0] {
        HELLO => Frame::Hello {
            version: *payload
                .first()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "short frame"))?,
            rows: u16_at(1)?,
            cols: u16_at(3)?,
        },
        OUTPUT => Frame::Output(payload),
        INPUT => Frame::Input(payload),
        RESIZE => Frame::Resize {
            rows: u16_at(0)?,
            cols: u16_at(2)?,
        },
        KILL => Frame::Kill,
        tag => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unknown frame {tag}"))),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let frames = [
            Frame::Hello {
                version: VERSION,
                rows: 40,
                cols: 120,
            },
            Frame::Output(b"hi\x1b[0m".to_vec()),
            Frame::Input(Vec::new()),
            Frame::Resize { rows: 1, cols: 65535 },
            Frame::Kill,
        ];
        let mut buf = Vec::new();
        for f in &frames {
            write(&mut buf, f).unwrap();
        }
        let mut r = buf.as_slice();
        for f in frames {
            assert_eq!(read(&mut r).unwrap(), Some(f));
        }
        assert_eq!(read(&mut r).unwrap(), None);
    }

    fn read_err(bytes: &[u8]) -> io::ErrorKind {
        read(&mut &bytes[..]).unwrap_err().kind()
    }

    #[test]
    fn malformed_frames_are_errors() {
        assert_eq!(read_err(&[OUTPUT, 0, 0]), io::ErrorKind::UnexpectedEof);
        assert_eq!(read_err(&[OUTPUT, 0, 0, 0, 4, b'a']), io::ErrorKind::UnexpectedEof);
        assert_eq!(read_err(&[99, 0, 0, 0, 0]), io::ErrorKind::InvalidData);
        assert_eq!(read_err(&[HELLO, 0, 0, 0, 3, VERSION, 0, 24]), io::ErrorKind::InvalidData);
        assert_eq!(read_err(&[RESIZE, 0, 0, 0, 2, 0, 24]), io::ErrorKind::InvalidData);
    }

    #[test]
    fn oversized_frames_are_refused_before_reading_them() {
        let mut head = vec![OUTPUT];
        head.extend(u32::MAX.to_be_bytes());
        assert_eq!(read_err(&head), io::ErrorKind::InvalidData);
        let mut head = vec![INPUT];
        head.extend(u32::try_from(MAX_PAYLOAD).unwrap().to_be_bytes());
        assert_eq!(read_err(&head), io::ErrorKind::UnexpectedEof);
    }
}
