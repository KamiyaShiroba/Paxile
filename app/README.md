# Paxile 托盘应用（设置界面）

Tauri 2 写的托盘常驻程序，负责配置、状态和运行时控制。
拦截引擎是独立的 `paxile-probe`；设置页可以显示启动命令，也可以通过本地控制管道切换正在运行的会话。

## 结构

```
app/
  src/                 Vite 前端（保留原生 JS 业务逻辑，React 挂载 Tabler 图标）
    index.html
    styles.css
    main.js
  src-tauri/
    Cargo.toml         独立包，不在根 workspace 内（根 Cargo.toml 已 exclude）
    tauri.conf.json    frontendDist 指向 ../dist；启动时按静默启动设置决定是否显示窗口
    capabilities/
    icons/             项目根目录 icon.png 的应用图标资源
    src/
      main.rs
      lib.rs           托盘、自绘托盘弹窗、按需创建/销毁设置窗口、命令
      config.rs        config.json 读写（与引擎共用 paxile_policy::Config）
```

## 运行

前端需要 Node.js 20.19+ 或 22.12+。先构建，再运行 Tauri：

```bash
cd app
npm ci
npm test
npm run build                 # 输出 app/dist；Tauri frontendDist 必须为 ../dist
cd src-tauri
cargo run
```

前端开发：在 `app` 下运行 `npm run dev`（127.0.0.1:1420）；生产预览：`npm run preview`。
`src/main.js` 保留配置 CRUD、实时日志和托盘业务逻辑，`src/icons.jsx` 用真实 React / `@tabler/icons-react` 渲染图标。
系统文件选择器调用 `pick_program`，返回文件名或 `null`；原生窗口按钮调用 `minimize_settings`、`close_settings`。
状态每秒读取 `get_core_status`，`requested_enabled: null` 不会覆盖已确认的开关状态。

首次启动会拉取 Tauri 依赖，耗时较长。运行时需要系统已装 **WebView2**
（Win11 自带）。

## 设计要点

- **自绘托盘菜单**：不使用 Windows 原生右键菜单。点击托盘图标会打开 Arc 风格的小型 WebView 弹窗，只有 `Paxile`、`打开设置`、`退出 Paxile` 三项。
- **静默启动**：开启开机启动后，静默启动会让登录启动只显示托盘并直接启动后端；关闭静默启动则打开设置页。引擎失败时会回退打开设置页。
- **关窗不退出**：设置页关闭只销毁设置窗口；托盘弹窗失焦自动隐藏。只有自绘菜单中的"退出 Paxile"才真正退出并结束由托盘启动的引擎。
- **配置共用**：界面读写的 `config.json` 就是 `paxile_policy::Config`
  序列化结果，位于 OS 的 app-config 目录，引擎读同一份。
- **运行时控制**：引擎 PID 输入框通过 Tauri 命令调用 `paxile-probe control status|on|off`，未授权的只读会话不能被界面打开拦截。
- **可扩展**：预设按软件列出；白名单里的 exe 一律放行。两者都写进 config，
  新增预设基本只改配置。

## 图标

`icons/icon.png` 和 `icons/icon.ico` 使用项目根目录的 `icon.png`。Windows 托盘和应用窗口复用这套图标；若后续要发布安装包，建议再为托盘制作一份针对 16–32px 优化的简化图标。
