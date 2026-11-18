//! 进程生命周期管理：启动 / 停止 / 重启 Spring Boot 服务。
//! 同时负责实时转发 stdout/stderr 日志到前端，以及就绪判定。
//!
//! 核心难点处理：
//! - `mvn spring-boot:run` 会 fork 子 JVM：停止时用 Windows taskkill /T 杀进程树
//! - 就绪判定：监听日志匹配 `Started .* in .* seconds`，比探端口准
//!   （本项目端口在 Nacos 远端，探端口不可靠）

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use tauri::{AppHandle, Emitter};

use crate::config;
use crate::log_dispatcher;
use crate::types::{LogEvent, ServiceStatus, StartMode, StatusEvent};

/// Windows 下隐藏控制台窗口的 creation flag
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// mvn fork 模式下，父进程退出后给真实 JVM 的启动宽限期（毫秒）。
/// 超过此时间端口仍未监听，才判为崩溃。
const PORT_GRACE_MS: u64 = 120_000;

/// 启动超时上限（毫秒）。无论父进程是否退出，自 started_at 起超过此时长仍未就绪
/// （端口未监听 + 日志未匹配到 Started），即判为崩溃。
///
/// 适配真实场景：mvn fork 出的子 JVM 在某些启动失败时**不会退出**（如 Nacos 的
/// Raft 初始化异常会让进程挂起，mvn 主进程因此一直阻塞等待子进程，永不返回）。
/// 此时 child 一直存活、parent_exited_at 永不置位，旧的"父进程退出后再宽限"逻辑
/// 永远触发不了崩溃判定，导致服务永远卡在 Starting。此上限兜底这种情况。
const START_TIMEOUT_MS: u64 = 180_000;

/// 统一 Maven 与其派生 Java 进程的配置、控制台输出编码。
const JAVA_UTF8_OPTIONS: &str =
    "-Dfile.encoding=UTF-8 -Dsun.stdout.encoding=UTF-8 -Dsun.stderr.encoding=UTF-8";

const LOG_FILE_WAIT_INTERVAL: Duration = Duration::from_millis(100);
const LOG_FILE_WAIT_RETRIES: usize = 100;
const LOG_FILE_READY_DELAY: Duration = Duration::from_millis(300);
const LOG_FILE_POLL_INTERVAL: Duration = Duration::from_millis(200);
const LOG_FILE_CHUNK_SIZE: usize = 8192;
const STOP_WAIT_INTERVAL: Duration = Duration::from_millis(100);
const STOP_WAIT_RETRIES: usize = 30;

/// 收编进程存活巡检间隔（毫秒）。收编进程没有 child 句柄，
/// 只能周期性按 PID / 启动类反查 java 进程表判断存活。
const ADOPTED_POLL_INTERVAL: Duration = Duration::from_millis(3000);

/// 活跃进程表：service_id -> 进程信息
static PROCESSES: Mutex<Option<HashMap<String, ServiceProcess>>> = Mutex::new(None);

struct ServiceProcess {
    /// 本 launcher 直接 spawn 的子进程句柄；收编进程（外部启动）为 None，
    /// 存活判断改用 PID / 启动类反查 java 进程表
    child: Option<Child>,
    pid: u32,
    started_at: u64,
    jdk_id: String,
    /// 启动类全限定名（mvn fork 模式下用于按命令行匹配真实 JVM 进程）
    main_class: String,
    /// 是否已进入 Running 状态（用于退出时区分崩溃 vs 正常停止）
    became_running: bool,
    /// 服务监听端口（用于 Mvn 模式下探测真实 JVM 存活）。0 表示未知。
    port: u16,
    /// 是否为 mvn spring-boot:run fork 模式（Windows 下父 cmd.exe 退出是预期行为，
    /// 不能用 child 退出判断服务终止）。
    mvn_forked: bool,
    /// 父 cmd.exe 退出的时间戳（毫秒）。0 表示尚未退出。
    /// 用于在 fork 模式下给真实 JVM 一个启动宽限期。
    parent_exited_at: u64,
    /// 是否已从日志中确认启动失败（如匹配到 "APPLICATION FAILED TO START" /
    /// "Nacos failed to start"）。置位后 snapshot/monitor_exit 立即判 Crashed，
    /// 不再等待端口宽限或启动超时。
    failed: bool,
    /// 该服务启动时所属的工作目录（用于前端按工作目录分组展示）。
    workspace: String,
    /// 是否为收编进程：不是本 launcher 本次会话 spawn 的，而是重新打开后
    /// 按启动类/jar 路径从系统 java 进程表里识别回来的存量进程。
    /// 这类进程没有 child 句柄、stdout 已丢失，只能按 PID/命令行管理生命周期。
    adopted: bool,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 探测端口是否在监听（判断真实 JVM 存活）。
/// 尝试建立 TCP 连接：成功说明有进程在监听该端口。
fn port_listening(port: u16) -> bool {
    use std::net::TcpStream;
    use std::time::Duration;
    let addr = format!("127.0.0.1:{port}");
    TcpStream::connect_timeout(
        &addr
            .parse()
            .unwrap_or_else(|_| "127.0.0.1:1".parse::<std::net::SocketAddr>().unwrap()),
        Duration::from_millis(500),
    )
    .is_ok()
}

/// 通过端口反查占用进程 PID（mvn fork 模式下停止服务用）。
/// 调用 netstat -ano 解析 ":<port>" 行的最后一列 PID。
fn pid_by_port(port: u16) -> Option<u32> {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("netstat");
        cmd.args(["-ano"]);
        cmd.creation_flags(CREATE_NO_WINDOW);
        let out = cmd.output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        // 找 LISTENING 且匹配端口的行
        for line in text.lines() {
            let trimmed = line.trim();
            // 只关心 LISTENING 状态
            if !trimmed.contains("LISTENING") {
                continue;
            }
            // 本地地址列含 :<port>（冒号后须紧跟端口，避免误匹配 :88480）
            let local_col = trimmed.split_whitespace().nth(1).unwrap_or("");
            // local_col 形如 "0.0.0.0:8848" / "[::]:8848" / "127.0.0.1:8848"
            if !port_matches(local_col, port) {
                continue;
            }
            // 最后一列是 PID
            return trimmed.split_whitespace().last()?.parse().ok();
        }
        None
    }
    #[cfg(not(windows))]
    {
        // 非 Windows：lsof -i :<port>
        let out = Command::new("lsof")
            .args(["-t", "-i", &format!(":{port}")])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        text.trim().parse().ok()
    }
}

/// 判断 netstat 本地地址列（如 "0.0.0.0:8848"、"[::]:8848"）是否正好监听该端口。
/// 冒号后须紧跟端口数字，避免 ":8848" 误匹配 ":88480"。
fn port_matches(local_col: &str, port: u16) -> bool {
    let Some(addr_part) = local_col.rsplit(':').next() else {
        return false;
    };
    addr_part == port.to_string()
}

/// 读取某服务记录的监听端口（0 表示未知）。用于杀进程树时端口反查真实 JVM。
fn port_for_service(service_id: &str) -> u16 {
    let guard = PROCESSES.lock().unwrap();
    guard
        .as_ref()
        .and_then(|m| m.get(service_id))
        .map(|sp| sp.port)
        .unwrap_or(0)
}

/// 杀进程树：Windows 用 taskkill /T /F 杀 launcher 子进程（pid）整棵树；
/// 若 port>0，再按端口反查真实 JVM pid 补杀（mvn fork 模式下真实 JVM 是独立进程）。
/// 用于 stop() 以及启动失败/超时后清理挂起的子 JVM。
/// 返回实际下发 taskkill 的 PID 列表（含端口反查到的），供日志展示。
fn kill_process_tree(pid: u32, port: u16, main_class: &str) -> Vec<u32> {
    let mut pids: Vec<u32> = vec![pid];
    if port > 0 {
        if let Some(jvm_pid) = pid_by_port(port) {
            pids.push(jvm_pid);
        }
    }
    #[cfg(windows)]
    if !main_class.is_empty() {
        pids.extend(
            list_java_processes()
                .into_iter()
                .filter(|process| java_process_matches(&process.cmdline, main_class))
                .map(|process| process.pid),
        );
    }
    pids.sort_unstable();
    pids.dedup();
    for kp in &pids {
        let mut kill_cmd = Command::new("taskkill");
        kill_cmd.args(["/PID", &kp.to_string(), "/T", "/F"]);
        #[cfg(windows)]
        kill_cmd.creation_flags(CREATE_NO_WINDOW);
        match kill_cmd.output() {
            Ok(out) if !out.status.success() => {
                log::warn!(
                    "taskkill (PID {kp}): {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            Err(e) => log::warn!("taskkill 执行失败 (PID {kp}): {e}"),
            _ => {}
        }
    }
    pids
}

/// 把状态变更广播给前端
fn emit_status(app: &AppHandle, id: &str, status: ServiceStatus, pid: u32) {
    let _ = app.emit(
        "service:status",
        StatusEvent {
            id: id.to_string(),
            status,
            pid,
        },
    );
}

/// 推送一行日志给前端
fn emit_log(app: &AppHandle, service_id: &str, line: &str, is_error: bool) {
    log_dispatcher::emit_log(
        app,
        service_id,
        LogEvent {
            line: strip_ansi_sequences(line),
            is_error,
        },
    );
}

/// 移除终端 ANSI CSI 控制序列，避免颜色代码在普通文本日志中直接显示。
fn strip_ansi_sequences(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'[') {
            let mut end = index + 2;
            while end < bytes.len() && !(0x40..=0x7e).contains(&bytes[end]) {
                end += 1;
            }
            if end < bytes.len() {
                index = end + 1;
                continue;
            }
        }
        result.push(bytes[index]);
        index += 1;
    }

    String::from_utf8(result).expect("移除 ASCII 控制序列后应保持 UTF-8 有效")
}

/// 解析配置拿到某服务的启动参数（JDK、模式、JVM参数、Spring参数、Maven profiles）
/// JDK 解析优先级：服务级 > 工作目录级 > 全局默认
/// 服务配置按工作目录分组：services[workspace][service_id]
fn resolve_service_launch(service_id: &str) -> (String, StartMode, String, String, String, String) {
    let cfg = config::load();
    let svc_cfg = cfg
        .services
        .get(&cfg.workspace)
        .and_then(|ws_svcs| ws_svcs.get(service_id));
    let jdk_id = svc_cfg.map(|s| s.jdk_id.clone()).unwrap_or_default();
    let jdk_id = if !jdk_id.is_empty() {
        jdk_id
    } else if let Some(ws_jdk) = cfg.workspace_jdks.get(&cfg.workspace) {
        // 服务未指定 JDK，先查工作目录级默认
        if !ws_jdk.is_empty() {
            ws_jdk.clone()
        } else {
            cfg.default_jdk_id
        }
    } else {
        // 工作目录级也没配，用全局默认
        cfg.default_jdk_id
    };
    let mode = svc_cfg.map(|s| s.mode).unwrap_or(StartMode::Mvn);
    let extra = svc_cfg.map(|s| s.extra_args.clone()).unwrap_or_default();
    let mvn_profiles = svc_cfg.map(|s| s.mvn_profiles.clone()).unwrap_or_default();
    // 拼装 JVM 参数（堆 + 元空间 + 其他），非法值会被 validate_size 丢弃
    let jvm_args = svc_cfg
        .map(|s| build_jvm_args(&s.jvm_xms, &s.jvm_xmx, &s.jvm_metaspace, &s.jvm_extra))
        .unwrap_or_default();
    let mvn_path = cfg.mvn_path.clone();
    (jdk_id, mode, extra, mvn_path, mvn_profiles, jvm_args)
}

