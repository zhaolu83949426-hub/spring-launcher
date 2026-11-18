//! 服务发现：扫描 Maven 多模块项目，识别可运行的 Spring Boot 服务。
//!
//! 判定规则（基于 pom.xml 结构化语义，用 quick-xml 解析）：
//! 1. packaging ≠ pom（未声明时默认 jar）
//! 2. 在 `<build><plugins>` 上下文（**排除** `<pluginManagement>`）下声明了
//!    spring-boot-maven-plugin
//!
//! 不要求显式 `<goal>repackage</goal>`——继承 spring-boot-starter-parent 时，
//! 子模块即使只写一句裸 `<plugin>spring-boot-maven-plugin</plugin>` 也会继承
//! 默认 repackage execution。这覆盖了 csgs-backend（显式 repackage）和
//! x-space（裸 plugin）两种写法。
//!
//! 适配本项目实际情况：启动类命名 *App.java（非 *Application.java），
//! 端口多在 Nacos 远端配置，本地 yml 读不到时返回 0。

use std::path::{Path, PathBuf};

use quick_xml::events::Event;
use quick_xml::Reader;
use walkdir::WalkDir;

use crate::types::ServiceInfo;

/// 扫描工作目录下所有可运行服务。
pub fn scan(workspace: &str) -> Vec<ServiceInfo> {
    let root = Path::new(workspace);
    if !root.is_dir() {
        return Vec::new();
    }

    let mut services = Vec::new();

    // 遍历所有 pom.xml（限制深度避免扫到 node_modules 之类）
    // filter_entry 在递归进入前就跳过干扰目录，不进入 target/bin/node_modules 等
    for entry in WalkDir::new(root)
        .max_depth(6)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !matches!(
                name.as_ref(),
                "target" | "bin" | "node_modules" | ".idea" | ".git" | "dist" | "build"
            )
        })
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()) != Some("pom.xml") {
            continue;
        }

        if let Some(info) = analyze_pom(path) {
            services.push(info);
        }
    }

    // 按 artifact_id 排序，输出稳定
    services.sort_by(|a, b| a.artifact_id.cmp(&b.artifact_id));
    services
}

/// 分析单个 pom.xml，如果是可运行服务则返回 ServiceInfo。
fn analyze_pom(pom_path: &Path) -> Option<ServiceInfo> {
    let content = std::fs::read_to_string(pom_path).ok()?;
    let pom = parse_pom(&content);

    // 跳过聚合 POM（packaging=pom）
    if pom.packaging.as_deref() == Some("pom") {
        return None;
    }
    // 必须在 <build><plugins>（非 pluginManagement）下声明 spring-boot-maven-plugin
    if !pom.has_active_boot_plugin {
        return None;
    }

    let artifact_id = pom.artifact_id?;
    let module_path = pom_path.parent()?.to_path_buf();
    let main_class = find_main_class(&module_path).unwrap_or_default();
    let port = read_port(&module_path);
    // 智能定位 repackage 产出的可执行 fat jar：优先无版本号旧式命名，
    // 其次在 target 下找 {artifactId}-{version}.jar（排除 .original / -sources 等），
    // 取最新修改时间者；都找不到则回退到无版本号路径（保持旧行为，启动时报清晰错误）。
    let jar_path = resolve_jar_path(&module_path, &artifact_id);

    Some(ServiceInfo {
        id: artifact_id.clone(),
        artifact_id,
        module_path: module_path.to_string_lossy().to_string(),
        main_class,
        port,
        jar_path,
    })
}

/// 从 pom.xml 解析出的结构化信息。
struct PomInfo {
    /// `<packaging>` 值；未声明时为 None（Maven 默认 jar）
    packaging: Option<String>,
    /// 模块自身的 `<artifactId>`（已跳过 `<parent>` 块内的）
    artifact_id: Option<String>,
    /// 是否在 `<build><plugins>`（非 pluginManagement）下声明了 spring-boot-maven-plugin
    has_active_boot_plugin: bool,
}

