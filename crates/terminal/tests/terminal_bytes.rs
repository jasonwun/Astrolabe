//! Log bytes as a terminal displays them (#17).

use astrolabe_terminal::terminal_bytes;

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

#[test]
fn an_empty_chunk_stays_empty() {
    assert_eq!(terminal_bytes(b""), b"");
}

#[test]
fn a_lone_line_feed_gains_a_carriage_return() {
    assert_eq!(terminal_bytes(b"\n"), b"\r\n");
}

#[test]
fn a_chunk_ending_in_a_carriage_return_is_unchanged() {
    assert_eq!(terminal_bytes(b"a\r"), b"a\r");
}

#[test]
fn a_carriage_return_ending_one_chunk_gains_a_second_before_the_next_chunks_line_feed() {
    let joined = [terminal_bytes(b"a\r"), terminal_bytes(b"\nb")].concat();
    assert_eq!(joined, b"a\r\r\nb");
}