/// 查 JDK 路径
fn find_jdk_path(jdk_id: &str) -> Result<String, String> {
    if jdk_id.is_empty() {
        return Err("未配置 JDK，请先在 JDK 页设置默认 JDK".into());
    }
    let cfg = config::load();
    let jdk = cfg
        .jdks
        .iter()
        .find(|j| j.id == jdk_id)
        .ok_or_else(|| format!("找不到 JDK id={jdk_id}"))?;
    Ok(jdk.path.clone())
}

/// 启动单个服务（命令入口）
pub fn start(
    app: &AppHandle,
    service_id: &str,
    override_mode: Option<StartMode>,
) -> Result<(), String> {
    // 已在运行则拒绝（收编进程给出更明确的提示）
    {
        let mut guard = PROCESSES.lock().unwrap();
        let map = guard.get_or_insert_with(HashMap::new);
        if let Some(sp) = map.get(service_id) {
            return Err(if sp.adopted {
                format!("{service_id} 已在后台运行（启动时识别），请先停止再启动")
            } else {
                format!("{service_id} 已在运行")
            });
        }
    }

    let (jdk_id, mode, extra_args, mvn_path, mvn_profiles, jvm_args) =
        resolve_service_launch(service_id);
    let mode = override_mode.unwrap_or(mode);
    let jdk_path = find_jdk_path(&jdk_id)?;
    let java_home = Path::new(&jdk_path).to_path_buf();
    let java_exe = java_home.join("bin").join("java.exe");

    // 读配置拿 workspace，重扫定位服务信息
    let cfg = config::load();
    let workspace = cfg.workspace.clone();
    let services = crate::project_scanner::scan(&workspace);
    let info = services
        .iter()
        .find(|s| s.id == service_id)
        .ok_or_else(|| format!("找不到服务 {service_id}，请先扫描"))?;

    // Windows Mvn 模式下，mvn.cmd 的 stdout/stderr 不能被 pipe
    // （批处理依赖控制台句柄，pipe 会导致提前退出）。
    // 改为：cmd /S /C "mvn ... > 日志文件 2>&1"，然后用线程 tail 日志文件。
    // Jar 模式直接用 java.exe，可以正常 pipe。
    #[cfg(windows)]
    let use_file_redirect = matches!(mode, StartMode::Mvn);
    #[cfg(not(windows))]
    let use_file_redirect = false;

    // 日志文件路径（仅 Windows Mvn 模式用到）
    let log_file = if use_file_redirect {
        let dir = config::logs_dir();
        let _ = std::fs::create_dir_all(&dir);
        dir.join(format!("{service_id}.log"))
            .to_string_lossy()
            .replace('\\', "/")
    } else {
        String::new()
    };

    let (mut cmd, cmd_desc) = build_command(
        mode,
        &mvn_path,
        &java_exe.to_string_lossy(),
        info,
        &extra_args,
        &jvm_args,
        &mvn_profiles,
        &workspace,
        use_file_redirect,
        &log_file,
    );

    // 关键：设置 JAVA_HOME 指向选定的 JDK，并继承 + 前置 PATH
    cmd.env("JAVA_HOME", &java_home);
    let path_env = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{};{}", java_home.join("bin").display(), path_env);
    cmd.env("PATH", new_path);
    let inherited_java_options = std::env::var("JAVA_TOOL_OPTIONS").unwrap_or_default();
    cmd.env(
        "JAVA_TOOL_OPTIONS",
        format!("{inherited_java_options} {JAVA_UTF8_OPTIONS}")
            .trim()
            .to_string(),
    );

    // Windows 下隐藏弹出的 cmd 窗口
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    if use_file_redirect {
        // 输出重定向到文件，不 pipe
        cmd.stdin(Stdio::null());
    } else {
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("启动 {service_id} 失败：{e}\n  执行命令: {cmd_desc}");
            emit_log(app, service_id, &msg, true);
            return Err(msg);
        }
    };

    let pid = child.id();
    let stdout = if !use_file_redirect {
        child.stdout.take()
    } else {
        None
    };
    let stderr = if !use_file_redirect {
        child.stderr.take()
    } else {
        None
    };

    // 记入进程表
    {
        let mut guard = PROCESSES.lock().unwrap();
        let map = guard.get_or_insert_with(HashMap::new);
        map.insert(
            service_id.to_string(),
            ServiceProcess {
                child: Some(child),
                pid,
                started_at: now_ms(),
                jdk_id: jdk_id.clone(),
                main_class: info.main_class.clone(),
                became_running: false,
                port: info.port,
                mvn_forked: use_file_redirect,
                parent_exited_at: 0,
                failed: false,
                workspace: workspace.clone(),
                adopted: false,
            },
        );
    }

    emit_status(app, service_id, ServiceStatus::Starting, pid);
    // Mvn 模式下追加模块路径与配置工作目录，便于排查 ${project.basedir} 错位
    // （如 Maven 报 systemPath 找不到 jar，路径与实际不符时，对比这三行可定位）。
    let mvn_diag = if matches!(mode, StartMode::Mvn) {
        format!(
            "\n  模块路径: {}\n  配置工作目录: {}",
            info.module_path, workspace,
        )
    } else {
        String::new()
    };
    emit_log(
        app,
        service_id,
        &format!(
            ">>> 启动 {service_id} (PID {pid}, JDK {}, 模式 {:?})\n  工作目录: {}\n  JAVA_HOME: {}\n  执行命令: {cmd_desc}{mvn_diag}",
            jdk_path,
            mode,
            cmd.get_current_dir()
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_default(),
            java_home.display(),
        ),
        false,
    );

    if use_file_redirect {
        // Windows Mvn 模式：tail 日志文件转发
        let app_for_file = app.clone();
        let sid_for_file = service_id.to_string();
        let lf = log_file.clone();
        thread::spawn(move || stream_log_file(&app_for_file, &sid_for_file, &lf));
    } else {
        // Jar 模式：直接读 pipe
        let app_for_out = app.clone();
        let sid_out = service_id.to_string();
        thread::spawn(move || stream_stdout(&app_for_out, &sid_out, stdout));

        let app_for_err = app.clone();
        let sid_err = service_id.to_string();
        thread::spawn(move || stream_stderr(&app_for_err, &sid_err, stderr));
    }

    // 监控进程退出（单独线程）
    let app2 = app.clone();
    let sid2 = service_id.to_string();
    thread::spawn(move || monitor_exit(app2, &sid2));

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_command(
    mode: StartMode,
    mvn_path: &str,
    java_exe: &str,
    info: &crate::types::ServiceInfo,
    extra_args: &str,
    jvm_args: &str,
    mvn_profiles: &str,
    workspace: &str,
    file_redirect: bool,
    log_file: &str,
) -> (Command, String) {
    match mode {
        StartMode::Jar => {
            // JVM 参数（-Xmx 等）必须放在 -jar 之前，否则被当作应用参数不生效。
            let mut c = Command::new(java_exe);
            if !jvm_args.is_empty() {
                for a in shell_split(jvm_args) {
                    c.arg(a);
                }
            }
            c.arg("-jar");
            c.arg(&info.jar_path);
            if !extra_args.is_empty() {
                for a in shell_split(extra_args) {
                    c.arg(a);
                }
            }
            c.current_dir(&info.module_path);
            let jvm_desc = if jvm_args.is_empty() {
                String::new()
            } else {
                format!(" {jvm_args}")
            };
            let desc = format!("{java_exe}{jvm_desc} -jar {} {extra_args}", info.jar_path);
            (c, desc.trim_end().to_string())
        }
        StartMode::Mvn => {
            // 找 Maven reactor 根（含聚合 pom.xml 的目录）。
            let reactor_root = find_reactor_root(&info.module_path, workspace);
            let reactor_root = reactor_root.as_deref().unwrap_or(workspace);

            // 用 :artifactId 语法指定模块，不依赖相对路径计算。
            // 避免扫描到 bin/ 等子目录下的 pom.xml 时 module_rel 算错
            // （如 eim-gateway/bin → Maven reactor 中找不到）。
            // Maven 3.1+ 支持 -pl :artifactId 按 artifactId 精确定位 reactor 内模块。
            let module_selector = format!(":{}", info.artifact_id);

            let resolved_mvn = resolve_mvn_path(mvn_path);

            // 解析本地仓库路径（从 settings.xml 读取，回退 ~/.m2/repository）。
            // 显式传 -Dmaven.repo.local 确保依赖解析用对本地仓库，
            // 避免 mvn 因找不到全局/用户 settings.xml 而用错仓库位置。
            // Windows 下把路径用引号包进 -Dmaven.repo.local="..."，路径含空格也安全。
            let repo_local = resolve_maven_repo_local(mvn_path);
            let repo_arg_cli: Option<String> = repo_local
                .as_ref()
                .map(|p| format!("-Dmaven.repo.local=\"{p}\""));
            let repo_arg_value: Option<String> = repo_local
                .as_ref()
                .map(|p| format!("-Dmaven.repo.local={p}"));

            // Maven profile 激活：用户填的 profile 串（逗号分隔）拼成 -Pxxx,yyy。
            // 仅 mvn 模式生效；校验每个 token（仅字母/数字/._- 及可选 ! 前缀），
            // 非法 token 静默丢弃，避免命令注入。
            let profile_arg = build_profile_arg(mvn_profiles);

            // JVM 参数（-Xmx 等）通过 -Dspring-boot.run.jvmArguments 传给 fork 出的子 JVM；
            // Spring 应用参数（--server.port 等）走 -Dspring-boot.run.arguments。
            // 两者必须分开，否则 -Xmx 会被当作应用参数不生效。
            let jvm_arg_cli: Option<String> = if jvm_args.is_empty() {
                None
            } else {
                Some(format!("-Dspring-boot.run.jvmArguments=\"{jvm_args}\""))
            };
            let jvm_arg_value: Option<String> = if jvm_args.is_empty() {
                None
            } else {
                Some(format!("-Dspring-boot.run.jvmArguments={jvm_args}"))
            };

            // 运行阶段只执行目标服务，避免对依赖模块执行 spring-boot:run。
            let run_desc_core = format!(
                "{resolved_mvn} -pl {module_selector} spring-boot:run -DskipTests{profile}{repo}{jvm}{extra}",
                profile = profile_arg
                    .as_ref()
                    .map(|p| format!(" {p}"))
                    .unwrap_or_default(),
                repo = repo_arg_cli
                    .as_ref()
                    .map(|s| format!(" {s}"))
                    .unwrap_or_default(),
                jvm = jvm_arg_cli
                    .as_ref()
                    .map(|s| format!(" {s}"))
                    .unwrap_or_default(),
                extra = if extra_args.is_empty() {
                    String::new()
                } else {
                    format!(" -Dspring-boot.run.arguments={extra_args}")
                }
            );

            #[cfg(windows)]
            let build_desc_core = format!(
                "{resolved_mvn} -pl {module_selector} -am install -DskipTests \
                 -Dspring-boot.repackage.skip=true -Dmaven.jar.forceCreation=true{profile}{repo}",
                profile = profile_arg
                    .as_ref()
                    .map(|p| format!(" {p}"))
                    .unwrap_or_default(),
                repo = repo_arg_cli
                    .as_ref()
                    .map(|s| format!(" {s}"))
                    .unwrap_or_default(),
            );
            #[cfg(windows)]
            if file_redirect {
                // Windows Mvn 模式：mvn.cmd 的 stdout/stderr 不能 pipe
                // （批处理依赖控制台句柄，pipe 导致提前退出）。
                // 先安装目标服务及其依赖链，再单独启动目标服务；构建失败时不会继续启动。
                // raw_arg 保证 cmd.exe 拿到完整的命令行字符串，不做参数拆分。
                //
                // 注意两个 cmd.exe 解析器坑（实测）：
                // 1. 括号组内以带引号的外部命令开头（`("mvn.cmd" ...)`）会静默失败
                //    （exit 1、零输出、重定向文件被创建但永远为空），内部命令不受影响；
                // 2. 因此 mvn 路径仅在含空格时才加引号，且不用括号分组，
                //    改为逐命令重定向：构建阶段 `>` 截断写日志，运行阶段 `>>` 追加。
                let mvn_bin = if resolved_mvn.contains(' ') {
                    format!("\"{resolved_mvn}\"")
                } else {
                    resolved_mvn.to_string()
                };
                // 关键：业务模块若配置了 spring-boot-maven-plugin:repackage，
                // mvn install 会装出可执行 fat jar（类在 BOOT-INF/classes 下，
                // 对依赖方不可见）。随后 spring-boot:run 从本地仓库解析这些
                // fat jar，模块类全部加载不到（表现为 No qualifying bean）。
                // 因此构建阶段跳过 repackage 装薄 jar；jar 插件默认有增量跳过，
                // 需 forceCreation 确保 target 里旧 fat jar 被重建覆盖。
                let mut build_cmdline = format!(
                    "{mvn_bin} -pl {module_selector} -am install -DskipTests \
                     -Dspring-boot.repackage.skip=true -Dmaven.jar.forceCreation=true"
                );
                if let Some(p) = &profile_arg {
                    build_cmdline.push(' ');
                    build_cmdline.push_str(p);
                }
                if let Some(arg) = &repo_arg_cli {
                    build_cmdline.push(' ');
                    build_cmdline.push_str(arg);
                }

                let mut run_cmdline = format!("{mvn_bin} -pl {module_selector} spring-boot:run -DskipTests");
                if let Some(p) = &profile_arg {
                    run_cmdline.push(' ');
                    run_cmdline.push_str(p);
                }
                if let Some(arg) = &repo_arg_cli {
                    run_cmdline.push(' ');
                    run_cmdline.push_str(arg);
                }
                if let Some(arg) = &jvm_arg_cli {
                    run_cmdline.push(' ');
                    run_cmdline.push_str(arg);
                }
                if !extra_args.is_empty() {
                    run_cmdline.push_str(&format!(" -Dspring-boot.run.arguments={extra_args}"));
                }
                let full_cmdline = format!(
                    "{build_cmdline} > \"{log_file}\" 2>&1 && {run_cmdline} >> \"{log_file}\" 2>&1"
                );
                let mut c = Command::new("cmd.exe");
                c.raw_arg(format!("/S /C \"{full_cmdline}\""));
                c.current_dir(reactor_root);
                return (
                    c,
                    format!("{build_desc_core} > \"{log_file}\" 2>&1 && {run_desc_core} >> \"{log_file}\" 2>&1"),
                );
            }

            // 非 Windows 或非文件重定向模式：直接运行 mvn
            let mut c = Command::new(&resolved_mvn);
            c.arg("-pl").arg(&module_selector);
            // 非 Windows 暂按单命令编译并启动目标服务。
            c.arg("compile").arg("spring-boot:run").arg("-DskipTests");
            if let Some(p) = &profile_arg {
                c.arg(p);
            }
            if let Some(arg) = &repo_arg_value {
                c.arg(arg);
            }
            if let Some(arg) = &jvm_arg_value {
                c.arg(arg);
            }
            if !extra_args.is_empty() {
                c.arg(format!("-Dspring-boot.run.arguments={extra_args}"));
            }
            c.current_dir(reactor_root);
            let desc = run_desc_core.replacen(" spring-boot:run", " compile spring-boot:run", 1);
            (c, desc)
        }
    }
}

