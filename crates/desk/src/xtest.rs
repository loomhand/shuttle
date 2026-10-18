//! Typing through XTest on the core keyboard, for text whose every character the current keyboard map holds.

use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{ConnectionExt as _, KEY_PRESS_EVENT, KEY_RELEASE_EVENT};
use x11rb::protocol::xtest::ConnectionExt as _;

const SHIFT_L: u32 = 0xffe1;
const TAB: u32 = 0xff09;

/// Types `text` as key presses of the keyboard map's own keycodes; false, with nothing typed, when a character
/// is not on the map, which the driver's typing then handles.
pub fn type_mapped(text: &str) -> Result<bool, String> {
    let (conn, n) = x11rb::connect(None).map_err(|e| format!("connect X: {e}"))?;
    let setup = conn.setup();
    let root = setup.roots[n].root;
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let map = conn
        .get_keyboard_mapping(min, max - min + 1)
        .map_err(|e| format!("keyboard map: {e}"))?
        .reply()
        .map_err(|e| format!("keyboard map: {e}"))?;
    let per = usize::from(map.keysyms_per_keycode.max(1));
    let find = |keysym: u32| {
        map.keysyms.chunks(per).enumerate().find_map(|(i, syms)| {
            let code = min.checked_add(u8::try_from(i).ok()?)?;
            match syms.iter().position(|s| *s == keysym) {
                Some(0) => Some((code, false)),
                Some(1) => Some((code, true)),
                _ => None,
            }
        })
    };
    let Some((shift, _)) = find(SHIFT_L) else {
        return Ok(false);
    };
    let keys: Option<Vec<(u8, bool)>> = text.chars().map(|c| find(keysym(c)?)).collect();
    let Some(keys) = keys else {
        return Ok(false);
    };
    let fake = |kind: u8, code: u8| {
        conn.xtest_fake_input(kind, code, x11rb::CURRENT_TIME, root, 0, 0, 0)
            .map(|_| ())
            .map_err(|e| format!("XTest: {e}"))
    };
    for (code, shifted) in keys {
        if shifted {
            fake(KEY_PRESS_EVENT, shift)?;
        }
        fake(KEY_PRESS_EVENT, code)?;
        fake(KEY_RELEASE_EVENT, code)?;
        if shifted {
            fake(KEY_RELEASE_EVENT, shift)?;
        }
    }
    conn.get_input_focus()
        .map_err(|e| format!("XTest: {e}"))?
        .reply()
        .map_err(|e| format!("XTest: {e}"))?;
    Ok(true)
}

/// The keysym of a printable ASCII character or a tab, whose Latin-1 keysyms equal their code points.
fn keysym(c: char) -> Option<u32> {
    match c {
        ' '..='~' => Some(u32::from(c)),
        '\t' => Some(TAB),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_ascii_and_tab_have_keysyms_and_other_text_has_none() {
        assert_eq!(keysym('a'), Some(0x61));
        assert_eq!(keysym('A'), Some(0x41));
        assert_eq!(keysym('/'), Some(0x2f));
        assert_eq!(keysym('\t'), Some(TAB));
        assert_eq!(keysym('é'), None);
        assert_eq!(keysym('草'), None);
        assert_eq!(keysym('\n'), None);
    }
}
