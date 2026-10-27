# 画布首键丢失：信号分析与尝试记录

这份笔记记录 v0.1.0 遗留问题——**在 CSP 画布上点击/拖拽之后，第一次按快捷键可能不生效**——的现场数据、根因分析和两次失败尝试，供后续方案参考。

结论先说：**问题不在监听或取消组字本身，而在“这次组字到底是不是文字输入”缺少稳定信号。**任何在按键层面“猜”的补发方案，都会在文本与画布之间来回误判。

## 1. 现行判定机制

DLL 在目标进程 UI 线程装两层钩子：`WH_GETMESSAGE`（取消息后）与 `WH_CALLWNDPROC`（发送消息）。所有 IMM 操作都在该线程执行。

观测到的事件分三类：

| 类别 | 消息 | 用途 |
|---|---|---|
| 上下文切换 | `WM_SETFOCUS`、`WM_KILLFOCUS`、`WM_ACTIVATE`(非激活)、`WM_ACTIVATEAPP`(切出)、`WM_LBUTTONDOWN`、`WM_RBUTTONDOWN`、`WM_POINTERDOWN`、`WM_INPUTLANGCHANGEREQUEST`、`WM_NCDESTROY`、`WM_IME_SETCONTEXT`(wParam=0) | 立即把 IME 挂回，清空候选证据 |
| 文字意图证据 | `WM_IME_NOTIFY` + `IMN_SETCANDIDATEPOS`(wParam=9) | 记录 `(hwnd, tick)`，**100 ms 内且同一窗口**有效 |
| 判定入口 | `WM_IME_STARTCOMPOSITION`（只认 posted，且焦点为该 hwnd） | 唯一做决定的位置 |

判定顺序（`crates/policy/src/lib.rs::decide`）：

```text
未启用 / 程序在 IME 白名单 / 系统插入符存在 / 有候选证据
/ 控件类是 Edit 或 RichEdit*            → Allow（放行，挂回 IME）
exe 匹配预设 && 控件类匹配 non_text_classes → Block（取消组字 + 解绑该窗口 HIMC）
其余                                     → Allow（fail-open）
```

`--block` 是启动授权门槛；运行时只能对已授权会话开关拦截。请求状态与真实应用状态分开记录，`applied_enabled` 保持 unknown。

## 2. 实测信号差异

三份采样日志（`logs/watch-*.jsonl`）显示：**画布与文本框的差异，在于 `wParam=9` 由谁发出、什么时候发出。**

### 2.1 画布

```text
110.400 WM_KEYDOWN  wp=229 (VK_PROCESSKEY)
110.400 WM_IME_NOTIFY wp=5  (IMN_OPENCANDIDATE)   hwnd=0x8054c
110.401 WM_IME_STARTCOMPOSITION                    hwnd=0x8054c
+19 ms  WM_IME_NOTIFY wp=9                         hwnd=0x40d12 (IME 辅助窗口)
```

- 目标窗口**从不**发 `wParam=9`；
- 只有 IME 自己的窗口（`IME`、`MSCTFIME UI`）在组字**之后**补发定位；
- 因此“有没有候选框”不能当作文字证据——画布上也会开候选窗口。

### 2.2 文本框（图层名）

```text
129.454 WM_KEYDOWN wp=229
129.454 WM_IME_NOTIFY wp=9   hwnd=0x70d7a  ← 与组字同一窗口，组字前 <1 ms
129.455 WM_IME_STARTCOMPOSITION hwnd=0x70d7a
```

判据应是：**`wParam=9` 来自正在组字的那个窗口本身**，而不是“候选框出现”。

### 2.3 类名不可靠

```text
0x51032  …20DFC6D08040（主类）  block=9  allow=6  cand=6
0xf1248  …5266782BDE30（子类）  allow=3  cand=3
0x80f44  …5266782BDE30（同一子类）block=1  cand=0
```

同一个类名既可能是画布也可能是文本控件，按类名列白名单/黑名单都不成立；层级文本控件（图层名、文本框）尤其如此。

## 3. 首键丢失的三种成因

