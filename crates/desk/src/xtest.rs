//! Typing through XTest on the core keyboard, for text whose every character the current keyboard map holds.

use std::sync::atomic::{AtomicBool, Ordering};

use x11rb::connection::{Connection as _, RequestConnection as _};
use x11rb::cookie::VoidCookie;
use x11rb::protocol::xkb::{ConnectionExt as _, ID};
use x11rb::protocol::xproto::{ConnectionExt as _, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, ModMask};
use x11rb::protocol::xtest::{self, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

const SHIFT_L: u32 = 0xffe1;
const TAB: u32 = 0xff09;

/// Types `text` with the keyboard map's own keycodes; false, typing nothing, when a character or modifier state needs the
/// driver or X is unreachable. Once `stop` is set no further key goes out, and an error leaves no key held.
pub fn type_mapped(text: &str, stop: &AtomicBool) -> Result<bool, String> {
    let Ok((conn, n)) = x11rb::connect(None) else {
        return Ok(false);
    };
    let setup = conn.setup();
    let root = setup.roots[n].root;
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let _ = conn.prefetch_extension_information(xtest::X11_EXTENSION_NAME);
    let map = conn.get_keyboard_mapping(min, max - min + 1);
    if !plain(&conn).unwrap_or(false) {
        return Ok(false);
    }
    let Some(map) = map.ok().and_then(|c| c.reply().ok()) else {
        return Ok(false);
    };
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
    let mut sent: Vec<VoidCookie<'_, RustConnection>> = Vec::new();
    let mut held: Vec<u8> = Vec::new();
    let typed = (|| {
        for (code, shifted) in keys {
            let events: &[(u8, u8)] = if shifted {
                &[
                    (KEY_PRESS_EVENT, shift),
                    (KEY_PRESS_EVENT, code),
                    (KEY_RELEASE_EVENT, code),
                    (KEY_RELEASE_EVENT, shift),
                ]
            } else {
                &[(KEY_PRESS_EVENT, code), (KEY_RELEASE_EVENT, code)]
            };
            for &(kind, key) in events {
                if stop.load(Ordering::Relaxed) {
                    return Err("stopped by the user".to_owned());
                }
                sent.push(
                    conn.xtest_fake_input(kind, key, x11rb::CURRENT_TIME, root, 0, 0, 0)
                        .map_err(|e| format!("XTest: {e}"))?,
                );
                if kind == KEY_PRESS_EVENT {
                    held.push(key);
                } else {
                    held.retain(|k| *k != key);
                }
            }
        }
        Ok(())
    })();
    for &key in held.iter().rev() {
        let _ = conn.xtest_fake_input(KEY_RELEASE_EVENT, key, x11rb::CURRENT_TIME, root, 0, 0, 0);
    }
    conn.get_input_focus()
        .map_err(|e| format!("XTest: {e}"))?
        .reply()
        .map_err(|e| format!("XTest: {e}"))?;
    typed?;
    for cookie in sent {
        cookie.check().map_err(|e| format!("XTest: {e}"))?;
    }
    Ok(true)
}

/// Whether the core keyboard is on its first group with no lock modifier, so a keycode types its map's first or shifted keysym.
fn plain(conn: &RustConnection) -> Result<bool, String> {
    let ext = conn
        .xkb_use_extension(1, 0)
        .map_err(|e| format!("XKB: {e}"))?;
    let state = conn
        .xkb_get_state(ID::USE_CORE_KBD.into())
        .map_err(|e| format!("XKB: {e}"))?;
    if !ext.reply().map_err(|e| format!("XKB: {e}"))?.supported {
        return Ok(false);
    }
    let state = state.reply().map_err(|e| format!("XKB: {e}"))?;
    Ok(u8::from(state.group) == 0 && !state.locked_mods.intersects(ModMask::LOCK | ModMask::SHIFT))
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
