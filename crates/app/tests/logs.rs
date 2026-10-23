//! Log bytes as a terminal displays them (#17).

use astrolabe::logs::terminal_bytes;

#[test]
fn line_feeds_return_the_carriage() {
    assert_eq!(terminal_bytes(b"a\nb\n"), b"a\r\nb\r\n");
}

#[test]
fn existing_carriage_returns_are_kept_once() {
    assert_eq!(terminal_bytes(b"a\r\nb\rc\n"), b"a\r\nb\rc\r\n");
}

#[test]
fn ansi_sequences_pass_through() {
    assert_eq!(terminal_bytes(b"\x1b[31merr\x1b[0m"), b"\x1b[31merr\x1b[0m");
}
