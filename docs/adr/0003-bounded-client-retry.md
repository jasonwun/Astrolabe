# The client retries 429/503/504 three times, not fifteen

`astrolabe_kube::client::client_for` turns kube-client's own retry off (`Config::default_retry = false`) and wraps the client in `BoundedRetryLayer`: three retries, 100 ms to 500 ms apart, on 429, 503 and 504. One client serves every request of a Cluster; there is no second client for Discovery.

## Why

kube-client's default retry makes 15 attempts, backing off by doubling to minutes. Its policy is not configurable (`Config` has only an on/off flag). On a Cluster without Aggregated Discovery, a group version whose APIService is unavailable answers 503, and every Discovery request to it went through those 15 attempts. Per-group Discovery skips such a group version only after the request fails, so the skip came after roughly ten minutes (#279). With the bounded retry, a v1.25 kind node with an APIService whose Service is gone skips it after 4 requests in 1.2 s.

A second client only for Discovery was rejected: it bends the Cluster's structure around one caller. Turning retry off altogether was rejected too: a single transient 503 would then drop a group from Discovery, with no sign to the user, until the next run.

## What it costs

- A Cluster that throttles (429) or is down for longer than about a second now surfaces that error after the third retry, where kube-client would have kept retrying for minutes. Watches have their own backoff; list, detail, Apply, logs and exec requests show the error.
- The retry layer sits outside kube-client's auth layer (the default sits inside), behind a `tower::buffer::Buffer` because `Retry` needs a `Clone` service and kube-client's stack is not one. Each retry goes through the auth layer again. The auth layer keeps no per-request state (`RefreshableToken::check` reads the shared token, refreshes it if it expires within 60 s, and overwrites the `Authorization` header), so this is safe by reading `kube-client` 4.2.0's `client/auth/mod.rs`. It has not been run against an exec credential plugin with an expiring token.
- Upgrading kube-client: check whether `Config` gained a retry policy knob, and whether the stack is still a `BoxService`.

## Revisit when

- A real Cluster (AKS, EKS, GKE) shows errors that kube-client's default retry would have absorbed: 429 from API Priority and Fairness, or 503/504 during a control-plane upgrade. Raise the retry count or the delays, or retry only idempotent reads longer.
- An exec credential plugin (`kubelogin`, `aws eks get-token`) fails on a request that was retried.
- kube-client makes its retry policy configurable: drop `BoundedRetryLayer` and the `Buffer`.
