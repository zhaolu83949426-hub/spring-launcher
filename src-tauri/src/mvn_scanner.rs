//! Maven 发现：扫描系统已安装的 Maven。
//! 来源：where mvn > MAVEN_HOME / M2_HOME > 常见安装目录 > IDEA 托管。
//! 返回的路径指向 Maven 的 bin 目录（含 mvn.cmd 的那一层）。

use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Windows 下隐藏子进程控制台窗口的 creation flag。
/// GUI 应用 spawn where.exe 会弹出新 cmd 窗口，必须显式抑制。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 扫描系统中所有 Maven 安装，返回 bin 目录路径列表（去重）。
/// 每个路径形如 `D:/apache-maven-3.6.3/bin`。
pub fn scan() -> Vec<String> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut add = |p: PathBuf, seen: &mut std::collections::HashSet<String>| {
        // 规范化去重
        let key = p
            .canonicalize()
            .unwrap_or_else(|_| p.clone())
            .to_string_lossy()
            .to_lowercase();
        if seen.insert(key) {
            paths.push(p);
        }
    };

    // 1. where mvn —— PATH 上的（最可靠，用户配过环境变量就能找到）
    //    where 返回的是 mvn.exe / mvn.cmd，取其所在 bin 目录。
    #[cfg(windows)]
    let mut where_cmd = std::process::Command::new("where");
    #[cfg(windows)]
    where_cmd.arg("mvn").creation_flags(CREATE_NO_WINDOW);
    #[cfg(not(windows))]
    let mut where_cmd = std::process::Command::new("where");
    #[cfg(not(windows))]
    where_cmd.arg("mvn");
    if let Ok(out) = where_cmd.output() {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                let exe = PathBuf::from(line.trim());
                if let Some(bin) = exe.parent() {
                    add(bin.to_path_buf(), &mut seen);
                }
            }
        }
    }

    // 2. MAVEN_HOME / M2_HOME 环境变量 —— 指向 Maven 根目录，需拼 bin
    for var in &["MAVEN_HOME", "M2_HOME"] {
        if let Ok(home) = std::env::var(var) {
            if !home.is_empty() {
                let bin = PathBuf::from(&home).join("bin");
                if bin.is_dir() {
                    add(bin, &mut seen);
                }
            }
        }
    }

    // 3. 常见安装目录
    let candidates: &[&str] = &[
        r"D:\java",
        r"D:\apache-maven",
        r"C:\apache-maven",
        r"C:\Program Files\apache-maven",
        r"C:\Program Files (x86)\apache-maven",
        r"D:\dev\apache-maven",
        r"D:\soft\apache-maven",
    ];
    for c in candidates {
        let base = Path::new(c);
        if !base.is_dir() {
            continue;
        }
        // 基目录本身就是 Maven 根（含 bin/mvn.cmd）
        if is_maven_bin_dir(&base.join("bin")) {
            add(base.join("bin"), &mut seen);
        }
        // 或者基目录下每个子目录都是 Maven 根（如 D:\java\apache-maven-3.6.3）
        if let Ok(entries) = std::fs::read_dir(base) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() && is_maven_bin_dir(&path.join("bin")) {
                    add(path.join("bin"), &mut seen);
                }
            }
        }
    }

    // 4. IDEA 托管 Maven：%APPDATA%\JetBrains\*\plugins\maven\lib\maven3
    //    （bundled Maven，较少直接用，这里做兜底）
    if let Some(config) = dirs::config_dir() {
        let jetbrains = config.join("JetBrains");
        if jetbrains.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&jetbrains) {
                for entry in entries.flatten() {
                    // 每个 IDE 版本目录
                    let maven3 = entry
                        .path()
                        .join("plugins")
                        .join("maven")
                        .join("lib")
                        .join("maven3");
                    if maven3.is_dir() && is_maven_bin_dir(&maven3.join("bin")) {
                        add(maven3.join("bin"), &mut seen);
                    }
                }
            }
        }
    }

    paths
        .into_iter()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect()
}

/// 判断某目录是否为 Maven 的 bin 目录（含 mvn.cmd 或 mvn）
fn is_maven_bin_dir(bin: &Path) -> bool {
    bin.join("mvn.cmd").is_file() || bin.join("mvn").is_file()
}
