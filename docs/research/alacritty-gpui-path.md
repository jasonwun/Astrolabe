# alacritty_terminal 接入 GPUI 渲染的可行路径调研

> Research ticket: #3（parent: #1 roadmap，blocking: #7）
> 日期：2026-10-19 · 一次性研究分支 `research/alacritty-gpui-path`，不合并。

## TL;DR

**可行。** alacritty_terminal + GPUI 自渲染已有多条生产级先例（Zed、Paneflow、gpui-terminal）。
建议：crates.io 的 `alacritty_terminal = "=0.26.0"` 精确锁定（配套 `vte 0.15`），不依赖
zed-industries 的 fork；第一天就把引擎隔离在自有 `TerminalBackend` 边界后面（Paneflow 迁移
libghostty-vt 的最大教训）。日志流可以复用同一 VT/滚动基础设施（display-only Term）。

## 1. alacritty_terminal crate 现状

### 发布渠道与版本

- crates.io 正常发布，与 alacritty 主版本同步：最新 **0.26.0**（2026-04-06），前一版 0.25.1（2025-10-18）。
- Apache-2.0，MSRV 1.85（0.26），Rust 2024 edition。月下载约 42 万，被 138 个 crate 依赖。
- 关键依赖：`vte 0.15`（`ansi` feature，含 `vte::ansi::Processor`）、`parking_lot`（`FairMutex`）、
  `regex-automata`（搜索）、Unix 下 `rustix-openpty`、Windows 下 ConPTY（windows-sys）、`polling`（mio 系事件循环）、默认 `serde` derive。

### API 稳定性

- 0.x，无稳定性承诺，小版本之间有 breaking change（vte、Term API 都动过）。
- **必须 `=0.26.0` 精确锁定** 并提交 `Cargo.lock`；升级时按 alacritty CHANGELOG 逐项核对。
- Zed 用 fork（`zed-industries/alacritty`，pin rev `4c129667`），fork 增量主要是 serde 序列化 +
  `Term::snapshot()/restore()`（pty-host 持久会话 RFC #50584 用）。Astrolabe 若不做跨进程会话
  恢复，不需要这些补丁。Paneflow 的教训原话："寄生于别人编辑器的 fork 不可持续"，0.26 发布后
  即迁回 crates.io。

### 渲染面清单（Term/Grid 暴露了什么）

| 需求 | API |
|---|---|
| 可视 cell 迭代 | `Term::renderable_content()` → `RenderableContent { display_iter: GridIterator<IndexedCell>, cursor, selection, mode, display_offset }`；cell 含 `c`、`fg`/`bg`（`term::color::Color`：Named/Indexed/RGB）、`flags`（BOLD/ITALIC/DIM/UNDERLINE/UNDERCURL/STRIKEOUT/INVERSE/WIDE_CHAR/WIDE_CHAR_SPACER/WRAPLINE…）、`zerowidth()`（组合字符）、`hyperlink()`（OSC 8） |
| 光标 | `RenderableCursor { shape, point }`（Block/Underline/Beam/HollowBlock/Hidden），`TermMode::SHOW_CURSOR` |
| 模式 | `TermMode`（ALT_SCREEN、APP_CURSOR、APP_KEYPAD、BRACKETED_PASTE、SGR_MOUSE、MOUSE_*、VI、LINE_WRAP…） |
| 滚动 | `Term::scroll_display(Scroll::{Delta,PageUp,PageDown,Top,Bottom})`、`grid().display_offset()`、`history_size()`、`Config::scrolling_history` |
| **damage 追踪** | `Term::damage()` → `TermDamage::{Full, Partial(TermDamageIterator)}`（行级 `LineDamageBounds`），`reset_damage()`。alacritty 自己的渲染器用它做增量重绘；Zed 没用（全量 layout），spike 值得验证能否减少 GPUI 侧 shaping |
| 选择 | `Term.selection`、`selection_to_string()`、Semantic/Lines/Block 选择类型 |
| 搜索 | `term::search::{RegexSearch, RegexIter}`（regex-automata） |
| 尺寸 | `grid::Dimensions` trait（自己实现行/列数），`Term::resize` |
| 事件 | `event::EventListener::send_event`：Wakeup、Title、ResetTitle、ClipboardStore/Load、ColorRequest、PtyWrite（应用回写，如 DA 应答）、TextAreaSizeRequest、CursorBlinkingChange、Bell、Exit、ChildExit、MouseCursorDirty |
| PTY/事件循环 | `tty::new(Options, WindowSize, id)`、`event_loop::{EventLoop, Msg, Notifier}`：`Msg::{Input,Resize,Shutdown}` |

