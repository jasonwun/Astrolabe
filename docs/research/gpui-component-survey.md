# gpui-component 组件盘点：DataTable / Editor / DockArea 能力现状与差距

> 对应 issue：jasonwun/Astrolabe#2
> 调研日期：2026-10-19 ｜ 调研对象：`longbridge/gpui-kit`（原 `longbridge/gpui-component`）main @ `6c13c1fc`、release tag `v0.8.0` @ `1676826d9`
> 调研方法：浅克隆仓库通读 `crates/component/src/{table,input,dock,highlighter}` 与 `crates/base/src/{input,dock}` 源码，核对 crates.io 发布记录。

## 0. 重要发现：仓库已更名、依赖关系已重构

原 `longbridge/gpui-component` 仓库已更名为 **`longbridge/gpui-kit`**（GitHub 自动重定向），并从单一组件库扩展为三层结构：

```
gpui-kit          伞 crate，应用只需依赖这一个（pin 匹配的 GPUI 版本并 re-export）
├── gpui-base     无样式行为层：编辑引擎、dock 布局树、virtual list 等
└── gpui-component  完整带样式组件库（DataTable、Editor、Dock 皮肤等）
```

### GPUI 版本对应关系（它跟踪 GPUI 的什么 ref）

gpui-kit **不再直接依赖 zed 的 git rev**，而是依赖 crates.io 上的 **`gpui-pre`** 包：

| gpui-kit 版本 | GPUI 依赖 | 说明 |
|---|---|---|
| ≤ v0.4.2 | `gpui = "0.2.2"`（crates.io） | zed-industries 官方发布的 `gpui` crate，已停更（最后更新 2025-10-22，0.2.2） |
| v0.6.0 | `gpui-pre = "0.3.1"` | 切换至 gpui-pre |
| v0.7.0 | `gpui-pre = "=0.3.7"` | 开始用 `=` 精确锁定 |
| v0.8.0（当前最新） | `gpui-pre = "=0.3.9"` | **zed@`93d74566`（2026-10-10）的快照** |

关键事实：

- `gpui-pre` 由 **huacnlee（Jason Lee，Longbridge / gpui-kit 作者，也是 Zed 贡献者）个人账号发布**，描述注明「gpui-pre snapshot of zed@93d7456」。
- 发布节奏为**每周一次**（0.3.3 起每周一前后：09-07、09-14、09-21、09-28、10-05、10-12），即每周快照 zed main。
- gpui-kit 用 `=` 精确版本锁定 gpui-pre，因此**锁定 gpui-kit 版本即锁定了 GPUI 快照**，Cargo.lock 会固定两者。
- 更早的 gpui-component 时代（v0.2.0 之前）曾直接 pin zed git rev；该模式已废弃。

### 建议锁定的 commit 对

```
gpui-kit = "=0.8.0"        # tag v0.8.0 = commit 1676826d9d49b90f4d7b6808b69924f58796f6f1
   └─ gpui-pre = "=0.3.9"  # 自动带入 = zed-industries/zed @ 93d745668805029476e6d7fbb19652116fed51de（2026-10-10）
```

理由：v0.8.0 是当前最新 release（2026-10，workspace 版本号 0.8.0），pin crates 版本比 pin git commit 更利于可重复构建；gpui-pre 的精确锁定使 GPUI 侧无需额外 pin。升级策略建议跟随 gpui-kit 的 release（其本身每周跟进 zed），不要自行追 gpui-pre 的新版本——gpui-kit 的 `=` 锁定意味着 gpui-pre 升级必须等 gpui-kit 发版。

风险备注：gpui-pre 是单一维护者（huacnlee）发布的非官方快照；若该渠道中断，回退方案是自己 fork zed 发快照或短期 pin git rev。这是依赖链上最主要的风险项。

## 1. DataTable —— 结论：开箱可用（附少量注意点）

源码：`crates/component/src/table/`（`data_table.rs`、`state.rs` ~3000 行、`delegate.rs`）。

