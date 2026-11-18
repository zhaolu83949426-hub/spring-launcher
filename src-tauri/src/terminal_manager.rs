//! 内嵌交互式终端：基于 portable-pty（Windows ConPTY / Unix ptys）驱动真实 shell。
//!
//! 与 process_manager（只读日志流）的区别：终端是双向交互的——
//! 前端把按键写入 PTY master，shell 的彩色输出实时回传前端。
//!
//! 设计要点：
//! - 输出按块 emit（`terminal:data:<id>`），读 EOF 时 emit `terminal:exit:<id>`
//! - Shell 默认按 pwsh → powershell → cmd 顺序探测（TerminalShell::Auto）
//! - UTF-8 优先解码（pwsh 默认 UTF-8；老 powershell/cmd 可能 GBK，用 lossy 兜底）
//! - PTY 子进程无法跨应用重启存活：重启后仅恢复配置，由前端「打开」触发 create

use std::collections::HashMap;
use std::io::Read;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

use crate::config;
use crate::types::{TerminalInfo, TerminalShell};

/// 读循环每块最大字节数（终端输出多为小批量交互，8K 足够且延迟低）
const READ_BUF_SIZE: usize = 8192;
/// 初始终端尺寸（列 × 行）。前端 FitAddon 挂载后会立即 resize 到实际值。
const INITIAL_COLS: u16 = 80;
const INITIAL_ROWS: u16 = 24;

/// 活跃终端表：terminal_id -> 会话
static TERMINALS: Mutex<Option<HashMap<String, TerminalSession>>> = Mutex::new(None);

struct TerminalSession {
    /// PTY master 句柄。保留 Box 防止其过早 drop（drop 会触发子进程退出）。
    /// resize 列宽经它。
    master: Box<dyn MasterPty + Send>,
    /// 持久 writer：portable-pty 规定 take_writer 只能调用一次，
    /// 且 writer drop 会向 slave 发 EOF（导致 shell 退出）。
    /// 因此必须在创建会话时 take 一次，之后所有按键写入都复用它。
    writer: Box<dyn std::io::Write + Send>,
    cwd: String,
    shell: TerminalShell,
    pid: u32,
    started_at: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 探测一个可执行文件是否存在于 PATH（或为绝对路径）。
fn which(exe: &str) -> Option<String> {
    // 绝对路径直接判存在
    let abs = std::path::Path::new(exe);
    if abs.is_absolute() {
        let with_ext = exe.ends_with(".exe");
        let candidate = if with_ext {
            exe.to_string()
        } else {
            format!("{exe}.exe")
        };
        return std::path::Path::new(&candidate)
            .exists()
            .then_some(candidate);
    }
    // 沿 PATH 查找
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for candidate in [format!("{exe}.exe"), exe.to_string()] {
            let full = dir.join(&candidate);
            if full.exists() {
                return Some(full.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// 按 shell 选择解析出可执行路径与显示用程序名。
/// Auto = pwsh → powershell → cmd 顺序回退。
fn resolve_shell(shell: TerminalShell) -> Result<(String, String), String> {
    let pick = |prog: &str| which(prog).map(|p| (p, prog.to_string()));
    match shell {
        TerminalShell::Pwsh => pick("pwsh").ok_or_else(|| "找不到 pwsh（PowerShell 7）".to_string()),
        TerminalShell::PowerShell => {
            pick("powershell").ok_or_else(|| "找不到 powershell".to_string())
        }
        TerminalShell::Cmd => which("cmd")
            .map(|p| (p, "cmd".to_string()))
            .ok_or_else(|| "找不到 cmd".to_string()),
        TerminalShell::Auto => pick("pwsh")
            .or_else(|| pick("powershell"))
            .or_else(|| which("cmd").map(|p| (p, "cmd".to_string())))
            .ok_or_else(|| "系统中找不到任何可用 shell（pwsh/powershell/cmd）".to_string()),
    }
}

/// 创建并启动一个交互式终端会话。
/// 返回新分配的 terminal id。
pub fn create(
    app: &AppHandle,
    cwd: &str,
    shell: TerminalShell,
    id: Option<String>,
) -> Result<String, String> {
    let (exe, prog_name) = resolve_shell(shell)?;
    let resolved_shell = match prog_name.as_str() {
        "pwsh" => TerminalShell::Pwsh,
        "powershell" => TerminalShell::PowerShell,
        "cmd" => TerminalShell::Cmd,
        _ => shell,
    };

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: INITIAL_ROWS,
            cols: INITIAL_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("创建 PTY 失败: {e}"))?;

    let mut cmd = CommandBuilder::new(&exe);
    if !cwd.is_empty() && std::path::Path::new(cwd).is_dir() {
        cmd.cwd(cwd);
    }
    // UTF-8 优先：尽量让 shell 输出 UTF-8，避免中文/彩色乱码。
    // pwsh 默认即 UTF-8；老 powershell/cmd 通过环境变量提示（不保证生效，lossy 兜底）。
    cmd.env("LANG", "C.UTF-8");
    cmd.env("PYTHONUTF8", "1");
    #[cfg(windows)]
    {
        // 老版 Windows 控制台代码页：尝试让 cmd/powershell 走 UTF-8（65001）。
        // ConPTY 下多数情况下子控制台会继承该 codepage。
        cmd.env("DOTNET_SYSTEM_CONSOLE_ENCODING", "utf-8");
    }

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("启动 shell 失败: {e}"))?;
    let pid = child.process_id();

    drop(pair.slave); // 关闭 slave 端，EOF 才能正确回传

    let id = id.unwrap_or_else(|| Uuid::new_v4().simple().to_string());
    let started_at = now_ms();

    // 关键：take_writer 只能调用一次，且 writer drop 会向 slave 发 EOF。
    // 因此在创建会话时取一次，持久存入 session，后续所有按键复用一个 writer。
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("获取 PTY writer 失败: {e}"))?;

    // 读线程：把 master 输出逐块转发给前端
    let app_clone = app.clone();
    let id_clone = id.clone();
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("克隆 PTY reader 失败: {e}"))?;

    // 把 master 移入会话表前，先克隆一份用于读线程（master 本身由会话持有）
    std::thread::spawn(move || {
        let mut buf = [0u8; READ_BUF_SIZE];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    let data = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let event_name = format!("terminal:data:{id_clone}");
                    let _ = app_clone.emit(&event_name, crate::types::TerminalDataEvent { data });
                }
                Err(e) => {
                    log::warn!("终端 {id_clone} 读取出错: {e}");
                    break;
                }
            }
        }
        // 读取结束（子进程退出）→ 通知前端并清理会话
        let exit_name = format!("terminal:exit:{id_clone}");
        let _ = app_clone.emit(
            &exit_name,
            crate::types::TerminalExitEvent { code: None },
        );
        remove_session(&id_clone);
    });

    let session = TerminalSession {
        master: pair.master,
        writer,
        cwd: cwd.to_string(),
        shell: resolved_shell,
        pid: pid.unwrap_or(0),
        started_at,
    };

    let mut guard = TERMINALS.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    map.insert(id.clone(), session);

    Ok(id)
}

