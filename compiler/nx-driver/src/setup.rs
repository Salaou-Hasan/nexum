//! First-run setup command for the nx CLI.
//!
//! Registers the .nx file icon with the vscode-icons and Material Icon
//! Theme packs by editing the VS Code settings.json.

use std::process::ExitCode;

const NX_SVG: &str = include_str!("../../../editors/vscode-nexum/icons/file_type_nx.svg");

pub(crate) fn vscode_extensions_dir() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    let home = std::env::var("USERPROFILE")
        .ok()
        .map(std::path::PathBuf::from)?;
    #[cfg(not(windows))]
    let home = std::env::var("HOME").ok().map(std::path::PathBuf::from)?;
    Some(home.join(".vscode/extensions"))
}

pub(crate) fn vscode_settings_path() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    let base = std::env::var("APPDATA")
        .ok()
        .map(std::path::PathBuf::from)?;
    #[cfg(target_os = "macos")]
    let base = std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join("Library/Application Support"))?;
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join(".config"))?;
    Some(base.join("Code/User/settings.json"))
}

pub(crate) fn nx_data_icons() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    let base = std::env::var("APPDATA")
        .ok()
        .map(std::path::PathBuf::from)?;
    #[cfg(target_os = "macos")]
    let base = std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join("Library/Application Support/Nexum"))?;
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join(".local/share/Nexum"))?;
    #[cfg(windows)]
    return Some(base.join("Nexum/icons"));
    #[cfg(not(windows))]
    return Some(base.join("icons"));
}

pub(crate) fn settings_theme(text: &str) -> Option<String> {
    text.split("\"workbench.iconTheme\"")
        .nth(1)?
        .split(':')
        .nth(1)?
        .split('"')
        .nth(1)
        .map(|s| s.to_string())
}

/// Insert `"key": value` before the root closing brace. Returns (text, changed).
pub(crate) fn json_insert_key(text: &str, key: &str, value: &str) -> (String, bool) {
    if text.contains(&format!("\"{key}\"")) {
        return (text.to_string(), false);
    }
    let close = match text.rfind('}') {
        Some(i) => i,
        None => return (text.to_string(), false),
    };
    let before: String = text[..close]
        .chars()
        .rev()
        .take_while(|c| c.is_whitespace())
        .collect();
    let trimmed = text[..close].trim_end();
    // Empty object -> no comma; existing trailing comma -> don't double it.
    let need_comma = !trimmed.ends_with('{') && !trimmed.ends_with(',');
    let mut out = String::with_capacity(text.len() + key.len() + value.len() + 8);
    out.push_str(trimmed);
    if need_comma {
        out.push(',');
    }
    out.push_str(&format!("\n    \"{key}\": {value}"));
    out.push_str(&before);
    out.push('}');
    out.push_str(&text[close + 1..]);
    (out, true)
}

