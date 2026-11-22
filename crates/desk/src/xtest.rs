//! Input through XTest: typing on the core keyboard, for text whose every character the current keyboard map holds, and pointer paths.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use x11rb::connection::{Connection as _, RequestConnection as _};
use x11rb::cookie::VoidCookie;
use x11rb::protocol::xkb::{ConnectionExt as _, ID};
use x11rb::protocol::xproto::{
    BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConnectionExt as _, KEY_PRESS_EVENT,
    KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT, ModMask, Window,
};
use x11rb::protocol::xtest::{self, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::desk::{Clicks, Gesture};

const SHIFT_L: u32 = 0xffe1;
const TAB: u32 = 0xff09;
const HOLD_MS: (u64, u64) = (30, 70);
const GAP_MS: (u64, u64) = (40, 120);
const MODIFIER_MS: (u64, u64) = (10, 30);
const SETTLE_MS: (u64, u64) = (40, 120);
const RELEASE_MS: (u64, u64) = (20, 50);
const STOPPED: &str = "stopped by the user";
const CLICK_HOLD_MS: (u64, u64) = (60, 120);
const MULTI_HOLD_MS: (u64, u64) = (40, 80);
const MULTI_GAP_MS: (u64, u64) = (60, 110);
const PATH_STEPS: (u64, u64) = (35, 60);
const PATH_STEP_MS: (u64, u64) = (8, 20);
const PATH_BEND_PX: (f64, f64) = (120.0, 80.0);

/// One XTest event, its keycode or button, and the pause after it.
type Step = (u8, u8, Option<(u64, u64)>);

/// Types `text` with the keyboard map's own keycodes; false, typing nothing, when a character or modifier state needs the driver or X is unreachable.
pub fn type_mapped(text: &str, human: bool, stop: &AtomicBool) -> Result<bool, String> {
    let Ok((conn, n)) = x11rb::connect(None) else {
        return Ok(false);
    };
    let setup = conn.setup();
    let root = setup.roots[n].root;
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let _ = conn.prefetch_extension_information(xtest::X11_EXTENSION_NAME);
    let map = conn.get_keyboard_mapping(min, max - min + 1);
    if !plain(&conn) {
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
    let mut pace = human.then(Rng::seeded);
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
            for (i, &(kind, key)) in events.iter().enumerate() {
                if stop.load(Ordering::Relaxed) {
                    return Err(STOPPED.to_owned());
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
                let wait = if i + 1 == events.len() {
                    Some(GAP_MS)
                } else if kind != KEY_PRESS_EVENT {
                    None
                } else if key == code {
                    Some(HOLD_MS)
                } else {
                    Some(MODIFIER_MS)
                };
                if let (Some(rng), Some(wait)) = (pace.as_mut(), wait) {
                    conn.flush().map_err(|e| format!("XTest: {e}"))?;
                    thread::sleep(rng.ms(wait));
                }
            }
        }
        Ok(())
    })();
    for &key in held.iter().rev() {
        let _ = conn.xtest_fake_input(KEY_RELEASE_EVENT, key, x11rb::CURRENT_TIME, root, 0, 0, 0);
    }
    conn.sync().map_err(|e| format!("XTest: {e}"))?;
    typed?;
    for cookie in sent {
        cookie.check().map_err(|e| format!("XTest: {e}"))?;
    }
    Ok(true)
}

pub(crate) fn hand(gesture: Gesture, stop: &AtomicBool) -> Result<bool, String> {
    match gesture {
        Gesture::Point(to, clicks) => glide(to, clicks, stop),
        Gesture::Keys(keysyms) => press_keys(&keysyms, stop),
    }
}

