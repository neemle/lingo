#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use reader_core::{Backup, Document, Original, Snapshot, Store, Vocabulary};
use serde::{Deserialize, Serialize};
use std::{
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};
use tauri::{Emitter, Manager, State};
struct AppState {
    store: Mutex<Store>,
    speech: Mutex<Option<Child>>,
    generation: AtomicU64,
    voice_list: Vec<Voice>,
}
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Voice {
    id: String,
    name: String,
    language: String,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpeechFinished {
    generation: u64,
    error: Option<String>,
}
#[tauri::command]
fn load(state: State<AppState>) -> Result<Snapshot> {
    state.store.lock().map_err(|e| e.to_string())?.snapshot()
}
#[tauri::command]
fn import_document(
    state: State<AppState>,
    name: String,
    bytes: Vec<u8>,
    language: String,
) -> Result<Document> {
    state
        .store
        .lock()
        .map_err(|e| e.to_string())?
        .import(&name, &bytes, &language)
}
#[tauri::command]
fn save_position(state: State<AppState>, id: String, position: usize) -> Result<()> {
    state
        .store
        .lock()
        .map_err(|e| e.to_string())?
        .position(&id, position)
}
#[tauri::command]
fn save_word(state: State<AppState>, entry: Vocabulary) -> Result<Vocabulary> {
    state
        .store
        .lock()
        .map_err(|e| e.to_string())?
        .save_word(entry)
}
#[tauri::command]
fn review(state: State<AppState>, id: String, direction: String, grade: u8) -> Result<()> {
    state
        .store
        .lock()
        .map_err(|e| e.to_string())?
        .review(&id, &direction, grade)
}
#[tauri::command]
fn export_backup(state: State<AppState>) -> Result<Backup> {
    state.store.lock().map_err(|e| e.to_string())?.export()
}
#[tauri::command]
fn restore_backup(state: State<AppState>, backup: Backup) -> Result<()> {
    state
        .store
        .lock()
        .map_err(|e| e.to_string())?
        .restore(backup)
}
#[tauri::command]
fn original(state: State<AppState>, id: String) -> Result<Original> {
    state.store.lock().map_err(|e| e.to_string())?.original(&id)
}
#[tauri::command]
async fn export_file(app: tauri::AppHandle, kind: String, id: Option<String>) -> Result<bool> {
    use tauri_plugin_dialog::DialogExt;
    let (name, bytes) = {
        let state = app.state::<AppState>();
        let store = state.store.lock().map_err(|e| e.to_string())?;
        if kind == "backup" {
            (
                "lingo-backup.json".to_string(),
                serde_json::to_vec(&store.export()?).map_err(|e| e.to_string())?,
            )
        } else if kind == "original" {
            let original = store.original(&id.ok_or("Не вказаний документ")?)?;
            (original.name, original.bytes)
        } else {
            return Err("Невідомий формат експорту".into());
        }
    };
    let selected = app
        .dialog()
        .file()
        .set_file_name(&name)
        .blocking_save_file();
    if let Some(path) = selected {
        let path = path.into_path().map_err(|e| e.to_string())?;
        std::fs::write(path, bytes).map_err(|e| e.to_string())?;
        Ok(true)
    } else {
        Ok(false)
    }
}
#[tauri::command]
fn voices(state: State<AppState>) -> Result<Vec<Voice>> {
    Ok(state.voice_list.clone())
}
fn detect_voices() -> Result<Vec<Voice>> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("/usr/bin/say")
            .args(["-v", "?"])
            .output()
            .map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let before = line.split('#').next()?.trim();
                let (name, language) = before.rsplit_once(char::is_whitespace)?;
                Some(Voice {
                    id: name.trim().into(),
                    name: name.trim().into(),
                    language: language.trim().replace('_', "-"),
                })
            })
            .collect())
    }
    #[cfg(target_os = "windows")]
    {
        let output=Command::new("powershell.exe").args(["-NoProfile","-NonInteractive","-Command","[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); Add-Type -AssemblyName System.Speech; $s=New-Object System.Speech.Synthesis.SpeechSynthesizer; ConvertTo-Json -Compress -InputObject @($s.GetInstalledVoices() | Where-Object Enabled | ForEach-Object { @{id=$_.VoiceInfo.Name;name=$_.VoiceInfo.Name;language=$_.VoiceInfo.Culture.Name} })"]).output().map_err(|e|e.to_string())?;
        serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Ok(Vec::new())
    }
}
fn stop(state: &AppState) -> Result<()> {
    state.generation.fetch_add(1, Ordering::SeqCst);
    if let Some(mut child) = state.speech.lock().map_err(|e| e.to_string())?.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}
