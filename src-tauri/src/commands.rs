//! Tauri 命令接口层：每个 #[tauri::command] 对应前端一次 invoke 调用。
//! 错误统一以 String 返回（前端 catch 后展示）。

use std::path::Path;

use tauri::{AppHandle, State};

use crate::config;
use crate::jdk_scanner;
use crate::mvn_scanner;
use crate::process_manager;
use crate::project_scanner;
use crate::terminal_manager;
use crate::types::{
    AppConfig, JdkInfo, ServiceInfo, ServiceMemory, ServiceRuntime, StartMode, TerminalConfig,
    TerminalInfo, TerminalShell,
};

/* ---------- JDK ---------- */

#[tauri::command]
pub fn scan_jdks() -> Vec<JdkInfo> {
    // 合并已保存的手动 JDK
    let saved = config::load();
    let mut found = jdk_scanner::scan();
    let mut seen: std::collections::HashSet<String> =
        found.iter().map(|j| j.path.to_lowercase()).collect();
    // 把已保存的手动 JDK 加回去（去重）
    for j in &saved.jdks {
        if j.manual && seen.insert(j.path.to_lowercase()) {
            found.push(j.clone());
        }
    }
    found
}

#[tauri::command]
pub fn add_jdk(path: String) -> Result<JdkInfo, String> {
    let root = jdk_scanner::find_jdk_root(Path::new(&path))
        .ok_or_else(|| format!("在 {path} 下找不到有效的 JDK（缺 bin/java.exe）"))?;
    let mut info = jdk_scanner::probe_jdk(&root)
        .ok_or_else(|| "无法解析 JDK 版本（java -version 执行失败）".to_string())?;
    info.manual = true;
    Ok(info)
}

#[tauri::command]
pub fn remove_jdk(id: String) -> Result<(), String> {
    config::mutate(|cfg| {
        cfg.jdks.retain(|j| j.id != id);
        if cfg.default_jdk_id == id {
            cfg.default_jdk_id.clear();
        }
        // 清理工作目录级默认中对该 JDK 的引用
        cfg.workspace_jdks.retain(|_, v| v != &id);
    })
}

/* ---------- 工作目录选中状态 ---------- */

/// 设置某个工作目录的服务选中状态（使用 config::mutate，不覆盖其他字段）
#[tauri::command]
pub fn set_workspace_selection(workspace: String, service_ids: Vec<String>) -> Result<(), String> {
    config::mutate(|cfg| {
        if service_ids.is_empty() {
            cfg.workspace_selections.remove(&workspace);
        } else {
            cfg.workspace_selections.insert(workspace, service_ids);
        }
    })
}

/// 设置某个工作目录的置顶服务列表（使用 config::mutate，不覆盖其他字段）
#[tauri::command]
pub fn set_workspace_pin(workspace: String, service_ids: Vec<String>) -> Result<(), String> {
    config::mutate(|cfg| {
        if service_ids.is_empty() {
            cfg.workspace_pins.remove(&workspace);
        } else {
            cfg.workspace_pins.insert(workspace, service_ids);
        }
    })
}

/// 设置某个工作目录的服务自定义排序（使用 config::mutate，不覆盖其他字段）
#[tauri::command]
pub fn set_workspace_order(workspace: String, service_ids: Vec<String>) -> Result<(), String> {
    config::mutate(|cfg| {
        if service_ids.is_empty() {
            cfg.workspace_orders.remove(&workspace);
        } else {
            cfg.workspace_orders.insert(workspace, service_ids);
        }
    })
}

/* ---------- 项目扫描 ---------- */

#[tauri::command]
pub fn scan_services(workspace: String) -> Vec<ServiceInfo> {
    project_scanner::scan(&workspace)
}

/* ---------- Maven ---------- */

/// 扫描系统中已安装的 Maven，返回 bin 目录路径列表。
#[tauri::command]
pub fn scan_mvn() -> Vec<String> {
    mvn_scanner::scan()
}

/* ---------- 进程管理 ---------- */