/// Presses `keysyms` in order, each held, and releases them in reverse; false, pressing nothing, when one has no unshifted keycode or X is unreachable.
fn press_keys(keysyms: &[u32], stop: &AtomicBool) -> Result<bool, String> {
    let Ok((conn, n)) = x11rb::connect(None) else {
        return Ok(false);
    };
    let root = conn.setup().roots[n].root;
    let Some(codes) = keycodes(&conn, keysyms) else {
        return Ok(false);
    };
    send_plan(&conn, root, &key_plan(&codes), &mut Rng::seeded(), stop)?;
    Ok(true)
}

/// Keycodes `codes` pressed in order, each held, and released in reverse.
fn key_plan(codes: &[u8]) -> Vec<Step> {
    let mut plan: Vec<Step> = codes
        .iter()
        .map(|&c| (KEY_PRESS_EVENT, c, Some(HOLD_MS)))
        .collect();
    plan.extend(codes.iter().rev().enumerate().map(|(i, &c)| {
        (
            KEY_RELEASE_EVENT,
            c,
            (i + 1 < codes.len()).then_some(RELEASE_MS),
        )
    }));
    plan
}

/// Moves the pointer from where it is to `to`, in root-window pixels, along a curved path with a person's speed, then presses `clicks` there with a person's timing; true once it pressed, false when it sent no press and leaves the click to the driver.
fn glide(to: (f64, f64), clicks: Option<Clicks>, stop: &AtomicBool) -> Result<bool, String> {
    let Ok((conn, n)) = x11rb::connect(None) else {
        return Ok(false);
    };
    let root = conn.setup().roots[n].root;
    let Some(at) = conn.query_pointer(root).ok().and_then(|c| c.reply().ok()) else {
        return Ok(false);
    };
    let from = (f64::from(at.root_x), f64::from(at.root_y));
    let mut rng = Rng::seeded();
    let points = path(from, to, &mut rng);
    for (x, y) in points {
        if stop.load(Ordering::Relaxed)
            || conn
                .xtest_fake_input(MOTION_NOTIFY_EVENT, 0, x11rb::CURRENT_TIME, root, x, y, 0)
                .is_err()
            || conn.flush().is_err()
        {
            return Ok(false);
        }
        thread::sleep(rng.ms(PATH_STEP_MS));
    }
    thread::sleep(rng.ms(SETTLE_MS));
    let Some(clicks) = clicks else {
        return Ok(false);
    };
    let mods = if clicks.modifiers.is_empty() {
        Some(Vec::new())
    } else {
        keycodes(&conn, &clicks.modifiers)
    };
    let Some(mods) = mods else {
        return Ok(false);
    };
    if stop.load(Ordering::Relaxed) {
        return Ok(false);
    }
    let plan = press_plan(&clicks, &mods);
    send_plan(&conn, root, &plan, &mut rng, stop)?;
    Ok(true)
}

/// The events of `clicks` with modifier keycodes `mods` held around them, each with the pause after it.
fn press_plan(clicks: &Clicks, mods: &[u8]) -> Vec<Step> {
    let hold = if clicks.count > 1 {
        MULTI_HOLD_MS
    } else {
        CLICK_HOLD_MS
    };
    let mut plan: Vec<_> = mods
        .iter()
        .map(|&m| (KEY_PRESS_EVENT, m, Some(MODIFIER_MS)))
        .collect();
    for i in 0..clicks.count {
        let last = i + 1 == clicks.count;
        let after = if !last {
            Some(MULTI_GAP_MS)
        } else if mods.is_empty() {
            None
        } else {
            Some(MODIFIER_MS)
        };
        plan.push((BUTTON_PRESS_EVENT, clicks.button, Some(hold)));
        plan.push((BUTTON_RELEASE_EVENT, clicks.button, after));
    }
    plan.extend(mods.iter().rev().map(|&m| (KEY_RELEASE_EVENT, m, None)));
    plan
}