## 2. 先例调研

### 2.1 Zed（`crates/terminal` + `crates/terminal_view`，约 9kLOC）

- 依赖：`alacritty_terminal = { git = "https://github.com/zed-industries/alacritty", rev = "4c129667" }`（fork），`vte = "0.15"`。
- 桥接（`crates/terminal/src/alacritty.rs`）：
  - `Arc<FairMutex<Term<ZedListener>>>`；`ZedListener` 是 `futures::channel::mpsc::UnboundedSender<PtyEvent>`
    的 newtype，`Term` 每次 grid 变更回调 `send_event`。
  - 直接用 alacritty 自带的 `EventLoop::new(...).spawn()`（内部 polling/mio 线程做 PTY 读写 + vte advance）。
  - 输入回写：`Notifier` 发 `Msg::Input/Resize/Shutdown`。
  - GPUI 侧 drain 事件流 → `make_content()` 把 `renderable_content()` 全量拷成自有 `Content` 快照 → 渲染。
- 已知坑：Zed 的 terminal crate 与它的 settings/task/theme/release_channel 深度耦合，无法直接移植
  （boltz_terminal 文档明确放弃移植）；我们学结构，不搬代码。
- fork 的增量：serde derive + `Term::snapshot/restore`（pty-host 持久会话）。Zed 有过事件去重/后台线程
  合并 alacritty 事件的内存修复（EventAccumulator），说明 wakeup 风暴是真实问题。

### 2.2 Paneflow（GPUI 终端 multiplexer，github.com/arthjean/paneflow，GPL-3.0）

对 Astrolabe 最有参考价值的独立先例：同样从零做 GPUI 应用（不是编辑器内嵌）。

- 0.7.x 及以前：crates.io `alacritty_terminal = "0.26"`，生产验证。
- **不用** alacritty 的 `EventLoop::spawn()`，手写两条线程（`terminal/pty_loops.rs`）：
  1. `pty_reader_loop`：4KB 缓冲读 PTY → OSC 扫描（OSC 7 CWD、OSC 133 prompt 边界、XTVersion）→
     锁 Term 做 `vte Processor::advance` → **DEC 2026 同步输出合并**（`processor.sync_bytes_count() < n`
     才发 Wakeup），避免 neovim/btop 一帧产生几百个 wakeup。
  2. `pty_message_loop`：mpsc 收 `Msg`，写输入/resize 回 PTY master。
- GPUI 侧：`cx.spawn` 循环 drain wakeup 通道，最多 4ms / 100 事件批量后一次 `cx.update()` + 一次
  `cx.notify()`。keystroke→pixel 路径有 `PANEFLOW_LATENCY_PROBE=1` 探针。
- PTY 用 `portable-pty = "0.8"`（ConPTY/openpty 无 `#[cfg]`），平台差异只在进程 kill 和 CWD 探测两处。
- 教训（原文）：
  - 轮询定时器会抵消 `cx.notify()` 的收益——全部改 EventEmitter 推送模型后空闲重绘归零。
  - 方块字符（U+2580–U+259F）渲染缝隙：根因是自建方块字符覆盖表漏了 11 个码点落回默认字体路径。
  - UI 状态用 `Rc<Cell<_>>`/`Rc<RefCell<_>>`，不要 `Arc<Mutex<_>>`（GPUI 单线程渲染树）。
