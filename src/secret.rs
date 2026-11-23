//! Где хранить токен. Windows: шифрование DPAPI, расшифровать может только та же учётная
//! запись на том же компьютере, в config.json лежит base64 шифротекста. Linux: связка ключей
//! пользователя через Secret Service, в config.json только имя записи. macOS: Связка ключей,
//! в config.json только имя секрета; открытым текстом токен на macOS не хранится никогда.

#[cfg(any(windows, target_os = "macos"))]
use base64::Engine as _;

/// Дополнительная «соль» DPAPI: другие программы того же пользователя без неё не расшифруют.
/// Старое имя оставлено намеренно: с другой солью сохранённые токены перестанут читаться.
#[cfg(windows)]
const ENTROPY: &[u8] = b"yamusic-lite token v1";

#[cfg(windows)]
mod dpapi {
    use super::ENTROPY;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn blob(b: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: b.len() as u32, pbData: b.as_ptr() as *mut u8 }
    }

    fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
        unsafe { LocalFree(out.pbData as _) };
        v
    }

    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        let (input, entropy) = (blob(data), blob(ENTROPY));
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = unsafe {
            CryptProtectData(&input, std::ptr::null(), &entropy, std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        };
        (ok != 0).then(|| take(out))
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        let (input, entropy) = (blob(data), blob(ENTROPY));
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = unsafe {
            CryptUnprotectData(&input, std::ptr::null_mut(), &entropy, std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        };
        (ok != 0).then(|| take(out))
    }
}

/// Связка ключей Linux через Secret Service (GNOME Keyring, KWallet, KeePassXC). Секрет лежит
/// там, в config.json остаётся только имя записи. Сессия шифрованная (Диффи — Хеллман и AES),
/// так что по шине D-Bus секрет открытым не идёт.
#[cfg(target_os = "linux")]
mod keyring {
    use crate::config::log;
    use secret_service::blocking::SecretService;
    use secret_service::EncryptionType;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    /// Что уже лежит в связке: те же значения повторно не пишем. Конфиг сохраняется на каждое
    /// изменение настроек, а запись в связку — это запрос по D-Bus.
    static STORED: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
    /// Службы нет (нет шины или демона): до конца работы больше не пытаемся.
    static MISSING: AtomicBool = AtomicBool::new(false);

    fn attributes(name: &str) -> HashMap<&str, &str> {
        HashMap::from([("application", "yamusic-rs"), ("secret", name)])
    }

    fn label(name: &str) -> String {
        let what = if name == "refresh" { "токен обновления" } else { "токен входа" };
        format!("{}: {what}", env!("APP_NAME"))
    }

    fn with<R>(f: impl FnOnce(&SecretService) -> Result<R, secret_service::Error>) -> Option<R> {
        if MISSING.load(Ordering::Relaxed) {
            return None;
        }
        let ss = match SecretService::connect(EncryptionType::Dh) {
            Ok(ss) => ss,
            Err(e) => {
                MISSING.store(true, Ordering::Relaxed);
                log(format!("связка ключей недоступна, токен останется в config.json: {e}"));
                return None;
            }
        };
        f(&ss).map_err(|e| log(format!("связка ключей: {e}"))).ok()
    }

    fn cached(f: impl FnOnce(&mut HashMap<String, String>)) {
        f(STORED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new));
    }

    pub fn store(name: &str, secret: &str) -> bool {
        let mut same = false;
        cached(|m| same = m.get(name).is_some_and(|v| v == secret));
        if same {
            return true;
        }
        let saved = with(|ss| {
            let c = ss.get_default_collection().or_else(|_| ss.get_any_collection())?;
            c.ensure_unlocked()?;
            c.create_item(&label(name), attributes(name), secret.as_bytes(), true, "text/plain")?;
            Ok(())
        });
        if saved.is_some() {
            cached(|m| {
                m.insert(name.into(), secret.into());
            });
        }
        saved.is_some()
    }

    pub fn load(name: &str) -> Option<String> {
        let secret = with(|ss| {
            let found = ss.search_items(attributes(name))?;
            let Some(item) = found.unlocked.first().or(found.locked.first()) else { return Ok(None) };
            item.ensure_unlocked()?;
            Ok(String::from_utf8(item.get_secret()?).ok())
        })??;
        cached(|m| {
            m.insert(name.into(), secret.clone());
        });
        Some(secret)
    }

    pub fn remove(name: &str) {
        let mut known = false;
        cached(|m| known = m.remove(name).is_some());
        if known {
            with(|ss| ss.search_items(attributes(name))?.unlocked.iter().try_for_each(|i| i.delete()));
        }
    }
}