/// Sends `plan` and releases whatever it still holds when it stops early.
fn send_plan(
    conn: &RustConnection,
    root: Window,
    plan: &[Step],
    rng: &mut Rng,
    stop: &AtomicBool,
) -> Result<(), String> {
    let mut sent: Vec<VoidCookie<'_, RustConnection>> = Vec::new();
    let mut held: Vec<(u8, u8)> = Vec::new();
    let done = (|| {
        for &(kind, detail, pause) in plan {
            if stop.load(Ordering::Relaxed) {
                return Err(STOPPED.to_owned());
            }
            sent.push(
                conn.xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, root, 0, 0, 0)
                    .map_err(|e| format!("XTest: {e}"))?,
            );
            match kind {
                KEY_PRESS_EVENT => held.push((KEY_RELEASE_EVENT, detail)),
                BUTTON_PRESS_EVENT => held.push((BUTTON_RELEASE_EVENT, detail)),
                _ => held.retain(|h| *h != (kind, detail)),
            }
            conn.flush().map_err(|e| format!("XTest: {e}"))?;
            if let Some(pause) = pause {
                thread::sleep(rng.ms(pause));
            }
        }
        Ok(())
    })();
    for &(kind, detail) in held.iter().rev() {
        let _ = conn.xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, root, 0, 0, 0);
    }
    let went = !sent.is_empty();
    let checked = conn
        .sync()
        .map_err(|e| format!("XTest: {e}"))
        .and(done)
        .and_then(|()| {
            sent.into_iter()
                .try_for_each(|c| c.check().map_err(|e| format!("XTest: {e}")))
        });
    checked.map_err(|e| {
        if went && !e.starts_with(STOPPED) {
            format!("{e}, after part of the input went out, so it may still take effect: read the window before sending it again")
        } else {
            e
        }
    })
}

fn keycodes(conn: &RustConnection, keysyms: &[u32]) -> Option<Vec<u8>> {
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let map = conn
        .get_keyboard_mapping(min, max - min + 1)
        .ok()?
        .reply()
        .ok()?;
    let per = usize::from(map.keysyms_per_keycode.max(1));
    keysyms
        .iter()
        .map(|keysym| {
            let i = map
                .keysyms
                .chunks(per)
                .position(|syms| syms.first() == Some(keysym))?;
            min.checked_add(u8::try_from(i).ok()?)
        })
        .collect()
}

/// The points after `from` on an eased quadratic curve to `to` through a control point a random offset from the midpoint, each but the last a pixel off the curve at most.
fn path(from: (f64, f64), to: (f64, f64), rng: &mut Rng) -> Vec<(i16, i16)> {
    if (to.0 - from.0).hypot(to.1 - from.1) < 1.0 {
        return Vec::new();
    }
    let steps = rng.int(PATH_STEPS);
    let (cx, cy) = (
        (from.0 + to.0) / 2.0 + rng.between(-PATH_BEND_PX.0, PATH_BEND_PX.0),
        (from.1 + to.1) / 2.0 + rng.between(-PATH_BEND_PX.1, PATH_BEND_PX.1),
    );
    (1..=steps)
        .map(|i| {
            let t = i as f64 / steps as f64;
            let t = t * t * (3.0 - 2.0 * t);
            let u = 1.0 - t;
            let x = u * u * from.0 + 2.0 * u * t * cx + t * t * to.0;
            let y = u * u * from.1 + 2.0 * u * t * cy + t * t * to.1;
            let jitter = if i < steps { 1.0 } else { 0.0 };
            (
                pixel(x + rng.between(-jitter, jitter)),
                pixel(y + rng.between(-jitter, jitter)),
            )
        })
        .collect()
}