/// Set `inner` to `value_json` inside the top-level object `top`.
/// Creates the object if missing. Handles JSONC comments. Returns (text, changed).
pub(crate) fn json_object_set(
    text: &str,
    top: &str,
    inner: &str,
    value_json: &str,
) -> (String, bool) {
    let key = format!("\"{top}\"");
    let kpos = match text.find(&key) {
        Some(p) => p,
        None => {
            let (t, _) = json_insert_key(text, top, &format!("{{\"{inner}\": {value_json}}}"));
            return (t, true);
        }
    };
    // Find object start after the key.
    let bytes = text.as_bytes();
    let mut i = kpos + key.len();
    let n = bytes.len();
    // skip whitespace/comments/colon
    let mut colon = false;
    let mut obj_start = None;
    while i < n {
        let c = bytes[i] as char;
        if c == ':' && !colon {
            colon = true;
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '/' && i + 1 < n && bytes[i + 1] as char == '/' {
            while i < n && bytes[i] as char != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '{' && colon {
            obj_start = Some(i);
            break;
        }
        break;
    }
    let obj_start = match obj_start {
        Some(p) => p,
        None => return (text.to_string(), false),
    };
    let obj_end = match match_brace(text, obj_start) {
        Some(p) => p,
        None => return (text.to_string(), false),
    };
    let inner_key = format!("\"{inner}\"");
    let body = &text[obj_start..=obj_end];
    if let Some(rel) = body.find(&inner_key) {
        // Replace existing value.
        let abs = obj_start + rel + inner_key.len();
        let mut j = abs;
        while j < n && (bytes[j] as char).is_whitespace() {
            j += 1;
        }
        if j >= n || bytes[j] as char != ':' {
            return (text.to_string(), false);
        }
        j += 1;
        while j < n && (bytes[j] as char).is_whitespace() {
            j += 1;
        }
        let vend = match value_end(text, j) {
            Some(p) => p,
            None => return (text.to_string(), false),
        };
        if text[j..vend].trim() == value_json {
            return (text.to_string(), false);
        }
        let mut out = String::new();
        out.push_str(&text[..j]);
        out.push_str(value_json);
        out.push_str(&text[vend..]);
        return (out, true);
    }
    // Insert new entry before the closing brace.
    let mut out = String::new();
    out.push_str(text[..obj_end].trim_end());
    if !text[obj_start + 1..obj_end].trim().is_empty() && !text[..obj_end].trim_end().ends_with(',')
    {
        out.push(',');
    }
    out.push_str(&format!("\n        \"{inner}\": {value_json}"));
    out.push_str(&text[obj_end..]);
    (out, true)
}

/// Find the matching close brace/bracket from an opener, JSONC-aware.
pub(crate) fn match_brace(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let n = bytes.len();
    let (mut depth, opener, closer) = (
        0i32,
        bytes[open] as char,
        match bytes[open] as char {
            '{' => '}',
            '[' => ']',
            _ => return None,
        },
    );
    let _ = opener;
    let mut i = open;
    let mut in_str = false;
    let mut esc = false;
    while i < n {
        let c = bytes[i] as char;
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
        } else if c == '/' && i + 1 < n && bytes[i + 1] as char == '/' {
            while i < n && bytes[i] as char != '\n' {
                i += 1;
            }
        } else if c == '/' && i + 1 < n && bytes[i + 1] as char == '*' {
            i += 2;
            while i + 1 < n && !(bytes[i] as char == '*' && bytes[i + 1] as char == '/') {
                i += 1;
            }
            i += 1;
        } else if c == '{' || c == '[' {
            depth += 1;
        } else if c == '}' || c == ']' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        let _ = closer;
        i += 1;
    }
    None
}

/// End (exclusive) of a JSON value starting at `start`.
pub(crate) fn value_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let n = bytes.len();
    if start >= n {
        return None;
    }
    match bytes[start] as char {
        '"' => {
            let mut i = start + 1;
            while i < n {
                let c = bytes[i] as char;
                if c == '\\' {
                    i += 2;
                    continue;
                }
                if c == '"' {
                    return Some(i + 1);
                }
                i += 1;
            }
            None
        }
        '{' | '[' => match_brace(text, start).map(|p| p + 1),
        _ => {
            let mut i = start;
            while i < n && !matches!(bytes[i] as char, ',' | '}' | ']') {
                i += 1;
            }
            Some(i)
        }
    }
}

