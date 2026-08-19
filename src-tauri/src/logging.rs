//! 极简日志实现。
//!
//! 不引第三方 logger 是为了控制体积和编译时间——这个程序需要的只是
//! 「把出错信息写到一个能翻的地方」。GUI 程序没有终端，所以同时写文件。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use once_cell::sync::OnceCell;

struct FileLogger {
    file: Mutex<Option<std::fs::File>>,
    level: log::LevelFilter,
}

static LOGGER: OnceCell<FileLogger> = OnceCell::new();

pub fn log_path() -> PathBuf {
    crate::config::config_dir().join("snaplingo.log")
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let line = format!(
            "[{}] {:<5} {}: {}\n",
            timestamp(),
            record.level(),
            record.target(),
            record.args()
        );

        // 从终端启动时能直接看到
        eprint!("{line}");

        // GUI 双击启动时没有终端，落盘才能事后排查
        if let Ok(mut guard) = self.file.lock() {
            if let Some(file) = guard.as_mut() {
                let _ = file.write_all(line.as_bytes());
                let _ = file.flush();
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.file.lock() {
            if let Some(file) = guard.as_mut() {
                let _ = file.flush();
            }
        }
    }
}

/// 本地时间戳。
///
/// 不引 chrono，但必须带时区偏移——之前直接用 UTC 小时数，
/// 排查问题时日志时间和用户看到的时间差 8 小时，会把人带沟里。
fn timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let local = secs + local_utc_offset_secs();
    let h = (local.rem_euclid(86400)) / 3600;
    let m = (local.rem_euclid(3600)) / 60;
    let s = local.rem_euclid(60);
    format!("{h:02}:{m:02}:{s:02}")
}

/// 用 libc 的 localtime 拿本机时区偏移，只算一次
fn local_utc_offset_secs() -> i64 {
    use std::sync::OnceLock;
    static OFFSET: OnceLock<i64> = OnceLock::new();

    *OFFSET.get_or_init(|| {
        // date +%z 返回 +0800 这种形式，解析出秒数
        std::process::Command::new("date")
            .arg("+%z")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|text| {
                let t = text.trim();
                if t.len() < 5 {
                    return None;
                }
                let sign = if t.starts_with('-') { -1 } else { 1 };
                let hours: i64 = t[1..3].parse().ok()?;
                let mins: i64 = t[3..5].parse().ok()?;
                Some(sign * (hours * 3600 + mins * 60))
            })
            .unwrap_or(0)
    })
}

pub fn init() {
    let level = if cfg!(debug_assertions) {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };

    let path = log_path();
    let _ = std::fs::create_dir_all(crate::config::config_dir());

    // 启动前把上一轮的日志留一份。
    //
    // 只截断不备份的话，排查时会很难受：应用一旦重启（尤其自动更新后会自动
    // 重启），刚复现出来的现场就被清空了，用户白试一次。留一份上轮日志，
    // 代价只是多一个文件，收益是"重启前发生了什么"永远可查。
    let previous = crate::config::config_dir().join("snaplingo.log.1");
    // 必须先删目标：Windows 上 rename 到已存在的文件会直接失败，
    // 结果是上一轮日志没备份、当前日志照样被截断，等于白做。
    let _ = std::fs::remove_file(&previous);
    if let Err(err) = std::fs::rename(&path, &previous) {
        // 首次运行时源文件不存在，这是正常的；其它错误值得让人看见
        if err.kind() != std::io::ErrorKind::NotFound {
            eprintln!("[snaplingo] 轮转上一轮日志失败: {err}");
        }
    }

    // 仍然每次启动开新文件，避免日志无限增长——这是排查工具不是审计日志
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .ok();

    let logger = LOGGER.get_or_init(|| FileLogger {
        file: Mutex::new(file),
        level,
    });

    if log::set_logger(logger).is_ok() {
        log::set_max_level(level);
        log::info!("SnapLingo 启动，日志文件: {}", path.display());
    }
}