/// 一次遍历 Event 流，结构化提取 pom 关键信息。
///
/// 用栈跟踪当前所在路径（仅记录语义关键节点：project / parent / build /
/// pluginManagement / plugins / plugin / artifactId / packaging）。
/// 仅在 project 直接子层取 packaging / artifactId（跳过 parent 块内的），
/// 仅在 build>plugins（排除 pluginManagement）路径下匹配 plugin 的 artifactId。
fn parse_pom(content: &str) -> PomInfo {
    let mut reader = Reader::from_str(content);
    reader.config_mut().trim_text(true);

    let mut info = PomInfo {
        packaging: None,
        artifact_id: None,
        has_active_boot_plugin: false,
    };

    // 语义路径栈：压入关键节点名，用于判断当前位置
    let mut path: Vec<&'static str> = Vec::new();
    // 是否正处于 <parent> 块内
    let mut in_parent = false;
    // 当进入 <plugin> 块时，暂存其下读到的 artifactId 文本，离开时判定
    let mut in_plugin = false;
    let mut plugin_artifact: Option<String> = None;
    // 当前 plugin 是否处于 build>plugins（非 pluginManagement）路径下
    let mut plugin_is_active = false;

    // 辅助：判断当前栈是否处于 build>plugins（非 pluginManagement）下
    let active_plugins_path = |path: &Vec<&'static str>| -> bool {
        // path 形如 [.., "build", "plugins"] 或 ["project", "build", "plugins"]
        // 排除 ["project", "build", "pluginManagement", "plugins"] 等
        let has_build = path.iter().any(|&s| s == "build");
        let has_plugins = path.iter().any(|&s| s == "plugins");
        let has_pm = path.iter().any(|&s| s == "pluginManagement");
        has_build && has_plugins && !has_pm
    };

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = tag_name(&e.name().as_ref());
                match name {
                    "parent" => in_parent = true,
                    "plugin" => {
                        in_plugin = true;
                        plugin_artifact = None;
                        plugin_is_active = active_plugins_path(&path);
                    }
                    "artifactId" | "packaging" => {
                        // project 直接子层（不在 parent/plugin 等嵌套块里）才记录模块级字段
                        // 这里仅压栈，文本在 Event::Text/E::Empty 后处理
                    }
                    _ => {}
                }
                push_path(&mut path, name);
            }
            Ok(Event::End(e)) => {
                let name = tag_name(&e.name().as_ref());
                match name {
                    "parent" => in_parent = false,
                    "plugin" => {
                        if in_plugin {
                            if plugin_is_active
                                && plugin_artifact.as_deref() == Some("spring-boot-maven-plugin")
                            {
                                info.has_active_boot_plugin = true;
                            }
                            in_plugin = false;
                            plugin_artifact = None;
                            plugin_is_active = false;
                        }
                    }
                    _ => {}
                }
                pop_path(&mut path, name);
            }
            Ok(Event::Empty(e)) => {
                // 自闭合标签：本项目 pom 里关键元素（packaging/artifactId/plugin）
                // 都不会自闭合，这里不处理。保留分支仅为穷尽 Event 变体。
                let _ = tag_name(&e.name().as_ref());
            }
            Ok(Event::Text(e)) => {
                // pom 的文本节点基本不会有 XML 转义字符（artifactId/packaging 都是
                // 普通标识符），直接按 UTF-8 解原始字节即可，避免引入 unescape 依赖。
                let bytes = e.into_inner();
                let text = std::str::from_utf8(&bytes).unwrap_or("").trim();
                if text.is_empty() {
                    continue;
                }
                let top = path.last().copied();
                match top {
                    Some("packaging") if !in_parent => {
                        if info.packaging.is_none() {
                            info.packaging = Some(text.to_string());
                        }
                    }
                    Some("artifactId") if !in_parent && !in_plugin => {
                        // project 直接子层的 artifactId（模块自身）
                        if info.artifact_id.is_none() {
                            info.artifact_id = Some(text.to_string());
                        }
                    }
                    Some("artifactId") if in_plugin => {
                        // plugin 块内的 artifactId
                        if plugin_artifact.is_none() {
                            plugin_artifact = Some(text.to_string());
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::CData(e)) => {
                // pom 几乎不用 CDATA，保留分支仅为穷尽 Event 变体。
                let _ = std::str::from_utf8(&e.into_inner()).unwrap_or("");
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        buf.clear();
    }

    info
}

/// 把 QName 字节转成静态标签名（仅识别语义关键标签，其余归一为 ""）。
/// 这样 path 栈里只存少数关键节点，判断简单。
fn tag_name(bytes: &[u8]) -> &'static str {
    match std::str::from_utf8(bytes).unwrap_or("") {
        "project" => "project",
        "parent" => "parent",
        "artifactId" => "artifactId",
        "packaging" => "packaging",
        "build" => "build",
        "pluginManagement" => "pluginManagement",
        "plugins" => "plugins",
        "plugin" => "plugin",
        _ => "",
    }
}

/// 压栈：仅记录语义关键节点，非关键节点压 "" 占位（保持栈深度语义）。
fn push_path(path: &mut Vec<&'static str>, name: &'static str) {
    path.push(name);
}

/// 弹栈：匹配栈顶同名节点弹出（容错：若栈顶不是同名，弹栈顶）。
fn pop_path(path: &mut Vec<&'static str>, name: &'static str) {
    if path.last() == Some(&name) {
        path.pop();
    } else if !path.is_empty() {
        path.pop();
    }
}

/// 在模块的 src/main/java 下查找启动类。
/// 优先匹配 *App.java（本项目命名规律），兜底找 @SpringBootApplication。
fn find_main_class(module_path: &Path) -> Option<String> {
    let java_root = module_path.join("src").join("main").join("java");
    if !java_root.is_dir() {
        return None;
    }

    let mut app_candidate: Option<PathBuf> = None;
    let mut spring_candidate: Option<PathBuf> = None;

    for entry in WalkDir::new(&java_root).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("java") {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        // 优先：文件名以 App.java 结尾（如 CsgsCustomerApp.java、CipWebApp.java）
        if name.ends_with("App.java") {
            app_candidate = Some(path.to_path_buf());
            break; // 命中即停
        }
        // 兜底：含 @SpringBootApplication 注解
        if spring_candidate.is_none() {
            if let Ok(content) = std::fs::read_to_string(path) {
                if content.contains("@SpringBootApplication") {
                    spring_candidate = Some(path.to_path_buf());
                }
            }
        }
    }

    let chosen = app_candidate.or(spring_candidate)?;
    path_to_fqcn(&chosen, &java_root)
}

/// 把 java 文件路径转成全限定类名。
/// 如 .../cn/net/cpgc/customer/CsgsCustomerApp.java -> cn.net.cpgc.customer.CsgsCustomerApp
fn path_to_fqcn(file: &Path, java_root: &Path) -> Option<String> {
    let rel = file.strip_prefix(java_root).ok()?;
    let rel = rel.with_extension(""); // 去掉 .java
    let mut parts: Vec<String> = Vec::new();
    for comp in rel.components() {
        parts.push(comp.as_os_str().to_string_lossy().to_string());
    }
    Some(parts.join("."))
}

/// 读 application.yml / application.properties 里的 server.port。
/// 读不到（本项目多数在 Nacos 远端）返回 0。
fn read_port(module_path: &Path) -> u16 {
    let resources = module_path.join("src").join("main").join("resources");
    // 先试 properties（nacos-app 用的是 properties 且写死 8848）
    if let Some(p) = try_read_port_properties(&resources) {
        return p;
    }
    // 再试 yml
    for name in &["application.yml", "application.yaml"] {
        let f = resources.join(name);
        if let Ok(content) = std::fs::read_to_string(&f) {
            if let Some(p) = parse_port_from_yml(&content) {
                return p;
            }
        }
    }
    0
}

fn try_read_port_properties(resources: &Path) -> Option<u16> {
    let f = resources.join("application.properties");
    let content = std::fs::read_to_string(&f).ok()?;
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("server.port") {
            // server.port=8848 或 server.port: 8848
            let rest = rest.trim_start_matches(['=', ':', ' ']);
            if let Ok(p) = rest.trim().parse::<u16>() {
                return Some(p);
            }
        }
    }
    None
}