| 硬性需求 | 现状 | 判定 |
|---|---|---|
| 虚拟化 | 行体使用 GPUI 的 `uniform_list` 实现（`state.rs` L2655），只渲染可见行；README 宣称「hundreds of thousands of rows」 | ✅ 开箱 |
| 1 万行数据 | 远在其设计量级之内 | ✅ 开箱 |
| 排序 | 表头点击、排序状态机（Default/Asc/Desc）、排序图标均内置；**实际比较逻辑由 delegate 的 `perform_sort` 实现**（框架只给状态与回调） | ✅ 开箱（排序比较器自己写，属正常分工） |
| 选中 | 行 / 列 / 单元格三种选中模式内置，含键盘导航（方向键、Tab、Home/End、PageUp/Down）、行头列、右键菜单、双击 | ✅ 开箱 |
| 20 次/秒增量更新 | 数据更新路径 = 改 delegate 数据 + `cx.notify()`，uniform_list 每帧只重建可见行元素，架构上可行；**无现成基准测试，需原型实测**（见差距清单 G1） | ⚠️ 架构支持，待实测 |

注意点：

- **行高必须均匀**：`uniform_list` 要求固定行高（行高由 `Size::table_row_height()` 给出）。K8s 资源列表行高均匀，满足；若未来需要变高行（如展开式行），需换 `gpui-base` 的 `VirtualList`（支持不同尺寸 item）或自建。
- 列能力附带：可拖拽调宽、拖拽换序、左侧冻结列、双击自适应宽度（`fit_column`）、多级表头分组。

## 2. Editor —— 结论：开箱可用 + 需小改（YAML 缩进规则与 feature 开关）

组件层 `crates/component/src/input/editor.rs` 是皮肤；引擎在 `crates/base/src/input/`（`base/` 共享编辑引擎 + `editor/` 代码编辑器部分：display_map、高亮、diagnostics、decorations、缩进、LSP）。

| 硬性需求 | 现状 | 判定 |
|---|---|---|
| YAML 语法高亮 | **高亮机制是 tree-sitter**（`crates/component/src/highlighter/`，增量解析 + 解析时间预算 + 语言注入），YAML 语法在 `tree-sitter-yaml` feature 下内置（含于 `tree-sitter-languages` 集合）；另有 syntect 备选后端（~200 语言、无语法树）和 regex 降级 | ✅ 开箱（需开 feature） |
| 编辑 | Rope 文本结构、多光标、bracket 自动闭合、软换行、缩进参考线、代码折叠、搜索，LSP diagnostics/completion/hover 接口均有 | ✅ 开箱 |
| undo/redo | 完整 undo manager（事务化、同类击键合并、光标位置随 undo 恢复，上限 1000 事务 × 1000 change/事务） | ✅ 开箱 |
| YAML 缩进延续 | Enter 默认保留上一行缩进（`indent_of_next_line`）；**冒号后自动增加一级缩进不会生效**：`LanguageConfig` 默认 `indentation_rules: None`，且组件层只预置了 text/json/python 三种语言配置 | ⚠️ 需小改 |
| 2MB 级文件表现 | 官方宣称「Stable performance at 200K lines with Tree-sitter highlighting」；2MB YAML ≈ 3–8 万行，在宣称范围内。高亮为增量 tree-sitter 解析（带时间预算与注入字节上限），编辑为 Rope，预期流畅；**需以真实 2MB Deployment/CRD YAML 原型验证**（见 G2） | ⚠️ 预期达标，待实测 |

「需小改」的具体内容（均有现成扩展点，非 fork）：

1. 开启 feature：`gpui-kit = { version = "=0.8.0", features = ["tree-sitter-yaml"] }`（或 `tree-sitter-languages` 全集）。
2. 注册 YAML `LanguageConfig`：在 `gpui-component::init` 之后调用公开的 `set_language_provider`，为 `yaml` 提供 `IndentationRules`（可复用其 Python 配置的同款模式 `[\{\(\[:]\s*$` 作 increase_indent_pattern）。约数十行代码。
3. 若需要 `- ` 列表项换行自动延续（Freelens 编辑器体验），`IndentationRules` 的两个正则不足以表达，需要评估是否自行处理 Enter action —— 这是 Editor 上唯一的潜在「需 fork」点，建议原型阶段先验证是否可接受。

