use std::{
    io::Read,
    path::{Path, PathBuf},
};

use base64::{Engine, engine::general_purpose::STANDARD};

fn data_directories() -> Vec<PathBuf> {
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    let directories =
        std::env::var_os("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    home.into_iter()
        .chain(std::env::split_paths(&directories))
        .filter(|path| path.is_absolute())
        .collect()
}

fn read_small_file(path: &Path) -> Option<Vec<u8>> {
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(256 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= 256 * 1024).then_some(bytes)
}

fn read_icon(path: &Path) -> Option<String> {
    let media_type = match path.extension()?.to_str()? {
        "png" => "image/png",
        "svg" => "image/svg+xml",
        _ => return None,
    };
    Some(format!(
        "data:{media_type};base64,{}",
        STANDARD.encode(read_small_file(path)?)
    ))
}

fn desktop_icon(text: &str) -> Option<&str> {
    let mut entry = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            entry = line == "[Desktop Entry]";
        }
        if entry
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "Icon"
            && !value.trim().is_empty()
        {
            return Some(value.trim());
        }
    }
    None
}

fn find_icon(desktop_id: &str, directories: &[PathBuf]) -> Option<String> {
    let entry = directories.iter().find_map(|directory| {
        String::from_utf8(read_small_file(
            &directory
                .join("applications")
                .join(format!("{desktop_id}.desktop")),
        )?)
        .ok()
    })?;
    let name = desktop_icon(&entry)?;
    if Path::new(name).is_absolute() {
        return read_icon(Path::new(name));
    }
    if name.contains('/') || name.contains('\\') {
        return None;
    }
    for directory in directories {
        for size in [
            "512x512", "256x256", "128x128", "64x64", "48x48", "32x32", "scalable",
        ] {
            for extension in ["png", "svg"] {
                if let Some(icon) = read_icon(
                    &directory
                        .join("icons/hicolor")
                        .join(size)
                        .join("apps")
                        .join(format!("{name}.{extension}")),
                ) {
                    return Some(icon);
                }
            }
        }
        for extension in ["png", "svg"] {
            if let Some(icon) = read_icon(
                &directory
                    .join("pixmaps")
                    .join(format!("{name}.{extension}")),
            ) {
                return Some(icon);
            }
        }
    }
    None
}

pub(super) fn application_icon(id: &str) -> Option<String> {
    let desktop_id = match id {
        "vscode" => "code",
        "vscodeinsiders" => "code-insiders",
        "zed" => "dev.zed.Zed",
        "sublimetext" => "sublime_text",
        "sublimemerge" => "sublime_merge",
        "ghostty" => "com.mitchellh.ghostty",
        "kitty" => "kitty",
        "gnometerminal" => "org.gnome.Terminal",
        "konsole" => "org.kde.konsole",
        _ => return None,
    };
    find_icon(desktop_id, &data_directories())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_declared_desktop_icon_from_hicolor_without_using_action_icons() {
        let directory = tempfile::tempdir().unwrap();
        let applications = directory.path().join("applications");
        let icons = directory.path().join("icons/hicolor/scalable/apps");
        std::fs::create_dir_all(&applications).unwrap();
        std::fs::create_dir_all(&icons).unwrap();
        std::fs::write(
            applications.join("code.desktop"),
            "[Desktop Entry]\nIcon=vscode\n[Desktop Action new-window]\nIcon=ignored\n",
        )
        .unwrap();
        let image = "<svg xmlns=\"http://www.w3.org/2000/svg\"/>";
        std::fs::write(icons.join("vscode.svg"), image).unwrap();
        assert_eq!(
            find_icon("code", &[directory.path().to_owned()]),
            Some(format!(
                "data:image/svg+xml;base64,{}",
                STANDARD.encode(image)
            ))
        );
        assert!(find_icon("missing", &[directory.path().to_owned()]).is_none());
    }
}
