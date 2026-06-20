// Прячем консольное окно в релизе на Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Вся игровая логика — во фронтенде (WASM); Tauri лишь нативное окно.
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("ошибка запуска Tauri-приложения");
}