- **后续迁移警示**：0.8.0（2026-09）起默认引擎切到 Ghostty `libghostty-vt`，0.17.4 全平台 Ghostty、
  移除 Alacritty 回退。迁移博文给出的关键经验不是 "alacritty 不行"，而是：
  1. **引擎边界要第一天就建**——Alacritty 类型一旦渗进 PTY 循环/渲染/搜索/选择/会话生命周期，
     换引擎就是全局手术。他们新建了自有 `TerminalBackend` facade，两个引擎产出同一套命令/事件/
     模式/渲染快照。
  2. alacritty 迁移期间仍保留为差分测试基准（同一 VT 流多种分块喂两个引擎比快照）——这个
     差分测试思路 Astrolabe 的 spike 可以直接借用。

### 2.3 其他社区先例

- **gpui-terminal**（crates.io 0.1.0，2025-12）：现成 GPUI `TerminalView` 组件，alacritty_terminal +
  portable-pty + flume；后台读线程 → flume → async 任务过 VTE → notify。完成度低（scrollback/选择
  "planned"，鼠标部分），Linux 为主，但结构图和 input 模块可直接当 spike 起点参考。
- **zTerm**（dev.to，2026-01）：GPUI + alacritty_terminal + gpui-component 的个人项目，佐证路径可独立走通。
- **boltz_terminal**（docs.rs）：用 alacritty_terminal 的 VT 解析维护 screen grid 并以纯文本暴露，
  佐证 display-only/无渲染用法。

## 3. 数据流设计（集成方案草图）

```
┌─ PTY 读线程（每会话）─────────────────────────────────┐
│ PTY master ──read 4KB──► [OSC 扫描(可选)] ──► 锁 Term │
│ vte::ansi::Processor::advance(&mut term, buf)          │
│ 若 sync_bytes_count() < n（非同步输出窗口）            │
│     └─► listener.send(Wakeup) ──► channel              │
└────────────────────────────────────────────────────────┘
                          │
┌─ GPUI 主线程 ───────────▼─────────────────────────────┐
│ cx.spawn 任务批量 drain（≤4ms / ≤100 事件）           │
│ 锁 Term，拷贝 renderable_content() 为自有快照          │
│ cx.notify() → Render::render                          │
│   快照 → TextSystem.shape_line 逐 run shaping → 绘制   │
│   背景 rect 按同色 run 合批；光标/选择单独画           │
└────────────────────────────────────────────────────────┘
┌─ 输入回写 ─────────────────────────────────────────────┐
│ GPUI key/mouse/paste 事件                              │
│  → 按 TermMode 编码（APP_CURSOR/BRACKETED_PASTE/       │
│    SGR_MOUSE/kitty 协议暂不做）                        │
│  → mpsc → pty_message_loop → write PTY master          │
│  → resize: Term::resize + Msg::Resize（SIGWINCH）      │
└────────────────────────────────────────────────────────┘
```

要点：

1. **方案选择**：两条路线——(A) 用 alacritty 自带 `EventLoop::spawn()`（Zed 路线，代码少）；
   (B) 手写 reader/writer 线程（Paneflow 路线，能做 OSC 扫描、同步输出合并、精细 shutdown）。
   **建议 spike 先走 (A) 快速打通，架构上按 (B) 预留**；Kubernetes 日志 follow 的高吞吐场景
   大概率需要 (B) 的合并策略。
2. **锁模型**：`Arc<FairMutex<Term<L>>>`。advance 时持锁；渲染侧只在拷贝快照时短暂持锁；
   锁内不做 shaping/绘制。FairMutex 防渲染长帧饿死写线程。
