/**
 * 前后端共享的数据结构 —— 与 Rust 侧 #[derive(Serialize)] 的 struct 字段一一对应。
 * Rust 用 snake_case 序列化（serde 默认），TS 侧保持一致，避免 rename。
 */

/** 一个已安装的 JDK */
export interface JdkInfo {
  id: string;
  /** JDK 根目录（含 bin/java 的那一层） */
  path: string;
  /** 版本号，如 "17.0.9"、"1.8.0_421" */
  version: string;
  /** 厂商：Temurin / Microsoft / Zulu / Corretto / Oracle / Unknown */
  vendor: string;
  /** 是否用户手动添加（而非自动扫描） */
  manual: boolean;
}

/** 扫描到的一个可运行服务 */
export interface ServiceInfo {
  /** 用 artifact_id 作为唯一标识 */
  id: string;
  /** Maven artifactId，如 csgs-backend-customer-app */
  artifact_id: string;
  /** 模块根目录绝对路径（含 pom.xml） */
  module_path: string;
  /** 启动类全限定名，如 cn.net.cpgc.customer.CsgsCustomerApp */
  main_class: string;
  /** 端口；读不到返回 0 表示未知（本项目多数在 Nacos 远端配置） */
  port: number;
  /** 推算的 jar 路径 {module_path}/target/{artifact_id}.jar */
  jar_path: string;
}

/** 服务运行状态枚举 */
export type ServiceStatus =
  | 'stopped'
  | 'starting'
  | 'running'
  | 'stopping'
  | 'crashed';

/** 单个服务的运行态快照 */
export interface ServiceRuntime {
  id: string;
  status: ServiceStatus;
  pid: number;
  /** 启动时的 UNIX 时间戳（毫秒），0 表示未运行 */
  started_at: number;
  /** 绑定的 JDK id（空串表示用全局默认） */
  jdk_id: string;
  /** 服务所属的工作目录（启动时记录），用于按工作目录分组展示 */
  workspace: string;
}

/** 单个服务的内存占用快照 */
export interface ServiceMemory {
  id: string;
  /** 内存占用（字节），0 表示未运行或获取失败 */
  memory_bytes: number;
}

/** 启动模式 */
export type StartMode = 'mvn' | 'jar';

/** 每个服务的持久化配置 */
export interface ServiceConfig {
  jdk_id: string;
  mode: StartMode;
  port: number;
  enabled: boolean;
  /** 额外的启动参数（Spring 应用参数），如 --server.port=8001 */
  extra_args: string;
  /** Maven profile 列表（逗号分隔，如 SpringCloud），仅 mvn 模式生效 */
  mvn_profiles: string;
  /** 初始堆大小 -Xms，如 "512m"。空串表示不设 */
  jvm_xms: string;
  /** 最大堆大小 -Xmx，如 "1024m"。空串表示不设。最关键的内存上限参数 */
  jvm_xmx: string;
  /** 元空间大小 -XX:MaxMetaspaceSize，如 "256m"。空串表示不设 */
  jvm_metaspace: string;
  /** 其他 JVM 参数，如 "-XX:+UseG1GC"。空串表示不设 */
  jvm_extra: string;
}

/** 启动分组定义 */
export interface StartGroup {
  name: string;
  services: string[];
  /** 该组全部启动后，等待多少秒再启动下一组 */
  delay_sec: number;
}

/** 堆内存默认规则：新扫描到的服务按 artifactId 关键词匹配后自动套用 JVM 参数 */
export interface HeapRule {
  id: string;
  /** 规则名，如 "网关" */
  name: string;
  /** 关键词，逗号分隔，如 "gateway,网关"；空串表示兜底规则（永远命中） */
  keywords: string;
  /** 套用的 -Xms，如 "256m" */
  jvm_xms: string;
  /** 套用的 -Xmx，如 "512m" */
  jvm_xmx: string;
  /** 套用的 -XX:MaxMetaspaceSize，如 "256m" */
  jvm_metaspace: string;
  /** 是否内置规则（不可删除） */
  builtin: boolean;
}

/** 整份配置文件 */
export interface AppConfig {
  workspace: string;
  /** 历史工作目录列表（用于下拉切换） */
  workspaces: string[];
  default_jdk_id: string;
  /** 工作目录级默认 JDK：workspace 路径 -> JDK id */
  workspace_jdks: Record<string, string>;
  /** 工作目录级服务选中状态：workspace 路径 -> 选中的服务 id 列表 */
  workspace_selections: Record<string, string[]>;
  /** 工作目录级置顶服务：workspace 路径 -> 置顶的服务 id 列表 */
  workspace_pins: Record<string, string[]>;
  /** 工作目录级自定义排序：workspace 路径 -> 服务 id 顺序列表。
   *  仅决定置顶分组内部、普通分组内部的先后，不跨越置顶边界。 */
  workspace_orders: Record<string, string[]>;
  mvn_path: string;
  jdks: JdkInfo[];
  /** 按工作目录分组的每服务配置：workspace 路径 -> service_id -> ServiceConfig */
  services: Record<string, Record<string, ServiceConfig>>;
  groups: StartGroup[];
  /** 堆内存默认规则列表（扫描新服务时按 artifactId 关键词匹配套用） */
  heap_rules: HeapRule[];
  /** 是否启用默认规则自动套用 */
  heap_rules_enabled: boolean;
  /** 内嵌控制台会话配置列表（应用重启后恢复，PTY 进程不自动重开） */
  terminals: TerminalConfig[];
}

/** 日志事件载荷 */
export interface LogEvent {
  line: string;
  /** stderr 为 true */
  is_error: boolean;
}

/** 批量日志事件载荷 */
export interface LogBatchEvent {
  lines: LogEvent[];
}

/** 状态变更事件载荷 */
export interface StatusEvent {
  id: string;
  status: ServiceStatus;
  pid: number;
}

/* ---------- 控制台（内嵌终端） ---------- */

/** Shell 类型。Auto 由后端按 pwsh → powershell → cmd 回退 */
export type TerminalShell = 'auto' | 'pwsh' | 'power_shell' | 'cmd';

/** 一个控制台会话的持久化配置（随 config.json 保存，重启后恢复列表） */
export interface TerminalConfig {
  id: string;
  /** 终端名，空串时展示默认名「终端 N」 */
  name: string;
  /** 工作目录绝对路径 */
  cwd: string;
  /** Shell 选择，默认 auto */
  shell: TerminalShell;
}

/** 终端运行态快照（运行时返回给前端，不持久化） */
export interface TerminalInfo {
  id: string;
  name: string;
  cwd: string;
  shell: TerminalShell;
  /** 是否正在运行（true=PTY 活跃） */
  running: boolean;
  pid: number;
  /** 启动时间戳（毫秒），0 表示未运行 */
  started_at: number;
}

/** 终端输出事件载荷 */
export interface TerminalDataEvent {
  data: string;
}

/** 终端退出事件载荷 */
export interface TerminalExitEvent {
  code: number | null;
}
