//! Яндекс Музыка RS — неофициальный клиент Яндекс Музыки. Точка входа: один экземпляр
//! на пользователя, выбор рендерера, запуск окна.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ahead;
mod api;
mod assets;
mod auth;
mod client;
mod config;
mod discord;
mod eq;
mod flac;
mod gui;
mod history;
mod i18n;
mod images;
mod landing;
#[cfg(target_os = "linux")]
mod mpris;
mod net;
#[cfg(target_os = "macos")]
mod now_playing;
mod palette;
mod player;
mod save;
mod secret;
#[cfg(target_os = "macos")]
mod security_cli;
mod stream;
#[cfg_attr(windows, path = "sys/windows.rs")]
#[cfg_attr(target_os = "linux", path = "sys/linux.rs")]
#[cfg_attr(target_os = "macos", path = "sys/macos.rs")]
mod sys;
mod tags;
#[cfg(windows)]
mod tray;
#[cfg(target_os = "macos")]
#[path = "tray_mac.rs"]
mod tray;
mod yandex_id;

fn main() -> Result<(), slint::PlatformError> {
    #[cfg(target_os = "linux")]
    sys::audio_identity();
    if !sys::claim_instance() {
        // Второй запуск просто показывает уже открытое окно.
        sys::wake_existing();
        return Ok(());
    }
    // Skia без видеокарты: память не зависит от драйвера (с OpenGL или D3D окно занимает
    // в 2–3 раза больше). Режим работы («Плавный» или «Лёгкий») меняет только анимации
    // и кэши и переключается на лету (gui::apply_mode). На macOS видеокарту (Metal) можно
    // включить в настройках; не получилось её выбрать — остаёмся на процессоре.
    let gpu = cfg!(target_os = "macos") && config::gpu_enabled();
    let gpu_selected = gpu && slint::BackendSelector::new().backend_name("winit".into()).renderer_name("skia-wgpu".into()).select().is_ok();
    if gpu && !gpu_selected {
        config::log("GPU-рендер не выбрался, интерфейс рисует процессор");
    }
    if !gpu_selected {
        slint::BackendSelector::new().backend_name("winit".into()).renderer_name("skia-software".into()).select()?;
    }
    // По app_id (Wayland) и WM_CLASS (X11) окружение находит .desktop и значок.
    #[cfg(target_os = "linux")]
    slint::set_xdg_app_id("yamusic-rs")?;
    sys::refresh_autostart();
    // Без трея на Linux спрятанное окно было бы не вернуть.
    let start_hidden = cfg!(any(windows, target_os = "macos")) && std::env::args().any(|a| a == "--tray");
    gui::run(start_hidden)
}
