//! Медиаклавиши и «Сейчас играет» на macOS через souvlaki (MPRemoteCommandCenter): клавиши
//! клавиатуры, пульт в Пункте управления, экран блокировки. macOS отдаёт клавиши приложению,
//! которое сообщило «Сейчас играет». Состояние, как и у статуса Discord, берётся у плеера из
//! своего потока, а команды отдаются наружу так же, как у MPRIS на Linux.

use crate::config::log;
use crate::player::Player;
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Команда от системы. Время — в секундах.
pub enum Event {
    PlayPause,
    Play,
    Pause,
    Next,
    Prev,
    Seek(f32),
}

/// Управление им пользуются основной поток (создание, `stop`) и поток метаданных.
static MEDIA: Mutex<Option<MediaControls>> = Mutex::new(None);

/// Подключить клавиши (из основного потока) и запустить поток, который переносит состояние
/// плеера в «Сейчас играет». Без клавиш приложение просто работает дальше, причина в логе.
pub fn spawn(player: Player, on_event: impl Fn(Event) + Send + 'static) {
    let cfg = PlatformConfig { dbus_name: "yamusic_rs", display_name: env!("APP_NAME"), hwnd: None };
    let mut controls = match MediaControls::new(cfg) {
        Ok(c) => c,
        Err(e) => {
            log(format!("медиаклавиши: {e}"));
            return;
        }
    };
    let attached = controls.attach(move |e| {
        on_event(match e {
            MediaControlEvent::Toggle => Event::PlayPause,
            MediaControlEvent::Play => Event::Play,
            MediaControlEvent::Pause | MediaControlEvent::Stop => Event::Pause,
            MediaControlEvent::Next => Event::Next,
            MediaControlEvent::Previous => Event::Prev,
            // Перемотка из Пункта управления: позиция в секундах.
            MediaControlEvent::SetPosition(MediaPosition(pos)) => Event::Seek(pos.as_secs_f32()),
            _ => return,
        })
    });
    if attached.is_err() {
        return;
    }
    *MEDIA.lock().unwrap() = Some(controls);
    let started = std::thread::Builder::new().name("now-playing".into()).spawn(move || follow(&player));
    if let Err(e) = started {
        log(format!("«Сейчас играет»: поток не запустился: {e}"));
    }
}

/// Перед выходом: убрать трек из Пункта управления и отключить клавиши.
pub fn stop() {
    if let Some(mut c) = MEDIA.lock().unwrap().take() {
        let _ = c.set_playback(MediaPlayback::Stopped);
        let _ = c.detach();
    }
}

/// Трек, исполнитель, альбом, обложка, пауза и позиция.
fn follow(player: &Player) {
    let mut track_id: Option<String> = None;
    // Когда, где и в каком режиме опубликована позиция: по ней macOS сам двигает ползунок.
    let mut published: Option<(Instant, f32, bool)> = None;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        // В этом потоке нет цикла событий, и объекты, которые souvlaki создаёт в AppKit, сами бы
        // не освобождались: пул опустошается на каждом шаге.
        objc2::rc::autoreleasepool(|_| step(player, &mut track_id, &mut published));
    }
}

fn step(player: &Player, track_id: &mut Option<String>, published: &mut Option<(Instant, f32, bool)>) {
    let mut guard = MEDIA.lock().unwrap();
    let Some(media) = guard.as_mut() else { return };
    let s = player.state();
    let id = s.track.as_ref().map(|t| t.id.clone());
    if id != *track_id {
        *published = None;
        match &s.track {
            Some(t) => {
                let title = if t.version.is_empty() { t.title.clone() } else { format!("{} ({})", t.title, t.version) };
                let cover = t.cover_url(400);
                let _ = media.set_metadata(MediaMetadata {
                    title: Some(&title),
                    artist: Some(&t.artists),
                    album: Some(&t.album_title).filter(|a| !a.is_empty()).map(|a| a.as_str()),
                    cover_url: cover.as_deref(),
                    duration: (s.duration > 1.0).then(|| Duration::from_secs_f32(s.duration)),
                });
            }
            None => {
                // Вышли из аккаунта или очередь пуста: старый трек из Пункта управления убираем.
                let _ = media.set_metadata(MediaMetadata::default());
                let _ = media.set_playback(MediaPlayback::Stopped);
            }
        }
        *track_id = id;
    }
    if s.track.is_none() {
        return;
    }
    let expected = published.map(|(at, pos, playing)| if playing { pos + at.elapsed().as_secs_f32() } else { pos });
    let moved = published.is_none_or(|(_, _, playing)| playing != s.playing) || expected.is_some_and(|e| (e - s.position).abs() > 2.0);
    if moved {
        let progress = Some(MediaPosition(Duration::from_secs_f32(s.position.max(0.0))));
        let _ = media.set_playback(if s.playing { MediaPlayback::Playing { progress } } else { MediaPlayback::Paused { progress } });
        *published = Some((Instant::now(), s.position, s.playing));
    }
}