/// 简单从 yml 文本里找 server: 下的 port: 值。
/// 不引入完整 yaml 解析器（省依赖），用缩进启发式。
fn parse_port_from_yml(content: &str) -> Option<u16> {
    let mut under_server = false;
    let mut server_indent = 0usize;
    for raw in content.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim();

        // 退出 server 块：遇到同级或更高级别的键
        if under_server && indent <= server_indent && !trimmed.starts_with('-') {
            under_server = false;
        }

        if trimmed == "server:" || trimmed.starts_with("server:") {
            under_server = true;
            server_indent = indent;
            // 处理 server: 8848 这种少见写法（一般 port 才会）
            continue;
        }

        if under_server && indent > server_indent {
            // port: 8848
            if let Some(rest) = trimmed.strip_prefix("port:") {
                let val = rest.trim();
                // 跳过占位符 ${...}
                if val.starts_with('$') {
                    return None;
                }
                if let Ok(p) = val.parse::<u16>() {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// 定位模块的可执行 fat jar 路径。
///
/// Maven repackage 默认产出 `{artifactId}-{version}.jar`（fat jar）和
/// `{artifactId}-{version}.jar.original`（repackage 前的 thin jar）。旧代码硬拼
/// `{artifactId}.jar`，在版本化产出的项目（如 x-space）上永远找不到真实 jar。
///
/// 查找优先级（命中即返回）：
/// 1. `{module}/target/{artifactId}.jar`（无版本号，向后兼容旧式/手动构造，最优先）
/// 2. `{module}/target/{artifactId}-{version}.jar`：扫描 target，排除 `.original` 后缀
///    与 `-sources`/`-javadoc`/`-tests` 等分类器 jar，多个候选取修改时间最新者
/// 3. 兜底：回退到无版本号路径（保持旧行为；启动时 java -jar 会给出明确的找不到文件错误）
///
/// fat jar 判定采用启发式排除（不解析 zip 内部 manifest）：标准 `mvn package` +
/// spring-boot-maven-plugin 场景下，排除 .original 与分类器后，剩余的就是 fat jar。
/// 极端情况（用户手动放了不可执行 jar）由 `java -jar` 报错兜底。
fn resolve_jar_path(module_path: &Path, artifact_id: &str) -> String {
    let target = module_path.join("target");

    // 规则 1：无版本号精确匹配（向后兼容，最优先）
    let plain = target.join(format!("{artifact_id}.jar"));
    if plain.is_file() {
        return plain.to_string_lossy().into_owned();
    }

    // 规则 2：扫描 target 找带版本号的 fat jar
    if let Some(found) = find_versioned_fat_jar(&target, artifact_id) {
        return found;
    }

    // 规则 3：兜底回退到无版本号路径（即便文件不存在，也返回这个语义路径，
    // java -jar 报错信息清晰可辨）
    plain.to_string_lossy().into_owned()
}

/// 在 target 目录下扫描 `{artifactId}-*.jar`，挑选最可能是 fat jar 的一个。
///
/// 过滤规则：
/// - 文件名必须以 `{artifactId}-` 开头、`.jar` 结尾
/// - 排除 `.original` 后缀（repackage 前的 thin jar）
/// - 排除分类器产物：`-sources.jar` / `-javadoc.jar` / `-tests.jar` 等
///   （位于 `.jar` 之前、形如 `{artifactId}-{version}-{classifier}.jar`）
///
/// 多个候选按修改时间降序，取最新一个。
/// 返回绝对路径字符串；无候选返回 None。
fn find_versioned_fat_jar(target: &Path, artifact_id: &str) -> Option<String> {
    let entries = std::fs::read_dir(target).ok()?;
    let prefix = format!("{artifact_id}-");
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        // 只看文件（跳过子目录，如 classes/、generated-sources/）
        if !path.is_file() {
            continue;
        }
        let Some(fname) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !fname.starts_with(&prefix) || !fname.ends_with(".jar") {
            continue;
        }
        // 排除 .original（虽然它不以 .jar 结尾，但保险起见再判一次）
        if fname.ends_with(".original") {
            continue;
        }
        // 排除分类器 jar：去掉 `.jar` 后缀后的"版本段"不能含分类器。
        // 形如 {artifactId}-{version}-{classifier}.jar → 去前缀和 .jar 后是 "{version}-{classifier}"
        let stem = &fname[prefix.len()..fname.len() - 4]; // 去掉 .jar
        if is_classifier_jar(stem) {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        candidates.push((mtime, path));
    }

    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    candidates
        .into_iter()
        .next()
        .map(|(_, p)| p.to_string_lossy().into_owned())
}

/// 判断去掉了前缀和 .jar 后的版本段是否表示分类器 jar（如 sources/javadoc/tests）。
///
/// 标准 fat jar 的版本段是纯版本号（`3.0`、`1.2.3-SNAPSHOT`），不含额外的 `-xxx`。
/// 分类器 jar 形如 `3.0-sources`、`3.0-javadoc`、`3.0-tests`，
/// 版本号 + `-` + 分类器名。这里用黑名单匹配常见分类器名，避免误杀带 `-` 的版本号。
///
/// 版本号本身也可能含 `-`（如 `1.0-rc1`），但常见分类器名是固定的小集合，
/// 用黑名单足够可靠且不会误判 SNAPSHOT/RC 版本号。
fn is_classifier_jar(version_segment: &str) -> bool {
    // 常见分类器：来源 jar、文档 jar、测试 jar 等
    const CLASSIFIERS: [&str; 6] = [
        "sources",
        "javadoc",
        "tests",
        "test-sources",
        "javadoc-jar",
        "tests-jar",
    ];
    // 版本段形如 "{version}-{classifier}"，找最后一个 - 后的部分与分类器比较
    let Some(after_last_dash) = version_segment.rsplit('-').next() else {
        return false;
    };
    CLASSIFIERS.iter().any(|c| *c == after_last_dash)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 裸 plugin 写法（x-space-cloud-nacos 实际形态）：
    /// build/plugins 下声明 spring-boot-maven-plugin，无 repackage execution，
    /// 靠继承 spring-boot-starter-parent 拿默认 execution。
    /// 预期：has_active_boot_plugin=true，artifactId 正确提取。
    #[test]
    fn parses_bare_plugin_in_build_plugins() {
        let pom = r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
    <modelVersion>4.0.0</modelVersion>
    <parent>
        <groupId>org.springframework.boot</groupId>
        <artifactId>spring-boot-starter-parent</artifactId>
        <version>2.3.5.RELEASE</version>
    </parent>
    <groupId>com.xspace</groupId>
    <artifactId>x-space-cloud-nacos</artifactId>
    <version>2.0.0</version>
    <build>
        <plugins>
            <plugin>
                <groupId>org.springframework.boot</groupId>
                <artifactId>spring-boot-maven-plugin</artifactId>
            </plugin>
        </plugins>
    </build>
</project>"#;
        let info = parse_pom(pom);
        assert_eq!(info.artifact_id.as_deref(), Some("x-space-cloud-nacos"));
        assert_eq!(info.packaging, None); // 默认 jar
        assert!(info.has_active_boot_plugin, "裸 plugin 应被识别为激活");
    }

    /// 显式 repackage 写法（csgs-backend 的 app 模块形态）。
    /// 预期：同样识别为激活（plan 目标：与旧规则兼容）。
    #[test]
    fn parses_explicit_repackage_execution() {
        let pom = r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
    <modelVersion>4.0.0</modelVersion>
    <parent>
        <groupId>cn.net.cpgc</groupId>
        <artifactId>csgs-backend-customer</artifactId>
        <version>4.0.0</version>
    </parent>
    <artifactId>csgs-backend-customer-app</artifactId>
    <build>
        <plugins>
            <plugin>
                <groupId>org.springframework.boot</groupId>
                <artifactId>spring-boot-maven-plugin</artifactId>
                <executions>
                    <execution>
                        <goals><goal>repackage</goal></goals>
                    </execution>
                </executions>
            </plugin>
        </plugins>
    </build>
</project>"#;
        let info = parse_pom(pom);
        assert_eq!(
            info.artifact_id.as_deref(),
            Some("csgs-backend-customer-app")
        );
        assert!(info.has_active_boot_plugin);
    }

    /// pluginManagement 声明（csgs-backend 根 pom 形态）：
    /// plugin 写在 pluginManagement 里，只是声明不激活。
    /// 预期：has_active_boot_plugin=false（即使有 repackage 字样也不算激活）。
    #[test]
    fn ignores_plugin_management_declaration() {
        let pom = r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
    <modelVersion>4.0.0</modelVersion>
    <groupId>cn.net.cpgc</groupId>
    <artifactId>csgs-backend</artifactId>
    <version>4.0.0</version>
    <packaging>pom</packaging>
    <build>
        <pluginManagement>
            <plugins>
                <plugin>
                    <groupId>org.springframework.boot</groupId>
                    <artifactId>spring-boot-maven-plugin</artifactId>
                    <executions>
                        <execution>
                            <goals><goal>repackage</goal></goals>
                        </execution>
                    </executions>
                </plugin>
            </plugins>
        </pluginManagement>
    </build>
</project>"#;
        let info = parse_pom(pom);
        assert_eq!(info.packaging.as_deref(), Some("pom"));
        assert!(
            !info.has_active_boot_plugin,
            "pluginManagement 内的声明不应被识别为激活"
        );
    }

    /// 聚合 pom（packaging=pom 且无 plugin）应被 analyze_pom 拒绝。
    #[test]
    fn analyze_rejects_aggregator_pom() {
        let pom = r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
    <modelVersion>4.0.0</modelVersion>
    <groupId>com.xspace</groupId>
    <artifactId>x-space-cloud-module</artifactId>
    <version>2.0.0</version>
    <packaging>pom</packaging>
    <modules>
        <module>x-space-cloud-gateway</module>
    </modules>
</project>"#;
        let info = parse_pom(pom);
        assert_eq!(info.packaging.as_deref(), Some("pom"));
        assert!(!info.has_active_boot_plugin);
    }

    /// 纯库模块（无 spring-boot-maven-plugin）不应被识别。
    #[test]
    fn ignores_library_module_without_boot_plugin() {
        let pom = r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
    <modelVersion>4.0.0</modelVersion>
    <parent>
        <groupId>com.xspace</groupId>
        <artifactId>x-space-starter</artifactId>
        <version>2.0.0</version>
    </parent>
    <artifactId>x-space-starter-cloud</artifactId>
    <dependencies>
        <dependency>
            <groupId>org.springframework.cloud</groupId>
            <artifactId>spring-cloud-starter</artifactId>
        </dependency>
    </dependencies>
</project>"#;
        let info = parse_pom(pom);
        assert_eq!(info.artifact_id.as_deref(), Some("x-space-starter-cloud"));
        assert!(!info.has_active_boot_plugin);
    }

    /* ---------- resolve_jar_path 测试 ---------- */

    /// 辅助：在临时模块目录下创建一个空 target 目录及若干 jar 文件。
    /// 返回模块目录路径（resolve_jar_path 的 module_path 入参）。
    fn make_module_with_jars(jar_filenames: &[&str]) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!(
            "sl_test_jar_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let target = tmp.join("target");
        std::fs::create_dir_all(&target).unwrap();
        for name in jar_filenames {
            std::fs::write(target.join(name), b"fake jar content").unwrap();
        }
        tmp
    }

    #[test]
    fn resolve_jar_path_prefers_plain_unversioned() {
        // 同时存在无版本号和带版本号 → 优先无版本号（向后兼容）
        let module = make_module_with_jars(&["my-app.jar", "my-app-3.0.jar"]);
        let got = resolve_jar_path(&module, "my-app");
        assert!(got.ends_with("my-app.jar"), "应优先无版本号，实际: {got}");
        let _ = std::fs::remove_dir_all(&module);
    }

    #[test]
    fn resolve_jar_path_finds_versioned_fat_jar() {
        // 只有带版本号的 fat jar（x-space 实际形态）→ 命中版本号 jar
        let module = make_module_with_jars(&["x-space-cloud-system-start-3.0.jar"]);
        let got = resolve_jar_path(&module, "x-space-cloud-system-start");
        assert!(
            got.ends_with("x-space-cloud-system-start-3.0.jar"),
            "应命中带版本号 fat jar，实际: {got}"
        );
        let _ = std::fs::remove_dir_all(&module);
    }

    #[test]
    fn resolve_jar_path_ignores_original_and_picks_fat_jar() {
        // repackage 典型产出：fat jar + .original，必须选 fat jar
        let module = make_module_with_jars(&["demo-1.0.jar", "demo-1.0.jar.original"]);
        let got = resolve_jar_path(&module, "demo");
        assert!(
            got.ends_with("demo-1.0.jar"),
            "应选 fat jar 而非 .original，实际: {got}"
        );
        assert!(!got.ends_with(".original"));
        let _ = std::fs::remove_dir_all(&module);
    }

    #[test]
    fn resolve_jar_path_ignores_classifier_jars() {
        // 存在 sources/javadoc 等分类器 jar，应被排除，命中 fat jar
        let module = make_module_with_jars(&[
            "svc-2.1.0.jar",
            "svc-2.1.0-sources.jar",
            "svc-2.1.0-javadoc.jar",
            "svc-2.1.0-tests.jar",
        ]);
        let got = resolve_jar_path(&module, "svc");
        assert!(
            got.ends_with("svc-2.1.0.jar"),
            "应排除分类器 jar 命中 fat jar，实际: {got}"
        );
        let _ = std::fs::remove_dir_all(&module);
    }

    #[test]
    fn resolve_jar_path_handles_snapshot_version_with_dash() {
        // 版本号含 -（如 1.0-rc1 / SNAPSHOT 形态）不应被误判为分类器
        let module = make_module_with_jars(&["app-1.0-rc1.jar"]);
        let got = resolve_jar_path(&module, "app");
        assert!(
            got.ends_with("app-1.0-rc1.jar"),
            "版本号含 - 不应被误判为分类器，实际: {got}"
        );
        let _ = std::fs::remove_dir_all(&module);
    }

    #[test]
    fn resolve_jar_path_picks_newest_when_multiple_versions() {
        // 多个版本号 jar 共存（如旧版未清理）→ 取修改时间最新者。
        // 先建 old 后建 new，sleep 保证 mtime 区分；new 应被选中。
        let tmp = std::env::temp_dir().join(format!(
            "sl_test_jar_multi_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let target = tmp.join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("svc-1.0.jar"), b"old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(target.join("svc-2.0.jar"), b"new").unwrap();

        let got = resolve_jar_path(&tmp, "svc");
        assert!(
            got.ends_with("svc-2.0.jar"),
            "多版本应取最新修改时间者，实际: {got}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_jar_path_fallback_when_no_target() {
        // 没有 target 目录 → 兜底返回无版本号路径（文件不存在也返回，由 java -jar 报错）
        let tmp = std::env::temp_dir().join(format!(
            "sl_test_jar_notarget_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let got = resolve_jar_path(&tmp, "ghost");
        assert!(
            got.ends_with("ghost.jar"),
            "无 target 应兜底无版本号路径，实际: {got}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_jar_path_fallback_when_only_original() {
        // 只有 .original（异常情况，repackage 失败）→ 兜底无版本号路径
        let module = make_module_with_jars(&["broken-1.0.jar.original"]);
        let got = resolve_jar_path(&module, "broken");
        assert!(
            got.ends_with("broken.jar"),
            "只有 .original 应兜底无版本号路径，实际: {got}"
        );
        assert!(!got.ends_with(".original"));
        let _ = std::fs::remove_dir_all(&module);
    }

    #[test]
    fn is_classifier_jar_detection() {
        // fat jar 版本段（纯版本号）不是分类器
        assert!(!is_classifier_jar("3.0"));
        assert!(!is_classifier_jar("1.2.3-SNAPSHOT"));
        assert!(!is_classifier_jar("1.0-rc1"));
        // 分类器 jar 版本段
        assert!(is_classifier_jar("3.0-sources"));
        assert!(is_classifier_jar("3.0-javadoc"));
        assert!(is_classifier_jar("3.0-tests"));
    }
}