/// 把用户填写的 Maven profile 串（逗号或空白分隔）解析并拼成 `-Pxxx,yyy`。
/// 校验每个 token：仅允许字母、数字、`.`、`_`、`-`，以及开头的 `!`（排除 profile）。
/// 任何非法 token 整体丢弃（静默忽略），防止命令注入。
/// 空串或全部非法时返回 None。
fn build_profile_arg(raw: &str) -> Option<String> {
    let mut valid: Vec<&str> = Vec::new();
    for token in raw.split(|c: char| c == ',' || c.is_whitespace()) {
        let t = token.trim();
        if t.is_empty() {
            continue;
        }
        // 去掉开头的可选 '!'（Maven 排除 profile 语法），再校验剩余
        let body = t.strip_prefix('!').unwrap_or(t);
        if !body.is_empty()
            && body
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        {
            valid.push(t);
        }
    }
    if valid.is_empty() {
        None
    } else {
        Some(format!("-P{}", valid.join(",")))
    }
}

/// 查找 Maven reactor 根目录。
/// 从模块路径开始向上逐级查找，找到第一个含 pom.xml 且 pom 是聚合 pom 的目录，
/// 即为 reactor 根。
///
/// 聚合 pom 的判定（与 Maven 自身语义一致）：声明了未被注释的 `<modules>` 块
/// （即至少含一个 `<module>` 子元素）。是否含 `<parent>` 与是否为聚合根无关——
/// 聚合 pom 完全可以有 `<parent>`（本项目两个聚合 pom 都有父）。
///
/// 早期实现用"没有 `<parent>`"判定聚合根，在父 pom 本身继承
/// spring-boot-starter-parent 的项目（如 x-space）上会误判：遍历到根 pom 仍因有
/// `<parent>` 跳过，最终回退到 workspace，但 workspace 根的默认 reactor 不含
/// 目标模块（模块声明在 profile 里），导致 `Could not find the selected project`。
///
/// 找不到（走到 workspace 上界仍无聚合 pom），回退到 None（调用方用 workspace）。
fn find_reactor_root(module_path: &str, workspace: &str) -> Option<String> {
    let module = Path::new(module_path);
    let ws = Path::new(workspace);

    // 从模块目录开始向上，直到 workspace（含）
    let mut dir = module;
    loop {
        let pom = dir.join("pom.xml");
        if pom.is_file() {
            if let Ok(content) = std::fs::read_to_string(&pom) {
                // 去掉 XML 注释 <!-- ... -->，再判断是否为聚合 pom
                let stripped = strip_xml_comments(&content);
                if is_aggregator_pom(&stripped) {
                    return Some(dir.to_string_lossy().into_owned());
                }
            }
        }
        // 到 workspace 就停（不再往上）。
        // Windows 文件系统不区分大小写，Path == 字节比较会在大小写不一致时误判为不等，
        // 导致遍历越过 workspace 上界选错 reactor 根。这里用小写比较兜底。
        if path_eq_ignore_case(dir, ws) {
            break;
        }
        dir = match dir.parent() {
            Some(p) => p,
            None => break,
        };
    }
    None
}

/// 判断一段（已去注释的）pom 文本是否为聚合 pom：含 `<modules>` 块且至少一个
/// `<module>` 子元素。这是 Maven reactor 根的真正信号（与有无 `<parent>` 无关）。
///
/// 用字符串探测而非完整 XML 解析，与本文件既有风格一致（见 strip_xml_comments/
/// read_local_repository）。匹配 `<module>` 时要求标签内含路径文本，
/// 排除空 `<module/>`。
fn is_aggregator_pom(pom_no_comments: &str) -> bool {
    // 粗略要求：同时出现 <modules> 与至少一个带内容的 <module>...</module>
    if !pom_no_comments.contains("<modules>") {
        return false;
    }
    // 在 <modules>...</modules> 区间内找 <module> 子元素
    let Some(modules_start) = pom_no_comments.find("<modules>") else {
        return false;
    };
    let after_open = &pom_no_comments[modules_start..];
    let close = after_open.find("</modules>").unwrap_or(after_open.len());
    let block = &after_open[..close];
    // 含至少一个 <module>xxx</module>（非空）
    block.matches("<module>").count() >= 1
}

