//! Client-to-device control messages (scrcpy wire format, big-endian).

pub const ACTION_DOWN: u8 = 0;
pub const ACTION_UP: u8 = 1;
pub const ACTION_MOVE: u8 = 2;

/// Pointer id scrcpy uses for the mouse, as opposed to a finger.
pub const POINTER_MOUSE: i64 = -1;
pub const BUTTON_PRIMARY: i32 = 1;
pub const BUTTON_SECONDARY: i32 = 2;

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    Key { action: u8, keycode: i32, repeat: i32, meta: i32 },
    Text(String),
    Touch { action: u8, pointer: i64, x: i32, y: i32, w: u16, h: u16, pressure: f32, action_button: i32, buttons: i32 },
    Scroll { x: i32, y: i32, w: u16, h: u16, hscroll: f32, vscroll: f32, buttons: i32 },
    BackOrScreenOn { action: u8 },
    ExpandNotifications,
    ExpandSettings,
    CollapsePanels,
    SetClipboard { text: String, paste: bool },
    StartApp(String),
    ResizeDisplay { w: u16, h: u16 },
}

impl Msg {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(32);
        match self {
            Msg::Key { action, keycode, repeat, meta } => {
                b.push(0);
                b.push(*action);
                b.extend(keycode.to_be_bytes());
                b.extend(repeat.to_be_bytes());
                b.extend(meta.to_be_bytes());
            }
            Msg::Text(text) => {
                b.push(1);
                string(&mut b, text, 4);
            }
            Msg::Touch { action, pointer, x, y, w, h, pressure, action_button, buttons } => {
                b.push(2);
                b.push(*action);
                b.extend(pointer.to_be_bytes());
                position(&mut b, *x, *y, *w, *h);
                b.extend(unorm16(*pressure).to_be_bytes());
                b.extend(action_button.to_be_bytes());
                b.extend(buttons.to_be_bytes());
            }
            Msg::Scroll { x, y, w, h, hscroll, vscroll, buttons } => {
                b.push(3);
                position(&mut b, *x, *y, *w, *h);
                // The server reads these as fixed-point in [-1, 1] and multiplies by 16.
                b.extend(snorm16(hscroll / 16.0).to_be_bytes());
                b.extend(snorm16(vscroll / 16.0).to_be_bytes());
                b.extend(buttons.to_be_bytes());
            }
            Msg::BackOrScreenOn { action } => b.extend([4, *action]),
            Msg::ExpandNotifications => b.push(5),
            Msg::ExpandSettings => b.push(6),
            Msg::CollapsePanels => b.push(7),
            Msg::SetClipboard { text, paste } => {
                b.push(9);
                b.extend(0u64.to_be_bytes()); // sequence: 0 = no ack wanted
                b.push(*paste as u8);
                string(&mut b, text, 4);
            }
            Msg::StartApp(name) => {
                b.push(16);
                string(&mut b, name, 1);
            }
            Msg::ResizeDisplay { w, h } => {
                b.push(21);
                b.extend(w.to_be_bytes());
                b.extend(h.to_be_bytes());
            }
        }
        b
    }
}

fn position(b: &mut Vec<u8>, x: i32, y: i32, w: u16, h: u16) {
    b.extend(x.to_be_bytes());
    b.extend(y.to_be_bytes());
    b.extend(w.to_be_bytes());
    b.extend(h.to_be_bytes());
}

/// Length-prefixed UTF-8, truncated on a char boundary to fit the prefix width.
fn string(b: &mut Vec<u8>, s: &str, len_bytes: usize) {
    let max = if len_bytes == 1 { u8::MAX as usize } else { 1 << 18 };
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    let len = (end as u32).to_be_bytes();
    b.extend(&len[4 - len_bytes..]);
    b.extend(&s.as_bytes()[..end]);
}

fn unorm16(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 65535.0).round() as u16
}

fn snorm16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_layout() {
        let m = Msg::Touch {
            action: ACTION_DOWN, pointer: POINTER_MOUSE, x: 100, y: 200, w: 1280, h: 800,
            pressure: 1.0, action_button: BUTTON_PRIMARY, buttons: BUTTON_PRIMARY,
        };
        let b = m.encode();
        assert_eq!(b.len(), 32);
        assert_eq!(&b[..2], &[2, 0]);
        assert_eq!(&b[2..10], &[0xff; 8]);
        assert_eq!(&b[10..14], &100i32.to_be_bytes());
        assert_eq!(&b[18..20], &1280u16.to_be_bytes());
        assert_eq!(&b[22..24], &[0xff, 0xff]);
    }

    #[test]
    fn start_app_and_resize() {
        assert_eq!(Msg::StartApp("a.b".into()).encode(), [16, 3, b'a', b'.', b'b']);
        assert_eq!(Msg::ResizeDisplay { w: 1024, h: 768 }.encode(), [21, 4, 0, 3, 0]);
    }

    #[test]
    fn string_truncates_on_char_boundary() {
        let long = "é".repeat(200); // 400 bytes
        let b = Msg::StartApp(long).encode();
        assert_eq!(b[1], 254);
        assert!(std::str::from_utf8(&b[2..]).is_ok());
    }
}
