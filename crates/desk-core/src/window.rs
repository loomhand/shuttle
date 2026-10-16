//! Window choice by title: exact, then ignoring case and outer spaces, then contained, then the window of the app a stale title names.

/// How many title tiers [`title_matches`] has.
pub const TITLE_TIERS: usize = 3;

const TITLE_SEPARATORS: [&str; 2] = [" - ", " — "];

/// A top-level window as the desk matches it.
pub trait Window: Clone {
    fn title(&self) -> &str;
    fn active(&self) -> bool;
    fn pid(&self) -> Option<u32>;
    /// Whether the window is a frame a caller may pick at all.
    fn listed(&self) -> bool {
        true
    }
    /// Whether the window may stand for the focused one: a plain window needs a title.
    fn focusable(&self) -> bool;
}

/// The window named `want` (by [`title_matches`] tiers), else the focused one; among equals the active one, else the newest.
pub fn pick<W: Window>(windows: Vec<W>, want: Option<&str>, pid: Option<u32>) -> Result<W, String> {
    let windows: Vec<W> = windows
        .into_iter()
        .filter(|w| w.listed() && pid.is_none_or(|p| w.pid() == Some(p)))
        .collect();
    let Some(want) = want else {
        let active: Vec<&W> = windows
            .iter()
            .filter(|w| w.active() && w.focusable())
            .collect();
        return best(&active).ok_or_else(|| "no focused window".to_owned());
    };
    for tier in 0..TITLE_TIERS {
        let hits: Vec<&W> = windows
            .iter()
            .filter(|w| title_matches(tier, w.title(), want))
            .collect();
        if let Some(w) = best(&hits) {
            return Ok(w);
        }
    }
    app_window(&windows, want)
}

/// Whether window title `title` matches `want` at `tier`: 0 the same, 1 the same ignoring case and outer
/// spaces, 2 containing it ignoring case.
pub fn title_matches(tier: usize, title: &str, want: &str) -> bool {
    let lower = want.trim().to_lowercase();
    match tier {
        0 => title == want,
        1 => title.trim().to_lowercase() == lower,
        _ => title.to_lowercase().contains(&lower),
    }
}

/// The window whose title ends with the app part of `want`, such as ` - Thunar` of `inbox - Thunar`:
/// the only one, else the active one among several; more than one and none active is an error naming them.
pub fn app_window<W: Window>(windows: &[W], want: &str) -> Result<W, String> {
    let lower = want.trim().to_lowercase();
    let app = TITLE_SEPARATORS
        .iter()
        .filter_map(|s| lower.rfind(s))
        .max()
        .map(|i| &lower[i..]);
    let hits: Vec<&W> = windows
        .iter()
        .filter(|w| app.is_some_and(|app| w.title().trim_end().to_lowercase().ends_with(app)))
        .collect();
    let active: Vec<&W> = hits.iter().copied().filter(|w| w.active()).collect();
    match (hits.as_slice(), active.as_slice()) {
        ([one], _) | (_, [one]) => Ok(W::clone(one)),
        ([], _) => {
            let titles: Vec<&str> = windows.iter().map(Window::title).collect();
            Err(format!("no window {want:?}; windows: {titles:?}"))
        }
        _ => {
            let titles: Vec<&str> = hits.iter().map(|w| w.title()).collect();
            Err(format!(
                "no window {want:?}; windows of its app: {titles:?}"
            ))
        }
    }
}

fn best<W: Window>(candidates: &[&W]) -> Option<W> {
    candidates
        .iter()
        .rev()
        .find(|w| w.active())
        .or_else(|| candidates.last())
        .map(|w| W::clone(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct W {
        title: &'static str,
        active: bool,
        pid: u32,
        listed: bool,
    }

    impl Window for W {
        fn title(&self) -> &str {
            self.title
        }
        fn active(&self) -> bool {
            self.active
        }
        fn pid(&self) -> Option<u32> {
            Some(self.pid)
        }
        fn listed(&self) -> bool {
            self.listed
        }
        fn focusable(&self) -> bool {
            !self.title.is_empty()
        }
    }

    fn w(title: &'static str, active: bool, pid: u32) -> W {
        W {
            title,
            active,
            pid,
            listed: true,
        }
    }

    #[test]
    fn a_window_is_picked_by_exact_then_trimmed_then_contained_title() {
        let mut panel = w("xfce4-panel", true, 3);
        panel.listed = false;
        let windows = vec![
            w("Untitled 1 - Mousepad", false, 7),
            w("Mousepad Preferences", true, 7),
            w("Settings", false, 9),
            panel,
            w("", true, 4),
        ];
        let title = |want: Option<&str>, pid| pick(windows.clone(), want, pid).map(|w| w.title);
        assert_eq!(title(None, None).unwrap(), "Mousepad Preferences");
        assert_eq!(title(Some("settings"), None).unwrap(), "Settings");
        assert_eq!(
            title(Some("Mousepad"), None).unwrap(),
            "Mousepad Preferences"
        );
        assert_eq!(
            title(Some("Mousepad"), Some(7)).unwrap(),
            "Mousepad Preferences"
        );
        assert!(title(Some("Mousepad"), Some(9)).is_err());
        assert!(title(Some("xfce4-panel"), None).is_err());
    }

    #[test]
    fn a_stale_title_takes_its_app_window_only_when_that_is_unambiguous() {
        let one = vec![
            w("images - Thunar", false, 4),
            w("notes.txt - Mousepad", true, 7),
        ];
        assert_eq!(
            pick(one, Some("inbox - Thunar"), None).unwrap().title,
            "images - Thunar"
        );
        let active = vec![w("images — Writer", false, 4), w("docs — Writer", true, 5)];
        assert_eq!(
            pick(active, Some("inbox — Writer"), None).unwrap().title,
            "docs — Writer"
        );
        let none_active = vec![
            w("images - Thunar", false, 4),
            w("docs - Thunar", false, 5),
            w("notes.txt - Mousepad", true, 7),
        ];
        assert_eq!(
            pick(none_active, Some("inbox - Thunar"), None).unwrap_err(),
            "no window \"inbox - Thunar\"; windows of its app: [\"images - Thunar\", \"docs - Thunar\"]"
        );
        assert_eq!(
            app_window(&[w("a", false, 1)], "report - Writer").unwrap_err(),
            "no window \"report - Writer\"; windows: [\"a\"]"
        );
    }
}
