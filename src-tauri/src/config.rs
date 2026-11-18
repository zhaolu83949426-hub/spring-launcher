//! 配置持久化：读写 %APPDATA%\spring-launcher\config.json
//! 用全局 Mutex<AppConfig> 缓存内存副本，避免每次磁盘 IO。

use std::fs;
use std::sync::Mutex;

use crate::types::{builtin_heap_rules, AppConfig};

/// 配置目录：%APPDATA%\spring-launcher
fn config_dir() -> std::path::PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("spring-launcher")
}

fn config_path() -> std::path::PathBuf {
    config_dir().join("config.json")
}

/// 日志目录：%APPDATA%\spring-launcher\logs
pub fn logs_dir() -> std::path::PathBuf {
    config_dir().join("logs")
}

/// 全局配置单例。首次用 OnceLock 初始化。
static CONFIG: Mutex<Option<AppConfig>> = Mutex::new(None);

/// 加载配置（文件不存在则返回空配置），并缓存到全局。
/// 首次加载若 heap_rules 为空，注入内置默认规则并落盘。
pub fn load() -> AppConfig {
    let mut guard = CONFIG.lock().unwrap();
    if let Some(cfg) = guard.as_ref() {
        return cfg.clone();
    }
    let mut cfg = read_from_disk().unwrap_or_default();
    // 老配置或首次运行：heap_rules 为空时注入内置规则
    if cfg.heap_rules.is_empty() {
        cfg.heap_rules = builtin_heap_rules();
        cfg.heap_rules_enabled = true;
        // 落盘（失败不阻塞加载）
        let _ = save(&cfg);
    }
    *guard = Some(cfg.clone());
    cfg
}

fn read_from_disk() -> Option<AppConfig> {
    let path = config_path();
    let content = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// 保存配置到磁盘，并刷新内存缓存
pub fn save(cfg: &AppConfig) -> Result<(), String> {
    // 确保目录存在
    let dir = config_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("创建配置目录失败: {e}"))?;
    fs::create_dir_all(logs_dir()).map_err(|e| format!("创建日志目录失败: {e}"))?;

    let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    fs::write(config_path(), json).map_err(|e| format!("写入配置失败: {e}"))?;

    let mut guard = CONFIG.lock().unwrap();
    *guard = Some(cfg.clone());
    Ok(())
}

/// 在闭包中修改配置，自动保存
pub fn mutate<F: FnOnce(&mut AppConfig)>(f: F) -> Result<(), String> {
    let mut guard = CONFIG.lock().unwrap();
    let cfg = guard.get_or_insert_with(AppConfig::default);
    let cfg = cfg.clone();
    drop(guard);

    let mut cfg = cfg;
    f(&mut cfg);
    save(&cfg)
}
