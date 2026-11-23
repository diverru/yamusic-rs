//! Значок в меню-баре macOS, как у Windows-версии (tray.rs): клик сворачивает и разворачивает
//! окно, правый клик — меню с управлением плеером. Медиаклавиши и «Сейчас играет» — в now_playing.rs.

use crate::config::log;
use crate::i18n::tr;
use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{define_class, msg_send, sel, AnyThread};
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Toggle,
    Show,
    PlayPause,
    Prev,
    Next,
    Terms,
    Recommendations,
    Support,
    Quit,
}

/// Что показать в меню: окно свёрнуто или спрятано, играет ли трек, есть ли соседние.
pub struct MenuState {
    pub minimized: bool,
    pub playing: bool,
    pub can_prev: bool,
    pub can_next: bool,
}

/// Пункты меню, подписи и доступность которых зависят от состояния плеера.
struct Items {
    toggle: MenuItem,
    play: MenuItem,
    prev: MenuItem,
    next: MenuItem,
}

struct Tray {
    /// Значок и меню создаются уже в цикле событий (`create_icon`), до этого их нет.
    _icon: Option<TrayIcon>,
    items: Option<Items>,
    on_event: Rc<dyn Fn(Event)>,
    state: Box<dyn Fn() -> MenuState>,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

/// Монохромный значок (шаблон): система сама красит его под светлое и тёмное меню-бар.
fn load_icon() -> Option<Icon> {
    let img = image::load_from_memory(include_bytes!("../icons/tray_mac.png")).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).ok()
}

fn item(id: &str, text: &str) -> MenuItem {
    MenuItem::with_id(id, tr(text), true, None)
}

/// Запоминает обработчики; значок создаётся позже, в цикле событий. События приходят в основной
/// поток через очередь Slint, а не из обработчика AppKit: так их обработка не вкладывается в цикл winit.
pub fn install(on_event: impl Fn(Event) + 'static, state: impl Fn() -> MenuState + 'static) {
    TRAY.with(|t| *t.borrow_mut() = Some(Tray { _icon: None, items: None, on_event: Rc::new(on_event), state: Box::new(state) }));
    // Как советует tray-icon, значок создаётся, когда цикл событий уже запущен. Заодно к этому
    // моменту AppKit поставил свои обработчики событий, и наш `reopen` их заменит, а не наоборот.
    slint::Timer::single_shot(Duration::ZERO, || {
        create_icon();
        handle_reopen();
    });
}

fn create_icon() {
    let items = Items { toggle: item("toggle", "Свернуть"), play: item("play", "Воспроизведение"), prev: item("prev", "Предыдущая песня"), next: item("next", "Следующая песня") };
    let (terms, recs, support, quit) =
        (item("terms", "Пользовательское соглашение"), item("recs", "Правила рекомендаций"), item("support", "Чат с поддержкой"), item("quit", "Закрыть приложение"));
    let about = Submenu::with_items(tr("О приложении"), true, &[&terms, &recs, &support]);
    let sep = PredefinedMenuItem::separator;
    let menu = Menu::new();
    let built = about.and_then(|about| menu.append_items(&[&items.toggle, &sep(), &items.play, &items.prev, &items.next, &sep(), &about, &sep(), &quit]));
    if let Err(e) = built {
        log(format!("меню в меню-баре не собралось: {e}"));
        return;
    }
    let Some(icon) = load_icon() else {
        log("значок меню-бара не загрузился");
        return;
    };
    let tray = TrayIconBuilder::new()
        .with_icon_templated(icon)
        .with_tooltip(env!("APP_NAME"))
        .with_menu(Box::new(menu))
        // Левый клик, как у Windows, сворачивает и разворачивает окно; меню — по правому.
        .with_menu_on_left_click(false)
        .build();
    let tray = match tray {
        Ok(t) => t,
        Err(e) => {
            log(format!("значок в меню-баре не создался: {e}"));
            return;
        }
    };
    MenuEvent::set_event_handler(Some(|e: MenuEvent| {
        emit(match e.id.0.as_str() {
            "toggle" => Event::Toggle,
            "play" => Event::PlayPause,
            "prev" => Event::Prev,
            "next" => Event::Next,
            "terms" => Event::Terms,
            "recs" => Event::Recommendations,
            "support" => Event::Support,
            "quit" => Event::Quit,
            _ => return,
        })
    }));
    TrayIconEvent::set_event_handler(Some(|e: TrayIconEvent| match e {
        TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } => emit(Event::Toggle),
        // Обработчик зовётся в основном потоке прямо перед показом меню: подписи успевают обновиться.
        TrayIconEvent::Click { button: MouseButton::Right, button_state: MouseButtonState::Down, .. } => refresh(),
        _ => {}
    }));
    TRAY.with(|t| {
        if let Some(t) = t.borrow_mut().as_mut() {
            t._icon = Some(tray);
            t.items = Some(items);
        }
    });
    refresh();
}

pub fn remove() {
    MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
    TrayIconEvent::set_event_handler(None::<fn(TrayIconEvent)>);
    TRAY.with(|t| t.borrow_mut().take());
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "YamusicReopenHandler"]
    struct ReopenHandler;

    impl ReopenHandler {
        #[unsafe(method(handleReopen:withReplyEvent:))]
        fn handle_reopen(&self, _event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            emit(Event::Show);
        }
    }
);

/// Повторный запуск .app и клик по значку в Dock: пока приложение работает, второй процесс
/// macOS не запускает, а шлёт ему событие «reopen» (aevt/rapp). Winit его не обрабатывает,
/// поэтому окно, спрятанное в меню-бар, само не вернулось бы.
fn handle_reopen() {
    const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
    const REOPEN_APPLICATION: u32 = u32::from_be_bytes(*b"rapp");
    let handler: Retained<ReopenHandler> = unsafe { msg_send![ReopenHandler::alloc(), init] };
    let manager = NSAppleEventManager::sharedAppleEventManager();
    unsafe {
        let _: () = msg_send![
            &*manager,
            setEventHandler: &*handler,
            andSelector: sel!(handleReopen:withReplyEvent:),
            forEventClass: CORE_EVENT_CLASS,
            andEventID: REOPEN_APPLICATION
        ];
    }
    // Менеджер событий объект не удерживает, а обработчик нужен до конца работы.
    std::mem::forget(handler);
}

fn emit(e: Event) {
    let _ = slint::invoke_from_event_loop(move || {
        let f = TRAY.with(|t| t.borrow().as_ref().map(|t| t.on_event.clone()));
        if let Some(f) = f {
            f(e);
        }
    });
}

/// Подписи и доступность пунктов по текущему состоянию плеера.
fn refresh() {
    TRAY.with(|t| {
        let t = t.borrow();
        let Some((t, items)) = t.as_ref().and_then(|t| Some((t, t.items.as_ref()?))) else { return };
        let s = (t.state)();
        items.toggle.set_text(tr(if s.minimized { "Развернуть" } else { "Свернуть" }));
        items.play.set_text(tr(if s.playing { "Пауза" } else { "Воспроизведение" }));
        items.prev.set_enabled(s.can_prev);
        items.next.set_enabled(s.can_next);
    });
}