/// Windows 下不区分大小写的路径相等比较；非 Windows 直接用 ==
/// （先做分隔符无关的标准 == 比较，true 则直接返回，避免无谓的字符串处理）
#[cfg(windows)]
fn path_eq_ignore_case(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

#[cfg(not(windows))]
fn path_eq_ignore_case(a: &Path, b: &Path) -> bool {
    a == b
}

/// 粗略去掉 XML 注释（<!-- ... -->），用于判断 <parent> 是否真实存在。
fn strip_xml_comments(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find("<!--") {
        result.push_str(&rest[..start]);
        rest = &rest[start + 4..];
        if let Some(end) = rest.find("-->") {
            rest = &rest[end + 3..];
        } else {
            // 未闭合注释，丢弃剩余
            break;
        }
    }
    result.push_str(rest);
    result
}

/// 解析 mvn 可执行文件路径，兼容三种配置形式：
/// 1. "mvn"（纯文件名，靠 PATH 查找）→ Windows 补 .cmd
/// 2. "D:/apache-maven/bin"（bin 目录）→ 拼 mvn.cmd / mvn
/// 3. "D:/apache-maven/bin/mvn.cmd"（完整文件路径）→ 原样返回
fn resolve_mvn_path(mvn_path: &str) -> String {
    let trimmed = mvn_path.trim();
    if trimmed.is_empty() {
        // 空配置：用 PATH 上的 mvn，Windows 补 .cmd
        #[cfg(windows)]
        {
            return "mvn.cmd".to_string();
        }
        #[cfg(not(windows))]
        {
            return "mvn".to_string();
        }
    }

    let p = Path::new(trimmed);
    // 情况 2：配置的是目录（bin 目录），拼上 mvn 可执行文件
    if p.is_dir() {
        #[cfg(windows)]
        {
            return p.join("mvn.cmd").to_string_lossy().into_owned();
        }
        #[cfg(not(windows))]
        {
            return p.join("mvn").to_string_lossy().into_owned();
        }
    }

    // 情况 1：纯文件名无扩展名（如 "mvn"），Windows 补 .cmd
    #[cfg(windows)]
    {
        let has_dir = p
            .parent()
            .map(|d| !d.as_os_str().is_empty())
            .unwrap_or(false);
        let has_ext = p.extension().is_some();
        if !has_dir && !has_ext {
            return format!("{trimmed}.cmd");
        }
    }

    // 情况 3：已带路径/扩展名的完整路径，原样返回
    trimmed.to_string()
}

/// 解析 Maven 本地仓库路径（用于 -Dmaven.repo.local）。
///
/// 读取顺序（与 mvn 实际行为一致）：
/// 1. 用户级 `~/.m2/settings.xml` 的 `<localRepository>`
/// 2. 全局级 `<maven 安装目录>/conf/settings.xml` 的 `<localRepository>`
/// 3. 默认 `~/.m2/repository`
///
/// `mvn_path` 为用户在设置里配置的 Maven 路径（bin 目录 / 完整文件路径 / 纯命令名），
/// 仅用于推断 Maven 安装目录以读全局 settings.xml。
/// 返回 None 表示读不到任何有效配置，调用方就不传 -Dmaven.repo.local，交给 mvn 默认行为。
fn resolve_maven_repo_local(mvn_path: &str) -> Option<String> {
    // 1. 用户级 ~/.m2/settings.xml
    if let Some(home) = dirs::home_dir() {
        let user_settings = home.join(".m2").join("settings.xml");
        if let Some(p) = read_local_repository(&user_settings) {
            return Some(p);
        }
    }

    // 2. 全局级 <maven 安装目录>/conf/settings.xml
    if let Some(maven_home) = infer_maven_home(mvn_path) {
        let global_settings = maven_home.join("conf").join("settings.xml");
        if let Some(p) = read_local_repository(&global_settings) {
            return Some(p);
        }
    }

    // 3. 默认 ~/.m2/repository（只有目录确实存在时才返回，避免误导 mvn）
    if let Some(home) = dirs::home_dir() {
        let default_repo = home.join(".m2").join("repository");
        if default_repo.is_dir() {
            return Some(default_repo.to_string_lossy().replace('\\', "/"));
        }
    }
    None
}

/// 从 settings.xml 中提取 <localRepository>...</localRepository> 的内容。
/// 不引入完整 XML 解析器（省依赖），用字符串查找：
/// 跳过注释、定位 localRepository 标签、trim 内容、做路径变量替换。
/// 路径返回 None 表示配置不存在/为空/解析失败。
fn read_local_repository(settings_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(settings_path).ok()?;
    // 去掉注释，避免读到被注释掉的 <localRepository>
    let stripped = strip_xml_comments(&content);

    let open = "<localRepository>";
    let close = "</localRepository>";
    let start = stripped.find(open)?;
    let after = &stripped[start + open.len()..];
    let end = after.find(close)?;
    let raw = after[..end].trim();
    if raw.is_empty() {
        return None;
    }

    // 替换常见路径变量：${user.home}（settings.xml 里最常用）
    let replaced = if raw.contains("${user.home}") {
        if let Some(home) = dirs::home_dir() {
            raw.replace("${user.home}", &home.to_string_lossy())
        } else {
            raw.to_string()
        }
    } else {
        raw.to_string()
    };

    // 规范化为正斜杠，与 mvn.cmd 引号包裹时更稳妥
    Some(replaced.replace('\\', "/"))
}

/// 从用户配置的 mvn_path 反推 Maven 安装根目录。
/// 兼容三种配置形式：
/// - "D:/apache-maven/bin/mvn.cmd" → 取上两级（bin 的父目录）
/// - "D:/apache-maven/bin"          → 取上一级
/// - "mvn" / 空（PATH 查找）         → 无法推断，返回 None
///
/// 按路径形式（最后一段名）判断而非 is_file/is_dir：
/// 即使配置路径不存在也能推断出 home（仅用于拼 settings.xml 路径，读不到自然返回 None）。
fn infer_maven_home(mvn_path: &str) -> Option<PathBuf> {
    let trimmed = mvn_path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let p = Path::new(trimmed);
    let last = p.file_name()?.to_string_lossy().to_lowercase();
    let has_parent = p
        .parent()
        .map(|d| !d.as_os_str().is_empty())
        .unwrap_or(false);
    // 没有父目录：纯命令名（如 "mvn"），靠 PATH，无法推断安装目录
    if !has_parent {
        return None;
    }
    // 最后一段是 mvn 可执行文件（mvn / mvn.cmd / mvn.bat）：取上两级
    if last == "mvn" || last == "mvn.cmd" || last == "mvn.bat" {
        let bin = p.parent()?;
        return bin.parent().map(|h| h.to_path_buf());
    }
    // 否则当作目录处理：取上一级。配 bin 目录时这里返回 home。
    // （若用户配的是 Maven 根目录而非 bin，这里会多上一级，但 settings.xml 读不到，
    //  最终回退到默认 ~/.m2/repository，不会出错。）
    p.parent().map(|h| h.to_path_buf())
}

/// 判断一行日志是否为启动完成标志。
/// - 标准 Spring Boot：匹配 "Started <类名> in <秒> seconds"
/// - Nacos：匹配 "Nacos started successfully"（不走 Spring Boot 的 Started 句式）
///
/// 不用 regex，用 contains。
fn is_started_line(line: &str) -> bool {
    let spring_boot =
        line.contains("Started ") && line.contains(" in ") && line.contains("seconds");
    let nacos = line.contains("Nacos started successfully");
    spring_boot || nacos
}

/// 判断一行日志是否为启动失败标志。
///
/// 关键场景：mvn fork 出的子 JVM 在某些启动失败时**不退出**（如 Nacos 的 Raft
/// 初始化异常会让进程挂起，mvn 主进程因此一直阻塞等待子进程）。此时仅靠"子进程
/// 退出 + 端口宽限"永远检测不到失败，服务永远卡在 Starting。这里从日志里抓取
/// 明确的失败句式，快速判定崩溃。
///
/// 覆盖的句式：
/// - Spring Boot 标准失败横幅："APPLICATION FAILED TO START"
/// - Nacos StartingApplicationListener："Nacos failed to start"
/// - 通用 "failed to start"（小写兜底）
///
/// 不用 regex，用 contains（大小写不敏感）。
fn is_failed_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("application failed to start")
        || lower.contains("nacos failed to start")
        || lower.contains("failed to start")
}

/// 读 stdout，逐行转发；匹配到 "Started Xxx in" 则标记 Running
fn stream_stdout(app: &AppHandle, service_id: &str, pipe: Option<std::process::ChildStdout>) {
    let Some(out) = pipe else {
        return;
    };
    for line in BufReader::new(out).lines().map_while(Result::ok) {
        emit_log(app, service_id, &line, false);
        if is_started_line(&line) {
            mark_running(app, service_id);
        } else if is_failed_line(&line) {
            mark_failed(app, service_id);
        }
    }
}

/// 读 stderr，逐行转发（标记为错误）；部分启动完成行也可能走 stderr
fn stream_stderr(app: &AppHandle, service_id: &str, pipe: Option<std::process::ChildStderr>) {
    let Some(err) = pipe else {
        return;
    };
    for line in BufReader::new(err).lines().map_while(Result::ok) {
        emit_log(app, service_id, &line, true);
        if is_started_line(&line) {
            mark_running(app, service_id);
        } else if is_failed_line(&line) {
            mark_failed(app, service_id);
        }
    }
}

/// Tail 日志文件，逐行转发到前端（Windows Mvn 模式用）。
/// mvn.cmd 的 stdout/stderr 被 cmd 重定向到文件，这里轮询读取新行。
/// 等待 cmd 完成文件创建和截断后只打开一次，避免高频输出时反复 open/seek。
fn stream_log_file(app: &AppHandle, service_id: &str, log_file: &str) {
    use std::io::Read;

    let path = Path::new(log_file);
    if !wait_for_log_file(app, service_id, path) {
        return;
    }
    thread::sleep(LOG_FILE_READY_DELAY);
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => {
            emit_log(
                app,
                service_id,
                &format!(">>> 打开日志文件失败: {error}"),
                true,
            );
            return;
        }
    };
    let mut leftover: Vec<u8> = Vec::new();
    let mut chunk = [0u8; LOG_FILE_CHUNK_SIZE];
    loop {
        match file.read(&mut chunk) {
            Ok(0) => {
                if !service_process_alive(service_id) {
                    if !leftover.is_empty() {
                        let line = bytes_to_string(&leftover);
                        emit_and_check(app, service_id, &line);
                    }
                    return;
                }
                thread::sleep(LOG_FILE_POLL_INTERVAL);
            }
            Ok(n) => {
                consume_log_chunk(&mut leftover, &chunk[..n], |line| {
                    emit_and_check(app, service_id, line)
                });
            }
            Err(error) => {
                emit_log(
                    app,
                    service_id,
                    &format!(">>> 读取日志文件失败: {error}"),
                    true,
                );
                return;
            }
        }
    }
}

fn wait_for_log_file(app: &AppHandle, service_id: &str, path: &Path) -> bool {
    for _ in 0..LOG_FILE_WAIT_RETRIES {
        if path.exists() {
            return true;
        }
        thread::sleep(LOG_FILE_WAIT_INTERVAL);
    }
    emit_log(app, service_id, ">>> 日志文件未生成", true);
    false
}

fn service_process_alive(service_id: &str) -> bool {
    let mut guard = PROCESSES.lock().unwrap();
    guard
        .as_mut()
        .and_then(|processes| processes.get_mut(service_id))
        .map(|process| match process.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            // 收编进程没有句柄：留在进程表内即视为存活（移除条目后 tail 结束）
            None => true,
        })
        .unwrap_or(false)
}

fn consume_log_chunk(leftover: &mut Vec<u8>, chunk: &[u8], mut emit: impl FnMut(&str)) {
    leftover.extend_from_slice(chunk);
    let mut start = 0;
    while let Some(offset) = leftover[start..].iter().position(|&byte| byte == b'\n') {
        let end = start + offset;
        let line = bytes_to_string(&leftover[start..end]);
        emit(&line);
        start = end + 1;
    }
    leftover.drain(..start);
}

/// 把字节转成字符串（UTF-8 失败时用 lossy 转换，不丢日志）
fn bytes_to_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches('\r')
        .to_string()
}

/// 发送一行日志并检查是否为启动完成 / 启动失败标志
fn emit_and_check(app: &AppHandle, service_id: &str, line: &str) {
    let is_error = line.contains("[ERROR]") || line.contains("Exception");
    emit_log(app, service_id, line, is_error);
    if is_started_line(line) {
        mark_running(app, service_id);
    } else if is_failed_line(line) {
        mark_failed(app, service_id);
    }
}

/// 把某服务标记为 Running 并广播
fn mark_running(app: &AppHandle, service_id: &str) {
    let pid = {
        let mut guard = PROCESSES.lock().unwrap();
        if let Some(map) = guard.as_mut() {
            if let Some(sp) = map.get_mut(service_id) {
                sp.became_running = true;
                Some(sp.pid)
            } else {
                None
            }
        } else {
            None
        }
    };
    if let Some(pid) = pid {
        emit_status(app, service_id, ServiceStatus::Running, pid);
    }
}

/// 从日志确认启动失败后：置 failed 标志、杀掉可能挂起的进程树、广播 Crashed。
///
/// 关键：mvn fork 模式下子 JVM 失败时常常不退出（挂起），mvn 主进程也会一直阻塞
/// 等待子进程。若不主动杀进程树，会造成 java.exe 长期占用内存（实测 Nacos 一个挂起
/// 实例 ~530MB）。这里把 launcher 记录的 pid（cmd.exe/mvn）以及端口反查到的真实 JVM
/// pid 一并 taskkill /T /F 杀掉。
fn mark_failed(app: &AppHandle, service_id: &str) {
    let (pid, port, mvn_forked, main_class) = {
        let mut guard = PROCESSES.lock().unwrap();
        let Some(map) = guard.as_mut() else {
            return;
        };
        let Some(sp) = map.get_mut(service_id) else {
            return;
        };
        // 已标记过失败/已停止，不重复处理
        if sp.failed {
            return;
        }
        // 收编进程不做日志失败判定：重新 tail 会回放历史日志，
        // 其中可能包含旧的失败句式，而进程当前实际存活（收编即 Running）；
        // 退出检测由 monitor_adopted 巡检负责。
        if sp.adopted {
            return;
        }
        sp.failed = true;
        sp.became_running = true; // 阻止后续 monitor_exit/snapshot 误判为崩溃-未就绪路径再处理
        (sp.pid, sp.port, sp.mvn_forked, sp.main_class.clone())
    };

    // 杀进程树并按启动类补杀真实 JVM，避免远程端口配置时残留挂起进程。
    let pids = kill_process_tree(pid, port, &main_class);

    // 移出进程表，避免 monitor_exit 继续轮询一个已被杀的 child
    {
        let mut guard = PROCESSES.lock().unwrap();
        if let Some(map) = guard.as_mut() {
            map.remove(service_id);
        }
    }
    emit_status(app, service_id, ServiceStatus::Crashed, 0);
    emit_log(
        app,
        service_id,
        &format!(
            ">>> 启动失败：日志检测到失败标志，已终止挂起进程 (PID {}){}",
            pids.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join("/"),
            if mvn_forked { " [mvn fork]" } else { "" }
        ),
        true,
    );
}

