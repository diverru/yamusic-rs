//! Связка ключей macOS через системную утилиту `/usr/bin/security`. Запись, созданная самим
//! приложением через API, помечается подписью (cdhash) его сборки в разделе partition_id, и
//! каждая следующая сборка получает запрос пароля связки; снять эту пометку без пароля входа
//! нельзя. Запись, созданную утилитой с `-A`, система помечает разделом «apple-tool:» и читает
//! её только та же утилита, поэтому подпись приложения больше ничего не решает. Секрет уходит
//! утилите через stdin (`security -i`), а не в аргументах: их видят другие процессы.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Дольше ждать ответа не стоит: если связка заперта, утилита показывает окно с паролем и ждёт.
const TIMEOUT: Duration = Duration::from_secs(10);
/// Код выхода `security`, когда записи нет.
const NOT_FOUND: i32 = 44;

pub enum Get {
    Found(String),
    Missing,
    Failed(String),
}

/// Запустить `security`: код выхода и stdout. Не запустилась или не ответила вовремя (тогда её убиваем) — Err.
fn run(args: &[&str], stdin: Option<&str>) -> Result<(i32, String), String> {
    let mut child = Command::new("/usr/bin/security")
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("security не запустилась: {e}"))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(text.as_bytes());
    }
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("security не ответила за 10 секунд".into());
            }
            Err(e) => return Err(format!("security: {e}")),
        }
    };
    let mut out = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut out);
    }
    Ok((status.code().unwrap_or(-1), out))
}

pub fn get(service: &str, account: &str) -> Get {
    match run(&["find-generic-password", "-s", service, "-a", account, "-w"], None) {
        Ok((0, out)) => Get::Found(out.trim_end_matches('\n').to_string()),
        Ok((NOT_FOUND, _)) => Get::Missing,
        Ok((code, _)) => Get::Failed(format!("security вернула код {code}")),
        Err(e) => Get::Failed(e),
    }
}

/// Создать запись или заменить её значение. `value` — только base64: кавычек и пробелов
/// в нём нет, поэтому строку команды не нужно экранировать.
pub fn set(service: &str, account: &str, value: &str) -> Result<(), String> {
    debug_assert!(value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=')));
    let command = format!("add-generic-password -A -U -s {service} -a {account} -w {value}\n");
    match run(&["-i"], Some(&command))? {
        (0, _) => Ok(()),
        (code, _) => Err(format!("security вернула код {code}")),
    }
}

/// Удалить запись; когда её уже нет, это не ошибка.
pub fn delete(service: &str, account: &str) -> Result<(), String> {
    match run(&["delete-generic-password", "-s", service, "-a", account], None)? {
        (0 | NOT_FOUND, _) => Ok(()),
        (code, _) => Err(format!("security вернула код {code}")),
    }
}