## 3. DockArea —— 结论：开箱可用

架构：布局树、拖拽几何、激活面板状态机、持久化 schema 在 `gpui_base::dock`；`gpui-component::dock` 是皮肤（`DockSkin`），`DockArea::new(...).with_renderer(DockSkin::new(cx))`。

- 布局能力：中心 + 左/右/下三边 dock、tab 合并、**嵌套 split**、面板拖拽换位、边缘折叠、zoom，布局**可序列化**（`DockState`/`PanelState::dump`）。
- **固定布局直接支持**：`DockArea::set_locked(true)` 锁定整个布局树（`dock_area.rs` L217），配合面板的 `closable(false)` / `zoomable(false)` 即可表达不可变的「列表 + 详情 + 终端 + 日志」四区布局；用户可调的只剩分割比例。
- 多面板表达：中心放详情（tab 化多个资源详情），左 dock 放资源列表，下 dock 放日志 + 终端 tab 组 —— 是其标准用法，`examples/dock`（608 行）有完整示例。
- 成本：每个面板需实现 `gpui_base::dock::Panel`（行为：id/title/closable）+ `gpui_component::dock::Panel`（展示：title 渲染等）两个 trait，模板化后每个面板约 30–50 行样板。

## 4. 差距清单（后续执行 issue 的来源）

| # | 差距 | 级别 | 建议 |
|---|---|---|---|
| G1 | DataTable 在 20 次/秒增量更新 + 1 万行下的帧率无现成数据 | 需验证 | 建原型：watch 流模拟 20Hz 行更新，profile 帧耗时 |
| G2 | Editor 打开 2MB YAML 的真实表现（滚动/编辑/高亮）未经实测 | 需验证 | 建原型：真实大 CRD YAML + tree-sitter-yaml，测首开时间与编辑延迟 |
| G3 | **无终端仿真器组件**：组件目录中没有 terminal，K8s exec/log 终端需自建（如集成 alacritty_terminal / wezterm-term 类的 PTY + 自绘，或纯只读日志用 Editor/VirtualList 替代） | 需自建 | 单独 issue 评估终端方案 |
| G4 | YAML 缩进延续需注册 `LanguageConfig`；`- ` 列表项自动延续可能需要更深介入 | 需小改（可能升级） | 原型阶段定夺 |
| G5 | 排序比较逻辑（含 K8s 语义排序，如 age、数值量纲 1000m vs 1）需自实现 | 需小改 | 在资源列表层做通用 comparator |
| G6 | 变高行 / 行内展开（如 Pod 展开 containers）超出 uniform_list 能力 | 需自建（如需要） | 用 `VirtualList` 或自建行组件；列入未来需求 |
| G7 | 依赖链风险：gpui-pre 为 huacnlee 单点发布的 zed 快照 | 风险项 | 接受并跟随 gpui-kit release；记录回退方案（自 fork zed 快照） |

## 5. 逐项总结论

| 组件 | 结论 |
|---|---|
| DataTable | **开箱可用**（均匀行高前提下；20Hz 更新需原型实测 G1） |
| Editor | **开箱可用 + 需小改**（开 `tree-sitter-yaml` feature、注册 YAML 缩进规则；2MB 表现预期达标、待实测 G2；`- ` 延续是潜在 fork 点 G4） |
| DockArea | **开箱可用**（`set_locked` + 面板 `closable(false)` 即固定多面板布局；终端面板本身需自建 G3） |
| 锁定版本 | **gpui-kit `=0.8.0`（commit `1676826d9`）→ 自动锁定 gpui-pre `=0.3.9`（zed@`93d74566`，2026-10-10）** |

