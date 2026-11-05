//! Desktop entries a launch name can repeat: an agent names an app as its menu shows it, and a name such as "libreoffice calc" otherwise runs `libreoffice` on a file named calc.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

static ENTRIES: Mutex<Entries> = Mutex::new(Entries {
    dirs: Vec::new(),
    apps: Vec::new(),
});

struct Entries {
    dirs: Vec<(PathBuf, Option<SystemTime>)>,
    apps: Vec<(String, String)>,
}

impl Entries {
    fn id_in(&mut self, name: &str, dirs: Vec<PathBuf>) -> Option<String> {
        let stamped: Vec<(PathBuf, Option<SystemTime>)> = dirs
            .into_iter()
            .map(|d| {
                let at = std::fs::metadata(&d).and_then(|m| m.modified()).ok();
                (d, at)
            })
            .collect();
        if self.dirs != stamped {
            self.apps = apps_in(stamped.iter().map(|(d, _)| d));
            self.dirs = stamped;
        }
        self.apps
            .iter()
            .find(|(title, _)| title.eq_ignore_ascii_case(name))
            .map(|(_, id)| id.clone())
    }
}

/// The desktop file id of the application entry whose `Name` equals `name` ignoring case, for a name of more than one word; the user's entries come before the system's, as the driver reads them.
pub fn id_for(name: &str) -> Option<String> {
    let name = name.trim();
    if !name.contains(char::is_whitespace) {
        return None;
    }
    ENTRIES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .id_in(name, dirs())
}

fn apps_in<'a>(dirs: impl Iterator<Item = &'a PathBuf>) -> Vec<(String, String)> {
    let mut apps = Vec::new();
    for dir in dirs {
        let Ok(listed) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = listed
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "desktop"))
            .collect();
        files.sort();
        apps.extend(files.into_iter().filter_map(|p| {
            let body = std::fs::read_to_string(&p).ok()?;
            let id = p.file_stem()?.to_str()?.to_owned();
            Some((shown_name(&body)?.to_owned(), id))
        }));
    }
    apps
}

/// The `Name` of the `[Desktop Entry]` group of `body` when it is a shown application.
fn shown_name(body: &str) -> Option<&str> {
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
    title.filter(|_| app && shown)
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

    fn id_in(name: &str, dirs: &[PathBuf]) -> Option<String> {
        Entries {
            dirs: Vec::new(),
            apps: Vec::new(),
        }
        .id_in(name, dirs.to_vec())
    }

    #[test]
    fn an_entry_is_named_by_its_desktop_entry_name_when_a_shown_application() {
        assert_eq!(shown_name(CALC), Some("LibreOffice Calc"));
        assert_eq!(
            shown_name(&CALC.replace("Type=Application", "Type=Link")),
            None
        );
        assert_eq!(
            shown_name(&CALC.replace("Version=1.0", "NoDisplay=true")),
            None
        );
        assert_eq!(
            shown_name(&CALC.replace("Version=1.0", "Hidden=true")),
            None
        );
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
        assert_eq!(id_in("libreoffice tabellen", &dirs), None);
        assert_eq!(id_in("new spreadsheet", &dirs), None);
    }

    #[test]
    fn the_entries_are_read_once_until_an_applications_dir_changes() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = vec![dir.path().to_path_buf()];
        let calc = dir.path().join("calc.desktop");
        std::fs::write(&calc, CALC).unwrap();
        let mut entries = Entries {
            dirs: Vec::new(),
            apps: Vec::new(),
        };
        assert_eq!(
            entries.id_in("libreoffice calc", dirs.clone()).as_deref(),
            Some("calc")
        );
        std::fs::write(
            &calc,
            CALC.replace("Name=LibreOffice Calc", "Name=Other App"),
        )
        .unwrap();
        assert_eq!(
            entries.id_in("libreoffice calc", dirs.clone()).as_deref(),
            Some("calc")
        );
        std::fs::write(
            dir.path().join("evince.desktop"),
            "[Desktop Entry]\nType=Application\nName=Document Viewer\n",
        )
        .unwrap();
        assert_eq!(
            entries.id_in("document viewer", dirs.clone()).as_deref(),
            Some("evince")
        );
        assert_eq!(entries.id_in("libreoffice calc", dirs), None);
    }
}
