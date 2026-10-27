//! Desktop entries a launch name can repeat: an agent names an app as its menu shows it, and a name such as "libreoffice calc" otherwise runs `libreoffice` on a file named calc.

use std::path::PathBuf;

/// The desktop file id of the application entry whose `Name` equals `name` ignoring case, for a name of more than one word; the user's entries come before the system's, as the driver reads them.
pub fn id_for(name: &str) -> Option<String> {
    id_in(name, &dirs())
}

fn id_in(name: &str, dirs: &[PathBuf]) -> Option<String> {
    let name = name.trim();
    if !name.contains(char::is_whitespace) {
        return None;
    }
    dirs.iter().find_map(|dir| {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "desktop"))
            .collect();
        files.sort();
        files.into_iter().find_map(|p| {
            let body = std::fs::read_to_string(&p).ok()?;
            named(&body, name).then(|| p.file_stem()?.to_str().map(str::to_owned))?
        })
    })
}

/// Whether the `[Desktop Entry]` group of `body` is a shown application named `name` ignoring case.
fn named(body: &str, name: &str) -> bool {
    let (mut title, mut app, mut shown) = (None, false, true);
    let group = body
        .lines()
        .map(str::trim)
        .skip_while(|l| *l != "[Desktop Entry]")
        .skip(1)
        .take_while(|l| !l.starts_with('['));
    for line in group {
        match line.split_once('=').map(|(k, v)| (k.trim(), v.trim())) {
            Some(("Name", v)) => title = Some(v),
            Some(("Type", v)) => app = v == "Application",
            Some(("NoDisplay" | "Hidden", v)) => shown &= v != "true",
            _ => {}
        }
    }
    app && shown && title.is_some_and(|t| t.eq_ignore_ascii_case(name))
}

fn dirs() -> Vec<PathBuf> {
    let home = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| format!("{h}/.local/share"))
        });
    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    home.into_iter()
        .chain(system.split(':').map(str::to_owned))
        .map(|d| PathBuf::from(d).join("applications"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CALC: &str = "[Desktop Entry]\nVersion=1.0\nType=Application\nName=LibreOffice Calc\nName[de]=LibreOffice Tabellen\nExec=libreoffice --calc %U\n\n[Desktop Action New]\nName=New Spreadsheet\n";

    #[test]
    fn a_name_matches_the_entry_name_ignoring_case_and_nothing_else() {
        assert!(named(CALC, "libreoffice calc"));
        assert!(named(CALC, "LibreOffice Calc"));
        assert!(!named(CALC, "LibreOffice Tabellen"));
        assert!(!named(CALC, "New Spreadsheet"));
        assert!(!named(
            &CALC.replace("Type=Application", "Type=Link"),
            "LibreOffice Calc"
        ));
        assert!(!named(
            &CALC.replace("Version=1.0", "NoDisplay=true"),
            "LibreOffice Calc"
        ));
        assert!(!named(
            &CALC.replace("Version=1.0", "Hidden=true"),
            "LibreOffice Calc"
        ));
    }

    #[test]
    fn a_multi_word_name_gives_the_id_of_its_entry_and_a_command_line_or_one_word_none() {
        let (user, system) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(system.path().join("libreoffice-calc.desktop"), CALC).unwrap();
        std::fs::write(
            system.path().join("org.gnome.Evince.desktop"),
            "[Desktop Entry]\nType=Application\nName=Document Viewer\nExec=evince %U\n",
        )
        .unwrap();
        let dirs = [user.path().to_path_buf(), system.path().to_path_buf()];
        assert_eq!(
            id_in("libreoffice calc", &dirs).as_deref(),
            Some("libreoffice-calc")
        );
        assert_eq!(
            id_in("document viewer", &dirs).as_deref(),
            Some("org.gnome.Evince")
        );
        assert_eq!(id_in("libreoffice --calc /tmp/x.ods", &dirs), None);
        assert_eq!(id_in("evince x.pdf", &dirs), None);
        assert_eq!(id_in("libreoffice", &dirs), None);
        std::fs::write(user.path().join("my-calc.desktop"), CALC).unwrap();
        assert_eq!(id_in("libreoffice calc", &dirs).as_deref(), Some("my-calc"));
    }
}
