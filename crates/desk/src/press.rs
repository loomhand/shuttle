//! Press planning: whether a press selects, clicks a point or runs the element's action.

use std::time::Duration;

use crate::keys::Os;
use crate::model::{Access, Act, Extras, Frame, Node};

pub(crate) const MENU_POLL: Duration = Duration::from_millis(30);
pub(crate) const MENU_POLLS: u32 = 10;
pub(crate) const BACKGROUND_NO_POINT: &str = "background input: the element shows no point to click in its window, and on Windows its UI Automation action would bring the window to the front";

const PRESSABLE_ROLES: [&str; 4] = ["button", "checkbox", "switch", "link"];
const SELECTED_ROLES: [&str; 5] = ["tab", "option", "radio", "listitem", "treeitem"];

type Cover = (String, Option<u32>);

/// What a press comes to after the backend's part of it.
#[derive(Debug, PartialEq)]
pub(crate) enum Press {
    Selected,
    Click(i32, i32),
    Acted(bool),
}

/// Presses `id` the way a user would: a select, a click at a point, or the element's action.
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
    let front_pressable =
        os == Os::Mac && !background && PRESSABLE_ROLES.contains(&node.role) && in_front(b, id)?;
    if (front_pressable || background && !uia_behind) && b.act(id, Act::Action)? {
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
        || !b.pid_of(id).ok().flatten().is_some_and(libreoffice)
        || in_dialog(b, &chain);
    if pixel && let Ok((x, y)) = reachable(b, id, &node, &chain, os, background) {
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

pub(crate) fn hit_centre<B: Access>(
    b: &mut B,
    id: &str,
    os: Os,
    background: bool,
) -> Result<Option<(i32, i32)>, String> {
    let node = b.node(id, Extras::BOUNDS, 0)?;
    let chain = b.ancestors(id)?;
    match reachable(b, id, &node, &chain, os, background) {
        Ok(point) => Ok(Some(point)),
        Err(None) => Ok(None),
        Err(Some((title, pid))) => Err(covered_text(&title, pid)),
    }
}

/// Whether process `pid` is LibreOffice, whose control extents can sit above the control.
pub(crate) fn libreoffice(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == "soffice.bin")
}

fn in_front<B: Access>(b: &mut B, id: &str) -> Result<bool, String> {
    let Some(pid) = b.pid_of(id)? else {
        return Ok(false);
    };
    let root = b.ancestors(id)?.into_iter().next();
    Ok(b.frames_of(pid)?
        .into_iter()
        .any(|f| f.active && root.as_ref() == Some(&f.id)))
}

/// Whether the top-level window of `chain` is a dialog, where LibreOffice draws controls at their extents.
fn in_dialog<B: Access>(b: &mut B, chain: &[String]) -> bool {
    chain
        .first()
        .and_then(|w| b.node(w, Extras::default(), 0).ok())
        .is_some_and(|w| matches!(w.role, "dialog" | "alert"))
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
    os: Os,
    background: bool,
) -> Result<(i32, i32), Option<Cover>> {
    let [x, y, w, h] = node.bounds.ok_or(None)?;
    if !node.has("showing") || w <= 0 || h <= 0 {
        return Err(None);
    }
    let (cx, cy) = (x + w / 2, y + h / 2);
    if node.role.starts_with("menu") {
        if os == Os::Windows
            && !background
            && let Some(cover) = b.menu_cover(id, chain, cx, cy).map_err(|_| None)?
        {
            return Err(Some(cover));
        }
        return Ok((cx, cy));
    }
    let inside = chain.iter().all(|a| match b.node(a, Extras::BOUNDS, 0) {
        Ok(n) => {
            n.role == "tab"
                || n.bounds.is_none_or(|[bx, by, bw, bh]| {
                    (bx..bx + bw).contains(&cx) && (by..by + bh).contains(&cy)
                })
        }
        Err(_) => os == Os::Linux,
    });
    if !inside {
        return Err(None);
    }
    let window = chain.first().ok_or(None)?;
    let hit = b.at(window, cx, cy).ok().flatten().ok_or(None)?;
    let reaches = hit == id || chain.contains(&hit) || {
        let above = b.ancestors(&hit).map_err(|_| None)?;
        above.iter().any(|a| a == id) || (background && above.first().unwrap_or(&hit) != window)
    };
    if !reaches {
        return Err(None);
    }
    if os != Os::Windows
        && !background
        && let Some(cover) = covered(b, id, window, cx, cy, os)
    {
        return Err(Some((cover, None)));
    }
    Ok((cx, cy))
}

fn covered_text(title: &str, pid: Option<u32>) -> String {
    let cover = match (title.is_empty(), pid) {
        (true, None) => "another window".to_owned(),
        (true, Some(p)) => format!("a window of pid {p}"),
        (false, None) => format!("the window {title:?}"),
        (false, Some(p)) => format!("the window {title:?} (pid {p})"),
    };
    format!(
        "{cover} covers the element's center, so nothing was clicked: bring the element's window to the front, or press it with desk press"
    )
}

fn covered<B: Access>(b: &mut B, id: &str, window: &str, x: i32, y: i32, os: Os) -> Option<String> {
    let mut frame = Frame {
        id: window.to_owned(),
        ..Frame::default()
    };
    if os != Os::Mac {
        let node = b.node(window, Extras::BOUNDS, 0).ok()?;
        frame.title = node.name;
        frame.bounds = node.bounds;
        frame.pid = b.pid_of(id).ok().flatten();
    }
    b.covering(&frame, x, y)
        .ok()
        .flatten()
        .map(|(title, _)| title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desk::tests::{Fake, notepad};
    use crate::model::Tree;

    #[test]
    fn a_shown_control_that_a_hit_test_finds_takes_a_click() {
        for os in [Os::Windows, Os::Linux] {
            let mut f = notepad();
            assert_eq!(
                plan_press(&mut f, "ok", os, false).unwrap(),
                Press::Click(140, 110)
            );
            assert!(f.acts.is_empty(), "{os:?}");
        }
    }

    #[test]
    fn a_windows_menu_entry_whose_point_another_window_holds_takes_no_click() {
        let mut m = Menu(notepad(), Ok(Some((String::new(), Some(4242)))), 0);
        let err = hit_centre(&mut m, "file", Os::Windows, false).unwrap_err();
        assert_eq!(err, covered_text("", Some(4242)));
        assert_eq!(
            hit_centre(&mut m, "file", Os::Windows, true).unwrap(),
            Some((20, 10))
        );
        assert_eq!(
            hit_centre(&mut m, "file", Os::Linux, false).unwrap(),
            Some((20, 10))
        );
        m.1 = Ok(None);
        assert_eq!(
            hit_centre(&mut m, "file", Os::Windows, false).unwrap(),
            Some((20, 10))
        );
    }

    #[test]
    fn a_foreground_windows_click_on_a_menu_role_element_by_act_or_press_reads_the_window_at_its_center()
     {
        let mut m = Menu(notepad(), Ok(None), 0);
        hit_centre(&mut m, "ok", Os::Windows, false).unwrap();
        hit_centre(&mut m, "file", Os::Windows, true).unwrap();
        hit_centre(&mut m, "file", Os::Linux, false).unwrap();
        hit_centre(&mut m, "file", Os::Mac, false).unwrap();
        assert_eq!(m.2, 0);
        hit_centre(&mut m, "file", Os::Windows, false).unwrap();
        assert_eq!(m.2, 1);
        plan_press(&mut m, "file", Os::Windows, false).unwrap();
        assert_eq!(m.2, 2);
        m.0.add(
            "open",
            Some("file"),
            "menuitem",
            &["showing"],
            Some([0, 20, 40, 20]),
        );
        plan_press(&mut m, "open", Os::Windows, false).unwrap();
        assert_eq!(m.2, 2);
    }

    #[test]
    fn a_windows_press_on_a_covered_menu_runs_its_action_and_a_clear_one_is_clicked() {
        let mut m = Menu(notepad(), Ok(Some(("Palette".into(), Some(7)))), 0);
        assert_eq!(
            plan_press(&mut m, "file", Os::Windows, false).unwrap(),
            Press::Acted(true)
        );
        m.1 = Err("no own window".into());
        assert_eq!(
            plan_press(&mut m, "file", Os::Windows, false).unwrap(),
            Press::Acted(true)
        );
        m.1 = Ok(None);
        assert_eq!(
            plan_press(&mut m, "file", Os::Windows, false).unwrap(),
            Press::Click(20, 10)
        );
        assert_eq!(m.0.acts.len(), 2);
    }

    #[test]
    fn a_windows_menu_entry_whose_own_window_cannot_be_read_takes_no_click_and_names_no_cover() {
        let mut m = Menu(notepad(), Err("no own window".into()), 0);
        assert_eq!(
            hit_centre(&mut m, "file", Os::Windows, false).unwrap(),
            None
        );
    }

    #[test]
    fn a_windows_menu_entry_gone_or_off_screen_takes_no_click_and_a_live_one_without_bounds_is_pressed()
     {
        let mut m = Menu(notepad(), Ok(None), 0);
        m.0.add(
            "closed",
            Some("file"),
            "menuitem",
            &[],
            Some([0, 20, 40, 20]),
        );
        assert_eq!(
            hit_centre(&mut m, "gone", Os::Windows, false).unwrap_err(),
            "gone"
        );
        assert_eq!(
            hit_centre(&mut m, "closed", Os::Windows, false).unwrap(),
            None
        );
        assert_eq!(
            hit_centre(&mut m, "save", Os::Windows, false).unwrap(),
            None
        );
        assert_eq!(
            plan_press(&mut m, "save", Os::Windows, false).unwrap(),
            Press::Acted(true)
        );
    }

    #[test]
    fn a_cover_refusal_names_the_window_at_the_point_by_title_and_pid_when_known() {
        let tail = " covers the element's center, so nothing was clicked: bring the element's window to the front, or press it with desk press";
        assert_eq!(
            covered_text("", Some(4242)),
            format!("a window of pid 4242{tail}")
        );
        assert_eq!(
            covered_text("Menus", Some(7)),
            format!("the window \"Menus\" (pid 7){tail}")
        );
        assert_eq!(
            covered_text("Calculator", None),
            format!("the window \"Calculator\"{tail}")
        );
        assert_eq!(covered_text("", None), format!("another window{tail}"));
    }

    #[test]
    fn a_linux_element_a_window_of_another_process_covers_takes_no_click() {
        let mut f = notepad();
        f.covers = Some(("Calculator".into(), Some(9)));
        let err = hit_centre(&mut f, "ok", Os::Linux, false).unwrap_err();
        assert!(err.starts_with("the window \"Calculator\" covers"), "{err}");
        assert_eq!(f.listings, 0);
        assert_eq!(
            plan_press(&mut f, "ok", Os::Linux, false).unwrap(),
            Press::Acted(true)
        );
        assert_eq!(f.acts, [("ok".to_owned(), Act::Action)]);
        assert_eq!(
            plan_press(&mut f, "file", Os::Linux, false).unwrap(),
            Press::Click(20, 10)
        );
        f.covers = Some((String::new(), Some(9)));
        let err = hit_centre(&mut f, "ok", Os::Linux, false).unwrap_err();
        assert!(err.starts_with("another window covers"), "{err}");
        assert_eq!(
            hit_centre(&mut f, "ok", Os::Windows, false).unwrap(),
            Some((140, 110))
        );
    }

    #[test]
    fn an_untitled_window_of_another_process_covers_an_untitled_linux_window() {
        let mut f = notepad();
        f.add("", None, "window", &["showing"], Some([400, 400, 300, 200]));
        f.add(
            "go",
            Some(""),
            "button",
            &["showing"],
            Some([500, 500, 40, 20]),
        );
        f.hits.insert((520, 510), "go".into());
        f.covers = Some((String::new(), Some(9)));
        let err = hit_centre(&mut f, "go", Os::Linux, false).unwrap_err();
        assert!(err.starts_with("another window covers"), "{err}");
        assert_eq!(
            plan_press(&mut f, "go", Os::Linux, false).unwrap(),
            Press::Acted(true)
        );
    }

    #[test]
    fn a_linux_element_any_window_the_backend_finds_over_it_takes_no_click() {
        let mut f = notepad();
        for cover in [("Find - Notepad", Some(4)), ("w", Some(9)), ("w", None)] {
            f.covers = Some((cover.0.into(), cover.1));
            let err = hit_centre(&mut f, "ok", Os::Linux, false).unwrap_err();
            assert!(
                err.starts_with(&format!("the window {:?} covers", cover.0)),
                "{err}"
            );
        }
        f.covers = None;
        assert_eq!(
            hit_centre(&mut f, "ok", Os::Linux, false).unwrap(),
            Some((140, 110))
        );
    }

    #[test]
    fn a_background_hit_test_refuses_another_element_of_the_window_and_passes_another_window() {
        let mut f = notepad();
        f.add(
            "banner",
            Some("w"),
            "group",
            &["showing"],
            Some([110, 100, 60, 20]),
        );
        f.add("calc", None, "window", &["showing"], Some([0, 0, 300, 300]));
        f.add(
            "key",
            Some("calc"),
            "button",
            &["showing"],
            Some([130, 100, 20, 20]),
        );
        f.hits.insert((140, 110), "banner".into());
        assert_eq!(hit_centre(&mut f, "ok", Os::Mac, true).unwrap(), None);
        f.hits.insert((140, 110), "key".into());
        assert_eq!(
            hit_centre(&mut f, "ok", Os::Mac, true).unwrap(),
            Some((140, 110))
        );
        assert_eq!(hit_centre(&mut f, "ok", Os::Mac, false).unwrap(), None);
    }

    #[test]
    fn a_macos_button_in_the_front_window_runs_its_action() {
        let mut f = notepad();
        assert_eq!(
            plan_press(&mut f, "ok", Os::Mac, false).unwrap(),
            Press::Acted(true)
        );
        assert_eq!(f.acts, [("ok".to_owned(), Act::Action)]);
    }

    #[test]
    fn a_macos_button_in_a_window_behind_takes_a_click() {
        let mut f = notepad();
        f.behind = true;
        assert_eq!(
            plan_press(&mut f, "ok", Os::Mac, false).unwrap(),
            Press::Click(140, 110)
        );
        assert!(f.acts.is_empty());
    }

    #[test]
    fn a_macos_element_a_window_of_another_app_covers_takes_no_click() {
        let mut f = notepad();
        f.behind = true;
        f.covers = Some(("Calculator".into(), Some(9)));
        let err = hit_centre(&mut f, "ok", Os::Mac, false).unwrap_err();
        assert!(err.starts_with("the window \"Calculator\" covers"), "{err}");
        assert_eq!(
            plan_press(&mut f, "ok", Os::Mac, false).unwrap(),
            Press::Acted(true)
        );
        assert_eq!(f.acts, [("ok".to_owned(), Act::Action)]);
    }

    #[test]
    fn a_macos_button_that_refuses_its_action_takes_a_click() {
        let mut f = notepad();
        f.add(
            "dead",
            Some("w"),
            "button",
            &["showing"],
            Some([500, 100, 80, 20]),
        );
        f.hits.insert((540, 110), "dead".into());
        assert_eq!(
            plan_press(&mut f, "dead", Os::Mac, false).unwrap(),
            Press::Click(540, 110)
        );
        assert_eq!(f.acts, [("dead".to_owned(), Act::Action)]);
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
                reachable(&mut f, "ok", &node, &chain, Os::Windows, background),
                Ok((140, 110))
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
    fn a_libreoffice_dialog_button_takes_a_click_and_a_main_window_button_its_action() {
        let (_dir, mut office) = office();
        let mut f = notepad();
        f.pid = Some(office.id());
        f.add(
            "d",
            None,
            "dialog",
            &["showing"],
            Some([500, 300, 400, 200]),
        );
        f.add(
            "discard",
            Some("d"),
            "button",
            &["showing"],
            Some([800, 450, 80, 20]),
        );
        f.hits.insert((840, 460), "discard".into());
        f.hits.insert((140, 110), "ok".into());
        assert_eq!(
            plan_press(&mut f, "discard", Os::Linux, false).unwrap(),
            Press::Click(840, 460)
        );
        assert!(f.acts.is_empty());
        assert_eq!(
            plan_press(&mut f, "ok", Os::Linux, false).unwrap(),
            Press::Acted(true)
        );
        assert_eq!(f.acts, [("ok".to_owned(), Act::Action)]);
        office.kill().unwrap();
        office.wait().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn only_a_soffice_bin_process_counts_as_libreoffice() {
        let (_dir, mut office) = office();
        let pid = office.id();
        assert!(
            libreoffice(pid),
            "the soffice.bin link never showed in /proc/{pid}/comm"
        );
        assert!(!libreoffice(std::process::id()));
        office.kill().unwrap();
        office.wait().unwrap();
    }

    #[cfg(target_os = "linux")]
    fn office() -> (tempfile::TempDir, std::process::Child) {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("soffice.bin");
        std::os::unix::fs::symlink("/bin/sleep", &bin).unwrap();
        let office = std::process::Command::new(&bin).arg("30").spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !libreoffice(office.id()) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        (dir, office)
    }

    struct Menu(Fake, Result<Option<Cover>, String>, usize);

    impl Access for Menu {
        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            self.0.frames()
        }
        fn tree(&mut self, frame: &Frame, extras: Extras) -> Result<Tree, String> {
            self.0.tree(frame, extras)
        }
        fn node(&mut self, id: &str, extras: Extras, chars: usize) -> Result<Node, String> {
            self.0.node(id, extras, chars)
        }
        fn ancestors(&mut self, id: &str) -> Result<Vec<String>, String> {
            self.0.ancestors(id)
        }
        fn at(&mut self, window: &str, x: i32, y: i32) -> Result<Option<String>, String> {
            self.0.at(window, x, y)
        }
        fn menu_cover(
            &mut self,
            _: &str,
            _: &[String],
            _: i32,
            _: i32,
        ) -> Result<Option<Cover>, String> {
            self.2 += 1;
            self.1.clone()
        }
        fn act(&mut self, id: &str, act: Act) -> Result<bool, String> {
            self.0.act(id, act)
        }
        fn close(&mut self, frame: &Frame) -> Result<(), String> {
            self.0.close(frame)
        }
    }
}
