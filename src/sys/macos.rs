//! macOS: окно через winit и AppKit, пути в ~/Library, диалоги NSOpenPanel, автозапуск через
//! LaunchAgent, один экземпляр через flock и unix-сокет. Меню-бар — в tray_mac.rs.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{define_class, msg_send, sel, AnyThread, MainThreadMarker};
use objc2_app_kit::{NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSApplicationWillTerminateNotification, NSModalResponseOK, NSOpenPanel, NSScreen, NSView, NSWindow};
use objc2_foundation::{NSArray, NSNotification, NSNotificationCenter, NSString, NSURL};
use slint::winit_030::{winit::raw_window_handle::{HasWindowHandle, RawWindowHandle}, WinitWindowAccessor};
use std::cell::RefCell;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Идентификатор приложения: тот же, что в Info.plist (tools/bundle_macos.sh), он же имя агента автозапуска.
const BUNDLE_ID: &str = "io.github.spolzermod.yamusic-rs";

/// NSWindow окна Slint; None, пока winit его не создал. Все вызовы идут из основного потока.
fn window(w: &slint::Window) -> Option<Retained<NSWindow>> {
    MainThreadMarker::new()?;
    let view = w
        .with_winit_window(|w| match w.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::AppKit(h)) => Some(h.ns_view.as_ptr()),
            _ => None,
        })
        .flatten()?;
    // SAFETY: адрес NSView отдаёт winit, пока окно живо; он спрашивается заново при каждом вызове.
    unsafe { (&*(view as *const NSView)).window() }
}

pub fn created(w: &slint::Window) -> bool {
    w.with_winit_window(|_| ()).is_some()
}

/// Значок окна задаёт .app (Info.plist), углы скругляет система.
pub fn prepare(_: &slint::Window) {}

/// Вывести окно вперёд и сделать приложение активным: окно, спрятанное в меню-бар,
/// иначе не выйдет на передний план.
pub fn raise(w: &slint::Window) {
    let Some(win) = window(w) else { return };
    if win.isMiniaturized() {
        win.deminiaturize(None);
    }
    win.makeKeyAndOrderFront(None);
    if let Some(mtm) = MainThreadMarker::new() {
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    }
}

/// Как IsWindowVisible: свёрнутое в Dock окно остаётся видимым.
pub fn is_shown(w: &slint::Window) -> bool {
    window(w).is_some_and(|w| w.isVisible() || w.isMiniaturized())
}

pub fn is_minimized(w: &slint::Window) -> bool {
    window(w).is_some_and(|w| w.isMiniaturized())
}

pub fn hide(w: &slint::Window) {
    if let Some(w) = window(w) {
        w.orderOut(None);
    }
}

/// Масштаб экрана с окном (2.0 на Retina).
pub fn scale(w: &slint::Window) -> f32 {
    w.with_winit_window(|w| w.scale_factor() as f32).unwrap_or(1.0)
}

/// Рабочая область экрана с окном (без меню-бара и Dock), в физических пикселях: лево, верх, право, низ.
/// Координаты те же, что у `set_position` winit: начало у левого верхнего угла основного экрана.
pub fn work_area(w: &slint::Window) -> (i32, i32, i32, i32) {
    let Some(mtm) = MainThreadMarker::new() else { return (0, 0, 0, 0) };
    let screens = NSScreen::screens(mtm);
    let (Some(screen), Some(primary)) = (window(w).and_then(|w| w.screen()).or_else(|| NSScreen::mainScreen(mtm)), screens.firstObject()) else {
        return (0, 0, 0, 0);
    };
    let k = screen.backingScaleFactor();
    let top_left = primary.frame().size.height;
    let v = screen.visibleFrame();
    let px = |p: f64| (p * k).round() as i32;
    (px(v.origin.x), px(top_left - (v.origin.y + v.size.height)), px(v.origin.x + v.size.width), px(top_left - v.origin.y))
}