#[tauri::command]
pub fn start_service(
    app: AppHandle,
    service_id: String,
    mode: Option<StartMode>,
) -> Result<(), String> {
    process_manager::start(&app, &service_id, mode)
}

#[tauri::command]
pub fn stop_service(app: AppHandle, service_id: String) -> Result<(), String> {
    process_manager::stop(&app, &service_id)
}

#[tauri::command]
pub fn restart_service(app: AppHandle, service_id: String) -> Result<(), String> {
    process_manager::restart(&app, &service_id)
}

#[tauri::command]
pub fn start_group(app: AppHandle, group_name: String) -> Result<(), String> {
    process_manager::start_group(&app, &group_name)
}

#[tauri::command]
pub fn start_all(app: AppHandle, service_ids: Vec<String>) -> Result<(), String> {
    process_manager::start_all(&app, &service_ids)
}

#[tauri::command]
pub fn stop_all(app: AppHandle) -> Result<(), String> {
    process_manager::stop_all(&app)
}

#[tauri::command]
pub fn get_service_status() -> Vec<ServiceRuntime> {
    process_manager::snapshot()
}

/// 收编后台存量服务：把扫描出的服务与系统 java 进程表按启动类 / jar 路径匹配，
/// 命中的更新为运行中并接管状态管理。返回收编的服务 id 列表。
#[tauri::command]
pub fn adopt_running_services(
    app: AppHandle,
    workspace: String,
    services: Vec<ServiceInfo>,
) -> Vec<String> {
    process_manager::adopt_running(&app, &workspace, &services)
}

#[tauri::command]
pub fn get_service_memory() -> Vec<ServiceMemory> {
    process_manager::memory_snapshot()
}

/* ---------- 配置 ---------- */

#[tauri::command]
pub fn get_config() -> AppConfig {
    config::load()
}

#[tauri::command]
pub fn save_config(config: AppConfig) -> Result<(), String> {
    config::save(&config)
}

/* ---------- 控制台（内嵌终端） ---------- */

/// 创建并启动一个交互式终端会话，返回新分配的 terminal id。
/// cwd 为空时使用用户主目录；shell 默认 Auto（pwsh→powershell→cmd 回退）。
#[tauri::command]
pub fn create_terminal(
    app: AppHandle,
    cwd: String,
    shell: Option<TerminalShell>,
    id: Option<String>,
) -> Result<String, String> {
    let cwd = if cwd.is_empty() {
        dirs::home_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        cwd
    };
    terminal_manager::create(&app, &cwd, shell.unwrap_or_default(), id)
}

/// 向终端写入按键数据（前端 xterm onData 转发过来）
#[tauri::command]
pub fn terminal_write(id: String, data: String) -> Result<(), String> {
    terminal_manager::write(&id, &data)
}

/// 调整终端行列尺寸（前端 FitAddon 计算后同步过来）
#[tauri::command]
pub fn terminal_resize(id: String, cols: u16, rows: u16) -> Result<(), String> {
    terminal_manager::resize(&id, cols, rows)
}

/// 关闭终端（drop master → 子进程退出 → 读线程读到 EOF）
#[tauri::command]
pub fn close_terminal(id: String) {
    terminal_manager::close(&id);
}

/// 当前所有活跃终端的运行态快照
#[tauri::command]
pub fn list_terminals() -> Vec<TerminalInfo> {
    terminal_manager::snapshot()
}

/// 持久化终端配置列表（名称/目录/shell），走 config::mutate 不覆盖其他字段
#[tauri::command]
pub fn save_terminals(list: Vec<TerminalConfig>) -> Result<(), String> {
    config::mutate(|cfg| {
        cfg.terminals = list;
    })
}

/// 供 Emitter/State 推断的占位（当前未直接用，保留以备将来注入共享状态）
#[allow(dead_code)]
pub struct AppState;

#[tauri::command]
#[allow(non_snake_case)]
pub fn _noop_state(_state: State<'_, AppState>) {}
