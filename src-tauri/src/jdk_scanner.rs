//! JDK 发现：扫描系统已安装的 JDK。
//! 来源优先级：JAVA_HOME > IDEA 托管 .jdks > 常见安装目录 > where java。

use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use uuid::Uuid;
use walkdir::WalkDir;

use crate::types::JdkInfo;

/// Windows 下隐藏子进程控制台窗口的 creation flag。
/// GUI 应用没有控制台，spawn 子进程会弹出新 cmd 窗口，必须显式抑制。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 扫描系统已安装的所有 JDK。返回去重后的列表。
pub fn scan() -> Vec<JdkInfo> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut add = |p: PathBuf, seen: &mut std::collections::HashSet<String>| {
        // 规范化路径去重（canonicalize 失败就用原路径）
        let key = p
            .canonicalize()
            .unwrap_or_else(|_| p.clone())
            .to_string_lossy()
            .to_lowercase();
        if seen.insert(key) {
            paths.push(p);
        }
    };

    // 1. JAVA_HOME
    if let Ok(jh) = std::env::var("JAVA_HOME") {
        add(PathBuf::from(&jh), &mut seen);
    }

    // 2. IDEA 托管的 JDK：%USERPROFILE%\.jdks\*
    if let Some(home) = dirs::home_dir() {
        let jdks_dir = home.join(".jdks");
        if jdks_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&jdks_dir) {
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        add(entry.path(), &mut seen);
                    }
                }
            }
        }
    }

    // 3. 常见安装目录（Program Files / Eclipse Adoptium / Microsoft / Zulu / Corretto）
    let candidates: &[&str] = &[
        r"C:\Program Files\Java",
        r"C:\Program Files\Eclipse Adoptium",
        r"C:\Program Files\Microsoft",
        r"C:\Program Files\Zulu",
        r"C:\Program Files\Amazon Corretto",
        r"C:\Program Files (x86)\Java",
    ];
    for c in candidates {
        let base = Path::new(c);
        if base.is_dir() {
            if let Ok(entries) = std::fs::read_dir(base) {
                for entry in entries.flatten() {
                    // 直接子目录
                    if entry.path().is_dir() {
                        add(entry.path(), &mut seen);
                    }
                }
            }
        }
    }

    // 4. where java（PATH 上的）—— 向上找 JDK 根
    #[cfg(windows)]
    let mut where_cmd = Command::new("where");
    #[cfg(windows)]
    where_cmd.arg("java").creation_flags(CREATE_NO_WINDOW);
    #[cfg(not(windows))]
    let mut where_cmd = Command::new("where");
    #[cfg(not(windows))]
    where_cmd.arg("java");
    if let Ok(out) = where_cmd.output() {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                // java.exe 通常在 <jdk>/bin/java.exe，取上两级
                let exe = PathBuf::from(line.trim());
                if let Some(bin) = exe.parent() {
                    if let Some(root) = bin.parent() {
                        add(root.to_path_buf(), &mut seen);
                    }
                }
            }
        }
    }

    // 对每个候选路径跑 java -version 解析版本
    let mut result: Vec<JdkInfo> = Vec::new();
    for p in paths {
        if let Some(info) = probe_jdk(&p) {
            result.push(info);
        }
    }
    result
}

/// 给定一个 JDK 根目录，验证 bin/java 存在并解析版本号/厂商。
/// 返回 None 表示这不是有效 JDK。
pub fn probe_jdk(root: &Path) -> Option<JdkInfo> {
    let java_bin = root.join("bin").join("java.exe");
    if !java_bin.is_file() {
        return None;
    }

    let mut cmd = Command::new(&java_bin);
    cmd.arg("-version");
    // 隐藏控制台窗口：java.exe 是控制台程序，GUI 应用 spawn 会弹窗
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd.output().ok()?;
    // java -version 输出在 stderr（历史原因）
    let text = String::from_utf8_lossy(&out.stderr);
    let (version, vendor) = parse_version_output(&text);

    // 尝试从路径/输出推算厂商
    let vendor = if vendor.is_empty() {
        guess_vendor(root, &text)
    } else {
        vendor
    };

    Some(JdkInfo {
        id: Uuid::new_v4().to_string(),
        path: root.to_string_lossy().to_string(),
        version,
        vendor,
        manual: false,
    })
}

/// 解析 `java -version` 输出，返回 (版本号, 厂商)。
/// 不用 regex（regex-syntax 0.8.11 在 Rust 1.96 下编译失败），
/// 用简单字符串查找：找 `version "X.Y.Z"` 的两个引号之间内容。
fn parse_version_output(text: &str) -> (String, String) {
    // 找 `version "xxx"` —— 定位 version 关键字后第一个引号对
    let version = extract_quoted_after(text, "version ").unwrap_or_default();

    let vendor = if text.contains("Temurin") {
        "Temurin"
    } else if text.contains("Microsoft") {
        "Microsoft"
    } else if text.contains("Zulu") {
        "Zulu"
    } else if text.contains("Corretto") {
        "Corretto"
    } else if text.contains("Oracle") {
        "Oracle"
    } else {
        "Unknown"
    };

    (version, vendor.to_string())
}

/// 在 text 中找 `prefix` 之后的第一对双引号内容。
/// 如 extract_quoted_after("openjdk version \"17.0.9\"", "version ") -> Some("17.0.9")
fn extract_quoted_after(text: &str, prefix: &str) -> Option<String> {
    let idx = text.find(prefix)?;
    let rest = &text[idx + prefix.len()..];
    let start = rest.find('"')? + 1;
    let rest = &rest[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// 从目录名或输出文本兜底猜厂商
fn guess_vendor(root: &Path, text: &str) -> String {
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if name.contains("temurin") || text.contains("Temurin") {
        "Temurin".into()
    } else if name.contains("microsoft") || text.contains("Microsoft") {
        "Microsoft".into()
    } else if name.contains("zulu") || text.contains("Zulu") {
        "Zulu".into()
    } else if name.contains("corretto") || text.contains("Corretto") {
        "Corretto".into()
    } else if name.contains("jdk") && text.contains("Oracle") {
        "Oracle".into()
    } else {
        "Unknown".into()
    }
}

/// 在某目录下递归查找名为 `bin` 的子目录，取其父作为 JDK 根候选。
/// （处理用户选了 SDK 嵌套目录的情况）
pub fn find_jdk_root(start: &Path) -> Option<PathBuf> {
    // 如果 start 本身就是 JDK 根（含 bin/java）
    if start.join("bin").join("java.exe").is_file() {
        return Some(start.to_path_buf());
    }
    // 向下找一层 jdk* 目录
    for entry in WalkDir::new(start).max_depth(2).into_iter().flatten() {
        if entry.file_type().is_dir() && entry.path().join("bin").join("java.exe").is_file() {
            return Some(entry.path().to_path_buf());
        }
    }
    None
}