/// Связка ключей macOS. Все секреты лежат одним JSON (в base64) в одной записи, в config.json
/// остаётся только имя секрета в base64. Запись читается один раз за запуск и пишется, только
/// если что-то изменилось. Читает и пишет её `security_cli`: так запись открыта любой сборке
/// приложения без запроса пароля, как DPAPI на Windows открыт любой программе пользователя.
#[cfg(target_os = "macos")]
mod keychain {
    use crate::config::log;
    use crate::security_cli::{self, Get};
    use base64::Engine as _;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    #[cfg(not(test))]
    const SERVICE: &str = "yamusic-rs";
    #[cfg(test)]
    const SERVICE: &str = "yamusic-rs-test";
    /// Все секреты лежат одним JSON в одной записи. Имя не «auth»: запись с таким именем созданная
    /// прежними сборками через API, привязана к подписи сборки, прочитать или удалить её без пароля
    /// нельзя, а новая запись с тем же именем не создастся. Такую запись приложение больше не трогает.
    const ACCOUNT: &str = "auth-v2";

    type Secrets = BTreeMap<String, String>;

    enum Read {
        Found(Secrets),
        Missing,
        Failed,
    }

    /// Откуда берутся секреты и куда пишутся; в тестах вместо связки ключей подставляется заглушка.
    trait Store {
        fn read(&mut self) -> Read;
        fn write(&mut self, m: &Secrets) -> bool;
    }

    struct Keychain;

    impl Store for Keychain {
        fn read(&mut self) -> Read {
            match security_cli::get(SERVICE, ACCOUNT) {
                Get::Found(b64) => Read::Found(decode(&b64).unwrap_or_else(|e| {
                    log(format!("связка ключей: запись повреждена: {e}"));
                    Secrets::new()
                })),
                Get::Missing => Read::Missing,
                Get::Failed(e) => {
                    log(format!("связка ключей: чтение не удалось: {e}"));
                    Read::Failed
                }
            }
        }

        fn write(&mut self, m: &Secrets) -> bool {
            #[cfg(test)]
            WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let res = if m.is_empty() {
                security_cli::delete(SERVICE, ACCOUNT)
            } else {
                // Обновление на месте: значение заменяется в той же записи.
                serde_json::to_vec(m).map_err(|e| e.to_string()).and_then(|json| security_cli::set(SERVICE, ACCOUNT, &base64::engine::general_purpose::STANDARD.encode(json)))
            };
            if let Err(e) = &res {
                log(format!("связка ключей: запись не удалась: {e}"));
            }
            res.is_ok()
        }
    }

    fn decode(b64: &str) -> Result<Secrets, String> {
        let json = base64::engine::general_purpose::STANDARD.decode(b64.trim()).map_err(|e| e.to_string())?;
        serde_json::from_slice(&json).map_err(|e| e.to_string())
    }

    /// Состояние за один запуск: что прочитано и что можно писать.
    #[derive(Default)]
    struct Cache {
        /// Содержимое записи, прочитанное один раз за процесс.
        secrets: Option<Secrets>,
        /// Запись не удалось прочитать (отказ в доступе, связка закрыта). Повторно за этот запуск
        /// не читаем, чтобы не спрашивать пароль снова и снова; следующий запуск попробует опять.
        unreadable: bool,
        /// Запись не удалась. Автоматические сохранения (громкость и прочее) больше не пишут,
        /// чтобы не вызывать запрос пароля на каждое; явный вход или выход пробует ещё раз.
        blocked: bool,
        /// После нечитаемой записи вход или выход обязаны перезаписать её, даже если в памяти
        /// пусто и «ничего не изменилось».
        force: bool,
    }

