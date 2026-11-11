# A Finder launch takes the login shell's PATH and KUBECONFIG, scoped, not process-wide

`astrolabe_kube` runs the user's login shell once and reads `PATH` and `KUBECONFIG` from it (`shell_env.rs`). It applies them in two places only: the `env` of each kubeconfig exec block (unless the block names `PATH` itself), and the kubeconfig location when the process has no `KUBECONFIG`. It never calls `std::env::set_var`.

## Why

A Finder launch inherits launchd's environment: `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, no `KUBECONFIG`. An exec plugin in Homebrew (`kubelogin`) is not found, and `kubelogin --login azurecli` cannot find the `az` it calls either (#379). There is no OS or Rust API for "the user's terminal environment"; running the login shell is the only source.

Prior art does the same capture:

- Freelens `main@29631d2b` (`features/shell-sync/main/`, `compute-unix-shell-environment`): `$SHELL -l -i -c 'printf MARK; /usr/bin/env -0; printf MARK'` at app load, fish with `-l -c`, csh-likes on stdin, 30 s timeout.
- Zed `main@985bbf42` (`util::load_login_shell_environment`, `util::shell_env::capture`): the same only when stdout is not a TTY, from `$HOME`, then `unsafe { std::env::set_var }` per variable.

Both then mutate the process. Rust 2024 makes `set_var` unsafe, because another thread reading the environment is a data race, and gpui and tokio threads are already running here. Scoping the values to the exec block `env` and to the kubeconfig read needs no unsafe code and no ordering against other threads.

## What it costs

- Only `PATH` and `KUBECONFIG` are taken. Other shell variables (`AWS_PROFILE`, `AZURE_CONFIG_DIR`, …) do not reach plugins. Add one when a real plugin needs it.
- Code that spawns a process outside the kubeconfig exec path (a future terminal, `kubectl` subprocess) does not see the shell's `PATH` unless it asks `shell_env`.
- The capture runs the user's rc files with stdin null and stdout piped. A slow or hanging rc delays the first cluster list by up to 15 s, then behaves as before (a warning in the log).
- Tests skip the capture with `ASTROLABE_NO_LOGIN_SHELL_ENV` (set in `.cargo/config.toml`), so a `cargo run` with stdout redirected skips it too: unset it there.
- The shell's `PATH` replaces, not extends, the one the app was started with, in each exec block. A directory only the app's own `PATH` has is lost to the plugin.
- A failed capture is only logged, not shown to the user.

## Revisit when

- A plugin needs another shell variable: widen `ShellEnv`.
- Something other than the kubeconfig needs the shell's `PATH` process-wide: then decide again whether a startup `set_var` before any thread starts is acceptable.