pub(crate) fn setup_cmd(rest: &[String]) -> ExitCode {
    let apply = rest.iter().any(|a| a == "--apply");
    let quiet = rest.iter().any(|a| a == "--quiet");
    let log = |msg: &str| {
        if !quiet {
            println!("{msg}");
        }
    };

    let settings = match vscode_settings_path() {
        Some(p) => p,
        None => {
            eprintln!("nx setup: cannot locate VS Code settings dir on this platform");
            return ExitCode::from(1);
        }
    };
    let text = match std::fs::read_to_string(&settings) {
        Ok(t) => t,
        Err(_) => {
            log("nx setup: no VS Code settings.json found (is VS Code installed?)");
            log("nx setup: open VS Code once, then re-run: nx setup --apply");
            return ExitCode::SUCCESS;
        }
    };
    let theme = settings_theme(&text).unwrap_or_default();
    log(&format!(
        "nx setup: icon theme is '{}'",
        if theme.is_empty() {
            "(default)"
        } else {
            &theme
        }
    ));

    if theme.contains("vsicons") {
        // vscode-icons: point it at our N icon, add the .nx mapping.
        let icons = match nx_data_icons() {
            Some(d) => d,
            None => {
                eprintln!("nx setup: cannot locate app data dir");
                return ExitCode::from(1);
            }
        };
        if let Err(e) = std::fs::create_dir_all(&icons)
            .and_then(|_| std::fs::write(icons.join("file_type_nx.svg"), NX_SVG))
        {
            eprintln!("nx setup: cannot write icon pack: {e}");
            return ExitCode::from(1);
        }
        let folder = icons.to_string_lossy().replace('\\', "\\\\");
        let assoc = r#"[{"icon": "nx", "extensions": ["nx"], "format": "svg"}]"#;
        if !apply {
            println!("nx setup: would add to {}", settings.display());
            println!("  \"vsicons.customIconFolderPath\": \"{folder}\"");
            println!("  \"vsicons.associations.files\": {assoc}");
            println!("nx setup: re-run with --apply to write it (a .bak backup is kept)");
            return ExitCode::SUCCESS;
        }
        let (t2, _) = json_insert_key(
            &text,
            "vsicons.customIconFolderPath",
            &format!("\"{folder}\""),
        );
        // Merge the nx entry into the associations array when present.
        let mut t3 = t2.clone();
        if t3.contains("\"vsicons.associations.files\"") {
            if !t3.contains("\"nx\"") {
                let entry = r#"{"icon": "nx", "extensions": ["nx"], "format": "svg"}"#;
                let key_pos = t3.find("\"vsicons.associations.files\"").unwrap();
                let open = t3[key_pos..].find('[').map(|b| key_pos + b).unwrap();
                let rest = &t3[open + 1..];
                let is_empty = rest.trim_start().starts_with(']');
                if is_empty {
                    t3.insert_str(open + 1, entry);
                } else {
                    t3.insert_str(open + 1, &format!("{entry},"));
                }
            }
        } else {
            let (t, _) = json_insert_key(&t3, "vsicons.associations.files", assoc);
            t3 = t;
        }
        if t3 == text {
            log("nx setup: already configured");
            return ExitCode::SUCCESS;
        }
        let bak = settings.with_extension("json.bak");
        let _ = std::fs::copy(&settings, &bak);
        match std::fs::write(&settings, t3) {
            Ok(_) => {
                log(&format!(
                    "nx setup: updated {} (backup at {})",
                    settings.display(),
                    bak.display()
                ));
                log("nx setup: reload VS Code window to see the N icon");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("nx setup: cannot write settings: {e}");
                ExitCode::from(1)
            }
        }
    } else if theme.contains("material-icon") {
        // Material Icon Theme supports custom SVGs referenced by path
        // (relative to the theme's dist folder, no .svg suffix).
        // Restriction: the folder must live inside .vscode/extensions.
        let ext_dir = match vscode_extensions_dir() {
            Some(d) => d,
            None => {
                eprintln!("nx setup: cannot locate VS Code extensions dir");
                return ExitCode::from(1);
            }
        };
        let icons = ext_dir.join("icons");
        if let Err(e) = std::fs::create_dir_all(&icons)
            .and_then(|_| std::fs::write(icons.join("nx.svg"), NX_SVG))
        {
            eprintln!("nx setup: cannot write icon: {e}");
            return ExitCode::from(1);
        }
        const WANT: &str = "\"../../icons/nx\"";
        if !apply {
            println!(
                "nx setup: icon written to {}",
                icons.join("nx.svg").display()
            );
            println!("nx setup: would set \"material-icon-theme.files.associations\": {{ \"*.nx\": {WANT} }}");
            println!("nx setup: re-run with --apply to write it (a .bak backup is kept)");
            return ExitCode::SUCCESS;
        }
        let (t2, _) = json_object_set(
            &text,
            "material-icon-theme.files.associations",
            "*.nx",
            WANT,
        );
        if t2 == text {
            log("nx setup: already configured");
            return ExitCode::SUCCESS;
        }
        let bak = settings.with_extension("json.bak");
        let _ = std::fs::copy(&settings, &bak);
        match std::fs::write(&settings, t2) {
            Ok(_) => {
                log("nx setup: updated with the original N icon, reload VS Code window");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("nx setup: cannot write settings: {e}");
                ExitCode::from(1)
            }
        }
    } else {
        // Policy: only vscode-icons and Material Icon Theme accept custom
        // SVGs (verified; Catppuccin explicitly refuses, the rest have no
        // API). Never borrow another pack's icon — leave the theme alone.
        log(&format!(
            "nx setup: '{theme}' has no custom-icon support, leaving it untouched"
        ));
        log("nx setup: to see the N logo, select the 'Nexum Icons' file icon theme");
        log("nx setup: Preferences -> File Icon Theme -> Nexum Icons (ships in the vsix)");
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn json_object_set_inserts_and_replaces() {
        let (t, c) = super::json_object_set(
            "{\n    \"a\": 1\n}",
            "material-icon-theme.files.associations",
            "*.nx",
            "\"../../icons/nx\"",
        );
        assert!(c);
        assert!(t.contains("\"*.nx\": \"../../icons/nx\""));
        // Idempotent.
        let (t2, c2) = super::json_object_set(
            &t,
            "material-icon-theme.files.associations",
            "*.nx",
            "\"../../icons/nx\"",
        );
        assert!(!c2);
        assert_eq!(t, t2);
        // Replace python mapping with the custom icon.
        let (t3, c3) = super::json_object_set(
            "{\"material-icon-theme.files.associations\": {\"*.nx\": \"python\"}}",
            "material-icon-theme.files.associations",
            "*.nx",
            "\"../../icons/nx\"",
        );
        assert!(c3);
        assert!(t3.contains("\"*.nx\": \"../../icons/nx\""));
        assert!(!t3.contains("python"));
    }
}
