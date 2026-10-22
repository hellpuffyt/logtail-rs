//! Reading newline-delimited input one line at a time with a hard cap on
//! how much of any single line is held in memory.
//!
//! `BufRead::lines`/`read_line` buffer a whole line however long it is and
//! fail the entire read on invalid UTF-8. For a log tool both are wrong: a
//! single multi-gigabyte line (a file with no newlines, a runaway log
//! statement) would be loaded into memory in full, and one corrupt byte
//! would abort the whole run. [`LineReader`] instead keeps at most
//! `max_line_bytes` of a line, discards (without buffering) the rest of an
//! over-long line, and reports non-UTF-8 lines as a distinct outcome so
//! the caller can count and skip them.

use std::io::{self, BufRead};

/// Default cap on the bytes kept from a single line: 1 MiB.
pub const DEFAULT_MAX_LINE_BYTES: usize = 1024 * 1024;

/// One line read by [`LineReader::next_line`].
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    /// A complete line, without its trailing `\n` / `\r\n`.
    Text(String),
    /// A line that is not valid UTF-8.
    InvalidUtf8,
    /// A line longer than the configured cap; its content was discarded.
    TooLong,
}

/// The result of one read: the line, how many bytes of input it consumed
/// (including the newline), and whether it was terminated by `\n` (as
/// opposed to running into end of input).
#[derive(Debug, PartialEq, Eq)]
pub struct ReadLine {
    /// What was read.
    pub line: Line,
    /// Bytes consumed from the input, including any newline.
    pub consumed: u64,
    /// Whether the line ended with `\n`.
    pub terminated: bool,
}

/// A newline splitter with bounded per-line memory.
#[derive(Debug)]
pub struct LineReader<R> {
    inner: R,
    max_line_bytes: usize,
    buf: Vec<u8>,
}

impl<R: BufRead> LineReader<R> {
    /// Wraps `inner`, keeping at most `max_line_bytes` (minimum 1) of any
    /// line.
    pub fn new(inner: R, max_line_bytes: usize) -> Self {
        LineReader {
            inner,
            max_line_bytes: max_line_bytes.max(1),
            buf: Vec::new(),
        }
    }

    /// Reads the next line. Returns `Ok(None)` at end of input.
    ///
    /// A final line without a trailing newline is still returned, with
    /// `terminated: false`, so callers that must not consume partial lines
    /// (follow mode) can tell the difference.
    ///
    /// # Errors
    /// Propagates I/O errors from the underlying reader. Invalid UTF-8 and
    /// over-long lines are *not* errors; see [`Line`].
    pub fn next_line(&mut self) -> io::Result<Option<ReadLine>> {
        self.buf.clear();
        let mut consumed: u64 = 0;
        let mut too_long = false;
        let mut terminated = false;
        loop {
            let available = match self.inner.fill_buf() {
                Ok(b) => b,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if available.is_empty() {
                break;
            }
            let (chunk, used, found_newline) = match available.iter().position(|&b| b == b'\n') {
                Some(i) => (&available[..i], i + 1, true),
                None => (available, available.len(), false),
            };
            if !too_long {
                if self.buf.len() + chunk.len() > self.max_line_bytes {
                    too_long = true;
                    self.buf.clear();
                } else {
                    self.buf.extend_from_slice(chunk);
                }
            }
            self.inner.consume(used);
            consumed += used as u64;
            if found_newline {
                terminated = true;
                break;
            }
        }
        if consumed == 0 {
            return Ok(None);
        }
        let line = if too_long {
            Line::TooLong
        } else {
            if self.buf.last() == Some(&b'\r') {
                self.buf.pop();
            }
            match String::from_utf8(std::mem::take(&mut self.buf)) {
                Ok(s) => Line::Text(s),
                Err(_) => Line::InvalidUtf8,
            }
        };
        Ok(Some(ReadLine {
            line,
            consumed,
            terminated,
        }))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    fn all(input: &[u8], max: usize) -> Vec<ReadLine> {
        // A tiny BufReader capacity forces lines to span many fill_buf calls.
        let mut r = LineReader::new(
            BufReader::with_capacity(3, Cursor::new(input.to_vec())),
            max,
        );
        let mut out = Vec::new();
        while let Some(l) = r.next_line().unwrap() {
            out.push(l);
        }
        out
    }

    #[test]
    fn splits_lines_and_strips_crlf() {
        let lines = all(b"a\r\nbc\n\nlast", 100);
        let text: Vec<_> = lines.iter().map(|l| &l.line).collect();
        assert_eq!(
            text,
            [
                &Line::Text("a".into()),
                &Line::Text("bc".into()),
                &Line::Text(String::new()),
                &Line::Text("last".into())
            ]
        );
        assert_eq!(lines.iter().map(|l| l.consumed).sum::<u64>(), 11);
        assert!(lines[2].terminated);
        assert!(!lines[3].terminated);
    }

    #[test]
    fn invalid_utf8_is_reported_not_an_error() {
        let lines = all(b"ok\n\xff\xfe\nok2\n", 100);
        assert_eq!(lines[1].line, Line::InvalidUtf8);
        assert_eq!(lines[2].line, Line::Text("ok2".into()));
    }

    #[test]
    fn over_long_lines_are_discarded_and_the_next_line_survives() {
        let mut input = vec![b'x'; 10_000];
        input.extend_from_slice(b"\nshort\n");
        let lines = all(&input, 64);
        assert_eq!(lines[0].line, Line::TooLong);
        assert_eq!(lines[0].consumed, 10_001);
        assert_eq!(lines[1].line, Line::Text("short".into()));
    }

    #[test]
    fn a_line_exactly_at_the_cap_is_kept() {
        let lines = all(b"abcd\nabcde\n", 4);
        assert_eq!(lines[0].line, Line::Text("abcd".into()));
        assert_eq!(lines[1].line, Line::TooLong);
    }

    #[test]
    fn buffer_never_exceeds_the_cap() {
        let input = vec![b'y'; 1_000_000];
        let mut r = LineReader::new(Cursor::new(input), 1024);
        let l = r.next_line().unwrap().unwrap();
        assert_eq!(l.line, Line::TooLong);
        assert!(r.buf.capacity() <= 2048, "capacity {}", r.buf.capacity());
    }
}