/// 监控子进程退出，更新状态。
/// 特殊处理：Windows Mvn 模式下 launcher 直接 spawn 的是 cmd.exe，
/// `mvn spring-boot:run` 会 fork 出独立的子 JVM（如 Nacos 的 java 进程），
/// cmd.exe/mvn 主进程会在 fork 后退出，但真实服务仍在运行。
/// 此时不能用 child 退出判断服务终止，需改用端口探测判断真实 JVM 存活。
fn monitor_exit(app: AppHandle, service_id: &str) {
    loop {
        thread::sleep(Duration::from_millis(500));
        let action = {
            let mut guard = PROCESSES.lock().unwrap();
            let Some(map) = guard.as_mut() else {
                return;
            };
            let Some(sp) = map.get_mut(service_id) else {
                return;
            };
            match sp
                .child
                .as_mut()
                .map(|child| child.try_wait())
            {
                // 收编进程没有句柄（存活由 monitor_adopted 巡检），这里不做退出判定
                None => ExitAction::Continue,
                Some(Ok(Some(_))) => {
                    if sp.mvn_forked {
                        // fork 模式：cmd.exe 退出是预期行为，记录退出时间，
                        // 交给循环下半段用端口探测判断真实 JVM。
                        if sp.parent_exited_at == 0 {
                            sp.parent_exited_at = now_ms();
                        }
                        ExitAction::Continue
                    } else {
                        // jar 模式：child 就是真实服务进程，退出即终止
                        let crashed = !sp.became_running;
                        let pid = sp.pid;
                        ExitAction::Remove(pid, crashed)
                    }
                }
                Some(Ok(None)) => ExitAction::Continue,
                Some(Err(_)) => ExitAction::Remove(sp.pid, true),
            }
        };
        match action {
            ExitAction::Continue => {
                // fork 模式下，父进程已退出，轮询端口判断真实 JVM 存活；
                // 兼顾子 JVM 挂起不退出的场景（parent_exited_at 永不置位）。
                // verdict: Some((alive, pid, kill_tree))；kill_tree=true 表示子进程没退出、
                // 需主动 taskkill /T 杀进程树（静默挂起场景）。
                let verdict: Option<(bool, u32, bool)> = {
                    let mut guard = PROCESSES.lock().unwrap();
                    let Some(map) = guard.as_mut() else {
                        return;
                    };
                    let Some(sp) = map.get_mut(service_id) else {
                        return;
                    };
                    if !sp.mvn_forked {
                        // 非 fork 模式：正常循环
                        continue;
                    }
                    if sp.became_running {
                        // 已就绪：正常循环
                        continue;
                    }

                    let pid = sp.pid;
                    let port = sp.port;

                    // 1) 启动超时兜底（与父进程是否退出无关）：
                    //    自 started_at 起 START_TIMEOUT_MS 仍未就绪 → 静默挂起崩溃。
                    //    适配 mvn fork 子 JVM 启动失败但不退出的场景（如 Nacos Raft 异常挂起）。
                    let since_start = now_ms().saturating_sub(sp.started_at);
                    if since_start > START_TIMEOUT_MS {
                        sp.failed = true;
                        Some((false, pid, true))
                    } else if sp.parent_exited_at == 0 {
                        // 2) 父进程还没退出，也没超时：正常等待
                        None
                    } else {
                        // 3) 父进程已退出（正常 fork 模式），走端口宽限判定
                        let port_ok = port > 0 && port_listening(port);
                        let alive = port_ok || (port == 0 && sp.became_running);
                        if alive {
                            sp.became_running = true;
                            Some((true, pid, false))
                        } else {
                            let elapsed = now_ms().saturating_sub(sp.parent_exited_at);
                            if elapsed > PORT_GRACE_MS {
                                Some((false, pid, false))
                            } else {
                                None
                            }
                        }
                    }
                };
                if let Some((alive, pid, kill_tree)) = verdict {
                    if alive {
                        emit_status(&app, service_id, ServiceStatus::Running, pid);
                    } else {
                        // 静默挂起：子进程没退出但已超时未就绪，需主动杀进程树，避免内存泄漏
                        if kill_tree {
                            let port = port_for_service(service_id);
                            kill_process_tree(pid, port, "");
                        }
                        {
                            let mut guard = PROCESSES.lock().unwrap();
                            if let Some(map) = guard.as_mut() {
                                map.remove(service_id);
                            }
                        }
                        emit_status(&app, service_id, ServiceStatus::Crashed, 0);
                        emit_log(
                            &app,
                            service_id,
                            &format!(
                                ">>> 服务未正常启动 (launcher PID {pid}, 启动超时/端口探测失败){}",
                                if kill_tree {
                                    " [挂起已终止]"
                                } else {
                                    " [异常退出]"
                                }
                            ),
                            true,
                        );
                        return;
                    }
                }
                continue;
            }
            ExitAction::Remove(pid, crashed) => {
                {
                    let mut guard = PROCESSES.lock().unwrap();
                    if let Some(map) = guard.as_mut() {
                        map.remove(service_id);
                    }
                }
                let status = if crashed {
                    ServiceStatus::Crashed
                } else {
                    ServiceStatus::Stopped
                };
                emit_status(&app, service_id, status, 0);
                emit_log(
                    &app,
                    service_id,
                    &format!(
                        ">>> 进程已退出 (PID {pid}){}",
                        if crashed { " [异常退出]" } else { "" }
                    ),
                    crashed,
                );
                return;
            }
        }
    }
}

enum ExitAction {
    Continue,
    Remove(u32, bool),
}

/// 停止单个服务。Windows 用 taskkill /T /F 杀进程树。
pub fn stop(app: &AppHandle, service_id: &str) -> Result<(), String> {
    let (pid, port, main_class, adopted) = {
        let guard = PROCESSES.lock().unwrap();
        let Some(sp) = guard.as_ref().and_then(|m| m.get(service_id)) else {
            return Err(format!("{service_id} 未在运行"));
        };
        (sp.pid, sp.port, sp.main_class.clone(), sp.adopted)
    };

    emit_status(app, service_id, ServiceStatus::Stopping, pid);

    // 同时使用 launcher PID、监听端口和启动类定位真实 JVM，覆盖远程端口配置场景。
    let killed_pids = kill_process_tree(pid, port, &main_class);
    // 收编进程的 PID 就是真实 JVM，按 PID 等待退出；
    // 本 launcher spawn 的进程仍按启动类匹配等待（覆盖 mvn fork 场景）。
    let exited = if adopted {
        wait_for_pids_exit(&killed_pids)
    } else {
        wait_for_java_exit(&main_class)
    };
    if !exited {
        emit_status(app, service_id, ServiceStatus::Running, pid);
        return Err(if adopted {
            format!("停止 {service_id} 失败：进程 {pid} 仍在运行")
        } else {
            format!("停止 {service_id} 失败：仍检测到启动类 {main_class}")
        });
    }

    // 标记为已正常停止，避免 monitor_exit 误判为崩溃
    {
        let mut guard = PROCESSES.lock().unwrap();
        if let Some(map) = guard.as_mut() {
            if let Some(sp) = map.get_mut(service_id) {
                sp.became_running = true;
            }
            map.remove(service_id);
        }
    }
    emit_status(app, service_id, ServiceStatus::Stopped, 0);
    emit_log(
        app,
        service_id,
        &format!(
            ">>> 已停止 (PID {})",
            killed_pids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join("/")
        ),
        false,
    );
    Ok(())
}

/// 重启：先停再启
pub fn restart(app: &AppHandle, service_id: &str) -> Result<(), String> {
    stop(app, service_id)?;
    start(app, service_id, None)
}

/// 停止所有运行中的服务
pub fn stop_all(app: &AppHandle) -> Result<(), String> {
    let ids: Vec<String> = {
        let guard = PROCESSES.lock().unwrap();
        guard
            .as_ref()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    };
    let mut errs = Vec::new();
    for id in ids {
        if let Err(e) = stop(app, &id) {
            errs.push(format!("{id}: {e}"));
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs.join("; "))
    }
}

/* ---------- 收编后台存量服务 ---------- */

/// 收编匹配方式
#[cfg(windows)]
enum AdoptMatch {
    /// 命令行含启动类全限定名（mvn spring-boot:run fork 出的真实 JVM 形态）
    MainClass,
    /// 命令行含 jar 路径（java -jar 形态）
    Jar,
}

/// 判断一个运行中的 java 进程命令行是否属于该服务：
/// - MainClass：某个参数正好等于启动类全限定名（mvn fork 的真实 JVM）
/// - Jar：某个参数正好等于推算的 jar 路径（java -jar 模式；Windows 不区分大小写）
#[cfg(windows)]
fn java_proc_matches_service(proc: &JavaProc, svc: &crate::types::ServiceInfo) -> Option<AdoptMatch> {
    if java_process_matches(&proc.cmdline, &svc.main_class) {
        return Some(AdoptMatch::MainClass);
    }
    if svc.jar_path.is_empty() {
        return None;
    }
    let jar_norm = svc.jar_path.replace('\\', "/");
    let hit = proc.cmdline.split_whitespace().any(|arg| {
        let arg_norm = arg.trim_matches('"').replace('\\', "/");
        arg_norm.eq_ignore_ascii_case(&jar_norm)
    });
    if hit {
        Some(AdoptMatch::Jar)
    } else {
        None
    }
}

/// 读取某工作目录下某服务解析后的 JDK id（服务级 > 工作目录级 > 全局默认）。
/// 收编进程无法从进程反推 JDK，仅用于前端展示与重启时配置一致。
#[cfg(windows)]
fn resolve_jdk_id_for(workspace: &str, service_id: &str) -> String {
    let cfg = config::load();
    let svc_jdk = cfg
        .services
        .get(workspace)
        .and_then(|ws_svcs| ws_svcs.get(service_id))
        .map(|s| s.jdk_id.clone())
        .unwrap_or_default();
    if !svc_jdk.is_empty() {
        return svc_jdk;
    }
    if let Some(ws_jdk) = cfg.workspace_jdks.get(workspace) {
        if !ws_jdk.is_empty() {
            return ws_jdk.clone();
        }
    }
    cfg.default_jdk_id
}

