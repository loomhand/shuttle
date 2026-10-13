//! macOS AX roles as ARIA roles after W3C Core-AAM, and AX attributes as desk state names.

/// The AX facts that decide an element's role.
#[derive(Clone, Copy, Debug, Default)]
pub struct Kind<'a> {
    pub role: &'a str,
    pub subrole: &'a str,
    pub named: bool,
    pub submenu: bool,
    pub marked: bool,
}

/// The AX facts that decide an element's states.
#[derive(Clone, Copy, Debug, Default)]
pub struct Facts {
    pub enabled: bool,
    pub focused: bool,
    pub selected: bool,
    pub expanded: Option<bool>,
    pub modal: bool,
    pub editable: bool,
    pub shown: bool,
    pub value: Option<i64>,
}

/// The ARIA role of `k`, none when ARIA has no match; a nameless container is generic.
pub fn role(k: Kind) -> Option<&'static str> {
    Some(match (k.role, k.subrole) {
        ("AXButton" | "AXDisclosureTriangle" | "AXColorWell" | "AXMenuButton", _) => "button",
        ("AXCheckBox", "AXSwitch") => "switch",
        ("AXCheckBox", "AXToggle") => "button",
        ("AXCheckBox", _) => "checkbox",
        ("AXRadioButton", "AXTabButton") => "tab",
        ("AXRadioButton", _) => "radio",
        ("AXTextField", "AXSearchField") => "searchbox",
        ("AXTextField" | "AXTextArea", _) => "textbox",
        ("AXStaticText", _) => "text",
        ("AXPopUpButton" | "AXComboBox", _) => "combobox",
        ("AXMenuBar", _) => "menubar",
        ("AXMenuBarItem", _) => "menu",
        ("AXMenuItem", _) if k.submenu => "menu",
        ("AXMenuItem", _) if k.marked => "menuitemcheckbox",
        ("AXMenuItem", _) => "menuitem",
        ("AXToolbar", _) => "toolbar",
        ("AXTabGroup", _) => "tablist",
        ("AXTable", _) => "table",
        ("AXOutline", _) => "treegrid",
        ("AXRow", "AXOutlineRow") => "treeitem",
        ("AXRow", _) => "row",
        ("AXCell", _) => "cell",
        ("AXList", _) => "list",
        ("AXImage", _) => "img",
        ("AXLink", _) => "link",
        ("AXSlider", _) => "slider",
        ("AXIncrementor", _) => "spinbutton",
        ("AXProgressIndicator" | "AXBusyIndicator" | "AXLevelIndicator", _) => "progressbar",
        ("AXSheet", _) => "dialog",
        ("AXWindow", "AXDialog" | "AXSystemDialog") => "dialog",
        ("AXWindow", _) => "window",
        ("AXHeading", _) => "heading",
        ("AXWebArea", _) => "document",
        ("AXHelpTag", _) => "tooltip",
        ("AXSplitter", _) => "separator",
        ("AXScrollBar", _) => "scrollbar",
        (
            "AXRadioGroup" | "AXGroup" | "AXScrollArea" | "AXSplitGroup" | "AXLayoutArea"
            | "AXMenu",
            _,
        ) => {
            if k.named {
                "group"
            } else {
                "generic"
            }
        }
        _ => return None,
    })
}

/// The desk states of an element of `role` with `f`.
pub fn states(role: &str, f: Facts) -> Vec<&'static str> {
    let mut out = Vec::new();
    let toggle = matches!(role, "checkbox" | "switch" | "radio" | "menuitemcheckbox");
    let pressable = role == "button" && f.value.is_some();
    match f.value {
        Some(1) if toggle => out.push("checked"),
        Some(1) if pressable => out.push("pressed"),
        _ => {}
    }
    if f.editable {
        out.push("editable");
    }
    if f.expanded == Some(true) {
        out.push("expanded");
    }
    if f.focused {
        out.push("focused");
    }
    if f.modal {
        out.push("modal");
    }
    if f.selected {
        out.push("selected");
    }
    if f.shown {
        out.push("showing");
    }
    if toggle {
        out.push("checkable");
    }
    if !f.enabled {
        out.push("disabled");
    }
    if toggle && f.value == Some(2) {
        out.push("mixed");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ax_roles_map_to_aria_roles() {
        let k = |role, subrole| Kind {
            role,
            subrole,
            ..Kind::default()
        };
        assert_eq!(role(k("AXButton", "")), Some("button"));
        assert_eq!(role(k("AXRadioButton", "AXTabButton")), Some("tab"));
        assert_eq!(role(k("AXTextField", "AXSecureTextField")), Some("textbox"));
        assert_eq!(role(k("AXSheet", "")), Some("dialog"));
        assert_eq!(role(k("AXMenu", "")), Some("generic"));
        assert_eq!(
            role(Kind {
                submenu: true,
                ..k("AXMenuItem", "")
            }),
            Some("menu")
        );
        assert_eq!(role(k("AXValueIndicator", "")), None);
    }

    #[test]
    fn check_values_become_checked_or_mixed() {
        let f = |value| Facts {
            enabled: true,
            shown: true,
            value,
            ..Facts::default()
        };
        assert_eq!(
            states("checkbox", f(Some(1))),
            ["checked", "showing", "checkable"]
        );
        assert_eq!(
            states("checkbox", f(Some(2))),
            ["showing", "checkable", "mixed"]
        );
        assert_eq!(states("button", f(Some(1))), ["pressed", "showing"]);
        assert_eq!(states("button", f(None)), ["showing"]);
        assert_eq!(
            states(
                "textbox",
                Facts {
                    editable: true,
                    ..Facts::default()
                }
            ),
            ["editable", "disabled"]
        );
    }
}
