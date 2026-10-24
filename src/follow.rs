//! Follow mode (`-F`/`--follow`): tails a file for new lines, detecting log rotation
//! (the file at `path` being replaced by a new file, as `logrotate` or
//! similar tools do) so that following does not silently go stale the way
//! plain `tail -f` can.
//!
//! # Platform differences in rotation detection
//!
//! On Unix, `file_identity` returns the file's inode number
//! (`MetadataExt::ino`), which is the textbook way to notice "the path now
//! refers to a different file": rename-based rotation (moving the old file
//! aside and creating a new one at the same path) always changes it.
//!
//! On Windows there is no stable-Rust equivalent of an inode.
//! `MetadataExt::file_index` exists but sits behind the unstable
//! `windows_by_handle` feature and does not compile on stable Rust; getting
//! the real NTFS file ID would require an `unsafe` FFI call into
//! `GetFileInformationByHandle`, which this crate forbids
//! (`unsafe_code = "forbid"`). Instead, `file_identity` on Windows uses
//! the file's creation timestamp (`Metadata::created`, stable and safe):
//! appending to a file does not change it, but replacing the file at a path
//! (logrotate-style rename-and-recreate) normally does. Combined with the
//! existing "file got shorter" check (which independently catches
//! copytruncate-style rotation on every platform), this catches the same
//! rotation patterns the Unix path does in the common case.
//!
//! [`Follower::poll`] backs the creation-time check with a second,
//! platform-independent signal (see `reopen_if_rotated`): an already-open
//! handle keeps reporting the size of whatever file it actually refers to,
//! even after the path has been repointed at a different file, while a
//! fresh stat of the path sees the new file. A mismatch there means the
//! path no longer refers to the file the handle was opened against,
//! independent of the identity check. That catches rename-based rotation
//! on Windows even when creation-time tunnelling defeats the primary
//! check, as long as the old and new files do not happen to be exactly the
//! same length at the moment of the check.
//!
//! The known gap: if NTFS tunnelling preserves the creation timestamp *and*
//! the replacement file happens to have the exact same byte length as the
//! file it replaced at the moment `poll` runs, rotation could be missed.
//! This is a real limitation of doing rotation detection without
//! `unsafe`/unstable APIs on Windows, documented here rather than silently
//! assumed away; see the platform-specific test below for what is actually
//! exercised.

use std::fs::{self, File};
use std::io::{self, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::lines::{Line, LineReader, DEFAULT_MAX_LINE_BYTES};

/// How many bytes one [`Follower::poll_lines`] call consumes at most
/// (always at least one whole line), so a large burst of appended data is
/// handed over in bounded batches rather than collected into memory at once.
pub const POLL_BYTE_BUDGET: u64 = 8 * 1024 * 1024;

#[cfg(unix)]
fn file_identity(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(windows)]
#[allow(clippy::cast_possible_truncation)]
fn file_identity(meta: &fs::Metadata) -> u64 {
    // No `unsafe`, no unstable features: derive a rotation-detection token
    // from the (stable, safe) creation timestamp rather than a true file
    // identifier. See the module-level docs for what this can and cannot
    // detect.
    meta.created()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64)
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_meta: &fs::Metadata) -> u64 {
    0
}

/// Tails `path`, tracking a byte offset and file identity so it can notice
/// when the file has been rotated (replaced) or truncated and react by
/// reopening from the start, rather than silently stalling.
pub struct Follower {
    path: PathBuf,
    file: File,
    identity: u64,
    offset: u64,
    max_line_bytes: usize,
}

impl Follower {
    /// Opens `path` and seeks to the current end of file: only lines
    /// appended after this call are returned by [`Follower::poll`].
    ///
    /// # Errors
    /// Returns an error if the file cannot be opened or its metadata read.
    pub fn open_at_end(path: &Path) -> io::Result<Self> {
        Self::open_impl(path, true)
    }

    /// Opens `path` from the beginning.
    ///
    /// # Errors
    /// Returns an error if the file cannot be opened or its metadata read.
    pub fn open_from_start(path: &Path) -> io::Result<Self> {
        Self::open_impl(path, false)
    }

    fn open_impl(path: &Path, at_end: bool) -> io::Result<Self> {
        let file = File::open(path)?;
        let meta = file.metadata()?;
        let offset = if at_end { meta.len() } else { 0 };
        Ok(Follower {
            path: path.to_path_buf(),
            file,
            identity: file_identity(&meta),
            offset,
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
        })
    }

    /// Sets the most bytes kept from any one line; longer lines are
    /// skipped and reported as [`Line::TooLong`] by [`Follower::poll_lines`].
    #[must_use]
    pub fn with_max_line_bytes(mut self, max_line_bytes: usize) -> Self {
        self.max_line_bytes = max_line_bytes;
        self
    }