    impl Cache {
        /// None — связка недоступна: тогда ничего не читаем и не пишем, чтобы не затереть запись.
        fn ready(&mut self, st: &mut dyn Store) -> Option<&mut Secrets> {
            if self.secrets.is_none() {
                if self.unreadable {
                    return None;
                }
                self.secrets = Some(match st.read() {
                    Read::Found(m) => m,
                    Read::Missing => Secrets::new(),
                    Read::Failed => {
                        self.unreadable = true;
                        return None;
                    }
                });
            }
            self.secrets.as_mut()
        }

        fn get(&mut self, st: &mut dyn Store, account: &str) -> Option<String> {
            self.ready(st)?.get(account).cloned()
        }

        /// Some — записать секрет, None — убрать. Все изменения — одной записью в связку ключей,
        /// и только если что-то изменилось. Не вышло — всё остаётся как было.
        fn apply(&mut self, st: &mut dyn Store, changes: &[(&str, Option<&str>)]) -> bool {
            let (blocked, force) = (self.blocked, self.force);
            let Some(m) = self.ready(st) else { return false };
            let before = m.clone();
            for (name, value) in changes {
                match value {
                    Some(v) => m.insert(name.to_string(), v.to_string()),
                    None => m.remove(*name),
                };
            }
            if *m == before && !force {
                return true;
            }
            let ok = !blocked && st.write(m);
            if ok {
                self.force = false;
                return true;
            }
            if let Some(m) = self.secrets.as_mut() {
                *m = before;
            }
            self.blocked = true;
            false
        }

        /// Пользователь сам вошёл или вышел. Если запись не читалась, берём чистый лист и
        /// обязательно перезаписываем её новым состоянием; если запись не удалась — пробуем ещё раз.
        fn explicit_change(&mut self) {
            if self.unreadable {
                self.unreadable = false;
                self.secrets = Some(Secrets::new());
                self.force = true;
            }
            self.blocked = false;
        }
    }

    static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
    #[cfg(test)]
    pub static TEST_LOCK: Mutex<()> = Mutex::new(());
    /// Сколько раз тест видел запись в связку ключей.
    #[cfg(test)]
    static WRITES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn with<R>(f: impl FnOnce(&mut Cache) -> R) -> R {
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        f(cache.get_or_insert_with(Cache::default))
    }

    pub fn get(account: &str) -> Option<String> {
        with(|c| c.get(&mut Keychain, account))
    }

    pub fn apply(changes: &[(&str, Option<&str>)]) -> bool {
        with(|c| c.apply(&mut Keychain, changes))
    }

    pub fn explicit_change() {
        with(Cache::explicit_change);
    }

