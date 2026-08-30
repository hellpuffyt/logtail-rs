//! Follow mode (`-f`): tails a file for new lines, detecting log rotation
//! (the file at `path` being replaced by a new inode/file, as `logrotate`
//! or similar tools do) so that following does not silently go stale the
//! way plain `tail -f` can.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[cfg(unix)]
fn file_identity(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(windows)]
fn file_identity(meta: &fs::Metadata) -> u64 {
    use std::os::windows::fs::MetadataExt;
    meta.file_index().unwrap_or(0)
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
        })
    }

    /// Checks for rotation/truncation and returns any newly available
    /// complete lines since the last call. A trailing partial line (no
    /// terminating `\n` yet) is left unconsumed for the next poll.
    ///
    /// # Errors
    /// Returns an error if a filesystem operation fails. A momentarily
    /// missing file (mid-rotation) is tolerated and simply yields no lines.
    pub fn poll(&mut self) -> io::Result<Vec<String>> {
        self.reopen_if_rotated();

        let len = self.file.metadata()?.len();
        if len < self.offset {
            // Truncated in place (e.g. `> file` or copytruncate rotation).
            self.offset = 0;
        }

        self.file.seek(SeekFrom::Start(self.offset))?;
        let mut lines = Vec::new();
        let mut reader = BufReader::new(&self.file);
        loop {
            let mut buf = String::new();
            let n = reader.read_line(&mut buf)?;
            if n == 0 {
                break;
            }
            if buf.ends_with('\n') {
                self.offset += n as u64;
                let trimmed = buf.trim_end_matches(['\r', '\n']).to_string();
                lines.push(trimmed);
            } else {
                // Partial line; wait for more data before consuming it.
                break;
            }
        }
        Ok(lines)
    }

    fn reopen_if_rotated(&mut self) {
        let Ok(disk_meta) = fs::metadata(&self.path) else {
            // File momentarily missing mid-rotation; try again next poll.
            return;
        };
        let disk_identity = file_identity(&disk_meta);
        if disk_identity != self.identity {
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
        assert!(follower.poll().unwrap().is_empty());

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
        assert!(follower.poll().unwrap().is_empty());

        // Simulate logrotate: move the old file aside, create a fresh one
        // at the same path.
        let rotated_path = dir.path().join("app.log.1");
        fs::rename(&path, &rotated_path).unwrap();
        fs::write(&path, "fresh-1\n").unwrap();
        sleep(Duration::from_millis(10));

        let lines = follower.poll().unwrap();
        assert_eq!(lines, vec!["fresh-1"]);
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
        assert!(follower.poll().unwrap().is_empty());

        writeln!(f, "two").unwrap();
        f.flush().unwrap();
        assert_eq!(follower.poll().unwrap(), vec!["two"]);
    }
}
