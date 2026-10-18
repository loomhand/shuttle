//! Press planning: whether a press selects, clicks a point or runs the element's action.

use desk_core::press::{MENU_POLL, MENU_POLLS, SELECTED_ROLES};

use crate::model::{Access, Act, Extras, Node};

/// What a press comes to after the backend's part of it.
#[derive(Debug, PartialEq)]
pub(crate) enum Press {
    Selected,
    Click(i32, i32),
    Acted(bool),
}

/// Presses `id` the way a user would; a menu entry runs its action, since a click right after its popup opens misses it, except on Linux, where it takes a click once it shows and a LibreOffice control runs its action.
pub(crate) fn plan_press<B: Access>(b: &mut B, id: &str, linux: bool) -> Result<Press, String> {
    let mut node = b.node(id, Extras::BOUNDS, 0)?;
    if SELECTED_ROLES.contains(&node.role) && b.act(id, Act::Select).unwrap_or(false) {
        return Ok(Press::Selected);
    }
    if node.role.starts_with("menuitem") && !linux {
        return Ok(Press::Acted(b.act(id, Act::Action)?));
    }
    let chain = b.ancestors(id)?;
    if node.role.starts_with("menu") && !node.has("showing") && (open_menus(b, &chain) || linux) {
        for _ in 0..MENU_POLLS {
            node = b.node(id, Extras::BOUNDS, 0)?;
            if node.has("showing") {
                break;
            }
            std::thread::sleep(MENU_POLL);
        }
    }
    let pixel = !linux
        || node.role.starts_with("menu")
        || !b.pid_of(id).ok().flatten().is_some_and(libreoffice);
    if pixel && let Some((x, y)) = reachable(b, id, &node, &chain, linux) {
        return Ok(Press::Click(x, y));
    }
    Ok(Press::Acted(b.act(id, Act::Action)?))
}

/// Whether process `pid` is LibreOffice, whose control extents can sit above the control.
fn libreoffice(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == "soffice.bin")
}

fn open_menus<B: Access>(b: &mut B, chain: &[String]) -> bool {
    let start = chain
        .iter()
        .map(|id| b.node(id, Extras::default(), 0).map_or("", |n| n.role))
        .rposition(|r| r != "menu")
        .map_or(0, |i| i + 1);
    for menu in &chain[start..] {
        let _ = b.act(menu, Act::Action);
    }
    start < chain.len()
}

fn reachable<B: Access>(
    b: &mut B,
    id: &str,
    node: &Node,
    chain: &[String],
    linux: bool,
) -> Option<(i32, i32)> {
    let [x, y, w, h] = node.bounds?;
    if !node.has("showing") || w <= 0 || h <= 0 || x < 0 || y < 0 {
        return None;
    }
    let (cx, cy) = (x + w / 2, y + h / 2);
    if node.role.starts_with("menu") {
        return Some((cx, cy));
    }
    let inside = chain.iter().all(|a| match b.node(a, Extras::BOUNDS, 0) {
        Ok(n) => {
            n.role == "tab"
                || n.bounds.is_none_or(|[bx, by, bw, bh]| {
                    (bx..bx + bw).contains(&cx) && (by..by + bh).contains(&cy)
                })
        }
        Err(_) => linux,
    });
    if !inside {
        return None;
    }
    let hit = b.at(chain.first()?, cx, cy).ok()??;
    let reaches =
        hit == id || chain.contains(&hit) || b.ancestors(&hit).ok()?.iter().any(|a| a == id);
    reaches.then_some((cx, cy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desk::tests::notepad;

    #[test]
    fn a_shown_control_that_a_hit_test_finds_takes_a_click() {
        let mut f = notepad();
        assert_eq!(
            plan_press(&mut f, "ok", false).unwrap(),
            Press::Click(140, 110)
        );
        assert!(f.acts.is_empty());
    }

    #[test]
    fn a_covered_control_runs_its_action() {
        let mut f = notepad();
        f.hits.insert((340, 110), "elsewhere".into());
        f.add("elsewhere", None, "button", &[], None);
        assert_eq!(
            plan_press(&mut f, "hidden", false).unwrap(),
            Press::Acted(true)
        );
        assert_eq!(f.acts, [("hidden".to_owned(), Act::Action)]);
    }

    #[test]
    fn a_tab_is_selected() {
        let mut f = notepad();
        assert_eq!(plan_press(&mut f, "tab", false).unwrap(), Press::Selected);
    }

    #[test]
    fn a_linux_menu_entry_takes_a_click_once_it_shows_else_opens_its_menus_and_runs_its_action() {
        let mut f = notepad();
        f.add(
            "copy",
            Some("file"),
            "menuitem",
            &["showing"],
            Some([0, 20, 40, 20]),
        );
        assert_eq!(
            plan_press(&mut f, "copy", true).unwrap(),
            Press::Click(20, 30)
        );
        assert_eq!(
            plan_press(&mut f, "save", true).unwrap(),
            Press::Acted(true)
        );
        assert_eq!(
            f.acts,
            [
                ("file".to_owned(), Act::Action),
                ("save".to_owned(), Act::Action)
            ]
        );
    }

    #[test]
    fn a_macos_menu_entry_runs_its_action_without_opening_its_menu() {
        let mut f = notepad();
        assert_eq!(
            plan_press(&mut f, "save", false).unwrap(),
            Press::Acted(true)
        );
        assert_eq!(f.acts, [("save".to_owned(), Act::Action)]);
    }
}