3. **executor 关系**：alacritty 事件循环/手写线程都是同步线程（polling/mio 或阻塞 read），
   **与 tokio 无关**；与 GPUI executor（smol 系）之间用 futures channel / flume 桥接即可。
   Astrolabe 的 k8s client 若用 tokio，PTY 线程保持独立，不要混进 tokio runtime。
4. **背压与合并**：wakeup 必须合并（同步输出窗口 + 4ms 批量化），否则 busy TUI 造成 wakeup
   风暴（Zed 修过这类内存/事件堆积 bug）。事件去重参考 Zed 的 EventAccumulator：Title 类只留
   最新，Wakeup/Bell 可折叠，PtyWrite/ClipboardLoad 不可折叠。
5. **渲染**：v1 全量 `renderable_content()` 快照 + GPUI shaping（Zed 就这么干，性能够）；
   `TermDamage` 行级 damage 作为 v2 优化在 spike 里做一次对照实验。
6. **引擎边界**：自有 `trait TerminalBackend`（命令/事件/模式/搜索/渲染快照均为自有类型），
   alacritty 类型不出 backend 模块。这是 Paneflow 用一次引擎迁移换来的教训，成本极低、收益极大。

## 4. 日志流能否复用同一 VT/滚动基础设施

**判断：可以，且应当。**

- 建一个无 PTY 的 display-only `Term`（`Term::new(config, &dimensions, listener)`，config 里关掉
  OSC52、alternate scroll 等），把 ANSI 日志字节流直接喂 `vte::ansi::Processor::advance`，
  即免费获得：ANSI 16/256/truecolor 颜色、scrollback、正则搜索、选择复制、OSC 8 超链接——
  与终端共用同一渲染管线和滚动/搜索 UI。
- Zed 有同款先例（`display_only_term_config`，用于 task 输出等无 PTY 终端）。
- 注意事项：
  - 高吞吐 follow（`kubectl logs -f` 风暴）需要与 PTY 读线程相同的批量喂入 + wakeup 合并；
    这也是倾向手写读循环（方案 B）的理由之一。
  - 日志源通常不需要 alt screen/mouse/输入回写，backend trait 上体现为能力子集。
  - 超长行/reflow、百万行级 scrollback 内存要在 spike 里实测（grid cell 不大，但行数无上限会爆）。

## 5. 终端 spike 检查清单（逐项验证，打勾才算过）

**集成与构建**
- [ ] `alacritty_terminal = "=0.26.0"` + GPUI 在 macOS（Apple Silicon）编译通过，无 fork、无 patch
- [ ] Linux 编译通过（CI 或本机容器）；vte 版本与直接依赖需求对齐（0.15）

**PTY 生命周期**
- [ ] `tty::new` 拉起 shell，`TERM`/`COLORTERM` 环境正确；窗口初始尺寸正确（行列数）
- [ ] 子进程退出 → `ChildExit` 事件 → UI 正确收尾；`Msg::Shutdown` 后无线程泄漏/zombie

**输出→重绘链路**
- [ ] Wakeup → `cx.notify` 全链路打通；keystroke→pixel 延迟可接受（加探针实测）
- [ ] 大批量输出（`cat` 大文件 / `yes`）下不卡死：4ms/100 事件批量化生效
- [ ] DEC 2026 同步输出合并生效：neovim/btop 不闪、wakeup 数量级下降（若走方案 A，验证 EventLoop 内部行为是否已合并；不够则切方案 B）
- [ ] damage 对照实验：`TermDamage` 增量 vs 全量快照的 CPU/shaping 开销（结论记录即可，v1 可全量）

**渲染正确性**
- [ ] 16/256/truecolor 颜色映射到 GPUI `Hsla` 正确；INVERSE/DIM/BOLD/ITALIC/underline/undercurl/strikeout
- [ ] 宽字符（CJK）、零宽/组合字符（`zerowidth()`）、emoji 宽度正确
- [ ] 方块/进度条字符 U+2580–U+259F 无缝隙（覆盖表完整，别落回默认字体路径——Paneflow 踩坑）
- [ ] 光标五种形状 + 隐藏 + 跟随 alt screen
- [ ] resize：行列重算 + `Msg::Resize` + TUI（vim/htop）收到 SIGWINCH 正确重排；reflow 不丢内容

