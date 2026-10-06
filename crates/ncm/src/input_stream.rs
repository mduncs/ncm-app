//! Decode the input ncm uses while retaining terminal replies alongside ordinary events.
use crate::input::Input;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

#[derive(Default)]
pub struct Decoder {
    bytes: Vec<u8>,
}
impl Decoder {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Input> {
        self.bytes.extend_from_slice(bytes);
        self.decode(false)
    }
    pub fn idle(&mut self) -> Vec<Input> {
        self.decode(true)
    }
    fn decode(&mut self, idle: bool) -> Vec<Input> {
        let mut events = Vec::new();
        let mut offset = 0;
        while offset < self.bytes.len() {
            let bytes = &self.bytes[offset..];
            let (len, event) = if bytes[0] != 27 {
                let Some((len, key)) = character(bytes, KeyModifiers::NONE) else {
                    break;
                };
                (len, Some(Input::Event(Event::Key(key))))
            } else if bytes.len() == 1 {
                if !idle {
                    break;
                }
                (1, Some(key(KeyCode::Esc, KeyModifiers::NONE)))
            } else if matches!(bytes[1], b'_' | b']' | b'P') {
                let end = bytes
                    .windows(2)
                    .position(|b| b == b"\x1b\\")
                    .map(|n| n + 2)
                    .or_else(|| {
                        (bytes[1] == b']')
                            .then(|| bytes.iter().position(|b| *b == 7))
                            .flatten()
                            .map(|n| n + 1)
                    });
                let Some(end) = end else {
                    if bytes.len() > 4096 {
                        offset = self.bytes.len();
                    }
                    break;
                };
                let reply = bytes[1] == b'_' && bytes[2..end].windows(8).any(|b| b == b"i=255;OK");
                (end, reply.then_some(Input::GraphicsReply))
            } else if bytes.starts_with(b"\x1b[200~") {
                let Some(end) = bytes[6..].windows(6).position(|b| b == b"\x1b[201~") else {
                    break;
                };
                (
                    end + 12,
                    Some(Input::Event(Event::Paste(
                        String::from_utf8_lossy(&bytes[6..end + 6]).into_owned(),
                    ))),
                )
            } else if bytes[1] == b'[' {
                let Some(end) = bytes[2..]
                    .iter()
                    .position(|b| (0x40..=0x7e).contains(b))
                    .map(|n| n + 2)
                else {
                    if bytes.len() > 256 {
                        offset = self.bytes.len();
                    }
                    break;
                };
                (end + 1, csi(&bytes[..end + 1]))
            } else if bytes[1] == b'O' {
                if bytes.len() < 3 {
                    break;
                }
                (
                    3,
                    navigation(bytes[2]).map(|code| key(code, KeyModifiers::NONE)),
                )
            } else {
                let Some((len, event)) = character(&bytes[1..], KeyModifiers::ALT) else {
                    break;
                };
                (len + 1, Some(Input::Event(Event::Key(event))))
            };
            offset += len;
            if let Some(event) = event {
                events.push(event);
            }
        }
        self.bytes.drain(..offset);
        events
    }
}
fn key(code: KeyCode, modifiers: KeyModifiers) -> Input {
    Input::Event(Event::Key(KeyEvent::new(code, modifiers)))
}
fn character(bytes: &[u8], mut modifiers: KeyModifiers) -> Option<(usize, KeyEvent)> {
    let first = *bytes.first()?;
    let len = match first {
        0..=127 => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    };
    if bytes.len() < len {
        return None;
    }
    let code = match first {
        9 => KeyCode::Tab,
        10 | 13 => KeyCode::Enter,
        27 => KeyCode::Esc,
        8 | 127 => KeyCode::Backspace,
        0..=31 => {
            modifiers |= KeyModifiers::CONTROL;
            KeyCode::Char(if first == 0 {
                '@'
            } else {
                char::from(first + if first < 27 { 96 } else { 64 })
            })
        }
        _ => KeyCode::Char(
            std::str::from_utf8(&bytes[..len])
                .ok()
                .and_then(|s| s.chars().next())
                .unwrap_or('\u{fffd}'),
        ),
    };
    Some((len, KeyEvent::new(code, modifiers)))
}
fn navigation(final_byte: u8) -> Option<KeyCode> {
    Some(match final_byte {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        _ => return None,
    })
}
fn modifiers(value: u16) -> KeyModifiers {
    let bits = value.saturating_sub(1);
    let mut modifiers = KeyModifiers::NONE;
    for (mask, modifier) in [
        (1, KeyModifiers::SHIFT),
        (2, KeyModifiers::ALT),
        (4, KeyModifiers::CONTROL),
    ] {
        if bits & mask != 0 {
            modifiers |= modifier;
        }
    }
    modifiers
}
fn csi(bytes: &[u8]) -> Option<Input> {
    let text = std::str::from_utf8(bytes).ok()?;
    if let Some(pixels) = crate::terminal::parse_cell_reply(text) {
        return Some(Input::Pixels(pixels));
    }
    let final_byte = *bytes.last()?;
    let body = std::str::from_utf8(&bytes[2..bytes.len() - 1]).ok()?;
    if let Some(mouse) = body.strip_prefix('<') {
        let values = mouse
            .split(';')
            .map(str::parse::<u16>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        let [button, column, row] = values.as_slice() else {
            return None;
        };
        let button_kind = match button & 3 {
            0 => MouseButton::Left,
            1 => MouseButton::Middle,
            _ => MouseButton::Right,
        };
        let kind = if button & 64 != 0 {
            match button & 3 {
                0 => MouseEventKind::ScrollUp,
                1 => MouseEventKind::ScrollDown,
                2 => MouseEventKind::ScrollLeft,
                _ => MouseEventKind::ScrollRight,
            }
        } else if final_byte == b'm' {
            MouseEventKind::Up(button_kind)
        } else if button & 32 != 0 {
            if button & 3 == 3 {
                MouseEventKind::Moved
            } else {
                MouseEventKind::Drag(button_kind)
            }
        } else {
            MouseEventKind::Down(button_kind)
        };
        let mut modifiers = KeyModifiers::NONE;
        for (mask, modifier) in [
            (4, KeyModifiers::SHIFT),
            (8, KeyModifiers::ALT),
            (16, KeyModifiers::CONTROL),
        ] {
            if button & mask != 0 {
                modifiers |= modifier;
            }
        }
        return Some(Input::Event(Event::Mouse(MouseEvent {
            kind,
            column: column.checked_sub(1)?,
            row: row.checked_sub(1)?,
            modifiers,
        })));
    }
    let values = body
        .split(';')
        .map(|s| s.parse::<u16>().unwrap_or(1))
        .collect::<Vec<_>>();
    let mods = modifiers(*values.get(1).unwrap_or(&1));
    if let Some(code) = navigation(final_byte) {
        return Some(key(code, mods));
    }
    let code = match final_byte {
        b'Z' => return Some(key(KeyCode::BackTab, KeyModifiers::SHIFT)),
        b'~' => match values[0] {
            1 | 7 => KeyCode::Home,
            2 => KeyCode::Insert,
            3 => KeyCode::Delete,
            4 | 8 => KeyCode::End,
            5 => KeyCode::PageUp,
            6 => KeyCode::PageDown,
            _ => return None,
        },
        b'u' => KeyCode::Char(char::from_u32(u32::from(values[0]))?),
        _ => return None,
    };
    Some(key(code, mods))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_late_reply_does_not_consume_keys_or_paste() {
        let mut decoder = Decoder::default();
        assert!(decoder.feed(b"\x1b_Gi=255;").is_empty());
        assert!(decoder.idle().is_empty());
        let events = decoder.feed(b"OK\x1b\\\x1b[C\x1b[200~i=255;OK\x1b[201~q");
        assert!(matches!(events[0], Input::GraphicsReply));
        assert!(matches!(
            events[1],
            Input::Event(Event::Key(KeyEvent {
                code: KeyCode::Right,
                ..
            }))
        ));
        assert!(matches!(&events[2], Input::Event(Event::Paste(text)) if text == "i=255;OK"));
        assert!(matches!(
            events[3],
            Input::Event(Event::Key(KeyEvent {
                code: KeyCode::Char('q'),
                ..
            }))
        ));
    }
    #[test]
    fn pixels_utf8_mouse_and_escape_survive_fragmentation() {
        let mut decoder = Decoder::default();
        let bytes = "\x1b[6;18;9t界\x1b[<0;4;5M\x1b".as_bytes();
        let mut events = Vec::new();
        for byte in bytes {
            events.extend(decoder.feed(&[*byte]));
        }
        events.extend(decoder.idle());
        assert!(matches!(events[0], Input::Pixels((9, 18))));
        assert!(matches!(
            events[1],
            Input::Event(Event::Key(KeyEvent {
                code: KeyCode::Char('界'),
                ..
            }))
        ));
        assert!(matches!(
            events[2],
            Input::Event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 3,
                row: 4,
                ..
            }))
        ));
        assert!(matches!(
            events[3],
            Input::Event(Event::Key(KeyEvent {
                code: KeyCode::Esc,
                ..
            }))
        ));
    }
}
