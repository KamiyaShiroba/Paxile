# Paxile

回想一下：
画画或者设计的时候，因为忘记关输入法了，按一下快捷键输入法却跳出来了；
关掉输入法以后，要是想要打字，又得把输入法给打开...
是否受够了这种感觉？没——错！作者也是这么想的！于是利用强大的ai，做了这个小玩意儿。

Paxile ，取自 Pax ，是罗马神话中的“和平女神”。而这个小工具就像和平女神一样，能够尽可能地介入调和你的操作与输入法之间的摩擦。

目前主要适配了 CLIP STUDIO PAINT ——当然后续还会有更多软件会适配上。如果想要套用到其它软件，也可以通过选择应用程序文件的形式添加白名单。

> 注意注意，毕竟还是alpha阶段，所以第一次用还请先保存好作品，先用空白的项目测试一下。不同的输入法的表现可能会不一样——作者不担保有100%的可靠性。

## 怎么用？

从 [Releases](https://github.com/KamiyaShiroba/Paxile/releases) 下载 Windows 安装程序，按提示安装即可。Paxile 默认是静默启动的，因此只要你在任务栏托盘看到图标，就说明它已经正在运行了。

列表里的**自定义项，其实是输入法白名单**——注意，是让指定程序正常用输入法，不是给任意软件强行开拦截。你可以直接填程序文件名，也可以点旁边按钮选择具体的文件。

## 如果输入法还是有冲突

我知道你很急，但是你先别急。
- 总开关开了没？
- 对应软件有没有被加进拦截白名单？
- 重启软件、paxile 和电脑或许可以解决 99% 的问题...
- 要是持续异常，保存作品后退出 Paxile，把相关日志附到 [Issue](https://github.com/KamiyaShiroba/Paxile/issues) 里。

## 从源码运行

需要 Windows、Rust MSVC 工具链和 Node.js（建议 Node 22 LTS）。在项目根目录先构建核心和界面：

```bash
cargo build --workspace --locked --target-dir target/reconnect
cd app
npm ci
npm run build
cargo run --manifest-path src-tauri/Cargo.toml --locked
```

默认还是静默启动，跑起来后双击托盘图标打开界面。改完前端记得重新跑 `npm run build`；只跑 `cargo run` 不会自动帮你运行 Vite。

跑检查时用同一组核心输出，免得 EXE 和 DLL 版本打架：

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

Windows 有时会锁住已经加载的 DLL。这个时候换个独立构建目录，或者保存作品后自己重启 CSP 即可。

## 想了解实现或参与适配

- [核心接口](BACKEND.md)
- [桌面程序与核心的生命周期](docs/app-runtime.md)
- [前端开发](app/README.md)
