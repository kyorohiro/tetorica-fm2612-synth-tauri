#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::menu::Menu;
use tauri_plugin_dialog::DialogExt;

fn local_origin(scheme: &str, host: Option<&str>) -> bool {
    (scheme == "tauri" && host == Some("localhost"))
        || (["http", "https"].contains(&scheme) && host == Some("tauri.localhost"))
}

fn allowed(window: &tauri::WebviewWindow) -> Result<(), String> {
    let url = window.url().map_err(|error| error.to_string())?;
    if window.label() != "main" || !local_origin(url.scheme(), url.host_str()) {
        return Err("Only the local Synth window can use desktop commands".into());
    }
    Ok(())
}

fn export_name(filename: &str) -> String {
    let name: String = filename
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|character| !character.is_control())
        .take(160)
        .collect();
    if name.is_empty() || name == "." || name == ".." {
        "export.bin".into()
    } else {
        name
    }
}

#[tauri::command]
async fn save_export(
    window: tauri::WebviewWindow,
    filename: String,
    bytes: Vec<u8>,
) -> Result<Option<String>, String> {
    allowed(&window)?;
    if bytes.len() > 128 * 1024 * 1024 {
        return Err("Export exceeds 128 MiB".into());
    }
    let filename = export_name(&filename);
    tauri::async_runtime::spawn_blocking(move || {
        let chosen = window
            .dialog()
            .file()
            .set_parent(&window)
            .set_file_name(&filename)
            .blocking_save_file();
        let Some(chosen) = chosen else {
            return Ok(None);
        };
        let path = chosen.into_path().map_err(|error| error.to_string())?;
        std::fs::write(&path, bytes).map_err(|error| error.to_string())?;
        Ok(Some(path.to_string_lossy().into_owned()))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
fn reload_window(window: tauri::WebviewWindow) -> Result<(), String> {
    allowed(&window)?;
    window.reload().map_err(|error| error.to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri::plugin::Builder::<tauri::Wry>::new("synth-desktop")
                .js_init_script(include_str!("../../desktop/desktop-interface.js"))
                .build(),
        )
        .invoke_handler(tauri::generate_handler![save_export, reload_window])
        .setup(|app| {
            app.set_menu(Menu::default(app.handle())?)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to run Tetorica FM2612 Synth");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_commands_only_accept_packaged_origins() {
        assert!(local_origin("tauri", Some("localhost")));
        assert!(local_origin("http", Some("tauri.localhost")));
        assert!(local_origin("https", Some("tauri.localhost")));
        assert!(!local_origin("tauri", Some("remote")));
        assert!(!local_origin("http", Some("localhost")));
        assert!(!local_origin("https", Some("example.com")));
    }
    #[test]
    fn suggested_export_name_is_only_a_filename() {
        assert_eq!(export_name("../../voice.tfi"), "voice.tfi");
        assert_eq!(export_name("C:\\voice.vgi"), "voice.vgi");
        assert_eq!(export_name("\n"), "export.bin");
        assert_eq!(export_name(".."), "export.bin");
    }
}