/// Системный диалог выбора картинки (JPEG, PNG). `None` — пользователь отказался.
pub fn pick_image(_: &slint::Window, title: &str) -> Option<String> {
    let panel = open_panel(title)?;
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    let types = NSArray::from_retained_slice(&[NSString::from_str("jpg"), NSString::from_str("jpeg"), NSString::from_str("png")]);
    #[allow(deprecated)]
    panel.setAllowedFileTypes(Some(&types));
    run_panel(&panel)
}

/// Системный диалог выбора папки, открывается на `start`. `None` — пользователь отказался.
pub fn pick_folder(_: &slint::Window, title: &str, start: &str) -> Option<String> {
    let panel = open_panel(title)?;
    panel.setCanChooseFiles(false);
    panel.setCanChooseDirectories(true);
    panel.setCanCreateDirectories(true);
    if !start.is_empty() {
        panel.setDirectoryURL(Some(&NSURL::fileURLWithPath(&NSString::from_str(start))));
    }
    run_panel(&panel)
}

fn open_panel(title: &str) -> Option<Retained<NSOpenPanel>> {
    let panel = NSOpenPanel::openPanel(MainThreadMarker::new()?);
    panel.setAllowsMultipleSelection(false);
    panel.setMessage(Some(&NSString::from_str(title)));
    Some(panel)
}

fn run_panel(panel: &NSOpenPanel) -> Option<String> {
    if panel.runModal() != NSModalResponseOK {
        return None;
    }
    Some(panel.URL()?.path()?.to_string())
}

pub fn open_url(url: &str) {
    if let Ok(mut child) = Command::new("open").arg(url).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        std::thread::spawn(move || child.wait());
    }
}

/// Память на macOS отдаёт система сама.
pub fn trim_memory() {}

pub fn release_memory() {}

/// Светлая ли тема macOS.
pub fn system_light_theme() -> bool {
    let Some(mtm) = MainThreadMarker::new() else { return true };
    let names = unsafe { NSArray::from_slice(&[NSAppearanceNameAqua, NSAppearanceNameDarkAqua]) };
    let best = NSApplication::sharedApplication(mtm).effectiveAppearance().bestMatchFromAppearancesWithNames(&names);
    !best.is_some_and(|n| &*n == unsafe { NSAppearanceNameDarkAqua })
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
}

pub fn config_base() -> PathBuf {
    home().join("Library").join("Application Support")
}

/// Кэш рядом с настройками, как на Windows.
pub fn cache_base() -> PathBuf {
    config_base()
}

pub fn music_dir() -> PathBuf {
    home().join("Music")
}

pub fn hostname() -> Option<String> {
    let out = Command::new("scutil").args(["--get", "ComputerName"]).output().ok()?;
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string()).filter(|n| !n.is_empty())
}

pub fn copy_text(text: &str) {
    if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

fn agent_path() -> Option<PathBuf> {
    Some(home().join("Library/LaunchAgents").join(format!("{BUNDLE_ID}.plist")))
}

/// Агент запуска: этот exe с `--tray`, то есть сразу в меню-баре, без окна.
fn agent_plist(exe: &str) -> String {
    let exe = exe.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{BUNDLE_ID}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>--tray</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
"#
    )
}

/// Записан ли автозапуск вместе с macOS.
pub fn autostart_enabled() -> bool {
    agent_path().is_some_and(|p| p.exists())
}

