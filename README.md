# Paxile

画画时想按一下 `B` 切回画笔，结果输入法开始组字；切到文字工具，又不想来回切换中英文。Paxile 就是为了解决这种打断。

它常驻 Windows 托盘，在已适配的软件里区分画布快捷键和文字输入：该用快捷键时不让输入法抢键，该打中文时正常打字。目前主要适配 **CLIP STUDIO PAINT（CSP）**，其他软件不会默认套用 CSP 的规则。

> 目前仍是实验阶段。第一次使用建议先保存作品，用空白文档试一下。不同输入法和软件版本的表现可能不同。

## 装好以后怎么用

从 [Releases](https://github.com/KamiyaShiroba/Paxile/releases) 下载 Windows 安装程序，按提示安装。Windows 11 通常已经带有运行界面需要的 WebView2。

Paxile 默认静默启动，不弹设置窗口。看到托盘图标就说明程序已启动：

- 双击左键打开设置窗口。
- 右键打开菜单。
- 左键收起菜单。
- 关掉设置窗口不会退出，真正退出请用托盘菜单里的「退出 Paxile」。

在「拦截」页添加 CSP 预设，再打开总开关。画布上试试 `B`、`G` 等快捷键，然后切到文字工具输入中文，确认两边都正常。总开关和列表修改会传给正在运行的核心，不需要手动输入命令或寻找进程号。

列表里的**自定义项是输入法白名单**：它的作用是让指定程序正常使用输入法，不是给任意软件强制开启拦截。可以直接输入程序文件名，也可以用旁边的按钮选择文件；默认筛选 `.exe`，需要时能切换到所有文件。

## 平时不用一直管它

「设置」里可以开启开机启动。静默启动默认开启，对手动启动和开机启动都生效；如果希望启动后直接看见窗口，把它关掉即可。

遇到问题可以先看设置页的实时日志。顶栏的「运行中」表示核心进程正在运行，**不等于已经确认每个 CSP 窗口都应用了拦截**。

## 更新与卸载

更新入口会检查最新正式版本并显示更新日志。确认更新后才会下载和安装，不会在你没同意时直接替换程序。更新包需要通过签名校验。

Windows 安装包采用 NSIS，安装后可以在系统「设置 → 应用 → 已安装的应用」中找到 Paxile 并卸载。卸载和更新的具体行为见 [发布说明](https://github.com/KamiyaShiroba/Paxile/releases)。没有正式发行版时，更新检查可能暂时找不到可用版本。

## 如果 CSP 没有拦截

先检查总开关、CSP 预设，以及 CSP 有没有被加进输入法白名单。未知软件、文字编辑控件和不确定的场景会放行，这是为了避免误伤正常输入。

新版核心支持断线后重新连接：退出再启动 Paxile，不应要求一起重启 CSP。但如果 CSP 里已经加载的是旧版 DLL，仍需要**重启 CSP 一次**才能换成新版；Paxile 不会替你关闭绘图软件。若持续异常，保存作品后退出 Paxile，并把相关日志附到 [Issue](https://github.com/KamiyaShiroba/Paxile/issues) 里。

## 从源码运行

需要 Windows、Rust MSVC 工具链和 Node.js（建议 Node 22 LTS）。在项目根目录先构建核心和界面：

```bash
cargo build --workspace --locked --target-dir target/reconnect
cd app
npm ci
npm run build
cargo run --manifest-path src-tauri/Cargo.toml --locked
```

默认开启静默启动，运行后双击托盘打开界面。修改前端后需要重新运行 `npm run build`；单独的 `cargo run` 不会自动运行 Vite。

跑检查时使用同一组核心输出，避免 EXE 和 DLL 版本不匹配：

```bash
cargo build --workspace --locked --target-dir target/reconnect
cargo test --workspace --locked --target-dir target/reconnect
cargo clippy --workspace --all-targets --locked --target-dir target/reconnect -- -D warnings
cd app
npm ci
npm test
npm run build
cargo test --manifest-path src-tauri/Cargo.toml --locked
```

Windows 可能锁住已经加载的 DLL。遇到这个情况，不要为了编译而强行结束 CSP；改用独立构建目录，或保存作品后自行重启 CSP。

## 想了解实现或参与适配

- [核心接口与验证边界](BACKEND.md)
- [桌面程序与核心的生命周期](docs/app-runtime.md)
- [前端开发](app/README.md)

当前适配依赖软件自己的窗口和输入法信号，不是简单地全局吞掉按键。增加新软件前，应先采集和验证它的输入行为；仅填一个软件名并不能保证兼容。
