//! Keyboard → escape-sequence encoding.
//!
//! `alacritty_terminal` deliberately does NOT contain this mapping (it
//! lives in the alacritty main crate), so we write it ourselves against
//! our own key types — the engine boundary stays intact and the encoder
//! is unit-testable without a PTY or a window.
//!
//! Encoding follows xterm: plain/modified cursor keys, APP_CURSOR (DECCKM)
//! variants, tilde-suffixed editing/function keys, Ctrl control-byte
//! mapping, Alt = ESC prefix, Shift+Tab = backtab, and bracketed paste.

use super::TerminalModes;

/// A logical key, already translated from the UI toolkit's key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A printable character (post-layout, e.g. shift-a → 'A').
    Char(char),
    Enter,
    Backspace,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1..=F12.
    F(u8),
}

/// Modifier state relevant to terminal encoding. The platform (cmd/super)
/// key is intentionally absent: it is consumed by the UI, not the child.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
    };

    fn any(self) -> bool {
        self.ctrl || self.alt || self.shift
    }

    /// xterm modifier parameter: 1 + shift + 2*alt + 4*ctrl.
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }
}

/// Encode one key press into bytes for the PTY, honoring terminal modes.
/// Returns `None` for keys with no terminal meaning.
pub fn encode_key(key: Key, mods: Mods, modes: &TerminalModes) -> Option<Vec<u8>> {
    let bytes: Vec<u8> = match key {
        Key::Char(c) => return Some(encode_char(c, mods)),
        Key::Enter => alt_prefix(mods, b"\r".to_vec()),
        Key::Backspace => alt_prefix(mods, vec![0x7f]),
        Key::Tab => {
            if mods.shift {
                // Backtab; ctrl/alt get the CSI modifier form.
                if mods.ctrl || mods.alt {
                    format!("\x1b[27;{};9~", mods.param()).into_bytes()
                } else {
                    b"\x1b[Z".to_vec()
                }
            } else {
                alt_prefix(mods, b"\t".to_vec())
            }
        }
        Key::Escape => alt_prefix(mods, b"\x1b".to_vec()),

        Key::Up | Key::Down | Key::Left | Key::Right => {
            let letter = match key {
                Key::Up => b'A',
                Key::Down => b'B',
                Key::Right => b'C',
                _ => b'D',
            };
            if mods.any() {
                format!("\x1b[1;{}{}", mods.param(), letter as char).into_bytes()
            } else if modes.app_cursor {
                vec![0x1b, b'O', letter]
            } else {
                vec![0x1b, b'[', letter]
            }
        }

        Key::Home | Key::End => {
            let letter = if matches!(key, Key::Home) { b'H' } else { b'F' };
            if mods.any() {
                format!("\x1b[1;{}{}", mods.param(), letter as char).into_bytes()
            } else if modes.app_cursor {
                vec![0x1b, b'O', letter]
            } else {
                vec![0x1b, b'[', letter]
            }
        }

        Key::Delete => tilde_key(3, mods),
        Key::Insert => tilde_key(2, mods),
        Key::PageUp => tilde_key(5, mods),
        Key::PageDown => tilde_key(6, mods),

        Key::F(n) => match n {
            1..=4 => {
                let letter = b'P' + (n - 1);
                if mods.any() {
                    format!("\x1b[1;{}{}", mods.param(), letter as char).into_bytes()
                } else {
                    vec![0x1b, b'O', letter]
                }
            }
            5..=12 => {
                let code = match n {
                    5 => 15,
                    6 => 17,
                    7 => 18,
                    8 => 19,
                    9 => 20,
                    10 => 21,
                    11 => 23,
                    _ => 24,
                };
                tilde_key(code, mods)
            }
            _ => return None,
        },
    };
    Some(bytes)
}