fn pixel(v: f64) -> i16 {
    v.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

struct Rng(u64);

impl Rng {
    fn seeded() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        Self((nanos ^ (u64::from(std::process::id()) << 32)) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn between(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * ((self.next() >> 11) as f64 / (1u64 << 53) as f64)
    }

    fn int(&mut self, (lo, hi): (u64, u64)) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }

    fn ms(&mut self, range: (u64, u64)) -> Duration {
        Duration::from_millis(self.int(range))
    }
}

/// Whether the core keyboard is on its first group with no lock modifier, so a keycode types its map's first or shifted keysym.
fn plain(conn: &RustConnection) -> bool {
    let (Ok(ext), Ok(state)) = (
        conn.xkb_use_extension(1, 0),
        conn.xkb_get_state(ID::USE_CORE_KBD.into()),
    ) else {
        return false;
    };
    ext.reply().is_ok_and(|e| e.supported)
        && state.reply().is_ok_and(|s| {
            u8::from(s.group) == 0 && !s.locked_mods.intersects(ModMask::LOCK | ModMask::SHIFT)
        })
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
        assert_eq!(keysym('\u{8349}'), None);
        assert_eq!(keysym('\n'), None);
    }

    #[test]
    fn a_path_ends_on_its_point_in_bounded_steps_near_the_line_and_a_reached_point_has_none() {
        let mut rng = Rng(7);
        for _ in 0..200 {
            let p = path((10.0, 10.0), (610.0, 410.0), &mut rng);
            assert_eq!(p.last(), Some(&(610, 410)));
            assert!((PATH_STEPS.0..=PATH_STEPS.1).contains(&(p.len() as u64)));
            assert!(
                p.iter()
                    .all(|&(x, y)| (-112..=732).contains(&x) && (-112..=532).contains(&y)),
                "{p:?}"
            );
        }
        let short = path((0.0, 0.0), (3.0, 4.0), &mut rng);
        assert!((PATH_STEPS.0..=PATH_STEPS.1).contains(&(short.len() as u64)));
        assert_eq!(short.last(), Some(&(3, 4)));
        assert_eq!(path((5.0, 5.0), (5.4, 5.0), &mut rng), []);
    }

    #[test]
    fn a_click_holds_its_button_and_modifiers_wrap_a_double_click() {
        let one = Clicks {
            button: 1,
            count: 1,
            modifiers: vec![],
        };
        assert_eq!(
            press_plan(&one, &[]),
            [
                (BUTTON_PRESS_EVENT, 1, Some(CLICK_HOLD_MS)),
                (BUTTON_RELEASE_EVENT, 1, None)
            ]
        );
        let two = Clicks {
            button: 3,
            count: 2,
            modifiers: vec![0xffe3],
        };
        assert_eq!(
            press_plan(&two, &[37]),
            [
                (KEY_PRESS_EVENT, 37, Some(MODIFIER_MS)),
                (BUTTON_PRESS_EVENT, 3, Some(MULTI_HOLD_MS)),
                (BUTTON_RELEASE_EVENT, 3, Some(MULTI_GAP_MS)),
                (BUTTON_PRESS_EVENT, 3, Some(MULTI_HOLD_MS)),
                (BUTTON_RELEASE_EVENT, 3, Some(MODIFIER_MS)),
                (KEY_RELEASE_EVENT, 37, None)
            ]
        );
    }

    #[test]
    fn keys_are_held_in_order_and_released_in_reverse() {
        assert_eq!(
            key_plan(&[37, 38]),
            [
                (KEY_PRESS_EVENT, 37, Some(HOLD_MS)),
                (KEY_PRESS_EVENT, 38, Some(HOLD_MS)),
                (KEY_RELEASE_EVENT, 38, Some(RELEASE_MS)),
                (KEY_RELEASE_EVENT, 37, None)
            ]
        );
    }

    #[test]
    fn pauses_and_offsets_stay_in_their_ranges() {
        let mut rng = Rng(9);
        for _ in 0..1000 {
            assert!((HOLD_MS.0..=HOLD_MS.1).contains(&(rng.ms(HOLD_MS).as_millis() as u64)));
            assert!((-1.0..1.0).contains(&rng.between(-1.0, 1.0)));
        }
    }
}