**滚动与选择**
- [ ] scrollback：滚轮/`scroll_display`/`display_offset` 正确；AlternateScroll 行为
- [ ] alt screen 切换（vim/htop/less 进出）内容不串
- [ ] 鼠标选择 + 复制（`selection_to_string`）；OSC 52 ClipboardStore
- [ ] 正则搜索（RegexSearch）高亮

**输入回写**
- [ ] 普通键、Ctrl/Alt 组合、功能键、方向键（含 APP_CURSOR 模式）编码正确
- [ ] 粘贴：bracketed paste（`\x1b[200~…\x1b[201~`）按 `TermMode::BRACKETED_PASTE` 包裹
- [ ] 鼠标事件 SGR 编码（htop 可点）；IME 输入不炸（GPUI 输入法路径）
- [ ] 应用回写（`Event::PtyWrite`，如 DA/device attributes 应答）正确送达 PTY

**日志流（display-only 复用验证）**
- [ ] 无 PTY Term 喂 ANSI 日志流，颜色/滚动/搜索/选择与终端同管线渲染
- [ ] 高吞吐 follow（模拟 `kubectl logs -f` 10k 行/s）下 UI 不卡、内存有界（scrollback 上限）
- [ ] 截断/坏序列、跨 read 边界的转义序列不 panic（可借 Paneflow 式差分/fuzz 思路）

**事件与边角**
- [ ] Title（OSC 0/2）、Bell、OSC 7 CWD（若做）事件到达 UI
- [ ] OSC 8 超链接可点
- [ ] 多终端并存（≥4 会话）CPU/内存可接受

## 6. 风险与开放问题

- **API 漂移**：0.x crate，升级 = 人工核对。缓解：精确锁版本 + backend 边界隔离。
- **libghostty-vt 的诱惑**：Paneflow 最终迁了过去（增量渲染状态、grapheme/reflow、输入编码开箱）。
  其 C ABI 仍不稳定且要静态链接 Zig 产物，接入成本高；alacritty_terminal 是纯 Rust、crates.io 直取。
  建议仍以 alacritty_terminal 起步，backend trait 保住未来换引擎的可能。
- **fork 之需**：只有在做跨进程会话持久化（pty-host 式 snapshot/restore）时才需要 Zed fork 的 serde
  补丁； roadmap 若排到该项再评估（届时上游 0.26 已默认带 serde derive，可能只需薄补丁）。
- **键盘映射工作量**：alacritty_terminal 不含键盘→转义序列映射（那部分在 alacritty 主 crate），
  需自写（参考 Zed `terminal/src/mappings/` 或 gpui-terminal `input` 模块）。这是 spike 中容易被
  低估的一块。

## 参考

- crates.io / lib.rs：alacritty_terminal 0.26.0（2026-04-06）、0.25.1（2025-10-18）
- docs.rs/alacritty_terminal 0.26.0（term 模块：RenderableContent/TermDamage/…）
- Zed：`crates/terminal/src/alacritty.rs`、`crates/terminal_view/`；workspace Cargo.toml pin
  `zed-industries/alacritty rev 4c129667`；pty-host RFC：zed-industries/zed Discussion #50584
- Paneflow：dev.to《Building a native terminal for AI coding agents in Rust + GPUI》（2026-05）；
  github.com/arthjean/paneflow（ARCHITECTURE.md）；《Paneflow 0.8.0：Linux 终端引擎从 Alacritty
  切换到 libghostty-vt》（paneflow.dev/blog/libghostty-linux，2026-09）
- gpui-terminal（crates.io 0.1.0）；zTerm（dev.to，2026-01）；boltz_terminal（docs.rs）