1. **点击把 IME 挂回来。** 为了让文本框可用，任何 `WM_LBUTTONDOWN` 都会 `restore()`。画布点击后，下一次按键先被 IME 吃掉（`VK_PROCESSKEY`），随后组字被取消，该键不会回到应用。
2. **组字未结束时的按键。** 已存在组字时，后续按键直接并入，不再发 `WM_IME_STARTCOMPOSITION`：

   ```text
   116.065 STARTCOMPOSITION
   118.100 WM_KEYDOWN wp=229   ← 被吃，没有 START
   124.809 WM_KEYDOWN wp=229   ← 同样没有 START
   ```

   这类按键不进入判定，既没取消也没补发。
3. **START 与 KEYDOWN 的间隔不稳定。** 生产中观察到 `KEYDOWN` 与 `STARTCOMPOSITION` 相隔可达 ~80 ms，期间状态不可控。

## 4. 两次尝试与失败原因

两次尝试都做成过“画布正常”，但都以文本输入回归收场。

### 4.1 在组字开始时补发（v0.1.1 初版）

做法：组字判定为画布后，取消组字、解绑，再按原始扫描码 `SendInput` 补一次。

结果：画布首键恢复；但成因 2 的按键仍救不回来（没有 START 可挂）。

### 4.2 在“键被接管”时结算（20 ms 宽限）

做法：`WM_KEYDOWN wp=229` 时记住扫描码与窗口，启 20 ms 定时器；若同窗口在此期间发 `wParam=9` 视为文本，否则当画布结算。

结果：文本输入被打断。日志：

```text
101.383 decision composition_start block=false candidate=true  ← 文本，判定正确
101.558 decision claimed_key       block=true  candidate=false ← 175 ms 后误判为画布
101.559 detached + replayed(scan=22)
```

原因：定时器被忙碌的 UI 线程拖到 175 ms 才跑，此时 100 ms 证据已过期；而**文本框继续打字不会重复发 `wParam=9`**。

补丁“记住已判为文本的组字窗口”仍未解决问题：随后画布快捷键反而把输入法唤了出来。两次都在 0.1.x 内回退。

## 5. 后续方向（未验证）

优先级从“少猜”到“多猜”：

1. **不做补发，改为阻止 IME 吃键。** 在 `wp=229` 之前就解绑需要预知文本态，风险同上；若能在按键前判定焦点控件类型，可以从根上避免。
2. **改变文本态判定源。** 目前只有“同窗口 `wParam=9`”。可考虑 `WM_IME_SETCONTEXT(wParam=1)`、`ImmGetContext` 是否有效、组字串长度、UIA/MSAA 控件类型等，但都要先采集数据证明其稳定性。
3. **接受 0.1.0 的取舍。** 点击后可能丢一键，但文本输入与画布快捷键都不会被误伤——这是目前唯一经过现场验收的组合。
4. **若继续走补发，必须有“文本态记忆”且覆盖组字未结束的情况，并有逐场景回归**（画布单键、画布连续、文本首键、文本连续、切换工具、切换窗口）。

## 6. 复现采样方法

只读采样（不解绑、不取消，输入法行为等同于未运行 Paxile）：

```bash
tail -f /dev/null | ./target/reconnect/debug/paxile-probe.exe watch --spy \
  --config "%APPDATA%/com.paxile.app/config.json"
```

日志落在 `logs/watch-<时间戳>.jsonl`，每条记录都带 `t`（QPC 秒）、`tid`、`hwnd`、`msg`、`wp`、`lp`、`src`（post/send）。比较时至少要看：

- `WM_KEYDOWN` 的 `wp` 是否等于 229（`VK_PROCESSKEY`）；
- `WM_IME_NOTIFY wParam=9` 的 `hwnd` 是否与组字窗口相同；
- `WM_IME_STARTCOMPOSITION` 与 `KEYDOWN` 的时间差；
- 有没有 `KEYDOWN 229` 但完全没有 `STARTCOMPOSITION` 的按键。

用这三条就能判断一次失败属于“被点击恢复吞掉”“并入已有组字”还是“信号缺失”。