/// Включить или выключить автозапуск: агент запуска в ~/Library/LaunchAgents, права не нужны.
pub fn set_autostart(on: bool) -> bool {
    let Some(path) = agent_path() else { return false };
    if !on {
        return match std::fs::remove_file(&path) {
            Ok(()) => true,
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        };
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    // Приложение запущено из архива, с загрузок или прямо из образа: macOS копирует его во временную
    // папку или путь исчезнет вместе с образом, и после перезагрузки автозапуск сломается.
    // Хороший путь в агенте не затираем.
    let exe_path = exe.to_string_lossy();
    if exe_path.contains("/AppTranslocation/") || exe_path.starts_with("/Volumes/") {
        crate::config::log("автозапуск не записан: приложение запущено с временного пути, перенесите его в «Программы»");
        return false;
    }
    let plist = agent_plist(&exe.display().to_string());
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(path, plist).is_ok()
}

/// Приложение перенесли — поправить путь в автозапуске, если он включён.
pub fn refresh_autostart() {
    if autostart_enabled() {
        set_autostart(true);
    }
}

/// Занять блокировку единственного экземпляра: flock на файле в папке данных. Файл держится
/// открытым, пока работает процесс, а при его завершении система снимает блокировку сама.
pub fn claim_instance() -> bool {
    static LOCK: std::sync::OnceLock<std::fs::File> = std::sync::OnceLock::new();
    let _ = std::fs::create_dir_all(crate::config::dir());
    let Ok(f) = std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(crate::config::dir().join("instance.lock")) else {
        return true;
    };
    match f.try_lock() {
        Ok(()) => {
            let _ = LOCK.set(f);
            true
        }
        Err(std::fs::TryLockError::WouldBlock) => false,
        Err(_) => true,
    }
}

/// Сокет, через который второй запуск будит работающий экземпляр.
fn wake_socket() -> PathBuf {
    crate::config::dir().join("wake.sock")
}

/// Второй запуск: попросить работающий экземпляр показать окно.
pub fn wake_existing() {
    if let Ok(mut s) = UnixStream::connect(wake_socket()) {
        let _ = s.write_all(b"show");
    }
}

/// Звать `f` из фонового потока, когда второй запуск просит показать окно.
pub fn on_wake(f: impl Fn() + Send + 'static) {
    let path = wake_socket();
    // Блокировка экземпляра уже наша, так что сокет остался от завершившегося процесса.
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            crate::config::log(format!("сокет второго запуска: {e}"));
            return;
        }
    };
    std::thread::Builder::new()
        .name("wake".into())
        .spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut buf = [0u8; 4];
                if s.read(&mut buf).is_ok_and(|n| n > 0) {
                    f();
                }
            }
        })
        .ok();
}

thread_local! {
    static ON_TERMINATE: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "YamusicTerminateObserver"]
    struct TerminateObserver;

    impl TerminateObserver {
        #[unsafe(method(willTerminate:))]
        fn will_terminate(&self, _note: &NSNotification) {
            ON_TERMINATE.with(|f| {
                if let Some(f) = f.borrow().as_ref() {
                    f();
                }
            });
        }
    }
);

/// Cmd+Q, «Завершить» в Dock и выключение компьютера идут через `-[NSApplication terminate:]`,
/// который сразу вызывает exit(), и цикл событий Slint не возвращается. Звать `f` из основного
/// потока перед этим, чтобы успеть сохранить настройки.
pub fn on_terminate(f: impl Fn() + 'static) {
    ON_TERMINATE.with(|t| *t.borrow_mut() = Some(Box::new(f)));
    let observer: Retained<TerminateObserver> = unsafe { msg_send![TerminateObserver::alloc(), init] };
    let center = NSNotificationCenter::defaultCenter();
    unsafe {
        let _: () = msg_send![
            &*center,
            addObserver: &*observer,
            selector: sel!(willTerminate:),
            name: NSApplicationWillTerminateNotification,
            object: std::ptr::null::<AnyObject>()
        ];
    }
    // Центр уведомлений наблюдателя не удерживает, а он нужен до конца работы.
    std::mem::forget(observer);
}

/// Перед выходом: убрать значок из меню-бара, «Сейчас играет» и сокет второго запуска.
pub fn cleanup() {
    crate::tray::remove();
    crate::now_playing::stop();
    let _ = std::fs::remove_file(wake_socket());
}

#[cfg(test)]
mod tests {
    #[test]
    fn agent_plist() {
        let p = super::agent_plist("/Applications/A & B.app/Contents/MacOS/yamusic-rs");
        assert!(p.contains("<string>/Applications/A &amp; B.app/Contents/MacOS/yamusic-rs</string>"));
        assert!(p.contains("<string>--tray</string>"));
        assert!(p.contains(&format!("<string>{}</string>", super::BUNDLE_ID)));
    }
}