    #[cfg(test)]
    pub fn forget_cache() {
        *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Заглушка вместо связки ключей: что прочитается, что записано и сколько было записей.
        struct Fake {
            read: Option<Secrets>,
            fail_read: bool,
            fail_write: bool,
            writes: usize,
        }

        impl Fake {
            fn new(read: Option<Secrets>) -> Fake {
                Fake { read, fail_read: false, fail_write: false, writes: 0 }
            }
        }

        impl Store for Fake {
            fn read(&mut self) -> Read {
                match (&self.read, self.fail_read) {
                    (_, true) => Read::Failed,
                    (Some(m), _) => Read::Found(m.clone()),
                    (None, _) => Read::Missing,
                }
            }

            fn write(&mut self, m: &Secrets) -> bool {
                self.writes += 1;
                if self.fail_write {
                    return false;
                }
                self.read = (!m.is_empty()).then(|| m.clone());
                true
            }
        }

        fn old_account() -> Secrets {
            Secrets::from([("token".to_string(), "old".to_string()), ("refresh".to_string(), "oldr".to_string())])
        }

        /// Запись не читалась, но пользователь вошёл: новое состояние обязано её перезаписать.
        #[test]
        fn login_after_failed_read_overwrites() {
            let mut st = Fake::new(Some(old_account()));
            st.fail_read = true;
            let mut c = Cache::default();
            assert!(!c.apply(&mut st, &[("token", Some("new"))]));
            assert!(c.unreadable && st.writes == 0);
            c.explicit_change();
            assert!(!c.unreadable);
            assert!(c.apply(&mut st, &[("token", Some("new")), ("refresh", Some("newr"))]));
            assert_eq!(st.writes, 1);
            assert_eq!(st.read.as_ref().and_then(|m| m.get("token")).map(String::as_str), Some("new"));
            assert_eq!(st.read.as_ref().and_then(|m| m.get("refresh")).map(String::as_str), Some("newr"));
        }

        /// Выход после нечитаемой записи тоже пишет: старый аккаунт не должен вернуться.
        #[test]
        fn logout_after_failed_read_clears_item() {
            let mut st = Fake::new(Some(old_account()));
            st.fail_read = true;
            let mut c = Cache::default();
            assert_eq!(c.get(&mut st, "token"), None);
            c.explicit_change();
            assert!(c.apply(&mut st, &[("token", None), ("refresh", None)]));
            assert_eq!(st.writes, 1);
            assert!(st.read.is_none());
            // Дальше без изменений ничего не пишется.
            assert!(c.apply(&mut st, &[("token", None), ("refresh", None)]));
            assert_eq!(st.writes, 1);
        }

        /// После неудачной записи автоматические сохранения не повторяют её; вход пробует снова.
        #[test]
        fn failed_write_is_not_retried() {
            let mut st = Fake::new(None);
            st.fail_write = true;
            let mut c = Cache::default();
            assert!(!c.apply(&mut st, &[("token", Some("t"))]));
            assert_eq!(st.writes, 1);
            for _ in 0..3 {
                assert!(!c.apply(&mut st, &[("token", Some("t"))]));
            }
            assert_eq!(st.writes, 1);
            // Состояние откатилось: без изменений запись и не нужна.
            assert!(c.apply(&mut st, &[("token", None)]));
            c.explicit_change();
            st.fail_write = false;
            assert!(c.apply(&mut st, &[("token", Some("t"))]));
            assert_eq!(st.writes, 2);
        }

        /// Обе записи в одном элементе, то же значение второй раз не пишется, выход всё убирает.
        /// Настоящая связка ключей, но служба тестовая.
        #[test]
        fn single_item() {
            use std::sync::atomic::Ordering::Relaxed;
            let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            forget_cache();
            let start = WRITES.load(Relaxed);
            assert!(apply(&[("token", Some("t1")), ("refresh", Some("r1"))]));
            assert!(apply(&[("token", Some("t1")), ("refresh", Some("r1"))]));
            assert_eq!(WRITES.load(Relaxed) - start, 1);
            forget_cache();
            assert_eq!(get("token").as_deref(), Some("t1"));
            assert_eq!(get("refresh").as_deref(), Some("r1"));
            assert!(matches!(security_cli::get(SERVICE, "token"), Get::Missing));
            assert!(apply(&[("token", None), ("refresh", None)]));
            assert_eq!(WRITES.load(Relaxed) - start, 2);
            forget_cache();
            assert!(matches!(security_cli::get(SERVICE, ACCOUNT), Get::Missing));
            assert_eq!(get("token"), None);
        }
    }
}

/// Префикс в config.json: секрет лежит в связке ключей, а не рядом.
#[cfg(target_os = "linux")]
const IN_KEYRING: &str = "keyring:";

