//! Press planning: whether a press selects, clicks a point or runs the element's action.

use desk_core::press::{MENU_POLL, MENU_POLLS, SELECTED_ROLES};

use crate::keys::Os;
use crate::model::{Access, Act, Extras, Node};

pub(crate) const BACKGROUND_NO_POINT: &str = "background input: the element shows no point to click in its window, and on Windows its UI Automation action would bring the window to the front";

/// What a press comes to after the backend's part of it.
#[derive(Debug, PartialEq)]
pub(crate) enum Press {
    Selected,
    Click(i32, i32),
    Acted(bool),
}

/// Presses `id` the way a user would; a menu entry runs its action, since a click right after its popup opens misses it, except on Linux, where it takes a click once it shows and a LibreOffice control runs its action; a Windows tree item takes a click first, since Explorer can ignore its select. With `background` input an element runs its action before any click, which needs no pointer.
pub(crate) fn plan_press<B: Access>(
    b: &mut B,
    id: &str,
    os: Os,
    background: bool,
) -> Result<Press, String> {
    let linux = os == Os::Linux;
    let uia_behind = background && os == Os::Windows;
    let mut node = b.node(id, Extras::BOUNDS, 0)?;
    let click_first = os == Os::Windows && node.role == "treeitem";
    if SELECTED_ROLES.contains(&node.role)
        && !click_first
        && !uia_behind
        && b.act(id, Act::Select).unwrap_or(false)
    {
        return Ok(Press::Selected);
    }
    if node.role.starts_with("menuitem") && !linux && !uia_behind {
        return Ok(Press::Acted(b.act(id, Act::Action)?));
    }
    if background && !uia_behind && b.act(id, Act::Action)? {
        return Ok(Press::Acted(true));
    }
    let chain = b.ancestors(id)?;
    if node.role.starts_with("menu")
        && !node.has("showing")
        && !uia_behind
        && (open_menus(b, &chain) || linux)
    {
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
    if pixel && let Some((x, y)) = reachable(b, id, &node, &chain, linux, background) {
        return Ok(Press::Click(x, y));
    }
    if uia_behind {
        return Err(BACKGROUND_NO_POINT.to_owned());
    }
    if click_first && b.act(id, Act::Select).unwrap_or(false) {
        return Ok(Press::Selected);
    }
    Ok(Press::Acted(b.act(id, Act::Action)?))
}

/// Whether process `pid` is LibreOffice, whose control extents can sit above the control.
pub(crate) fn libreoffice(pid: u32) -> bool {
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
    background: bool,
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
    if hit == id || chain.contains(&hit) {
        return Some((cx, cy));
    }
    let above = b.ancestors(&hit).ok()?;
    let reaches = above.iter().any(|a| a == id)
        || (background && above.first().unwrap_or(&hit) != chain.first()?);
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
            plan_press(&mut f, "ok", Os::Mac, false).unwrap(),
            Press::Click(140, 110)
        );
        assert!(f.acts.is_empty());
    }

    #[test]
    fn a_background_press_runs_the_action_of_a_control_that_would_take_a_click() {
        let mut f = notepad();
        assert_eq!(
            plan_press(&mut f, "ok", Os::Mac, true).unwrap(),
            Press::Acted(true)
        );
        assert_eq!(f.acts, [("ok".to_owned(), Act::Action)]);
    }

    #[test]
    fn a_direct_hit_reads_no_ancestry_of_the_hit() {
        let mut f = notepad();
        let node = f.node("ok", Extras::BOUNDS, 0).unwrap();
        let chain = f.ancestors("ok").unwrap();
        f.ancestry = 0;
        for background in [false, true] {
            assert_eq!(
                reachable(&mut f, "ok", &node, &chain, false, background),
                Some((140, 110))
            );
        }
        assert_eq!(f.ancestry, 0);
    }

    #[test]
    fn a_windows_background_press_clicks_in_the_window_and_runs_no_ui_automation_action() {
        let mut f = notepad();
        f.add(
            "downloads",
            Some("w"),
            "treeitem",
            &["showing"],
            Some([10, 200, 60, 30]),
        );
        f.add(
            "calc",
            None,
            "window",
            &["showing"],
            Some([0, 150, 300, 300]),
        );
        f.add(
            "key",
            Some("calc"),
            "button",
            &["showing"],
            Some([30, 200, 30, 30]),
        );
        f.hits.insert((40, 215), "key".into());
        assert_eq!(
            plan_press(&mut f, "ok", Os::Windows, true).unwrap(),
            Press::Click(140, 110)
        );
        assert_eq!(
            plan_press(&mut f, "downloads", Os::Windows, true).unwrap(),
            Press::Click(40, 215)
        );
        let err = plan_press(&mut f, "save", Os::Windows, true).unwrap_err();
        assert!(err.contains("no point to click"), "{err}");
        f.hits.insert((40, 215), "tab".into());
        let err = plan_press(&mut f, "downloads", Os::Windows, true).unwrap_err();
        assert!(err.contains("no point to click"), "{err}");
        assert!(f.acts.is_empty(), "{:?}", f.acts);
    }

    #[test]
    fn a_background_action_that_fails_is_never_followed_by_a_click() {
        let mut f = notepad();
        f.add(
            "slow",
            Some("w"),
            "button",
            &["showing"],
            Some([500, 100, 80, 20]),
        );
        f.hits.insert((540, 110), "slow".into());
        let err = plan_press(&mut f, "slow", Os::Mac, true).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert_eq!(f.acts, [("slow".to_owned(), Act::Action)]);
    }

    #[test]
    fn a_covered_control_runs_its_action() {
        let mut f = notepad();
        f.hits.insert((340, 110), "elsewhere".into());
        f.add("elsewhere", None, "button", &[], None);
        assert_eq!(
            plan_press(&mut f, "hidden", Os::Mac, false).unwrap(),
            Press::Acted(true)
        );
        assert_eq!(f.acts, [("hidden".to_owned(), Act::Action)]);
    }

    #[test]
    fn a_tab_is_selected() {
        let mut f = notepad();
        assert_eq!(
            plan_press(&mut f, "tab", Os::Mac, false).unwrap(),
            Press::Selected
        );
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
            plan_press(&mut f, "copy", Os::Linux, false).unwrap(),
            Press::Click(20, 30)
        );
        assert_eq!(
            plan_press(&mut f, "save", Os::Linux, false).unwrap(),
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
            plan_press(&mut f, "save", Os::Mac, false).unwrap(),
            Press::Acted(true)
        );
        assert_eq!(f.acts, [("save".to_owned(), Act::Action)]);
    }

    #[test]
    fn a_windows_tree_item_takes_a_click_where_a_hit_test_finds_it_else_is_selected() {
        let mut f = notepad();
        f.add(
            "downloads",
            Some("w"),
            "treeitem",
            &["showing"],
            Some([10, 200, 60, 30]),
        );
        f.add("off", Some("w"), "treeitem", &[], Some([10, 900, 60, 30]));
        f.hits.insert((40, 215), "downloads".into());
        assert_eq!(
            plan_press(&mut f, "downloads", Os::Windows, false).unwrap(),
            Press::Click(40, 215)
        );
        assert!(f.acts.is_empty());
        assert_eq!(
            plan_press(&mut f, "off", Os::Windows, false).unwrap(),
            Press::Selected
        );
        assert_eq!(f.acts, [("off".to_owned(), Act::Select)]);
    }

    #[test]
    fn a_macos_tree_item_is_selected() {
        let mut f = notepad();
        f.add(
            "downloads",
            Some("w"),
            "treeitem",
            &["showing"],
            Some([10, 200, 60, 30]),
        );
        f.hits.insert((40, 215), "downloads".into());
        assert_eq!(
            plan_press(&mut f, "downloads", Os::Mac, false).unwrap(),
            Press::Selected
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn only_a_soffice_bin_process_counts_as_libreoffice() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("soffice.bin");
        std::os::unix::fs::symlink("/bin/sleep", &bin).unwrap();
        let mut office = std::process::Command::new(&bin).arg("30").spawn().unwrap();
        let pid = office.id();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !libreoffice(pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            libreoffice(pid),
            "the soffice.bin link never showed in /proc/{pid}/comm"
        );
        assert!(!libreoffice(std::process::id()));
        office.kill().unwrap();
        office.wait().unwrap();
    }
}