/// 收编后台存量服务：把扫描出的服务与当前系统 java 进程表按启动类 / jar 路径匹配，
/// 命中的视为"上次会话遗留（或外部启动）且仍在运行"的进程，写入进程表并广播 Running。
/// 之后这些服务在本 launcher 内可正常停止 / 重启 / 查状态 / 统计内存。
///
/// 适配场景：用户直接关闭 launcher 窗口但未停服务，重开后服务状态不再显示"未启动"。
/// 已在进程表中的服务跳过（本会话启动中或已收编）。返回收编的服务 id 列表。
///
/// 收编进程的局限：无 child 句柄、stdout 已丢失。
/// - mvn 模式真实 JVM 仍持续写 `{logs_dir}/{service_id}.log`，重新 tail 恢复增量日志
/// - jar 模式无日志文件，仅恢复状态管理
pub fn adopt_running(
    app: &AppHandle,
    workspace: &str,
    services: &[crate::types::ServiceInfo],
) -> Vec<String> {
    #[cfg(not(windows))]
    {
        let _ = (app, workspace, services);
        return Vec::new();
    }
    #[cfg(windows)]
    {
        let java_procs = list_java_processes();
        if java_procs.is_empty() || services.is_empty() {
            return Vec::new();
        }
        // (service_id, pid, main_class, 可恢复 tail 的日志文件路径)
        let mut adopted: Vec<(String, u32, String, Option<String>)> = Vec::new();
        {
            let mut guard = PROCESSES.lock().unwrap();
            let map = guard.get_or_insert_with(HashMap::new);
            for svc in services {
                if map.contains_key(&svc.id) {
                    continue;
                }
                let Some((proc, kind)) = java_procs
                    .iter()
                    .find_map(|p| java_proc_matches_service(p, svc).map(|k| (p, k)))
                else {
                    continue;
                };
                map.insert(
                    svc.id.clone(),
                    ServiceProcess {
                        child: None,
                        pid: proc.pid,
                        started_at: proc.started_at_ms,
                        jdk_id: resolve_jdk_id_for(workspace, &svc.id),
                        main_class: svc.main_class.clone(),
                        // 收编即视为已就绪（进程是早已启动完成的存量 JVM）
                        became_running: true,
                        port: svc.port,
                        mvn_forked: false,
                        parent_exited_at: 0,
                        failed: false,
                        workspace: workspace.to_string(),
                        adopted: true,
                    },
                );
                // 仅 mvn 形态（按启动类命中）且日志文件存在时恢复 tail；
                // jar 形态命中时日志文件可能属于历史 mvn 会话，不回放
                let log_file = if matches!(kind, AdoptMatch::MainClass) {
                    let path = config::logs_dir()
                        .join(format!("{}.log", svc.id))
                        .to_string_lossy()
                        .replace('\\', "/");
                    if Path::new(&path).is_file() {
                        Some(path)
                    } else {
                        None
                    }
                } else {
                    None
                };
                adopted.push((svc.id.clone(), proc.pid, svc.main_class.clone(), log_file));
            }
        }
        let adopted_ids: Vec<String> = adopted.iter().map(|(sid, ..)| sid.clone()).collect();
        for (sid, pid, main_class, log_file) in adopted {
            emit_status(app, &sid, ServiceStatus::Running, pid);
            emit_log(
                app,
                &sid,
                &format!(
                    ">>> 识别到该服务已在后台运行 (PID {pid})，已接管状态管理；接管前的终端输出无法回溯{}",
                    if log_file.is_some() { "，正在重新读取日志文件" } else { "" }
                ),
                false,
            );
            if let Some(lf) = log_file {
                let app_for_file = app.clone();
                let sid_for_file = sid.clone();
                thread::spawn(move || stream_log_file(&app_for_file, &sid_for_file, &lf));
            }
            let app2 = app.clone();
            let sid2 = sid;
            thread::spawn(move || monitor_adopted(app2, sid2, pid, main_class));
        }
        adopted_ids
    }
}

/// 巡检收编进程存活：按 PID / 启动类反查 java 进程表，进程消失后
/// 移出进程表并广播 Stopped。
/// 查询返回空表（PowerShell 抖动或确无 java 进程）连续两次才判退出，
/// 避免单次查询失败误报。
#[cfg(windows)]
fn monitor_adopted(app: AppHandle, service_id: String, pid: u32, main_class: String) {
    let mut missed_queries = 0u32;
    loop {
        thread::sleep(ADOPTED_POLL_INTERVAL);
        let tracked = {
            let guard = PROCESSES.lock().unwrap();
            guard
                .as_ref()
                .and_then(|m| m.get(&service_id))
                .map(|sp| sp.adopted)
                .unwrap_or(false)
        };
        if !tracked {
            // 已被用户停止/重启时移出进程表，安静退出（状态由 stop() 广播）
            return;
        }
        let procs = list_java_processes();
        if procs.is_empty() {
            missed_queries += 1;
            if missed_queries >= 2 {
                break;
            }
            continue;
        }
        missed_queries = 0;
        let alive = procs.iter().any(|p| {
            p.pid == pid || (!main_class.is_empty() && java_process_matches(&p.cmdline, &main_class))
        });
        if alive {
            continue;
        }
        break;
    }
    {
        let mut guard = PROCESSES.lock().unwrap();
        if let Some(map) = guard.as_mut() {
            map.remove(&service_id);
        }
    }
    emit_status(&app, &service_id, ServiceStatus::Stopped, 0);
    emit_log(
        &app,
        &service_id,
        &format!(">>> 检测到后台进程已退出 (PID {pid})"),
        false,
    );
}

/// 等待指定 PID 全部从 java 进程表消失（收编进程停止校验：PID 即真实 JVM）。
fn wait_for_pids_exit(pids: &[u32]) -> bool {
    #[cfg(windows)]
    {
        if pids.is_empty() {
            return true;
        }
        for _ in 0..STOP_WAIT_RETRIES {
            let procs = list_java_processes();
            // 空表可能是查询失败：隔一个间隔再查一次确认，避免把查询抖动当成已退出
            if procs.is_empty() {
                thread::sleep(STOP_WAIT_INTERVAL);
                if list_java_processes().is_empty() {
                    return true;
                }
                continue;
            }
            if !procs.iter().any(|p| pids.contains(&p.pid)) {
                return true;
            }
            thread::sleep(STOP_WAIT_INTERVAL);
        }
        false
    }
    #[cfg(not(windows))]
    {
        let _ = pids;
        true
    }
}

/// 按分组顺序启动：组内并行，组间等待 delay_sec
pub fn start_group(app: &AppHandle, group_name: &str) -> Result<(), String> {
    let cfg = config::load();
    let group = cfg
        .groups
        .iter()
        .find(|g| g.name == group_name)
        .ok_or_else(|| format!("找不到分组 {group_name}"))?
        .clone();

    let app_clone = app.clone();
    thread::spawn(move || {
        for sid in &group.services {
            if let Err(e) = start(&app_clone, sid, None) {
                emit_start_failed(&app_clone, sid, &e);
                continue;
            }
            // 组内串行更稳妥（避免 mvn 抢资源），等就绪或固定延迟
            // 简化处理：固定 sleep 后再起下一个
        }
        // 组全部发起后，等待 delay_sec 再让调用方感知（分组间延迟由调用方处理）
        thread::sleep(Duration::from_secs(group.delay_sec));
    });
    Ok(())
}

/// 启动多个服务（一键启动）：按分组定义的顺序，组间延迟
pub fn start_all(app: &AppHandle, service_ids: &[String]) -> Result<(), String> {
    let cfg = config::load();
    let id_set: std::collections::HashSet<&String> = service_ids.iter().collect();

    // 按 groups 顺序排出启动序列
    let mut ordered: Vec<(String, u64)> = Vec::new();
    for g in &cfg.groups {
        for sid in &g.services {
            if id_set.contains(sid) {
                ordered.push((sid.clone(), g.delay_sec));
            }
        }
    }
    // 不在任何分组里的，追加到末尾
    for sid in service_ids {
        if !ordered.iter().any(|(id, _)| id == sid) {
            ordered.push((sid.clone(), 0));
        }
    }

    let app_clone = app.clone();
    thread::spawn(move || {
        for (sid, delay) in ordered {
            if let Err(e) = start(&app_clone, &sid, None) {
                emit_start_failed(&app_clone, &sid, &e);
            }
            // 服务间间隔，给上一个服务一点启动时间
            thread::sleep(Duration::from_secs(delay.max(5)));
        }
    });
    Ok(())
}

/// 启动失败时：写一条错误日志 + 广播 crashed 状态，让前端立刻可见
fn emit_start_failed(app: &AppHandle, service_id: &str, err: &str) {
    let msg = format!(">>> 启动失败: {err}");
    emit_log(app, service_id, &msg, true);
    emit_status(app, service_id, ServiceStatus::Crashed, 0);
}

/// 获取所有运行态快照
pub fn snapshot() -> Vec<crate::types::ServiceRuntime> {
    // 收编进程的存活反查结果（懒加载：存在收编条目才发起 PowerShell 查询）
    #[cfg(windows)]
    let mut java_procs: Option<Vec<JavaProc>> = None;
    let mut stale_ids: Vec<String> = Vec::new();
    let mut guard = PROCESSES.lock().unwrap();
    let Some(map) = guard.as_mut() else {
        return Vec::new();
    };
    let mut result = Vec::with_capacity(map.len());
    for (id, sp) in map.iter_mut() {
        if sp.adopted {
            // 收编进程：按 PID 反查 java 进程表确认真实 JVM 仍存活。
            // 查询失败（返回空表）时不判死，也不产出快照条目，
            // 避免把仍在运行的服务误报为停止；退出检测由 monitor_adopted 巡检兜底。
            #[cfg(windows)]
            {
                let procs = java_procs.get_or_insert_with(list_java_processes);
                let pid = sp.pid;
                let alive = !procs.is_empty() && procs.iter().any(|p| p.pid == pid);
                if alive {
                    result.push(crate::types::ServiceRuntime {
                        id: id.clone(),
                        status: ServiceStatus::Running,
                        pid: sp.pid,
                        started_at: sp.started_at,
                        jdk_id: sp.jdk_id.clone(),
                        workspace: sp.workspace.clone(),
                    });
                } else if !procs.is_empty() {
                    // 查询成功但 PID 已消失：清理收编记录
                    stale_ids.push(id.clone());
                }
            }
            #[cfg(not(windows))]
            {
                stale_ids.push(id.clone());
            }
            continue;
        }
        let child_alive = match sp.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => true,
        };
        // fork 模式下：cmd.exe 退出是预期行为，真实 JVM 存活与否看端口。
        // 非 fork 模式：child 就是真实进程，直接看 child。
        let status = if sp.mvn_forked {
            // 端口已知且在监听 → Running（最可靠）
            let port_ok = sp.port > 0 && port_listening(sp.port);
            // 端口未知（0，配在 Nacos 远端）且日志已匹配启动完成 → Running
            let alive = port_ok || (sp.port == 0 && sp.became_running);
            if alive {
                sp.became_running = true;
                ServiceStatus::Running
            } else if sp.failed {
                // 日志已确认启动失败（mark_failed 置位；通常随后被移除，这里兜底）
                ServiceStatus::Crashed
            } else if child_alive {
                // 父进程还活着，真实 JVM 还没就绪：判断是否已超启动超时上限。
                // 超时则判 Crashed（实际杀进程由 monitor_exit 负责），否则 Starting。
                let elapsed = now_ms().saturating_sub(sp.started_at);
                if elapsed > START_TIMEOUT_MS {
                    ServiceStatus::Crashed
                } else {
                    ServiceStatus::Starting
                }
            } else if sp.became_running && sp.port > 0 {
                // 之前端口在监听过，现在没了 → 已停止
                ServiceStatus::Stopped
            } else {
                // 父进程已退出且从未就绪
                let elapsed = now_ms().saturating_sub(sp.started_at);
                if elapsed > PORT_GRACE_MS {
                    ServiceStatus::Crashed
                } else {
                    ServiceStatus::Starting
                }
            }
        } else if !child_alive {
            if sp.became_running {
                ServiceStatus::Stopped
            } else {
                ServiceStatus::Crashed
            }
        } else if sp.became_running {
            ServiceStatus::Running
        } else {
            // 启动超过 60s 仍未匹配到 Started，也视作 running（兜底）
            let elapsed = now_ms().saturating_sub(sp.started_at);
            if elapsed > 60_000 {
                ServiceStatus::Running
            } else {
                ServiceStatus::Starting
            }
        };
        result.push(crate::types::ServiceRuntime {
            id: id.clone(),
            status,
            pid: sp.pid,
            started_at: sp.started_at,
            jdk_id: sp.jdk_id.clone(),
            workspace: sp.workspace.clone(),
        });
    }
    // 已消亡的收编条目移出进程表（状态广播由 monitor_adopted 负责）
    for id in &stale_ids {
        map.remove(id);
    }
    result
}

/* ---------- 内存占用统计 ---------- */

/// 一次性获取所有 java.exe 进程的 PID / 内存 / 命令行（Windows）。
/// 非 Windows 平台不支持，返回空（前端按 0 / "-" 处理）。
#[cfg(windows)]
struct JavaProc {
    pid: u32,
    memory_bytes: u64,
    /// 进程创建时间（UNIX 毫秒），查询失败为 0
    started_at_ms: u64,
    cmdline: String,
}

