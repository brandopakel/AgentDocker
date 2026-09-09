//! Terminal key vocabulary, independent from the window toolkit.
#[derive(Clone, Copy, Debug)]
pub enum Key {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    OpenBracket,
    Backslash,
    CloseBracket,
    Space,
    Minus,
    Enter,
    Backspace,
    Tab,
    Escape,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowUp,
    ArrowDown,
    ArrowRight,
    ArrowLeft,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub mac_cmd: bool,
}
#[derive(Clone, Debug)]
pub enum Event {
    Text(String),
    Key {
        key: Key,
        pressed: bool,
        modifiers: Modifiers,
    },
}

pub fn encode(event: &iced::keyboard::Event, application_cursor: bool) -> Vec<u8> {
    use iced::keyboard::{Event as E, Key as K, key::Named as N};
    let E::KeyPressed {
        key,
        modifiers,
        text,
        ..
    } = event
    else {
        return Vec::new();
    };
    if modifiers.logo() {
        return Vec::new();
    }
    if modifiers.control()
        && let K::Character(c) = key
    {
        let c = c.to_ascii_lowercase();
        if c.len() == 1 {
            let b = c.as_bytes()[0];
            let letters = [
                Key::A,
                Key::B,
                Key::C,
                Key::D,
                Key::E,
                Key::F,
                Key::G,
                Key::H,
                Key::I,
                Key::J,
                Key::K,
                Key::L,
                Key::M,
                Key::N,
                Key::O,
                Key::P,
                Key::Q,
                Key::R,
                Key::S,
                Key::T,
                Key::U,
                Key::V,
                Key::W,
                Key::X,
                Key::Y,
                Key::Z,
            ];
            let key = match b {
                b'a'..=b'z' => Some(letters[(b - b'a') as usize]),
                b'[' => Some(Key::OpenBracket),
                b'\\' => Some(Key::Backslash),
                b']' => Some(Key::CloseBracket),
                b' ' | b'@' => Some(Key::Space),
                b'_' | b'-' => Some(Key::Minus),
                b'^' => return vec![30],
                _ => None,
            };
            if let Some(key) = key {
                return super::keystrokes(&[Event::Key {
                    key,
                    pressed: true,
                    modifiers: Modifiers {
                        ctrl: true,
                        mac_cmd: false,
                    },
                }]);
            }
        }
    }
    let named = match key {
        K::Named(N::Enter) => Some(Key::Enter),
        K::Named(N::Backspace) => Some(Key::Backspace),
        K::Named(N::Tab) => Some(Key::Tab),
        K::Named(N::Escape) => Some(Key::Escape),
        K::Named(N::Delete) => Some(Key::Delete),
        K::Named(N::Home) => Some(Key::Home),
        K::Named(N::End) => Some(Key::End),
        K::Named(N::PageUp) => Some(Key::PageUp),
        K::Named(N::PageDown) => Some(Key::PageDown),
        K::Named(N::ArrowUp) => Some(Key::ArrowUp),
        K::Named(N::ArrowDown) => Some(Key::ArrowDown),
        K::Named(N::ArrowLeft) => Some(Key::ArrowLeft),
        K::Named(N::ArrowRight) => Some(Key::ArrowRight),
        _ => None,
    };
    if matches!(named, Some(Key::Tab)) && modifiers.shift() {
        return b"\x1b[Z".to_vec();
    }
    let mut bytes = if let Some(key) = named {
        super::keystrokes(&[Event::Key {
            key,
            pressed: true,
            modifiers: Modifiers::default(),
        }])
    } else {
        text.as_ref()
            .map(|t| super::keystrokes(&[Event::Text(t.to_string())]))
            .unwrap_or_default()
    };
    if application_cursor
        && bytes.len() == 3
        && bytes[..2] == *b"\x1b["
        && b"ABCDHF".contains(&bytes[2])
    {
        bytes[1] = b'O';
    }
    if modifiers.alt() && !bytes.is_empty() {
        bytes.insert(0, 27);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::{Event as E, Key as K, Modifiers as M, key::Named as N};
    fn event(key: K, modifiers: M, text: Option<&str>) -> E {
        E::KeyPressed {
            modified_key: key.clone(),
            key,
            physical_key: iced::keyboard::key::Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced::keyboard::Location::Standard,
            modifiers,
            text: text.map(Into::into),
            repeat: false,
        }
    }
    #[test]
    fn iced_unicode_control_application_cursor_and_escape_keys_reach_the_vt_stream() {
        assert_eq!(
            encode(
                &event(K::Character("λ".into()), M::empty(), Some("λ")),
                false
            ),
            "λ".as_bytes()
        );
        assert_eq!(
            encode(&event(K::Character("]".into()), M::CTRL, None), false),
            [29]
        );
        assert_eq!(
            encode(&event(K::Named(N::Tab), M::SHIFT, None), false),
            b"\x1b[Z"
        );
        assert_eq!(
            encode(&event(K::Named(N::ArrowUp), M::empty(), None), true),
            b"\x1bOA"
        );
        assert_eq!(
            encode(&event(K::Character("b".into()), M::ALT, Some("b")), false),
            b"\x1bb"
        );
        assert!(encode(&event(K::Named(N::F6), M::empty(), None), false).is_empty());
        assert!(encode(&event(K::Character("q".into()), M::LOGO, Some("q")), false).is_empty());
    }
}
