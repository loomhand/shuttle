//! How a desk press reaches an element, the same on every host.

use std::time::Duration;

/// The roles a press selects instead of clicking.
pub const SELECTED_ROLES: [&str; 5] = ["tab", "option", "radio", "listitem", "treeitem"];
/// The wait between checks that a menu entry shows after its menus opened.
pub const MENU_POLL: Duration = Duration::from_millis(30);
/// How many times a press checks that a menu entry shows.
pub const MENU_POLLS: u32 = 10;
