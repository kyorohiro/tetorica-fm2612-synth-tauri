#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;

use tauri::menu::Menu;
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;

const MAX_EXPORT_BYTES: usize = 128 * 1024 * 1024;
const MAX_EXPORT_HEADER_BYTES: usize = 4 * 1024;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportHeader {
    filename: String,
}

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

fn validate_export_size(length: usize) -> Result<(), String> {
    if length > MAX_EXPORT_BYTES {
        return Err("Export exceeds 128 MiB".into());
    }
    Ok(())
}

// Raw IPC avoids representing each file byte as a JSON number. The small
// UTF-8 header is length-prefixed; the remaining bytes are the original file.
fn parse_export_packet(packet: &[u8]) -> Result<(String, &[u8]), String> {
    if packet.len() < 4 {
        return Err("Export packet is missing its header length".into());
    }
    let header_length = u32::from_le_bytes(packet[..4].try_into().unwrap()) as usize;
    if header_length == 0 || header_length > MAX_EXPORT_HEADER_BYTES {
        return Err("Export header must contain 1..4096 bytes".into());
    }
    let data_offset = 4 + header_length;
    if data_offset > packet.len() {
        return Err("Export packet contains a truncated header".into());
    }
    let bytes = &packet[data_offset..];
    validate_export_size(bytes.len())?;
    let header: ExportHeader = serde_json::from_slice(&packet[4..data_offset])
        .map_err(|error| format!("Invalid export header: {error}"))?;
    Ok((export_name(&header.filename), bytes))
}

#[tauri::command]
async fn save_export(
    window: tauri::WebviewWindow,
    filename: String,
    bytes: Vec<u8>,
) -> Result<Option<String>, String> {
    allowed(&window)?;
    validate_export_size(bytes.len())?;
    let filename = export_name(&filename);
    save_export_file(window, filename, bytes).await
}

#[tauri::command]
async fn save_export_binary(
    window: tauri::WebviewWindow,
    request: tauri::ipc::Request<'_>,
) -> Result<Option<String>, String> {
    allowed(&window)?;
    let tauri::ipc::InvokeBody::Raw(packet) = request.body() else {
        return Err("Export requires a binary file packet".into());
    };
    let (filename, bytes) = parse_export_packet(packet)?;
    // The request is borrowed by Tauri. Own the file bytes before handing them
    // to the blocking native dialog task; no JSON conversion is involved.
    save_export_file(window, filename, bytes.to_vec()).await
}

async fn save_export_file(
    window: tauri::WebviewWindow,
    filename: String,
    bytes: Vec<u8>,
) -> Result<Option<String>, String> {
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
            save_export_binary,
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
        assert_eq!(
            export_name("../../音色\0.cassette.zip"),
            "音色.cassette.zip"
        );
        assert_eq!(export_name(&"界".repeat(200)).chars().count(), 160);
    }
    fn binary_export_packet(header: &[u8], bytes: &[u8]) -> Vec<u8> {
        let mut packet = (header.len() as u32).to_le_bytes().to_vec();
        packet.extend_from_slice(header);
        packet.extend_from_slice(bytes);
        packet
    }
    #[test]
    fn binary_export_keeps_utf8_filename_and_original_file_bytes() {
        let header = serde_json::to_vec(&serde_json::json!({
            "filename": "../../音色.cassette.zip"
        }))
        .unwrap();
        let original: Vec<u8> = (0..=255).collect();
        let packet = binary_export_packet(&header, &original);
        let (filename, bytes) = parse_export_packet(&packet).unwrap();
        assert_eq!(filename, "音色.cassette.zip");
        assert_eq!(bytes, original);
        // Parsing borrows the data instead of making another full-size copy.
        assert_eq!(bytes.as_ptr(), packet[4 + header.len()..].as_ptr());
    }
    #[test]
    fn binary_export_accepts_empty_files_without_confusing_header_with_data() {
        let packet = binary_export_packet(br#"{"filename":"empty.bin"}"#, &[]);
        let (filename, bytes) = parse_export_packet(&packet).unwrap();
        assert_eq!(filename, "empty.bin");
        assert!(bytes.is_empty());
    }
    #[test]
    fn binary_export_rejects_missing_oversized_or_truncated_headers() {
        for length in 0..4 {
            assert!(parse_export_packet(&vec![0; length]).is_err());
        }
        assert!(parse_export_packet(&[0; 4]).is_err());
        for length in [MAX_EXPORT_HEADER_BYTES as u32 + 1, u32::MAX] {
            assert!(parse_export_packet(&length.to_le_bytes()).is_err());
        }
        let mut truncated = 20u32.to_le_bytes().to_vec();
        truncated.extend_from_slice(b"{}");
        assert!(parse_export_packet(&truncated)
            .unwrap_err()
            .contains("truncated header"));
    }
    #[test]
    fn binary_export_header_requires_valid_utf8_json_with_only_a_filename() {
        for header in [
            b"not json".as_slice(),
            b"\xff".as_slice(),
            br#"[]"#.as_slice(),
            br#"{}"#.as_slice(),
            br#"{"filename":1}"#.as_slice(),
            br#"{"filename":"a","path":"/tmp"}"#.as_slice(),
            br#"{"filename":"a","filename":"b"}"#.as_slice(),
        ] {
            let packet = binary_export_packet(header, b"file data");
            assert!(parse_export_packet(&packet)
                .unwrap_err()
                .starts_with("Invalid export header:"));
        }
    }
    #[test]
    fn export_size_limit_is_shared_by_legacy_and_binary_saves() {
        assert!(validate_export_size(0).is_ok());
        assert!(validate_export_size(16 * 1024 * 1024).is_ok());
        assert!(validate_export_size(MAX_EXPORT_BYTES).is_ok());
        assert_eq!(
            validate_export_size(MAX_EXPORT_BYTES + 1).unwrap_err(),
            "Export exceeds 128 MiB"
        );
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