    /// Checks for rotation/truncation and returns any newly available
    /// complete, valid UTF-8 lines since the last call. A trailing partial
    /// line (no terminating `\n` yet) is left unconsumed for the next poll.
    /// Lines that are not valid UTF-8 or exceed the line-length cap are
    /// dropped; use [`Follower::poll_lines`] to see them.
    ///
    /// # Errors
    /// Returns an error if a filesystem operation fails. A momentarily
    /// missing file (mid-rotation) is tolerated and simply yields no lines.
    pub fn poll(&mut self) -> io::Result<Vec<String>> {
        Ok(self
            .poll_lines()?
            .into_iter()
            .filter_map(|line| match line {
                Line::Text(s) => Some(s),
                Line::InvalidUtf8 | Line::TooLong => None,
            })
            .collect())
    }

    /// Like [`Follower::poll`], but also reports invalid-UTF-8 and
    /// over-long lines, and consumes at most [`POLL_BYTE_BUDGET`] bytes per
    /// call (call again immediately if it returned lines).
    ///
    /// Memory is bounded by the line cap and the byte budget, never by the
    /// size of what was appended.
    ///
    /// # Errors
    /// As [`Follower::poll`].
    pub fn poll_lines(&mut self) -> io::Result<Vec<Line>> {
        self.reopen_if_rotated();

        let len = self.file.metadata()?.len();
        if len < self.offset {
            // Truncated in place (e.g. `> file` or copytruncate rotation).
            self.offset = 0;
        }

        self.file.seek(SeekFrom::Start(self.offset))?;
        let mut lines = Vec::new();
        let mut reader = LineReader::new(BufReader::new(&self.file), self.max_line_bytes);
        let mut consumed_this_poll = 0u64;
        while consumed_this_poll < POLL_BYTE_BUDGET {
            let Some(read) = reader.next_line()? else {
                break;
            };
            if !read.terminated {
                // Partial line; wait for more data before consuming it.
                break;
            }
            self.offset += read.consumed;
            consumed_this_poll += read.consumed;
            lines.push(read.line);
        }
        Ok(lines)
    }

    fn reopen_if_rotated(&mut self) {
        let Ok(disk_meta) = fs::metadata(&self.path) else {
            // File momentarily missing mid-rotation; try again next poll.
            return;
        };
        let disk_identity = file_identity(&disk_meta);
        let identity_changed = disk_identity != self.identity;

        // Second, platform-independent signal: an already-open handle keeps
        // reporting the size of whatever file it actually refers to, even
        // after the path has been repointed at a different file (renamed
        // away and recreated). A fresh stat of the path, by contrast, sees
        // the new file. If those disagree, the path no longer refers to the
        // file this handle was opened against - regardless of whether the
        // identity check above could tell. This is what keeps rotation
        // detection working on Windows even in the case described in the
        // module docs, where NTFS tunnelling can make a recreated file's
        // creation time look unchanged: the two files still need to
        // coincidentally share a byte length for that gap to bite in
        // practice.
        let size_diverged = self
            .file
            .metadata()
            .is_ok_and(|handle_meta| handle_meta.len() != disk_meta.len());

        if identity_changed || size_diverged {
            if let Ok(new_file) = File::open(&self.path) {
                if let Ok(new_meta) = new_file.metadata() {
                    self.file = new_file;
                    self.identity = file_identity(&new_meta);
                    self.offset = 0;
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::thread::sleep;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn reads_lines_appended_after_open() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("app.log");
        fs::write(&path, "old line\n").unwrap();

        let mut follower = Follower::open_at_end(&path).unwrap();
        assert_eq!(follower.poll().unwrap(), [] as [std::string::String; 0]);

        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "new line 1").unwrap();
        writeln!(f, "new line 2").unwrap();
        f.flush().unwrap();

        let lines = follower.poll().unwrap();
        assert_eq!(lines, vec!["new line 1", "new line 2"]);
    }

    #[test]
    fn from_start_reads_existing_content() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("app.log");
        fs::write(&path, "a\nb\n").unwrap();
        let mut follower = Follower::open_from_start(&path).unwrap();
        assert_eq!(follower.poll().unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn leaves_partial_line_unconsumed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("app.log");
        fs::write(&path, "complete\npartial").unwrap();
        let mut follower = Follower::open_from_start(&path).unwrap();
        assert_eq!(follower.poll().unwrap(), vec!["complete"]);

        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f).unwrap();
        f.flush().unwrap();
        assert_eq!(follower.poll().unwrap(), vec!["partial"]);
    }

    #[test]
    fn detects_rotation_by_replacing_the_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("app.log");
        fs::write(&path, "old-1\nold-2\n").unwrap();
        let mut follower = Follower::open_at_end(&path).unwrap();
        assert_eq!(follower.poll().unwrap(), [] as [std::string::String; 0]);

        // Simulate logrotate: move the old file aside, create a fresh one
        // at the same path.
        let rotated_path = dir.path().join("app.log.1");
        fs::rename(&path, &rotated_path).unwrap();
        fs::write(&path, "fresh-1\n").unwrap();
        sleep(Duration::from_millis(10));

        let lines = follower.poll().unwrap();
        assert_eq!(lines, vec!["fresh-1"]);
    }