/// Printable character with Ctrl/Alt handling.
fn encode_char(c: char, mods: Mods) -> Vec<u8> {
    if mods.ctrl {
        // Control-byte mapping: letters → 0x01..0x1a, plus the C0 set.
        let lower = c.to_ascii_lowercase();
        let byte: Option<u8> = match lower {
            'a'..='z' => Some(lower as u8 - b'a' + 1),
            '@' | '`' => Some(0x00),
            ' ' => Some(0x00),
            '[' | '{' => Some(0x1b),
            '\\' | '|' => Some(0x1c),
            ']' | '}' => Some(0x1d),
            '^' | '~' => Some(0x1e),
            '_' | '/' => Some(0x1f),
            '?' => Some(0x7f),
            _ => None,
        };
        if let Some(b) = byte {
            return alt_prefix(mods, vec![b]);
        }
        // Non-mappable ctrl combo (e.g. ctrl+%): send the char unmodified.
    }
    let mut buf = [0u8; 4];
    let s = c.encode_utf8(&mut buf);
    alt_prefix(mods, s.as_bytes().to_vec())
}

fn alt_prefix(mods: Mods, mut bytes: Vec<u8>) -> Vec<u8> {
    if mods.alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

/// `\x1b[{code}~` or `\x1b[{code};{mod}~`.
fn tilde_key(code: u8, mods: Mods) -> Vec<u8> {
    if mods.any() {
        format!("\x1b[{code};{}~", mods.param()).into_bytes()
    } else {
        format!("\x1b[{code}~").into_bytes()
    }
}

/// Encode pasted text, wrapping in bracketed-paste markers when the
/// application requested it (DECSET 2004).
pub fn encode_paste(text: &str, modes: &TerminalModes) -> Vec<u8> {
    if modes.bracketed_paste {
        let mut out = Vec::with_capacity(text.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.as_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes() -> TerminalModes {
        TerminalModes::default()
    }

    fn app_cursor() -> TerminalModes {
        TerminalModes {
            app_cursor: true,
            ..Default::default()
        }
    }

    #[test]
    fn plain_chars() {
        assert_eq!(
            encode_key(Key::Char('a'), Mods::NONE, &modes()),
            Some(b"a".to_vec())
        );
        assert_eq!(
            encode_key(Key::Char('A'), Mods::NONE, &modes()),
            Some(b"A".to_vec())
        );
        assert_eq!(
            encode_key(Key::Char('你'), Mods::NONE, &modes()),
            Some("你".as_bytes().to_vec())
        );
    }

    #[test]
    fn ctrl_combos() {
        assert_eq!(
            encode_key(
                Key::Char('c'),
                Mods {
                    ctrl: true,
                    ..Mods::NONE
                },
                &modes()
            ),
            Some(vec![0x03])
        );
        assert_eq!(
            encode_key(
                Key::Char('d'),
                Mods {
                    ctrl: true,
                    ..Mods::NONE
                },
                &modes()
            ),
            Some(vec![0x04])
        );
        // Ctrl is case-insensitive.
        assert_eq!(
            encode_key(
                Key::Char('C'),
                Mods {
                    ctrl: true,
                    ..Mods::NONE
                },
                &modes()
            ),
            Some(vec![0x03])
        );
        assert_eq!(
            encode_key(
                Key::Char('['),
                Mods {
                    ctrl: true,
                    ..Mods::NONE
                },
                &modes()
            ),
            Some(vec![0x1b])
        );
        assert_eq!(
            encode_key(
                Key::Char(' '),
                Mods {
                    ctrl: true,
                    ..Mods::NONE
                },
                &modes()
            ),
            Some(vec![0x00])
        );
    }

    #[test]
    fn alt_prefixes_esc() {
        assert_eq!(
            encode_key(
                Key::Char('x'),
                Mods {
                    alt: true,
                    ..Mods::NONE
                },
                &modes()
            ),
            Some(vec![0x1b, b'x'])
        );
        assert_eq!(
            encode_key(
                Key::Enter,
                Mods {
                    alt: true,
                    ..Mods::NONE
                },
                &modes()
            ),
            Some(vec![0x1b, b'\r'])
        );
        assert_eq!(
            encode_key(
                Key::Backspace,
                Mods {
                    alt: true,
                    ..Mods::NONE
                },
                &modes()
            ),
            Some(vec![0x1b, 0x7f])
        );
    }

    #[test]
    fn arrows_plain_vs_app_cursor() {
        assert_eq!(
            encode_key(Key::Up, Mods::NONE, &modes()),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_key(Key::Down, Mods::NONE, &modes()),
            Some(b"\x1b[B".to_vec())
        );
        assert_eq!(
            encode_key(Key::Up, Mods::NONE, &app_cursor()),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            encode_key(Key::Left, Mods::NONE, &app_cursor()),
            Some(b"\x1bOD".to_vec())
        );
    }

    #[test]
    fn arrows_with_modifiers_use_csi_params() {
        // shift+up: 1;2A, ctrl+right: 1;5C, ctrl+shift+right: 1;6C
        assert_eq!(
            encode_key(
                Key::Up,
                Mods {
                    shift: true,
                    ..Mods::NONE
                },
                &app_cursor()
            ),
            Some(b"\x1b[1;2A".to_vec())
        );
        assert_eq!(
            encode_key(
                Key::Right,
                Mods {
                    ctrl: true,
                    ..Mods::NONE
                },
                &modes()
            ),
            Some(b"\x1b[1;5C".to_vec())
        );
        assert_eq!(
            encode_key(
                Key::Right,
                Mods {
                    ctrl: true,
                    shift: true,
                    ..Mods::NONE
                },
                &app_cursor()
            ),
            Some(b"\x1b[1;6C".to_vec())
        );
    }

    #[test]
    fn home_end() {
        assert_eq!(
            encode_key(Key::Home, Mods::NONE, &modes()),
            Some(b"\x1b[H".to_vec())
        );
        assert_eq!(
            encode_key(Key::End, Mods::NONE, &app_cursor()),
            Some(b"\x1bOF".to_vec())
        );
        assert_eq!(
            encode_key(
                Key::Home,
                Mods {
                    ctrl: true,
                    ..Mods::NONE
                },
                &modes()
            ),
            Some(b"\x1b[1;5H".to_vec())
        );
    }

    #[test]
    fn editing_keys() {
        assert_eq!(
            encode_key(Key::Backspace, Mods::NONE, &modes()),
            Some(vec![0x7f])
        );
        assert_eq!(
            encode_key(Key::Delete, Mods::NONE, &modes()),
            Some(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            encode_key(
                Key::Delete,
                Mods {
                    ctrl: true,
                    ..Mods::NONE
                },
                &modes()
            ),
            Some(b"\x1b[3;5~".to_vec())
        );
        assert_eq!(
            encode_key(Key::PageUp, Mods::NONE, &modes()),
            Some(b"\x1b[5~".to_vec())
        );
        assert_eq!(
            encode_key(Key::PageDown, Mods::NONE, &modes()),
            Some(b"\x1b[6~".to_vec())
        );
    }

    #[test]
    fn function_keys() {
        assert_eq!(
            encode_key(Key::F(1), Mods::NONE, &modes()),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            encode_key(Key::F(4), Mods::NONE, &modes()),
            Some(b"\x1bOS".to_vec())
        );
        assert_eq!(
            encode_key(Key::F(5), Mods::NONE, &modes()),
            Some(b"\x1b[15~".to_vec())
        );
        assert_eq!(
            encode_key(Key::F(12), Mods::NONE, &modes()),
            Some(b"\x1b[24~".to_vec())
        );
        assert_eq!(
            encode_key(
                Key::F(5),
                Mods {
                    shift: true,
                    ..Mods::NONE
                },
                &modes()
            ),
            Some(b"\x1b[15;2~".to_vec())
        );
    }

    #[test]
    fn tab_and_backtab() {
        assert_eq!(
            encode_key(Key::Tab, Mods::NONE, &modes()),
            Some(b"\t".to_vec())
        );
        assert_eq!(
            encode_key(
                Key::Tab,
                Mods {
                    shift: true,
                    ..Mods::NONE
                },
                &modes()
            ),
            Some(b"\x1b[Z".to_vec())
        );
    }

    #[test]
    fn bracketed_paste_wraps_when_mode_set() {
        let plain = encode_paste("hello", &modes());
        assert_eq!(plain, b"hello".to_vec());
        let bracketed = encode_paste(
            "hello",
            &TerminalModes {
                bracketed_paste: true,
                ..Default::default()
            },
        );
        assert_eq!(bracketed, b"\x1b[200~hello\x1b[201~".to_vec());
    }
}
