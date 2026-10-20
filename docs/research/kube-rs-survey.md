# kube-rs 能力盘点（issue #4）

> 研究日期：2026-10-19。调研对象：[kube-rs/kube](https://github.com/kube-rs/kube) 最新主线，
> 当前最新 release 为 **kube 4.0.0（2026-06-16）**，配套 k8s-openapi 0.28（Kubernetes v1_36），MSRV 1.82。
> 本文逐项给出结论、最小代码路径、已知坑。第 4 节是「apply 错误 → YAML 行号映射」（issue #10）的前置事实。

## 结论速览

| 能力 | 结论 | API 入口 |
| --- | --- | --- |
| exec 认证插件 | 开箱支持，token 自动刷新（4.0 已修掉主要阻塞/解析问题） | `Client::try_default()`（kubeconfig `exec:` 段） |
| pod portforward | 纯 kube-rs 可用，websocket 实现；要自己起本地 TCP listener | `Api::<Pod>::portforward` → `Portforwarder` |
| exec / attach | 完整支持 TTY、stdin/stdout/stderr 通道、终端 resize | `Api::<Pod>::exec/attach` + `AttachParams` → `AttachedProcess` |
| server-side apply | 一等支持；validation 错误结构化（`causes[].field`），**ownership 冲突只有文本消息** | `Api::patch` + `Patch::Apply` + `PatchParams::apply` |

建议锁定版本：**`kube = "4.0"`（features: `ws`, `rustls-tls`）+ `k8s-openapi = "0.28"`**。
`ws` feature 隐含 `client`；GCP ADC 认证另加 `oauth`，kubeconfig 内 OIDC provider 刷新另加 `oidc`。

---

## 1. exec 认证插件（EKS/GKE/AKS）

**结论：支持。** kubeconfig 中 `users[].user.exec` 段（client-go credential plugin 协议）被完整实现：
`aws eks get-token`、`gke-gcloud-auth-plugin`、`kubelogin`（az aks get-credentials 生成的配置用的就是 kubelogin）
都走同一条路径，无需为每家云写专门代码。代价与 kubectl 相同：**用户机器上必须安装对应 CLI**。

实现要点（`kube-client/src/client/auth/mod.rs`）：

- `Auth::try_from(&AuthInfo)` 读取 `exec` 配置，以子进程方式运行插件命令，设置
  `KUBERNETES_EXEC_INFO` 环境变量（含 `interactive` 标志；`provideClusterInfo: true` 时携带 cluster 信息）。
- **Token 刷新**：插件返回 `expirationTimestamp` 时封装为 `RefreshableToken::Exec`，挂在 tower 的
  `AsyncFilterLayer`（`AuthLayer`）上，每个请求前检查 `now + 60s >= expiry` 则重跑插件刷新。
  刷新路径自 3.1.0 起走 `tokio::task::spawn_blocking`，不会阻塞 runtime worker（PR #1950）。
- 4.0.0 起 exec 插件输出可用 YAML 解析（`serde_saphyr`），兼容只输出 YAML 的插件（PR #2003）。
- 交互式插件（如 kubelogin device code 流程需要终端交互）：`interactiveMode` 非 `Never` 时
  stdin/stderr 继承父进程，Windows 下 `CREATE_NO_WINDOW` 由环境变量
  `KUBE_RS_UNSTABLE_CREATE_NO_WINDOW=1` 显式开启（默认关，因为会破坏交互式插件的 stderr 继承）。

已知坑：

1. **首次取 token 是同步阻塞调用**：`Auth::try_from` 用 `std::process::Command`，发生在
   `Client::try_default()` / 配置加载阶段。在 GUI（GPUI）里不要在 UI 线程构造 Client，放后台线程池。
2. **legacy `auth-provider: azure` 明确不支持**，报错信息直接指向 kubelogin。AKS 旧 kubeconfig 需要
   用户先跑 `kubelogin convert-kubeconfig`（kubectl 同样如此，行为一致）。
3. legacy `auth-provider: gcp`（cmd-path 方式）支持；GCP Application Default Credentials 需 `oauth` feature。
4. kubeconfig `auth-provider: oidc` 的 token 刷新需 `oidc` feature（0.84+，PR #1229）；不开则只有静态 id-token。
5. 插件命令行参数解析（gcp legacy provider 的 `cmd-args`）按空格切分，带空格的路径/参数有风险——
   但现代 kubeconfig 都用 `exec:` 段（args 是数组），不受影响。

## 2. pod portforward

**结论：可用，纯 kube-rs 实现（不起 kubectl）。** 但它是"传输层"API：只给每端口一个
`AsyncRead + AsyncWrite` 流，**不绑定本地端口**——本地 TCP listener + accept loop + 双向 copy 要自己写
（examples 里有 `port_forward*.rs` 三个示例可抄）。

最小路径：

```rust
let pods: Api<Pod> = Api::namespaced(client, "default");
let mut pf = pods.portforward("my-pod", &[8080, 9090]).await?; // ws feature
let mut stream = pf.take_stream(8080).unwrap(); // 每端口只能 take 一次
// 另取 pf.take_error(8080) 监听服务端异常（端口不可用时由 server 写入）
```

已知坑：

1. **无 SPDY 回退**：kube-rs 只实现 WebSocket 传输（KEP-4006），apiserver 需 ≥ ~1.30
   （PortForwardWebsockets / RemoteCommandWebsockets feature gate）。更老的集群直接 upgrade 失败，
   表现为 `Error::UpgradeConnection`。Freelens 时代靠 kubectl 进程天然兼容老集群，这是能力回退点，
   需要在文档/错误提示里明确。
2. `take_stream` 每端口至多返回一次；error channel（`take_error`）同样，future resolve 即视为该端口不可用。
3. 连接清理、后台任务取消的边界问题在 0.7x–0.9x 陆续修过（PR #854、#973）；3.0 加了 websocket
   ping keepalive（PR #1887/#1889），长连接过 LB/代理不会被 idle timeout 杀掉。**不要锁在 3.0 之前**。
4. 多端口：一次 `portforward(name, &[ports])` 调用可带多个端口，共享一条 websocket 连接。

## 3. exec / attach（终端 spike 的数据源）

**结论：能力完整。** `Api::<Pod>::exec(name, command, &AttachParams)` / `attach(name, &AttachParams)`
（均需 `ws` feature），返回 `AttachedProcess`：

- `stdin()` → `impl AsyncWrite`（`AttachParams.stdin` 为 true 时可用）
- `stdout()` / `stderr()` → `impl AsyncRead`
- `terminal_size()` → 可写 `TerminalSize { width, height }`，即 **PTY resize 通道已实现**
  （0.74 加入，PR #983）。注意 `AttachParams.tty` 字段文档里 "Terminal resizing is not implemented yet"
  的注释已过时，以 `AttachedProcess::terminal_size()` 为准。
- `take_status()` 取进程退出状态；`join()` 等待远端命令结束；`abort()` 中止。
- 缓冲区上限可调：`max_stdin_buf_size` / `max_stdout_buf_size` / `max_stderr_buf_size`（默认 1024 字节，
  大输出场景建议调大）。

通道模型：websocket 消息首字节为 channel id（stdin=0、stdout=1、stderr=2、error=3、resize=4），
kube-rs 内部解复用，上层拿到的是干净的字节流。

已知坑：

1. **`tty=true` 时 `stderr` 必须为 false**（apimachinery 协议约束，TTY 模式不做多路复用），
   否则 `AttachParams` 校验直接拒绝。交互 shell 场景 `AttachParams::interactive_tty()`（stdin+stdout+tty）。
2. 同 portforward：**WebSocket only，无 SPDY 回退，apiserver 需 ≥ ~1.30**。
3. 长 idle 的 attach 会话在 3.0 之前有被中间代理断连的问题（已修，websocket ping）。
4. stdin 关闭后 stdout 仍可能有数据——2.0.1 修复了过早退出的问题（PR #1693），旧版本会丢尾部输出。
5. 4.0 修了 ws task 泄漏和 `join()` 死锁（PR #1978）——又一个锁 4.0 的理由。

## 4. server-side apply（Patch::Apply）与错误体结构

**结论：SSA 一等支持。** 最小路径：

```rust
use kube::api::{Api, Patch, PatchParams};

let ssapply = PatchParams::apply("astrolabe").force(); // force 仅对 Apply 有效
let patch = serde_json::json!({
    "apiVersion": "apps/v1", "kind": "Deployment",
    "metadata": { "name": "nginx", "namespace": "default" },
    "spec": { "replicas": 2 }
});
let obj = deploys.patch("nginx", &ssapply, &Patch::Apply(&patch)).await?;
```

- `field_manager` 对 Apply **必填**（`PatchParams::apply("...")`）；`.force()` 覆盖 ownership 冲突；
  `.dry_run()` 可预览；`.validate_strict()` 对应 `fieldValidation=Strict`（未知/重复字段报错，
  `Warn` 则走 warning）。
- `Patch::Apply` 序列化为 `application/apply-patch+yaml`（JSON 是合法 YAML，直接发 JSON 即可）。
- 注意：apply patch 的 TypeMeta（apiVersion/kind）必须与目标资源一致。
- RBAC 上 SSA 需要 `patch` 权限，创建新对象另需 `create`。

### 错误体结构（issue #10 的前置事实）

失败统一进 `kube::Error::Api(Box<kube::core::Status>)`（3.0 起 ErrorResponse 已合并进 Status）。
`Status` 结构：`{ status, message, reason, details: Option<StatusDetails>, code }`；
`StatusDetails { name, group, kind, uid, causes: Option<Vec<StatusCause>>, retry_after_seconds }`；
`StatusCause { reason, message, field }`。

**(a) validation 错误 —— HTTP 422，reason="Invalid"，causes 结构化，可直接用于行号映射：**

```json
{
  "kind": "Status",
  "apiVersion": "v1",
  "metadata": {},
  "status": "Failure",
  "message": "Deployment.apps \"nginx\" is invalid: [spec.template.spec.containers[0].image: Required value, spec.template.spec.containers[0].resources.limits.cpu: Invalid value: \"abc\": must be a valid resource quantity]",
  "reason": "Invalid",
  "details": {
    "name": "nginx",
    "group": "apps",
    "kind": "Deployment",
    "causes": [
      {
        "reason": "FieldValueRequired",
        "message": "Required value",
        "field": "spec.template.spec.containers[0].image"
      },
      {
        "reason": "FieldValueInvalid",
        "message": "Invalid value: \"abc\": must be a valid resource quantity",
        "field": "spec.template.spec.containers[0].resources.limits.cpu"
      }
    ]
  },
  "code": 422
}
```

每个 cause 的 `field` 是无前导点的字段路径（数组下标形式 `containers[0]`）。
`fieldValidation=Strict` 的未知字段错误也走 causes（`FieldValueInvalid`，message 形如
`strict decoding error: unknown field "spec.iamge"`），重复字段会产出多条 cause。

**(b) field ownership 冲突 —— HTTP 409，reason="Conflict"，无结构化 causes，只有文本 message：**

```json
{
  "kind": "Status",
  "apiVersion": "v1",
  "metadata": {},
  "status": "Failure",
  "message": "Apply failed with 2 conflicts: conflicts with \"kubectl\" using apps/v1:\n- .spec.replicas\n- .spec.template.spec.containers[name=\"nginx\"].image",
  "reason": "Conflict",
  "details": {},
  "code": 409
}
```

冲突信息只存在于 `message` 字符串中，需要**自行解析**。

### 对行号映射的直接推论

1. **两条解析路径**：422 → 遍历 `details.causes[].field`（结构化，可靠）；409 → 正则解析
   `message` 中的 `- .path` 列表。映射器必须同时支持。
2. **两种路径语法**：validation 用 `spec.template.spec.containers[0].image`（下标、无前导点）；
   SSA 冲突用 `.spec.template.spec.containers[name="nginx"].image`（listMapKey 选择器、有前导点）。
   映射到 YAML AST（如 `saphyr` 节点）前需把两种语法都规范化成统一的段序列
   （`spec` → `template` → `spec` → `containers` → key `name=nginx` → `image`）。
3. **粒度兜底**：部分错误没有 field 信息（admission webhook 拒绝、CRD CEL validation 的部分实现、
   RBAC 403），只能落到 `message` 全文 + 文档级（第 0 行）展示。
4. `dry_run` apply 可以在不落盘的情况下提前触发 422/409，适合做"保存前校验"交互。

---

## 版本与 feature 建议汇总

```toml
kube = { version = "4.0", features = ["ws", "rustls-tls", "oidc", "oauth"] }
k8s-openapi = { version = "0.28", features = ["v1_30"] } # earliest supported 版本按目标集群下限调
tokio = { version = "1", features = ["full"] }
futures = "0.3"
```

锁 4.0 的理由：exec 插件 YAML 输出（#2003）、ws task 泄漏/join 死锁修复（#1978）、CA 轮换处理（#1962）、
非 watch 请求默认 RetryPolicy（#2007）、kubeconfig 解析换 serde-saphyr（#1975，甩掉废弃 serde_yaml）。
不建议 < 3.0：缺 websocket keepalive，长连接 exec/portforward 过 LB 会被断。

## 参考来源

- [kube-rs changelog（kube.rs/changelog）](https://kube.rs/changelog/)
- [docs.rs: kube::api::Api](https://docs.rs/kube/latest/kube/api/struct.Api.html)（exec/attach/portforward/patch 入口）
- [docs.rs: AttachedProcess](https://docs.rs/kube/latest/kube/api/struct.AttachedProcess.html) /
  [AttachParams](https://docs.rs/kube/latest/kube/api/struct.AttachParams.html) /
  [Portforwarder](https://docs.rs/kube/latest/kube/api/struct.Portforwarder.html) /
  [PatchParams](https://docs.rs/kube/latest/kube/api/struct.PatchParams.html) /
  [kube::Error](https://docs.rs/kube/latest/kube/enum.Error.html)
- [kube-client auth 源码（exec 插件与刷新逻辑）](https://github.com/kube-rs/kube/blob/main/kube-client/src/client/auth/mod.rs)
- [Kubernetes 官方文档：Server-Side Apply](https://kubernetes.io/docs/reference/using-api/server-side-apply/)
- [mirrord#4918：kube-rs portforward 无 SPDY 回退、要求 apiserver ≥1.30 的实证](https://github.com/metalbear-co/mirrord/issues/4918)
