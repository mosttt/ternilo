use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

#[test]
fn official_launcher_switches_the_complete_release_and_restores_without_changing_old_files() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("user's installation");
    let bin = root.join("releases/0.2.20-x86_64-unknown-linux-gnu/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir(root.join("bin")).unwrap();
    for name in ["ternilo", "ternilo-plugin"] {
        fs::write(bin.join(name), "#!/bin/sh\nprintf 'old:%s' \"$*\"\n").unwrap();
        fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let source = format!(
        "#!/bin/sh\n{}\nif [ \"$#\" -eq 0 ]; then\n  set -- serve --open-browser\nfi\nexec \"$ternilo_binary\" \"$@\"\n",
        binary_assignment(&bin.join("ternilo")).unwrap()
    );
    fs::write(root.join("bin/ternilo"), &source).unwrap();
    fs::set_permissions(root.join("bin/ternilo"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink(bin.join("ternilo-plugin"), root.join("bin/ternilo-plugin")).unwrap();
    let installation = Installation::detect(&bin.join("ternilo")).unwrap();
    assert_eq!(installation.control_dir(), root);
    let staged = directory.path().join("stage");
    fs::create_dir(&staged).unwrap();
    for name in ["ternilo", "ternilo-plugin"] {
        fs::write(staged.join(name), "#!/bin/sh\nprintf 'new:%s' \"$*\"\n").unwrap();
        fs::set_permissions(staged.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let backup = root.join("backup");
    let next = installation
        .install(&staged, &backup, "0.2.21", "x86_64-unknown-linux-gnu")
        .unwrap();
    assert_eq!(
        next.bin(),
        root.join("releases/0.2.21-x86_64-unknown-linux-gnu/bin")
    );
    let run = |name, args: &[&str]| {
        let result = std::process::Command::new(root.join("bin").join(name))
            .args(args)
            .output()
            .unwrap();
        assert!(result.status.success());
        String::from_utf8(result.stdout).unwrap()
    };
    assert_eq!(run("ternilo", &[]), "new:serve --open-browser");
    assert_eq!(run("ternilo", &["--version"]), "new:--version");
    assert_eq!(run("ternilo-plugin", &["list"]), "new:list");
    assert!(
        fs::read_to_string(bin.join("ternilo"))
            .unwrap()
            .contains("old:")
    );
    assert!(
        next.check_destination("0.2.21", "x86_64-unknown-linux-gnu")
            .is_err()
    );
    installation
        .restore(&backup, "x86_64-unknown-linux-gnu")
        .unwrap();
    installation
        .restore(&backup, "x86_64-unknown-linux-gnu")
        .unwrap();
    assert_eq!(run("ternilo", &[]), "old:serve --open-browser");
    assert_eq!(run("ternilo-plugin", &["list"]), "old:list");
    assert_eq!(
        fs::read_to_string(root.join("bin/ternilo")).unwrap(),
        source
    );
    assert!(next.bin().join("ternilo").is_file());
}
