#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;

use tauri::menu::Menu;
use tauri::Manager;
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
    window.state::<audio::Audio>().shutdown();
    window.reload().map_err(|error| error.to_string())
}

#[tauri::command]
fn window_pin_state(window: tauri::WebviewWindow) -> Result<bool, String> {
    allowed(&window)?;
    window.is_always_on_top().map_err(|error| error.to_string())
}

#[tauri::command]
async fn set_window_pin(window: tauri::WebviewWindow, pinned: bool) -> Result<bool, String> {
    allowed(&window)?;
    tauri::async_runtime::spawn_blocking(move || {
        window
            .set_always_on_top(pinned)
            .map_err(|error| error.to_string())?;
        // On macOS, Tao queues NSWindow.setLevel asynchronously. An immediate
        // getter can still return the previous state. Wait off the main thread
        // so native window operations can finish before confirming the toggle.
        confirm_window_pin(
            pinned,
            || window.is_always_on_top().map_err(|error| error.to_string()),
            || std::thread::sleep(std::time::Duration::from_millis(10)),
        )
    })
    .await
    .map_err(|error| error.to_string())?
}

fn confirm_window_pin(
    pinned: bool,
    mut read_state: impl FnMut() -> Result<bool, String>,
    mut wait: impl FnMut(),
) -> Result<bool, String> {
    for attempt in 0..100 {
        let actual = read_state()?;
        if actual == pinned {
            return Ok(actual);
        }
        if attempt < 99 {
            wait();
        }
    }
    Err("Window pin state did not change".into())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri::plugin::Builder::<tauri::Wry>::new("synth-desktop")
                .js_init_script(include_str!("../../desktop/desktop-interface.js"))
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            save_export,
            reload_window,
            window_pin_state,
            set_window_pin,
            audio::audio_devices,
            audio::audio_start,
            audio::audio_write,
            audio::audio_poll,
            audio::audio_stop,
        ])
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::Destroyed) {
                window.state::<audio::Audio>().shutdown();
            }
        })
        .setup(|app| {
            app.manage(audio::Audio::default());
            // Desktop-only extensions leave the pinned Synth assets untouched.
            tauri::WebviewWindowBuilder::from_config(app, &app.config().app.windows[0])?
                .on_web_resource_request(|request, response| {
                    let local = local_origin(
                        request.uri().scheme_str().unwrap_or(""),
                        request.uri().host(),
                    );
                    if local && request.uri().path() == "/desktop-audio-worklet.js" {
                        *response.status_mut() = tauri::http::StatusCode::OK;
                        *response.body_mut() = std::borrow::Cow::Owned(
                            include_bytes!("../../desktop/audio-worklet.js").to_vec(),
                        );
                        response
                            .headers_mut()
                            .insert("content-type", "text/javascript".parse().unwrap());
                        response.headers_mut().remove("content-length");
                    }
                    if local
                        && request.uri().path() == "/synth.js"
                        && response.status().is_success()
                    {
                        let mut bytes = response.body().to_vec();
                        bytes.extend_from_slice(b"\n;\n");
                        bytes.extend_from_slice(include_bytes!("../../desktop/audio-interface.js"));
                        *response.body_mut() = std::borrow::Cow::Owned(bytes);
                        response.headers_mut().remove("content-length");
                        response.headers_mut().remove("etag");
                    }
                })
                .build()?;
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
    #[test]
    fn pin_confirmation_waits_for_queued_native_change() {
        for pinned in [false, true] {
            let waits = std::cell::Cell::new(0);
            let reads = std::cell::Cell::new(0);
            let actual = confirm_window_pin(
                pinned,
                || {
                    reads.set(reads.get() + 1);
                    Ok(if waits.get() >= 2 { pinned } else { !pinned })
                },
                || waits.set(waits.get() + 1),
            )
            .unwrap();
            assert_eq!(actual, pinned);
            assert_eq!(reads.get(), 3);
            assert_eq!(waits.get(), 2);
        }
    }
    #[test]
    fn pin_confirmation_is_bounded_when_native_state_does_not_change() {
        let waits = std::cell::Cell::new(0);
        let reads = std::cell::Cell::new(0);
        let error = confirm_window_pin(
            true,
            || {
                reads.set(reads.get() + 1);
                Ok(false)
            },
            || waits.set(waits.get() + 1),
        )
        .unwrap_err();
        assert_eq!(error, "Window pin state did not change");
        assert_eq!(reads.get(), 100);
        assert_eq!(waits.get(), 99);
    }
    #[test]
    fn pin_confirmation_stops_on_native_error() {
        let error = confirm_window_pin(
            true,
            || Err("window closed".into()),
            || panic!("A failed getter must not continue polling"),
        )
        .unwrap_err();
        assert_eq!(error, "window closed");
    }
}