    // Windows-only regression test for `file_identity`. It cannot be
    // expressed portably: on Unix `file_identity` reads the inode, which
    // this test has no reason to doubt, while the Windows implementation is
    // the one this whole module is being careful about, so it gets its own
    // targeted assertion rather than only being exercised indirectly by
    // `detects_rotation_by_replacing_the_file` above. It only runs in CI's
    // `windows-latest` job; it has not been run against a real Windows
    // machine as part of this change. It cannot exercise the NTFS
    // tunnelling gap documented at the top of this module - reliably
    // triggering tunnelling would require deleting and recreating a file
    // within its cache window and is not something this test controls.
    #[cfg(windows)]
    #[test]
    fn windows_file_identity_is_stable_across_appends() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("app.log");
        fs::write(&path, "one\n").unwrap();
        let id_before = file_identity(&fs::metadata(&path).unwrap());

        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "two").unwrap();
        f.flush().unwrap();

        let id_after = file_identity(&fs::metadata(&path).unwrap());

        // Appending must not change the identity token: if it did, every
        // poll would look like a rotation and Follower would re-read the
        // whole file (and re-emit already-seen lines) on every call.
        assert_eq!(id_before, id_after);
    }

    #[test]
    fn detects_in_place_truncation() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("app.log");
        // A long pre-truncate log, so the post-truncate content below is
        // unambiguously shorter than the offset we had already consumed -
        // the realistic copytruncate case (the file resets to near-empty,
        // then slowly regrows).
        fs::write(&path, "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n").unwrap();
        let mut follower = Follower::open_from_start(&path).unwrap();
        assert_eq!(
            follower.poll().unwrap(),
            vec!["one", "two", "three", "four", "five", "six", "seven", "eight"]
        );

        // copytruncate-style rotation: same inode, file emptied in place,
        // then appended to with far fewer bytes than were previously read.
        {
            let f = File::create(&path).unwrap(); // truncates to zero length
            drop(f);
        }
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "new").unwrap();
        f.flush().unwrap();

        let lines = follower.poll().unwrap();
        assert_eq!(lines, vec!["new"]);
    }

    #[test]
    fn multiple_polls_do_not_repeat_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("app.log");
        fs::write(&path, "").unwrap();
        let mut follower = Follower::open_from_start(&path).unwrap();

        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "one").unwrap();
        f.flush().unwrap();
        assert_eq!(follower.poll().unwrap(), vec!["one"]);
        assert_eq!(follower.poll().unwrap(), [] as [std::string::String; 0]);

        writeln!(f, "two").unwrap();
        f.flush().unwrap();
        assert_eq!(follower.poll().unwrap(), vec!["two"]);
    }

    #[test]
    fn poll_lines_reports_invalid_utf8_and_over_long_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("app.log");
        fs::write(&path, "").unwrap();
        let mut follower = Follower::open_at_end(&path)
            .unwrap()
            .with_max_line_bytes(16);
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"ok\n\xff\xfe\n").unwrap();
        f.write_all(&[b'x'; 100]).unwrap();
        f.write_all(b"\nafter\n").unwrap();
        f.flush().unwrap();
        let lines = follower.poll_lines().unwrap();
        assert_eq!(
            lines,
            vec![
                Line::Text("ok".into()),
                Line::InvalidUtf8,
                Line::TooLong,
                Line::Text("after".into())
            ]
        );
    }

    #[test]
    fn poll_lines_hands_over_large_appends_in_bounded_batches() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("app.log");
        fs::write(&path, "").unwrap();
        let mut follower = Follower::open_at_end(&path).unwrap();
        let line = format!("{}\n", "z".repeat(1023));
        let total_lines = usize::try_from(POLL_BYTE_BUDGET / 1024).unwrap() * 3;
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        for _ in 0..total_lines {
            f.write_all(line.as_bytes()).unwrap();
        }
        f.flush().unwrap();
        let mut batches = 0;
        let mut seen = 0;
        loop {
            let lines = follower.poll_lines().unwrap();
            if lines.is_empty() {
                break;
            }
            assert!(lines.len() * 1024 <= usize::try_from(POLL_BYTE_BUDGET).unwrap());
            seen += lines.len();
            batches += 1;
        }
        assert_eq!(seen, total_lines);
        assert_eq!(batches, 3);
    }
}
