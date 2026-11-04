//! Verifies the terminal query responder used by interactive worktree flows.

use pretty_assertions::assert_eq;

// Retain only the unfinished terminal query, never the full output stream.
#[derive(Default)]
struct TerminalQueries(Vec<u8>);

impl TerminalQueries {
    fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut replies = Vec::new();
        for &byte in bytes {
            self.0.push(byte);
            if self.0 == b"\x1b[6n" {
                replies.extend_from_slice(b"\x1b[1;1R");
                self.0.clear();
            } else if self.0 == b"\x1b[c" {
                replies.extend_from_slice(b"\x1b[?1;2c");
                self.0.clear();
            } else {
                while !b"\x1b[6n".starts_with(&self.0) && !b"\x1b[c".starts_with(&self.0) {
                    self.0.remove(0);
                }
            }
        }
        replies
    }
}

#[test]
fn terminal_queries_reply_once_across_every_split() {
    let input = b"noise\x1b[6n\x1b[c\x1b[6n";
    let expected = b"\x1b[1;1R\x1b[?1;2c\x1b[1;1R";
    for split in 0..=input.len() {
        let mut queries = TerminalQueries::default();
        let mut replies = queries.feed(&input[..split]);
        replies.extend(queries.feed(&input[split..]));
        assert_eq!(replies, expected);
        assert_eq!(queries.feed(b"more output"), Vec::<u8>::new());
    }
}
