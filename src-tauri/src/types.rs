//! 前后端共享的数据结构。字段名用 snake_case（serde 默认），
//! 与前端 src/types.ts 一一对应。

use serde::{Deserialize, Serialize};

/// 一个已安装的 JDK
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JdkInfo {
    pub id: String,
    /// JDK 根目录（含 bin/java 的那一层）
    pub path: String,
    /// 版本号，如 "17.0.9"、"1.8.0_421"
    pub version: String,
    /// 厂商：Temurin / Microsoft / Zulu / Corretto / Oracle / Unknown
    pub vendor: String,
    /// 是否用户手动添加
    #[serde(default)]
    pub manual: bool,
}

/// 扫描到的一个可运行服务
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceInfo {
    /// 用 artifact_id 作为唯一标识
    pub id: String,
    pub artifact_id: String,
    /// 模块根目录绝对路径（含 pom.xml）
    pub module_path: String,
    /// 启动类全限定名
    pub main_class: String,
    /// 端口；读不到返回 0
    #[serde(default)]
    pub port: u16,
    /// 推算的 jar 路径
    pub jar_path: String,
}

/// 服务运行状态
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
    Crashed,
}

/// 单个服务的运行态快照
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceRuntime {
    pub id: String,
    pub status: ServiceStatus,
    pub pid: u32,
    /// 启动时间戳（毫秒），0 表示未运行
    pub started_at: u64,
    /// 绑定的 JDK id
    #[serde(default)]
    pub jdk_id: String,
    /// 服务所属的工作目录（启动时记录），用于前端按工作目录分组
    #[serde(default)]
    pub workspace: String,
}

/// 单个服务的内存占用快照
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceMemory {
    pub id: String,
    /// 内存占用（字节），0 表示未运行或获取失败
    #[serde(default)]
    pub memory_bytes: u64,
}

/// 启动模式
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StartMode {
    #[default]
    Mvn,
    Jar,
}

/// 每个服务的持久化配置
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ServiceConfig {
    #[serde(default)]
    pub jdk_id: String,
    #[serde(default = "default_mode")]
    pub mode: StartMode,
    #[serde(default)]
    pub port: u16,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub extra_args: String,
    /// Maven profile 列表（逗号分隔，如 "SpringCloud"），仅 mvn 模式生效。
    /// 用于激活声明在项目根 pom <profile> 内、但未 activeByDefault 的模块。
    #[serde(default)]
    pub mvn_profiles: String,
    /// 初始堆大小 -Xms，如 "512m"。空串表示不设。仅大小写字母前缀，后端校验后注入。
    #[serde(default)]
    pub jvm_xms: String,
    /// 最大堆大小 -Xmx，如 "1024m"。空串表示不设。最关键的内存上限参数。
    #[serde(default)]
    pub jvm_xmx: String,
    /// 元空间大小 -XX:MaxMetaspaceSize，如 "256m"。空串表示不设。JDK8+ 类元数据区。
    #[serde(default)]
    pub jvm_metaspace: String,
    /// 其他 JVM 参数，如 "-XX:+UseG1GC -XX:MaxDirectMemorySize=256m"。空串表示不设。
    #[serde(default)]
    pub jvm_extra: String,
}

fn default_mode() -> StartMode {
    StartMode::Mvn
}
fn default_true() -> bool {
    true
}

/// 启动分组
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartGroup {
    pub name: String,
    pub services: Vec<String>,
    pub delay_sec: u64,
}

/// 堆内存默认规则：新扫描到的服务按 artifactId 关键词匹配后自动套用 JVM 参数。
/// 兜底规则（keywords 为空）永远命中，排在最后。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HeapRule {
    pub id: String,
    /// 规则名，如 "网关"
    pub name: String,
    /// 关键词，逗号分隔，如 "gateway,网关"；空串表示兜底规则（永远命中）
    pub keywords: String,
    /// 套用的 -Xms，如 "256m"
    #[serde(default)]
    pub jvm_xms: String,
    /// 套用的 -Xmx，如 "512m"
    #[serde(default)]
    pub jvm_xmx: String,
    /// 套用的 -XX:MaxMetaspaceSize，如 "256m"
    #[serde(default)]
    pub jvm_metaspace: String,
    /// 是否内置规则（不可删除）
    #[serde(default)]
    pub builtin: bool,
}