#[tauri::command]
fn stop_speech(state: State<AppState>) -> Result<()> {
    stop(&state)
}
#[tauri::command]
fn speak(
    app: tauri::AppHandle,
    state: State<AppState>,
    text: String,
    voice: String,
    rate: f64,
) -> Result<u64> {
    if text.len() > 100_000 {
        return Err("Речення завелике для озвучення".into());
    }
    if !rate.is_finite() || !(0.5..=2.0).contains(&rate) {
        return Err("Некоректна швидкість".into());
    }
    if !state.voice_list.iter().any(|v| v.id == voice) {
        return Err("Голос не встановлено".into());
    }
    stop(&state)?;
    let generation = state.generation.load(Ordering::SeqCst);
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = Command::new("/usr/bin/say");
        c.args([
            "-v",
            &voice,
            "-r",
            &format!("{}", (180.0 * rate) as u32),
            "-f",
            "-",
        ]);
        c
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut c = Command::new("powershell.exe");
        c.args(["-NoProfile","-NonInteractive","-Command","[Console]::InputEncoding=[System.Text.UTF8Encoding]::new($false); Add-Type -AssemblyName System.Speech; $s=New-Object System.Speech.Synthesis.SpeechSynthesizer; $s.SelectVoice($env:LR_VOICE); $s.Rate=[int]$env:LR_RATE; $s.Speak([Console]::In.ReadToEnd())"]);
        c.env("LR_VOICE", voice)
            .env("LR_RATE", format!("{}", ((rate - 1.0) * 5.0) as i32));
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = Command::new("false");
    use std::io::Write;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(mut input) = child.stdin.take() {
        input
            .write_all(text.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    *state.speech.lock().map_err(|e| e.to_string())? = Some(child);
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(80));
        let state = app.state::<AppState>();
        if state.generation.load(Ordering::SeqCst) != generation {
            break;
        }
        let completed = {
            let mut guard = match state.speech.lock() {
                Ok(g) => g,
                Err(_) => break,
            };
            match guard.as_mut().map(|child| child.try_wait()) {
                Some(Ok(Some(status))) => {
                    guard.take();
                    Some(if status.success() {
                        None
                    } else {
                        Some("Системний голос не завершив озвучення".to_string())
                    })
                }
                Some(Err(e)) => {
                    guard.take();
                    Some(Some(e.to_string()))
                }
                _ => None,
            }
        };
        if let Some(error) = completed {
            let _ = app.emit("speech-finished", SpeechFinished { generation, error });
            break;
        }
    });
    Ok(generation)
}
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let path = app.path().app_data_dir()?;
            std::fs::create_dir_all(&path)?;
            let store = Store::open(&path.join("reader.db")).map_err(std::io::Error::other)?;
            app.manage(AppState {
                store: Mutex::new(store),
                speech: Mutex::new(None),
                generation: AtomicU64::new(0),
                voice_list: detect_voices().unwrap_or_default(),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            load,
            import_document,
            save_position,
            save_word,
            review,
            export_backup,
            restore_backup,
            original,
            voices,
            speak,
            stop_speech,
            export_file
        ])
        .build(tauri::generate_context!())
        .expect("Не вдалося запустити Reader")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                let _ = stop(&app.state::<AppState>());
            }
        });
}