#[cfg(windows)]
fn list_java_processes() -> Vec<JavaProc> {
    let mut cmd = Command::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        // CreationDate 在 PowerShell 里已被 CIM 转成本地时间 DateTime（显示随区域设置变化，
        // 如 "09/16/2026 09:26:56"），这里直接在 PS 侧转 Unix 毫秒，避免 Rust 侧解析区域格式
        "Get-CimInstance Win32_Process -Filter \"Name='java.exe'\" | ForEach-Object { \"$($_.ProcessId)|$($_.WorkingSetSize)|$(([DateTimeOffset]$_.CreationDate).ToUnixTimeMilliseconds())|$($_.CommandLine)\" }",
    ]);
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = match cmd.output() {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut result = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // 每行格式：PID|WorkingSetSize(字节)|CreationTime(Unix 毫秒)|CommandLine
        let mut parts = line.splitn(4, '|');
        let pid: u32 = match parts.next().and_then(|s| s.trim().parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        let memory_bytes: u64 = parts
            .next()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let started_at_ms = parts
            .next()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let cmdline = parts.next().unwrap_or("").to_string();
        result.push(JavaProc {
            pid,
            memory_bytes,
            started_at_ms,
            cmdline,
        });
    }
    result
}

fn java_process_matches(cmdline: &str, main_class: &str) -> bool {
    !main_class.is_empty()
        && cmdline
            .split_whitespace()
            .any(|arg| arg.trim_matches('"') == main_class)
}

fn wait_for_java_exit(main_class: &str) -> bool {
    #[cfg(windows)]
    {
        for _ in 0..STOP_WAIT_RETRIES {
            let running = list_java_processes()
                .iter()
                .any(|process| java_process_matches(&process.cmdline, main_class));
            if !running {
                return true;
            }
            thread::sleep(STOP_WAIT_INTERVAL);
        }
        false
    }
    #[cfg(not(windows))]
    {
        let _ = main_class;
        true
    }
}