/// Спрятать секрет для хранения в конфиге. `name` различает секреты в связке ключей Linux,
/// DPAPI на Windows он не нужен. None — спрятать негде.
#[cfg(not(target_os = "macos"))]
pub fn seal(name: &str, plain: &str) -> Option<String> {
    #[cfg(windows)]
    {
        let _ = name;
        dpapi::protect(plain.as_bytes()).map(|b| base64::engine::general_purpose::STANDARD.encode(b))
    }
    #[cfg(target_os = "linux")]
    {
        keyring::store(name, plain).then(|| format!("{IN_KEYRING}{name}"))
    }
    // Android: config.json и так лежит в личной папке приложения, другим приложениям её не прочесть.
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (name, plain);
        None
    }
}

/// Спрятать несколько секретов разом; `None` вместо значения — секрет не трогать. На macOS все
/// изменения уходят в связку ключей одной записью: система спрашивает пароль отдельно на каждый
/// доступ к ней, а `name` — ключ внутри записи. На остальных системах это вызовы `seal` по очереди.
pub fn seal_all<const N: usize>(items: [(&str, Option<&str>); N]) -> [Option<String>; N] {
    #[cfg(target_os = "macos")]
    {
        let changes: Vec<(&str, Option<&str>)> = items.iter().filter(|(_, plain)| plain.is_some()).copied().collect();
        let ok = !changes.is_empty() && keychain::apply(&changes);
        items.map(|(name, plain)| (ok && plain.is_some()).then(|| base64::engine::general_purpose::STANDARD.encode(name)))
    }
    #[cfg(not(target_os = "macos"))]
    {
        items.map(|(name, plain)| plain.and_then(|p| seal(name, p)))
    }
}

/// Пользователь сам вошёл (macOS): новое состояние нужно записать, даже если связка ключей
/// перед этим не прочиталась, и пробовать записать ещё раз, если прошлая запись не удалась.
pub fn explicit_change() {
    #[cfg(target_os = "macos")]
    keychain::explicit_change();
}

/// Достать секрет из конфига; None — чужая учётная запись, нет записи или испорченные данные.
pub fn open(sealed: &str) -> Option<String> {
    #[cfg(windows)]
    {
        let raw = base64::engine::general_purpose::STANDARD.decode(sealed.trim()).ok()?;
        String::from_utf8(dpapi::unprotect(&raw)?).ok()
    }
    #[cfg(target_os = "linux")]
    {
        keyring::load(sealed.trim().strip_prefix(IN_KEYRING)?)
    }
    #[cfg(target_os = "macos")]
    {
        let raw = base64::engine::general_purpose::STANDARD.decode(sealed.trim()).ok()?;
        keychain::get(&String::from_utf8(raw).ok()?)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = sealed;
        None
    }
}

/// Секрет больше не нужен (вышли из аккаунта). DPAPI ничего не хранит, связку ключей чистим.
/// На macOS это всегда решение пользователя: запись перезаписывается, даже если при запуске
/// связка ключей не прочиталась.
pub fn forget(name: &str) {
    forget_all(&[name]);
}

/// Несколько секретов разом; на macOS это одна запись в связку ключей.
pub fn forget_all(names: &[&str]) {
    #[cfg(target_os = "linux")]
    names.iter().for_each(|name| keyring::remove(name));
    #[cfg(target_os = "macos")]
    {
        keychain::explicit_change();
        keychain::apply(&names.iter().map(|name| (*name, None)).collect::<Vec<_>>());
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = names;
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn roundtrip() {
        let s = super::seal("token", "y0_secret").unwrap();
        assert!(!s.contains("y0_secret"));
        assert_eq!(super::open(&s).as_deref(), Some("y0_secret"));
        assert_eq!(super::open("AAAA"), None);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn roundtrip() {
        let _g = super::keychain::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Отдельное имя: записи самого приложения тест не трогает.
        let [s] = super::seal_all([("test-roundtrip", Some("y0_secret"))]);
        let s = s.unwrap();
        assert!(!s.contains("y0_secret"));
        assert_eq!(super::open(&s).as_deref(), Some("y0_secret"));
        assert_eq!(super::open("AAAA"), None);
        super::forget("test-roundtrip");
        // В связке ключей запись исчезает.
        assert_eq!(super::open(&s), None);
    }
}
