//! Spring 启动器 —— Tauri 应用库入口。
//! Tauri 2 推荐结构：lib.rs 定义 run()，main.rs 调用它。

mod commands;
mod config;
mod jdk_scanner;
mod log_dispatcher;
mod mvn_scanner;
mod process_manager;
mod project_scanner;
mod terminal_manager;
mod types;

use commands::*;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 初始化日志（控制台输出，便于调试；release 下几乎无开销）
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            // JDK
            scan_jdks,
            add_jdk,
            remove_jdk,
            // 扫描
            scan_services,
            // 进程
            start_service,
            stop_service,
            restart_service,
            start_group,
            start_all,
            stop_all,
            get_service_status,
            get_service_memory,
            adopt_running_services,
            // 配置
            get_config,
            save_config,
            // 工作目录选中状态
            set_workspace_selection,
            // 工作目录置顶服务
            set_workspace_pin,
            // 工作目录自定义排序
            set_workspace_order,
            // Maven
            scan_mvn,
            // 控制台（内嵌终端）
            create_terminal,
            terminal_write,
            terminal_resize,
            close_terminal,
            list_terminals,
            save_terminals,
        ])
        .run(tauri::generate_context!())
        .expect("启动 Tauri 应用失败");
}