/// 获取所有运行中服务的内存占用快照。
/// - jar 模式：launcher 记录的 pid 就是 java 进程，直接查内存。
/// - mvn fork 模式：launcher 记录的 pid 是 cmd.exe，真实 JVM 是独立 java 进程，
///   按启动类全限定名匹配命令行定位。
pub fn memory_snapshot() -> Vec<crate::types::ServiceMemory> {
    #[cfg(windows)]
    {
        let java_procs = list_java_processes();
        let guard = PROCESSES.lock().unwrap();
        let Some(map) = guard.as_ref() else {
            return Vec::new();
        };
        map.iter()
            .map(|(id, sp)| {
                let memory_bytes = if sp.adopted {
                    // 收编进程：记录的 PID 就是真实 JVM，直接按 PID 定位
                    java_procs
                        .iter()
                        .find(|p| p.pid == sp.pid)
                        .map(|p| p.memory_bytes)
                        .unwrap_or(0)
                } else if sp.mvn_forked {
                    // mvn fork：按 main_class 匹配真实 JVM 命令行
                    if !sp.main_class.is_empty() {
                        java_procs
                            .iter()
                            .find(|p| p.cmdline.contains(&sp.main_class))
                            .map(|p| p.memory_bytes)
                            .unwrap_or(0)
                    } else {
                        0
                    }
                } else {
                    // jar 模式：pid 即 java 进程
                    java_procs
                        .iter()
                        .find(|p| p.pid == sp.pid)
                        .map(|p| p.memory_bytes)
                        .unwrap_or(0)
                };
                crate::types::ServiceMemory {
                    id: id.clone(),
                    memory_bytes,
                }
            })
            .collect()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// 简单的 shell 参数切分（按空格，本项目够用）
fn shell_split(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

/// 校验堆/元空间大小字符串：仅允许 数字 + 可选单位 k/K/m/M/g/G，如 "512m"、"1024"、"1g"。
/// 合法返回 true。防止 "-Xmx=xxx;恶意命令" 之类的注入。
fn validate_size(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let bytes = s.as_bytes();
    // 末尾若是合法单位，拆出数字部分和单位
    let digits = if matches!(
        bytes[bytes.len() - 1],
        b'k' | b'K' | b'm' | b'M' | b'g' | b'G'
    ) {
        &s[..s.len() - 1]
    } else {
        s
    };
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// 把结构化的 JVM 堆参数拼成命令行片段。
/// 如 xms="512m" xmx="1g" metaspace="256m" extra="-XX:+UseG1GC"
/// → "-Xms512m -Xmx1g -XX:MaxMetaspaceSize=256m -XX:+UseG1GC"
/// 非法值（不符合 validate_size）静默丢弃，extra 走 shell_split 后逐项透传。
fn build_jvm_args(xms: &str, xmx: &str, metaspace: &str, extra: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if validate_size(xms) {
        parts.push(format!("-Xms{xms}"));
    }
    if validate_size(xmx) {
        parts.push(format!("-Xmx{xmx}"));
    }
    if validate_size(metaspace) {
        parts.push(format!("-XX:MaxMetaspaceSize={metaspace}"));
    }
    // 其他 JVM 参数（如 -XX:+UseG1GC）按空白拆分逐项加入。
    // 仅接受单短横开头的真 JVM 参数（-X / -XX / -D），
    // 排除双短横开头的 Spring 应用参数（如 --server.port），避免误混入。
    for tok in shell_split(extra) {
        if tok.starts_with('-') && !tok.starts_with("--") {
            parts.push(tok);
        }
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_sequences_removes_colored_log_codes() {
        let line = "2026-07-13 | \x1b[34m INFO 53880\x1b[0;39m | \x1b[1;33mpool-13-thread-1\x1b[0;39m | 日志";
        assert_eq!(
            strip_ansi_sequences(line),
            "2026-07-13 |  INFO 53880 | pool-13-thread-1 | 日志"
        );
    }

    #[test]
    fn strip_ansi_sequences_preserves_plain_log() {
        let line = "2026-07-13 | INFO | [main] 服务启动完成";
        assert_eq!(strip_ansi_sequences(line), line);
    }

    #[test]
    fn java_process_matches_exact_main_class_argument() {
        let main_class = "cn.iocoder.keydom.server.KeydomServerApplication";
        let cmdline = format!("java.exe -cp app.jar {main_class}");
        assert!(java_process_matches(&cmdline, main_class));
        assert!(!java_process_matches(
            &cmdline,
            "cn.iocoder.keydom.server.KeydomServer"
        ));
    }

    #[test]
    fn consume_log_chunk_keeps_partial_line_until_completed() {
        let mut leftover = Vec::new();
        let mut lines = Vec::new();

        consume_log_chunk(&mut leftover, b"first\nsecond", |line| {
            lines.push(line.to_string())
        });
        assert_eq!(lines, vec!["first"]);
        assert_eq!(leftover, b"second");

        consume_log_chunk(&mut leftover, b" line\nthird\n", |line| {
            lines.push(line.to_string())
        });
        assert_eq!(lines, vec!["first", "second line", "third"]);
        assert!(leftover.is_empty());
    }

    #[test]
    fn is_failed_line_detects_spring_boot_banner() {
        // Spring Boot 标准失败横幅
        let line =
            "##########################\nAPPLICATION FAILED TO START\n##########################";
        assert!(is_failed_line(line));
    }

    #[test]
    fn is_failed_line_detects_nacos_listener() {
        // Nacos StartingApplicationListener（实际日志中观测到的句式）
        let line = "2026-07-15 22:01:02.271 ERROR 17080 --- [main] c.a.n.c.l.StartingApplicationListener : Nacos failed to start, please see ...";
        assert!(is_failed_line(line));
    }

    #[test]
    fn is_failed_line_case_insensitive() {
        assert!(is_failed_line("Bean failed to start due to exception"));
    }

    #[test]
    fn is_failed_line_ignores_normal_logs() {
        // 正常启动/运行日志不应误判为失败
        assert!(!is_failed_line("Started CsgsCustomerApp in 12.345 seconds"));
        assert!(!is_failed_line("Nacos started successfully"));
        assert!(!is_failed_line("Tomcat started on port(s): 8080"));
        assert!(!is_failed_line("INFO 初始化数据源"));
    }

    #[test]
    fn is_started_line_still_works() {
        // 回归：正常启动判定不受失败检测影响
        assert!(is_started_line("Started CsgsCustomerApp in 12.345 seconds"));
        assert!(is_started_line("Nacos started successfully"));
        assert!(!is_started_line("Nacos failed to start"));
    }

    #[test]
    fn read_local_repository_basic() {
        let xml = r#"<settings>
  <localRepository>D:/maven-repo</localRepository>
</settings>"#;
        let tmp = std::env::temp_dir().join("sl_test_settings_basic.xml");
        std::fs::write(&tmp, xml).unwrap();
        let got = read_local_repository(&tmp).unwrap();
        assert_eq!(got, "D:/maven-repo");
    }

    #[test]
    fn read_local_repository_user_home_var() {
        let xml = r#"<settings>
  <localRepository>${user.home}/.m2/myrepo</localRepository>
</settings>"#;
        let tmp = std::env::temp_dir().join("sl_test_settings_var.xml");
        std::fs::write(&tmp, xml).unwrap();
        let got = read_local_repository(&tmp).unwrap();
        assert!(got.ends_with("/.m2/myrepo"));
        assert!(!got.contains("${user.home}"));
    }

    #[test]
    fn read_local_repository_skips_commented_out() {
        // 被注释掉的 localRepository 应被忽略
        let xml = r#"<settings>
  <!-- <localRepository>SHOULD_NOT_MATCH</localRepository> -->
  <localRepository>E:/real-repo</localRepository>
</settings>"#;
        let tmp = std::env::temp_dir().join("sl_test_settings_comment.xml");
        std::fs::write(&tmp, xml).unwrap();
        let got = read_local_repository(&tmp).unwrap();
        assert_eq!(got, "E:/real-repo");
    }

    #[test]
    fn read_local_repository_backslash_normalized() {
        let xml = r#"<settings>
  <localRepository>D:\maven-repo</localRepository>
</settings>"#;
        let tmp = std::env::temp_dir().join("sl_test_settings_backslash.xml");
        std::fs::write(&tmp, xml).unwrap();
        let got = read_local_repository(&tmp).unwrap();
        assert_eq!(got, "D:/maven-repo");
    }

    #[test]
    fn read_local_repository_missing_returns_none() {
        let xml = "<settings></settings>";
        let tmp = std::env::temp_dir().join("sl_test_settings_missing.xml");
        std::fs::write(&tmp, xml).unwrap();
        assert!(read_local_repository(&tmp).is_none());
    }

    #[test]
    fn read_local_repository_trim_whitespace() {
        let xml =
            "<settings>\n  <localRepository>\n\t D:/repo  \n  </localRepository>\n</settings>";
        let tmp = std::env::temp_dir().join("sl_test_settings_trim.xml");
        std::fs::write(&tmp, xml).unwrap();
        let got = read_local_repository(&tmp).unwrap();
        assert_eq!(got, "D:/repo");
    }

    #[test]
    fn infer_maven_home_from_bin_dir() {
        // 配置成 bin 目录 → home 是其父目录
        let home = infer_maven_home("E:/JAVATOOLS/maven/apache-maven-3.8.1/bin");
        assert!(home.is_some());
        assert_eq!(
            home.unwrap().to_string_lossy().replace('\\', "/"),
            "E:/JAVATOOLS/maven/apache-maven-3.8.1"
        );
    }

    #[test]
    fn infer_maven_home_from_full_exe_path() {
        let home = infer_maven_home("E:/JAVATOOLS/maven/apache-maven-3.8.1/bin/mvn.cmd");
        assert!(home.is_some());
        assert_eq!(
            home.unwrap().to_string_lossy().replace('\\', "/"),
            "E:/JAVATOOLS/maven/apache-maven-3.8.1"
        );
    }

    #[test]
    fn infer_maven_home_from_bare_name_none() {
        assert!(infer_maven_home("mvn").is_none());
        assert!(infer_maven_home("").is_none());
    }

    #[test]
    fn path_eq_ignore_case_matches() {
        // 分隔符不同（Rust Path == 本身就处理）应相等
        assert!(path_eq_ignore_case(
            Path::new(r"E:\workspace\ggsg\server"),
            Path::new("E:/workspace/ggsg/server"),
        ));
        // Windows 大小写不同也应相等
        assert!(path_eq_ignore_case(
            Path::new(r"E:\workspace\GGSG\server"),
            Path::new(r"E:\workspace\ggsg\server"),
        ));
        // 不同路径不应相等
        assert!(!path_eq_ignore_case(
            Path::new(r"E:\workspace\ggsg\server"),
            Path::new(r"E:\workspace\server"),
        ));
    }

    #[test]
    fn is_aggregator_pom_detects_modules_block() {
        // 含 <modules> + <module> 子元素 → 聚合 pom
        let pom = r#"<project>
    <packaging>pom</packaging>
    <modules>
        <module>x-space-base</module>
        <module>x-space-module-system</module>
    </modules>
</project>"#;
        assert!(is_aggregator_pom(pom));
    }

    #[test]
    fn is_aggregator_pom_works_with_parent() {
        // 关键：聚合 pom 可以有 <parent>（x-space 根 pom 形态）。
        // 旧启发式（无 <parent>）会漏判，新逻辑（看 <modules>）应识别为聚合 pom。
        let pom = r#"<project>
    <parent>
        <groupId>org.springframework.boot</groupId>
        <artifactId>spring-boot-starter-parent</artifactId>
    </parent>
    <packaging>pom</packaging>
    <modules>
        <module>x-space-cloud-module</module>
    </modules>
</project>"#;
        assert!(is_aggregator_pom(pom), "有 <parent> 的聚合 pom 也应被识别");
    }

    #[test]
    fn is_aggregator_pom_ignores_commented_modules() {
        // 注释掉的 <modules> 不算（调用方已 strip_xml_comments，这里直接传已剥离文本）
        let pom = r#"<project>
    <packaging>pom</packaging>
    <modules>
        <module>x-space-base</module>
    </modules>
</project>"#;
        assert!(is_aggregator_pom(pom));
        // 无 <modules> 块的纯 pom（聚合器的子聚合声明在 profile 里）
        let profile_only = r#"<project>
    <packaging>pom</packaging>
    <profiles><profile><id>SpringCloud</id></profile></profiles>
</project>"#;
        assert!(!is_aggregator_pom(profile_only));
    }

    #[test]
    fn is_aggregator_pom_rejects_plain_jar_module() {
        // 普通可运行 jar 模块（无 <modules>）不是聚合 pom
        let pom = r#"<project>
    <artifactId>x-space-cloud-nacos</artifactId>
    <build><plugins><plugin><artifactId>spring-boot-maven-plugin</artifactId></plugin></plugins></build>
</project>"#;
        assert!(!is_aggregator_pom(pom));
    }

    #[test]
    fn find_reactor_root_picks_aggregator_with_parent() {
        // x-space 形态：根 pom 既有 <parent>（spring-boot-starter-parent）又有 <modules>。
        // 旧逻辑会因 <parent> 跳过根 pom 回退到 workspace；新逻辑应命中根 pom。
        let tmp = std::env::temp_dir().join("sl_test_reactor_with_parent");
        let _ = std::fs::remove_dir_all(&tmp);
        let root = tmp.join("backEndNew");
        let agg = root.join("x-space-cloud-module");
        let nacos = agg.join("x-space-cloud-nacos");
        std::fs::create_dir_all(&nacos).unwrap();
        // 根 pom：有 <parent> + <modules>（聚合）
        std::fs::write(
            root.join("pom.xml"),
            r#"<project>
    <parent>
        <groupId>org.springframework.boot</groupId>
        <artifactId>spring-boot-starter-parent</artifactId>
    </parent>
    <packaging>pom</packaging>
    <modules>
        <module>x-space-cloud-module</module>
    </modules>
</project>"#,
        )
        .unwrap();
        // 中间聚合 pom：也有 <parent> + <modules>
        std::fs::write(
            agg.join("pom.xml"),
            r#"<project>
    <parent>
        <groupId>com.keydom.bus</groupId>
        <artifactId>x-space-parent</artifactId>
    </parent>
    <packaging>pom</packaging>
    <modules>
        <module>x-space-cloud-nacos</module>
    </modules>
</project>"#,
        )
        .unwrap();

        let workspace = root.to_string_lossy().into_owned();
        let module = nacos.to_string_lossy().into_owned();
        // 从 nacos 向上：先命中中间聚合 pom（x-space-cloud-module），
        // 因为它是最靠近模块的 <modules> 聚合根。
        let result = find_reactor_root(&module, &workspace).unwrap();
        let result_norm = result.replace('\\', "/");
        assert!(
            result_norm.ends_with("x-space-cloud-module"),
            "应命中最近的聚合 pom（x-space-cloud-module），实际: {result_norm}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_reactor_root_case_insensitive_workspace_boundary() {
        // 构造真实临时目录结构：
        //   <tmp>/WS/PARENTROOT/pom.xml  (无 <parent>，会被误判为 reactor root)
        //   <tmp>/ws/server/submod/      (module 起点，无 pom)
        // workspace 配置成 <tmp>/WS/server（大小写与实际遍历路径 ws 不同）。
        // 若 find_reactor_root 大小写敏感，会越过 server 继续往上找到 PARENTROOT 的 pom；
        // 修正后应在 server 上界停下，返回 None。
        // 注意：Windows 文件系统本身不区分大小写，所以 WS 与 ws 指向同一目录，
        // 关键是验证不会越过 server 把 PARENTROOT 当成 reactor root。
        let tmp = std::env::temp_dir().join("sl_test_reactor_case");
        let _ = std::fs::remove_dir_all(&tmp);
        // 在 server 的上级放一个"无 parent 的 pom"，诱使遍历越过
        let parent_root = tmp.join("server_parent");
        std::fs::create_dir_all(&parent_root).unwrap();
        std::fs::write(parent_root.join("pom.xml"), "<project></project>").unwrap();
        // server 目录（reactor 根候选，故意不放无 parent 的 pom）
        let server = parent_root.join("server");
        std::fs::create_dir_all(server.join("submod")).unwrap();
        // workspace 用不同大小写（Windows 文件系统会指向同一目录）
        let workspace = parent_root.join("Server").to_string_lossy().into_owned();
        let module = server.join("submod").to_string_lossy().into_owned();

        let result = find_reactor_root(&module, &workspace);
        // 应在 server（=workspace）停下，不会越过到 server_parent
        // server 下没有 pom.xml，所以返回 None
        assert!(
            result.is_none(),
            "应在 workspace(server) 上界停下返回 None，不应越过到 server_parent；实际: {result:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn validate_size_accepts_numbers_with_units() {
        assert!(validate_size("512m"));
        assert!(validate_size("1g"));
        assert!(validate_size("384M"));
        assert!(validate_size("256K"));
        assert!(validate_size("1024")); // 纯数字（字节）合法
    }

    #[test]
    fn validate_size_rejects_injection_and_garbage() {
        // 防注入：含分号/空格/等号等一律拒绝
        assert!(!validate_size(""));
        assert!(!validate_size("512m;rm -rf /"));
        assert!(!validate_size("-Xmx512m"));
        assert!(!validate_size("abc"));
        assert!(!validate_size("512x")); // 非法单位
        assert!(!validate_size("m")); // 无数字
    }

    #[test]
    fn build_jvm_args_assembles_and_filters() {
        // 正常拼装：堆 + 元空间 + 其他参数
        assert_eq!(
            build_jvm_args("512m", "1g", "256m", "-XX:+UseG1GC"),
            "-Xms512m -Xmx1g -XX:MaxMetaspaceSize=256m -XX:+UseG1GC"
        );
        // 部分空：只输出有值的
        assert_eq!(build_jvm_args("", "768m", "", ""), "-Xmx768m");
        // 全空
        assert_eq!(build_jvm_args("", "", "", ""), "");
        // 非法值被静默丢弃（不进命令行）
        assert_eq!(build_jvm_args("bad", "768m", "", ""), "-Xmx768m");
    }

    #[test]
    fn build_jvm_args_drops_non_dash_extra() {
        // extra 里非 - 开头的 token（疑似应用参数）不混入 JVM 参数
        assert_eq!(
            build_jvm_args("", "512m", "", "-XX:+UseG1GC --server.port=8080"),
            "-Xmx512m -XX:+UseG1GC"
        );
    }

    #[test]
    #[cfg(windows)]
    fn java_proc_matches_service_by_main_class() {
        let svc = crate::types::ServiceInfo {
            id: "gateway".into(),
            artifact_id: "gateway".into(),
            module_path: "H:/proj/gateway".into(),
            main_class: "cn.net.cpgc.gateway.CipGatewayApp".into(),
            port: 9003,
            jar_path: "H:/proj/gateway/target/gateway.jar".into(),
        };
        let proc = JavaProc {
            pid: 1,
            memory_bytes: 0,
            started_at_ms: 0,
            cmdline:
                "java.exe -XX:TieredStopAtLevel=1 -cp @C:\\Temp\\spring-boot-1.argfile cn.net.cpgc.gateway.CipGatewayApp"
                    .into(),
        };
        assert!(matches!(
            java_proc_matches_service(&proc, &svc),
            Some(AdoptMatch::MainClass)
        ));
    }

    #[test]
    #[cfg(windows)]
    fn java_proc_matches_service_by_jar_path() {
        let svc = crate::types::ServiceInfo {
            id: "web".into(),
            artifact_id: "web".into(),
            module_path: "H:/proj/web".into(),
            main_class: "cn.net.cpgc.web.CipWebApp".into(),
            port: 9101,
            // 反斜杠 / 大小写混合也应命中
            jar_path: "h:/PROJ/web/target/web.jar".into(),
        };
        let proc = JavaProc {
            pid: 2,
            memory_bytes: 0,
            started_at_ms: 0,
            cmdline: "java.exe -Xmx512m -jar \"H:\\proj\\web\\target\\web.jar\" --server.port=9101".into(),
        };
        assert!(matches!(
            java_proc_matches_service(&proc, &svc),
            Some(AdoptMatch::Jar)
        ));
    }

    #[test]
    #[cfg(windows)]
    fn java_proc_matches_service_no_match_on_others() {
        let svc = crate::types::ServiceInfo {
            id: "web".into(),
            artifact_id: "web".into(),
            module_path: "H:/proj/web".into(),
            main_class: "cn.net.cpgc.web.CipWebApp".into(),
            port: 9101,
            jar_path: "H:/proj/web/target/web.jar".into(),
        };
        // 别的服务的 JVM（maven launcher），既不含启动类也不含 jar 路径
        let proc = JavaProc {
            pid: 3,
            memory_bytes: 0,
            started_at_ms: 0,
            cmdline: "java.exe -classpath D:/maven/boot/plexus-classworlds-2.6.0.jar org.codehaus.plexus.classworlds.launcher.Launcher".into(),
        };
        assert!(java_proc_matches_service(&proc, &svc).is_none());
    }
}