/// 整份配置文件
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub workspace: String,
    /// 历史工作目录列表（用于下拉切换）
    #[serde(default)]
    pub workspaces: Vec<String>,
    #[serde(default)]
    pub default_jdk_id: String,
    /// 工作目录级默认 JDK：workspace 路径 -> JDK id
    #[serde(default)]
    pub workspace_jdks: std::collections::HashMap<String, String>,
    /// 工作目录级服务选中状态：workspace 路径 -> 选中的服务 id 列表
    #[serde(default)]
    pub workspace_selections: std::collections::HashMap<String, Vec<String>>,
    /// 工作目录级置顶服务：workspace 路径 -> 置顶的服务 id 列表
    #[serde(default)]
    pub workspace_pins: std::collections::HashMap<String, Vec<String>>,
    /// 工作目录级自定义排序：workspace 路径 -> 服务 id 顺序列表。
    /// 仅影响置顶分组内部与普通分组内部的先后；未出现在列表中的服务
    /// （如后来新扫描到的）排在自定义项之后，仍按扫描顺序展示。
    #[serde(default)]
    pub workspace_orders: std::collections::HashMap<String, Vec<String>>,
    #[serde(default = "default_mvn")]
    pub mvn_path: String,
    #[serde(default)]
    pub jdks: Vec<JdkInfo>,
    /// 按工作目录分组的每服务配置：workspace 路径 -> service_id -> ServiceConfig
    #[serde(default)]
    pub services:
        std::collections::HashMap<String, std::collections::HashMap<String, ServiceConfig>>,
    #[serde(default)]
    pub groups: Vec<StartGroup>,
    /// 堆内存默认规则列表（扫描新服务时按 artifactId 关键词匹配套用）
    #[serde(default)]
    pub heap_rules: Vec<HeapRule>,
    /// 是否启用默认规则自动套用
    #[serde(default = "default_true")]
    pub heap_rules_enabled: bool,
    /// 内嵌控制台会话配置列表（应用重启后恢复，PTY 进程不自动重开）
    #[serde(default)]
    pub terminals: Vec<TerminalConfig>,
}

fn default_mvn() -> String {
    "mvn".to_string()
}

/// 内置默认堆规则。基于 Spring Cloud 微服务典型画像：
/// 网关轻量、注册中心中等、主服务重、其余兜底。
/// 兜底规则（空 keywords）永远命中，必须排在最后。
pub fn builtin_heap_rules() -> Vec<HeapRule> {
    vec![
        HeapRule {
            id: "builtin-gateway".into(),
            name: "网关".into(),
            keywords: "gateway".into(),
            jvm_xms: "256m".into(),
            jvm_xmx: "384m".into(),
            jvm_metaspace: "192m".into(),
            builtin: true,
        },
        HeapRule {
            id: "builtin-nacos".into(),
            name: "注册中心".into(),
            keywords: "nacos".into(),
            jvm_xms: "512m".into(),
            jvm_xmx: "512m".into(),
            jvm_metaspace: "256m".into(),
            builtin: true,
        },
        HeapRule {
            id: "builtin-system".into(),
            name: "主服务".into(),
            keywords: "system".into(),
            jvm_xms: "768m".into(),
            jvm_xmx: "1024m".into(),
            jvm_metaspace: "256m".into(),
            builtin: true,
        },
        HeapRule {
            id: "builtin-wechat".into(),
            name: "微信端".into(),
            keywords: "wechat,teacher,parents".into(),
            jvm_xms: "512m".into(),
            jvm_xmx: "768m".into(),
            jvm_metaspace: "256m".into(),
            builtin: true,
        },
        HeapRule {
            id: "builtin-default".into(),
            name: "默认".into(),
            keywords: "".into(),
            jvm_xms: "256m".into(),
            jvm_xmx: "768m".into(),
            jvm_metaspace: "256m".into(),
            builtin: true,
        },
    ]
}

/// 内嵌终端的 Shell 类型。
/// Auto 由后端按 pwsh → powershell → cmd 顺序探测；其余为用户显式指定。
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TerminalShell {
    #[default]
    Auto,
    Pwsh,
    PowerShell,
    Cmd,
}

/// 一个控制台会话的持久化配置（随 config.json 保存，应用重启后恢复列表）。
/// 注意：PTY 子进程无法跨重启存活，重启后仅恢复配置，需用户手动「打开」。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TerminalConfig {
    /// 唯一 id（uuid v4，不带连字符）
    pub id: String,
    /// 终端名，空串时前端展示默认名「终端 N」
    #[serde(default)]
    pub name: String,
    /// 工作目录绝对路径
    #[serde(default)]
    pub cwd: String,
    /// Shell 选择（默认 Auto）
    #[serde(default)]
    pub shell: TerminalShell,
}

/// 终端运行态快照（不持久化，仅运行时返回给前端）
#[derive(Clone, Debug, Serialize)]
pub struct TerminalInfo {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub shell: TerminalShell,
    /// 是否正在运行（true=PTY 活跃；false=已退出或尚未打开）
    pub running: bool,
    /// 子进程 PID，0 表示未知
    #[serde(default)]
    pub pid: u32,
    /// 启动时间戳（毫秒），0 表示未运行
    #[serde(default)]
    pub started_at: u64,
}

/// 终端输出事件载荷
#[derive(Clone, Serialize)]
pub struct TerminalDataEvent {
    pub data: String,
}

/// 终端退出事件载荷
#[derive(Clone, Serialize)]
pub struct TerminalExitEvent {
    /// 退出码，None 表示读取异常或无法获取
    pub code: Option<i32>,
}

/// 日志事件载荷
#[derive(Clone, Serialize)]
pub struct LogEvent {
    pub line: String,
    pub is_error: bool,
}

/// 批量日志事件载荷，用于减少高频日志产生的 IPC 调用。
#[derive(Clone, Serialize)]
pub struct LogBatchEvent {
    pub lines: Vec<LogEvent>,
}

/// 状态变更事件载荷
#[derive(Clone, Serialize)]
pub struct StatusEvent {
    pub id: String,
    pub status: ServiceStatus,
    pub pid: u32,
}
