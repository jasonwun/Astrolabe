//! Bytes as a terminal must receive them.

/// Log bytes as a terminal must receive them: a bare `\n` only moves the
/// cursor down, so each gets the carriage return a TTY would add. A `\r`
/// ending one chunk before the next chunk's `\n` gains a harmless second
/// `\r`.
pub fn terminal_bytes(chunk: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(chunk.len() + chunk.len() / 16);
    for (index, byte) in chunk.iter().enumerate() {
        if *byte == b'\n' && (index == 0 || chunk[index - 1] != b'\r') {
            bytes.push(b'\r');
        }
        bytes.push(*byte);
    }
    bytes
}