/// 向终端写入按键数据（前端 onData 转发过来）。
/// 复用创建时取出的持久 writer（portable-pty 的 take_writer 只能调用一次，
/// 且每次新建 writer 都会发 EOF，故必须复用同一个）。
pub fn write(id: &str, data: &str) -> Result<(), String> {
    let mut guard = TERMINALS.lock().unwrap();
    let map = match guard.as_mut() {
        Some(m) => m,
        None => return Err("终端会话不存在".to_string()),
    };
    let session = map
        .get_mut(id)
        .ok_or_else(|| format!("终端 {id} 不存在或已退出"))?;
    use std::io::Write;
    session
        .writer
        .write_all(data.as_bytes())
        .map_err(|e| format!("写入 PTY 失败: {e}"))?;
    session
        .writer
        .flush()
        .map_err(|e| format!("flush PTY 失败: {e}"))?;
    Ok(())
}

/// 调整终端行列尺寸（前端 FitAddon 计算后同步过来）
pub fn resize(id: &str, cols: u16, rows: u16) -> Result<(), String> {
    let mut guard = TERMINALS.lock().unwrap();
    let map = match guard.as_mut() {
        Some(m) => m,
        None => return Err("终端会话不存在".to_string()),
    };
    let session = map
        .get(id)
        .ok_or_else(|| format!("终端 {id} 不存在或已退出"))?;
    session
        .master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("resize PTY 失败: {e}"))?;
    Ok(())
}

/// 从会话表移除并 drop master（触发子进程退出）
fn remove_session(id: &str) {
    let mut guard = TERMINALS.lock().unwrap();
    if let Some(map) = guard.as_mut() {
        map.remove(id);
    }
}

/// 关闭终端：移除会话（drop master）→ 子进程随之退出 → 读线程读到 EOF
pub fn close(id: &str) {
    remove_session(id);
}

/// 当前所有活跃终端的运行态快照（合并持久化配置的 name/cwd/shell）
pub fn snapshot() -> Vec<TerminalInfo> {
    let cfg = config::load();
    let guard = TERMINALS.lock().unwrap();
    let map = guard.as_ref();

    let mut result = Vec::new();
    // 先放运行中的
    if let Some(map) = map {
        for (id, sess) in map {
            let tc = cfg.terminals.iter().find(|t| t.id == *id);
            result.push(TerminalInfo {
                id: id.clone(),
                name: tc.map(|t| t.name.clone()).unwrap_or_default(),
                cwd: sess.cwd.clone(),
                shell: sess.shell,
                running: true,
                pid: sess.pid,
                started_at: sess.started_at,
            });
        }
    }
    result
}
