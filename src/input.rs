use alacritty_terminal::term::TermMode;
use rune::clipboard::ClipboardContent;
use rune::{Key, KeyEvent, Mods};

pub fn key(event: &KeyEvent, mode: TermMode) -> Vec<u8> {
    let m = event.mods;
    let ctrl = m.ctrl || m.cmd;
    let modifiers = 1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(ctrl);
    let enhanced =
        mode.intersects(TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALL_KEYS_AS_ESC);
    let csi_u = |code: u32| format!("\x1b[{code};{modifiers}u");
    let seq = match &event.key {
        Key::Text(s) if (s == " " && ctrl && !m.alt) => {
            if enhanced {
                csi_u(32)
            } else {
                "\0".into()
            }
        }
        Key::Text(s) if m.alt && !ctrl => format!("\x1b{s}"),
        Key::Text(s) => s.clone(),
        Key::Command(c) if enhanced => csi_u(*c as u32),
        Key::Command(c) => {
            let ch = c.to_ascii_uppercase();
            let b = match ch {
                '@'..='_' => ch as u8 & 31,
                ' ' | '2' => 0,
                '?' | '8' => 127,
                _ => return vec![],
            };
            return if m.alt { vec![27, b] } else { vec![b] };
        }
        Key::Enter | Key::Tab | Key::Backspace | Key::Escape => {
            let code = match event.key {
                Key::Enter => 13,
                Key::Tab => 9,
                Key::Backspace => 127,
                _ => 27,
            };
            if enhanced
                && (modifiers != 1
                    || matches!(event.key, Key::Escape)
                    || mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC))
            {
                csi_u(code)
            } else if event.key == Key::Tab && m.shift {
                "\x1b[Z".into()
            } else if event.key == Key::Backspace && ctrl {
                "\x08".into()
            } else {
                format!("{}{}", if m.alt { "\x1b" } else { "" }, code as u8 as char)
            }
        }
        Key::Up
        | Key::Down
        | Key::Right
        | Key::Left
        | Key::Home
        | Key::End
        | Key::Function(1..=4) => {
            let last = match event.key {
                Key::Up => 'A',
                Key::Down => 'B',
                Key::Right => 'C',
                Key::Left => 'D',
                Key::Home => 'H',
                Key::End => 'F',
                Key::Function(n) => (b'P' + n - 1) as char,
                _ => unreachable!(),
            };
            if modifiers != 1 {
                format!("\x1b[1;{modifiers}{last}")
            } else if mode.contains(TermMode::APP_CURSOR) || matches!(event.key, Key::Function(_)) {
                format!("\x1bO{last}")
            } else {
                format!("\x1b[{last}")
            }
        }
        Key::Insert | Key::Delete | Key::PageUp | Key::PageDown | Key::Function(5..=12) => {
            let n = match event.key {
                Key::Insert => 2,
                Key::Delete => 3,
                Key::PageUp => 5,
                Key::PageDown => 6,
                Key::Function(n) => [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)],
                _ => unreachable!(),
            };
            if modifiers == 1 {
                format!("\x1b[{n}~")
            } else {
                format!("\x1b[{n};{modifiers}~")
            }
        }
        _ => return vec![],
    };
    seq.into_bytes()
}

pub fn clipboard(content: &ClipboardContent, mode: TermMode) -> Vec<u8> {
    if !content.files().is_empty() {
        let paths = content
            .files()
            .iter()
            .filter_map(|path| {
                let path = path.to_string_lossy();
                (!path.is_empty() && !path.contains(|ch: char| ch == '"' || ch.is_control()))
                    .then(|| format!("\"{path}\""))
            })
            .collect::<Vec<_>>()
            .join(" ");
        return paste(&paths, mode);
    }
    if content.image().is_some() {
        // The official CLI reads the Windows bitmap when it receives Ctrl+V.
        return vec![0x16];
    }
    paste(content.text(), mode)
}

pub fn paste(text: &str, mode: TermMode) -> Vec<u8> {
    if text.is_empty() {
        return vec![];
    }
    let s = text.replace("\r\n", "\n").replace('\r', "\n");
    if mode.contains(TermMode::BRACKETED_PASTE) {
        format!("\x1b[200~{}\x1b[201~", s.replace(['\x1b', '\u{9b}'], "")).into_bytes()
    } else {
        s.replace('\n', "\r").into_bytes()
    }
}

pub fn mouse(button: u8, col: u16, row: u16, up: bool, mods: Mods, mode: TermMode) -> Vec<u8> {
    let code =
        button + 4 * u8::from(mods.shift) + 8 * u8::from(mods.alt) + 16 * u8::from(mods.ctrl);
    if mode.contains(TermMode::SGR_MOUSE) {
        return format!(
            "\x1b[<{code};{};{}{}",
            col + 1,
            row + 1,
            if up { 'm' } else { 'M' }
        )
        .into_bytes();
    }
    let utf8 = mode.contains(TermMode::UTF8_MOUSE);
    if col >= if utf8 { 2015 } else { 223 } || row >= if utf8 { 2015 } else { 223 } {
        return vec![];
    }
    let mut bytes = vec![27, b'[', b'M', 32 + if up { (code & !3) | 3 } else { code }];
    for value in [col + 33, row + 33] {
        if utf8 {
            bytes.extend(
                char::from_u32(u32::from(value))
                    .unwrap()
                    .encode_utf8(&mut [0; 4])
                    .as_bytes(),
            );
        } else {
            bytes.push(value as u8);
        }
    }
    bytes
}
