# Spring 启动器（Spring Launcher）

<div align="center">

**轻量级 Spring / Spring Cloud 本地服务启动管理器**

基于 Tauri 2 + Vue 3 · 内存占用 ~50–70MB · 安装包 ~8MB · 适配任意 Spring Boot 多模块项目

</div>

---

## 为什么做这个

IDEA 太重（2–4GB 内存），本地开发时常常只是想快速拉起一组 Spring Cloud 微服务。
这个工具不打开 IDE 即可完成服务的**发现、启动、停止、重启、日志查看与终端操作**，把内存留给业务服务本身。

## 功能特性

- **JDK 管理**：自动扫描系统已装 JDK（含 IDEA 托管的 `.jdks` 目录），支持手动添加、设默认
- **服务发现**：选择工作目录后自动扫描所有 Maven 模块，识别含 `spring-boot-maven-plugin` 的可运行服务
- **启动模式**：Maven 自动构建依赖链后启动（`-am install` + `spring-boot:run`）/ `java -jar`（预编译）两种模式
- **per-service JDK 绑定**：nacos-app 用 JDK 8，其余用 JDK 17，互不干扰
- **分组启动**：按依赖顺序分组（nacos → 基础设施 → 框架应用 → 业务服务），组间自动延迟
- **进程管理**：启动 / 停止（`taskkill /T` 杀进程树）/ 重启 / 一键全停
- **存量进程收编**：直接关窗后服务仍在后台跑？重开应用时自动按启动类 / jar 路径识别存量 JVM，恢复状态显示与停止 / 重启 / 内存统计管理；mvn 模式还会重新挂接日志文件，继续接收增量日志
- **实时日志**：每个服务独立日志标签页，流式输出 stdout/stderr，支持过滤、暂停、清屏
- **内置终端**：多标签交互式终端（xterm.js），可指定工作目录与 shell，随手执行 `mvn`、`git` 等命令
- **就绪判定**：监听 `Started XxxApp in N seconds` 日志判定服务就绪（比探端口准，适配端口在 Nacos 远端的场景）

## 环境要求

- Node.js ≥ 20 + pnpm ≥ 10
- Rust（stable）
- WebView2（Win11 已内置）

## 快速开始

```bash
# 克隆后安装依赖
pnpm install

# 仅启动前端（端口 1420）
pnpm dev

# 完整 Tauri 开发（前端 + Rust 窗口）
pnpm tauri dev

# 打包（生成 src-tauri/target/release/bundle/ 下的 NSIS 安装包）
pnpm tauri build
```

## 使用流程

1. 打开应用 → **总览**页点「选择」指定后端工作目录（多模块 Maven 项目根目录）
2. 自动扫描出所有可运行服务
3. 进 **JDK** 页点「重新扫描」，设一个默认 JDK
4. 进 **设置**页，为特殊服务（如 nacos）单独绑定旧版 JDK，其余用默认
5. 回**总览**页，勾选要启动的服务，或直接点某分组的「启动」
6. 进**日志**页查看各服务实时输出；需要敲命令时用**控制台**页的内嵌终端

## 技术栈

| 层 | 技术 |
|---|---|
| 前端 | Vue 3.5 · TypeScript · Pinia · Vue Router · Tailwind CSS 3 · xterm.js · lucide-vue-next |
| 桌面壳 | Tauri 2（系统 WebView2，非 Electron） |
| 后端 | Rust（stable） |
| 持久化 | JSON 文件（`%APPDATA%\spring-launcher\config.json`） |

## 项目结构

```
spring-launcher/
├── src/                         # Vue 前端
│   ├── api/tauri.ts             # 对 Rust 命令的封装
│   ├── stores/                  # Pinia: settings / jdk / services / terminals
│   ├── views/                   # Dashboard / ServiceList / LogViewer / Console / JdkManager / Settings
│   ├── components/              # StatusBadge 等公共组件
│   ├── types.ts                 # 前后端共享 DTO
│   └── utils.ts                 # 通用工具函数
└── src-tauri/                   # Rust 后端
    └── src/
        ├── lib.rs               # Tauri 入口 + 命令注册
        ├── commands.rs          # #[tauri::command] 接口层
        ├── jdk_scanner.rs       # JDK 发现
        ├── project_scanner.rs   # Spring Boot 服务扫描
        ├── mvn_scanner.rs       # Maven 模块 / 依赖链解析
        ├── process_manager.rs   # 进程生命周期 + 存量进程收编
        ├── log_dispatcher.rs    # 日志流分发
        ├── terminal_manager.rs  # 内嵌终端（PTY）管理
        ├── config.rs            # 配置持久化
        └── types.rs             # 共享数据结构
```

## 设计细节

| 实际场景 | 工具适配 |
|---|---|
| 启动类命名 `*App.java`（非 `*Application`） | 扫描优先匹配 `App.java` 结尾 |
| 部分服务需 JDK 8，其余 JDK 17 | 支持 per-service JDK 绑定 |
| 端口在 Nacos 远端配置，本地 yml 读不到 | 就绪用日志匹配而非端口探测；端口留空不影响启动 |
| Maven 先 `-am install` 再执行目标服务的 `spring-boot:run` | 依赖模块修改后重启即可生效，构建失败不会启动旧代码 |
| 业务模块配置了 `spring-boot-maven-plugin:repackage`，install 装出可执行 fat jar | 构建阶段自动加 `-Dspring-boot.repackage.skip=true -Dmaven.jar.forceCreation=true` 装薄 jar，否则 `spring-boot:run` 从仓库解析依赖时类藏在 `BOOT-INF/classes` 中不可见（典型报错：`No qualifying bean of type ...CommonApi`） |
| `mvn spring-boot:run` 会 fork 子 JVM | 停止用 `taskkill /T /F` 杀整棵进程树 |
| 直接关窗后服务仍在后台跑，重开应用状态丢失 | 扫描后按启动类 / jar 路径收编系统 java 进程表中的存量 JVM，接管状态管理（收编进程按 PID 巡检存活；mvn 模式重挂日志文件，jar 模式接管前日志不可回溯） |
| jar 名 = artifactId（`<finalName>`） | jar 路径自动推算 `{module}/target/{artifactId}.jar` |

## 已知限制

- 日志缓冲上限 2000 行（前端内存控制），超长自动丢弃旧行
- 启动顺序依赖靠分组延迟（秒级），非严格就绪信号串联
- 仅适配 Windows（taskkill）；macOS/Linux 需改用 `kill -9 <pgid>`

## License

[MIT](LICENSE)
