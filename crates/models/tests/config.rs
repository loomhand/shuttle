#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use shuttle_models::config::{Config, Confirm, DecideApi, default_path};

    fn file(text: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(text.as_bytes()).unwrap();
        f
    }

    #[test]
    fn a_missing_file_is_openrouter_with_its_key_variable() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config::load(&dir.path().join("config.toml")).unwrap();
        assert_eq!(c.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(c.decisions_url, "https://openrouter.ai/api/alpha/decisions");
        assert_eq!(c.api_key_env, "OPENROUTER_API_KEY");
        assert_eq!(c.decide_api, DecideApi::Decisions);
        assert_eq!(c.models.decide, "typesafe/jev-1.14");
        assert_eq!(c.models.verify, None);
        assert_eq!(c.timeout_ms, 60_000);
        assert_eq!(c.decide_build, None);
        let pinned = file("decide_build = \"typesafe/jev-1.14-########\"\n");
        assert_eq!(
            Config::load(pinned.path()).unwrap().decide_build.as_deref(),
            Some("typesafe/jev-1.14-########")
        );
        let empty = file("decide_build = \"\"\n");
        let err = Config::load(empty.path()).unwrap_err();
        assert!(err.contains("decide_build is empty"), "{err}");
    }

    #[test]
    fn a_file_overrides_some_fields_and_unknown_fields_are_refused() {
        let f = file(
            "base_url = \"http://127.0.0.1:8000/v1\"\ndecide_api = \"chat\"\ntimeout_ms = 5000\n\n[models]\ndecide = \"local-vl\"\nverify = \"local-vl-small\"\n",
        );
        let c = Config::load(f.path()).unwrap();
        assert_eq!(c.base_url, "http://127.0.0.1:8000/v1");
        assert_eq!(c.decisions_url, "https://openrouter.ai/api/alpha/decisions");
        assert_eq!((c.decide_api, c.timeout_ms), (DecideApi::Chat, 5000));
        assert_eq!(c.models.decide, "local-vl");
        assert_eq!(c.models.verify.as_deref(), Some("local-vl-small"));
        assert_eq!(c.models.ground, "qwen/qwen3-vl-235b-a22b-instruct");
        let bad = file("api_kee = \"k\"\n");
        assert!(Config::load(bad.path()).unwrap_err().contains("api_kee"));
        let role = file("[models]\nplan = \"m\"\n");
        assert!(Config::load(role.path()).unwrap_err().contains("plan"));
        let api = file("timeout_ms = 5\ndecide_api = \"anthropic\"\n");
        let err = Config::load(api.path()).unwrap_err();
        assert!(err.contains("line 2") && err.contains("anthropic"), "{err}");
    }

    #[test]
    fn the_safety_and_tools_tables_load_and_default_to_every_app_and_script_run_on() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config::load(&dir.path().join("config.toml")).unwrap();
        assert!(c.safety.apps.is_empty() && c.tools.script_run && c.safety.indicator);
        assert_eq!(c.safety.confirm, Confirm::Off);
        let f = file(
            "[safety]\napps = [\"TextEdit\", \"notepad.exe\"]\n\n[tools]\nscript_run = false\n",
        );
        let c = Config::load(f.path()).unwrap();
        assert_eq!(c.safety.apps, ["TextEdit", "notepad.exe"]);
        assert!(!c.tools.script_run);
        assert_eq!(c.tools.plugins, None);
        let dir_set = file("[tools]\nplugins = \"/etc/shuttle/tools.d\"\n");
        assert_eq!(
            Config::load(dir_set.path()).unwrap().tools.plugins,
            Some("/etc/shuttle/tools.d".into())
        );
        let ui = file(
            "[safety]\nkill_hotkey = \"ctrl+shift+k\"\nindicator = false\nconfirm = \"coordinate\"\n",
        );
        let c = Config::load(ui.path()).unwrap();
        assert_eq!(
            (c.safety.kill_hotkey.as_str(), c.safety.indicator),
            ("ctrl+shift+k", false)
        );
        assert_eq!(c.safety.confirm, Confirm::Coordinate);
        let dropped = file("[safety]\nuser_idle_ms = 1500\n");
        assert!(
            Config::load(dropped.path())
                .unwrap_err()
                .contains("user_idle_ms")
        );
    }

    #[test]
    fn the_driver_table_turns_the_overlay_off_and_defaults_to_on() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            Config::load(&dir.path().join("config.toml"))
                .unwrap()
                .driver
                .overlay
        );
        let off = file("[driver]\noverlay = false\n");
        assert!(!Config::load(off.path()).unwrap().driver.overlay);
        let bad = file("[driver]\ncursor = false\n");
        assert!(Config::load(bad.path()).unwrap_err().contains("cursor"));
        let text = file("[driver]\noverlay = \"off\"\n");
        let err = Config::load(text.path()).unwrap_err();
        assert!(err.contains("line 2") && err.contains("bool"), "{err}");
    }

    #[test]
    fn the_key_comes_from_the_file_else_from_its_variable() {
        let c = Config {
            api_key_env: "CARGO_PKG_NAME".to_owned(),
            ..Config::default()
        };
        assert_eq!(c.key().unwrap().expose(), env!("CARGO_PKG_NAME"));
        let c = Config {
            api_key: Some("sk-file".to_owned().into()),
            ..c
        };
        assert_eq!(c.key().unwrap().expose(), "sk-file");
        let c = Config {
            api_key_env: "SHUTTLE_TEST_UNSET_KEY".to_owned(),
            ..Config::default()
        };
        assert!(c.key().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_file_with_a_key_must_be_owner_only_and_the_key_never_prints() {
        use std::os::unix::fs::PermissionsExt;

        let f = file("api_key = \"sk-secret\"\n");
        std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = Config::load(f.path()).unwrap_err();
        assert!(err.contains("chmod 600"), "{err}");
        assert!(!err.contains("sk-secret"));
        std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
        let c = Config::load(f.path()).unwrap();
        assert!(!format!("{c:?}").contains("sk-secret"));
        assert_eq!(c.key().unwrap().expose(), "sk-secret");
    }

    #[test]
    fn a_daemon_section_names_the_socket_group_and_an_empty_one_is_refused() {
        assert!(
            Config::load(file("").path())
                .unwrap()
                .daemon
                .socket_group
                .is_none()
        );
        let shared = file("[daemon]\nsocket_group = \"agents\"\n");
        let loaded = Config::load(shared.path());
        if cfg!(windows) {
            assert!(loaded.unwrap_err().contains("unix socket"));
        } else {
            assert_eq!(
                loaded.unwrap().daemon.socket_group.as_deref(),
                Some("agents")
            );
        }
        let err = Config::load(file("[daemon]\nsocket_group = \"\"\n").path()).unwrap_err();
        assert!(err.contains("socket_group is empty"), "{err}");
        let err = Config::load(file("[daemon]\nsocket_mode = 438\n").path()).unwrap_err();
        assert!(err.contains("socket_mode"), "{err}");
    }

    #[test]
    fn the_config_path_is_per_program() {
        assert!(default_path("cocoon-shuttle").ends_with("cocoon-shuttle/config.toml"));
    }

    #[test]
    fn a_config_error_names_the_line_but_never_prints_it() {
        let f = file("base_url = \"x\"\ntimeout_ms = 5\n\n[models]\napikey = \"sk-live-secret\"\n");
        let err = Config::load(f.path()).unwrap_err();
        assert!(err.contains("line 5"), "{err}");
        assert!(!err.contains("sk-live-secret"), "{err}");
    }

    #[test]
    fn an_android_section_names_the_device_and_its_files() {
        assert!(
            Config::load(file("").path()).unwrap().android.is_none(),
            "no section serves this desktop"
        );
        let c = Config::load(
            file("[android]\nserial = \"127.0.0.1:5555\"\nui_jar = \"/u.jar\"\nscrcpy_jar = \"/s.jar\"\nsu = \"/su\"\n").path(),
        )
        .unwrap();
        let a = c.android.unwrap();
        assert_eq!(a.adb, std::path::PathBuf::from("adb"));
        assert_eq!(a.serial.as_deref(), Some("127.0.0.1:5555"));
        assert_eq!(a.ui_jar, Some(std::path::PathBuf::from("/u.jar")));
        assert_eq!(a.su, Some(std::path::PathBuf::from("/su")));
        let phone = Config::load(file("[android]\nscrcpy_jar = \"/s.jar\"\n").path())
            .unwrap()
            .android
            .unwrap();
        assert!(phone.su.is_none(), "a phone has no su");
        assert!(
            phone.ui_jar.is_none(),
            "the bundled UI server jar by default"
        );
        let bad = file(
            "[android]\nui_jar = \"/u.jar\"\nscrcpy_jar = \"/s.jar\"\nsu = \"/su\"\nport = 5\n",
        );
        assert!(Config::load(bad.path()).unwrap_err().contains("port"));
    }

    #[test]
    fn an_android_section_refuses_an_app_allowlist_it_does_not_enforce() {
        let android = "[android]\nui_jar = \"/u.jar\"\nscrcpy_jar = \"/s.jar\"\nsu = \"/su\"\n";
        let err = Config::load(file(&format!("[safety]\napps = [\"Settings\"]\n{android}")).path())
            .unwrap_err();
        assert!(err.contains("[safety] apps is not enforced"), "{err}");
        assert!(Config::load(file(&format!("[safety]\napps = []\n{android}")).path()).is_ok());
        assert!(Config::load(file("[safety]\napps = [\"Settings\"]\n").path()).is_ok());
    }
}
