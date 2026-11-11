//! The login shell's `PATH` and `KUBECONFIG`, for an app launched from
//! Finder (#379).
//!
//! A Finder launch inherits launchd's environment: `PATH` is
//! `/usr/bin:/bin:/usr/sbin:/sbin` and there is no `KUBECONFIG`, so an exec
//! credential plugin in Homebrew (`kubelogin`, and the `az` it calls) is
//! not found. The user's login shell knows the PATH their terminal has. It
//! is captured once and applied *scoped*: to each exec block of the
//! kubeconfig and to the kubeconfig location, never to this process's own
//! environment. See `docs/adr/0004-scoped-login-shell-environment.md`.

use std::io::{IsTerminal as _, Read as _, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::Duration;

/// Set to anything, this skips the capture. Tests set it
/// (`.cargo/config.toml`) so they never run the developer's shell.
const OPT_OUT: &str = "ASTROLABE_NO_LOGIN_SHELL_ENV";

/// How long the login shell may take; its rc files can be slow.
const TIMEOUT: Duration = Duration::from_secs(15);

/// Brackets the `env -0` output in the shell's stdout, so whatever the rc
/// files print around it is ignored.
pub(crate) const MARKER: &str = "_ASTROLABE_LOGIN_SHELL_ENV_6b1f_";

/// The variables taken from the login shell. Everything else stays out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShellEnv {
    path: Option<String>,
    kubeconfig: Option<String>,
}

impl ShellEnv {
    /// Read `PATH` and `KUBECONFIG` out of a login shell's stdout: the
    /// NUL-separated `env -0` output between two [`MARKER`]s. A value may
    /// hold newlines and `=`; output outside the markers is ignored.
    pub(crate) fn parse(stdout: &[u8]) -> Self {
        let text = String::from_utf8_lossy(stdout);
        let Some((_, rest)) = text.split_once(MARKER) else {
            return Self::default();
        };
        let Some((vars, _)) = rest.split_once(MARKER) else {
            return Self::default();
        };
        let mut shell = Self::default();
        for entry in vars.split('\0') {
            match entry.split_once('=') {
                Some(("PATH", value)) if !value.is_empty() => {
                    shell.path = Some(value.to_owned());
                }
                Some(("KUBECONFIG", value)) if !value.is_empty() => {
                    shell.kubeconfig = Some(value.to_owned());
                }
                _ => {}
            }
        }
        shell
    }

    /// The shell's `KUBECONFIG`, when it has one.
    pub(crate) fn kubeconfig(&self) -> Option<&str> {
        self.kubeconfig.as_deref()
    }

    /// Give every exec block of `kubeconfig` the shell's `PATH`, unless the
    /// block names `PATH` itself: the kubeconfig's own entry wins. A bare
    /// `command` is searched on the child's `PATH`, so this resolves it too.
    pub(crate) fn apply(&self, kubeconfig: &mut kube::config::Kubeconfig) {
        let Some(path) = &self.path else { return };
        for exec in kubeconfig
            .auth_infos
            .iter_mut()
            .filter_map(|user| user.auth_info.as_mut()?.exec.as_mut())
        {
            let env = exec.env.get_or_insert_with(Vec::new);
            if env
                .iter()
                .any(|entry| entry.get("name").is_some_and(|name| name == "PATH"))
            {
                continue;
            }
            env.push(
                [
                    ("name".to_owned(), "PATH".to_owned()),
                    ("value".to_owned(), path.clone()),
                ]
                .into(),
            );
        }
    }
}

/// The login shell's environment, captured on the first call. Blocks while
/// the shell runs: call off the UI thread. Never captures when stdout is a
/// terminal (the process already has the terminal's environment) or when
/// [`OPT_OUT`] is set.
pub(crate) fn shell_env() -> &'static ShellEnv {
    CAPTURED.get_or_init(|| {
        if std::io::stdout().is_terminal() || std::env::var_os(OPT_OUT).is_some() {
            return ShellEnv::default();
        }
        capture(login_shell(), TIMEOUT).unwrap_or_else(|reason| {
            tracing::warn!("login shell environment not captured: {reason}");
            ShellEnv::default()
        })
    })
}

/// What [`shell_env`] has captured, without waiting or starting it: for a
/// caller on the UI thread. `None` until a background caller has finished.
pub(crate) fn captured_shell_env() -> Option<&'static ShellEnv> {
    CAPTURED.get()
}

static CAPTURED: OnceLock<ShellEnv> = OnceLock::new();

/// The shell to run: `$SHELL`, else the passwd entry's.
fn login_shell() -> Option<String> {
    let shell = shell_from(std::env::var("SHELL").ok());
    #[cfg(unix)]
    let shell = shell.or_else(passwd_shell);
    shell
}

/// `$SHELL`'s value, when it names a shell.
fn shell_from(value: Option<String>) -> Option<String> {
    value.filter(|shell| !shell.is_empty())
}

/// The shell the passwd entry of the current user names.
#[cfg(unix)]
fn passwd_shell() -> Option<String> {
    use std::ffi::CStr;
    // SAFETY: `getpwuid` returns null or a pointer to a static passwd
    // entry, whose `pw_shell` is null or a NUL-terminated string; it is
    // copied before any other call.
    unsafe {
        let entry = libc::getpwuid(libc::getuid()).as_ref()?;
        let shell = CStr::from_ptr(entry.pw_shell.as_ref()?).to_string_lossy();
        shell_from(Some(shell.into_owned()))
    }
}

/// Run the login shell from `$HOME`, interactive and login, so the rc files
/// that set `PATH` (and direnv-like hooks) run, and parse what it prints.
fn capture(shell: Option<String>, timeout: Duration) -> Result<ShellEnv, String> {
    let shell = shell.ok_or("no $SHELL and no passwd shell")?;
    let command = format!("printf {MARKER}; /usr/bin/env -0; printf {MARKER}");
    let name = shell.rsplit('/').next().unwrap_or(&shell);
    // fish takes `-c` but not `-i` the same way; csh-likes take the command
    // on stdin.
    let (args, stdin_command): (&[&str], bool) = match name {
        "fish" => (&["-l", "-c", &command], false),
        "csh" | "tcsh" => (&["-l"], true),
        _ => (&["-l", "-i", "-c", &command], false),
    };
    let mut process = Command::new(&shell);
    process.args(args).stdin(if stdin_command {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        process.current_dir(home);
    }
    let mut child = process
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot run {shell}: {error}"))?;
    if stdin_command && let Some(mut stdin) = child.stdin.take() {
        // A failed write shows as the shell's own failure below.
        let _ = writeln!(stdin, "{command}\nexit");
    }
    let mut stdout = child.stdout.take().ok_or("no stdout pipe")?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        let _ = sender.send(bytes);
    });
    let Ok(bytes) = receiver.recv_timeout(timeout) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("{shell} did not finish within {timeout:?}"));
    };
    let status = child.wait().map_err(|error| format!("{shell}: {error}"))?;
    if !status.success() {
        return Err(format!("{shell} exited with {status}"));
    }
    Ok(ShellEnv::parse(&bytes))
}

#[cfg(test)]
mod tests {
    use super::{
        MARKER, ShellEnv, capture, captured_shell_env, passwd_shell, shell_env, shell_from,
    };
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    fn fake_shell(name: &str, body: &str) -> String {
        let dir = std::env::temp_dir().join(format!("astrolabe-shell-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn capture_runs_the_shell_as_login_and_reads_its_environment() {
        // Takes the `-c` command, as `sh -c` would, and adds a PATH.
        let shell = fake_shell(
            "good",
            r#"echo "rc noise"; PATH=/opt/x:/usr/bin KUBECONFIG=/k /bin/sh -c "$4""#,
        );
        let captured = capture(Some(shell), Duration::from_secs(5)).unwrap();
        assert_eq!(captured.path.as_deref(), Some("/opt/x:/usr/bin"));
        assert_eq!(captured.kubeconfig(), Some("/k"));
    }

    #[test]
    fn fish_gets_the_command_after_dash_c_and_csh_on_stdin() {
        // fish: `-l -c <command>`; the fake runs the command it is given.
        let fish = fake_shell("fish", r#"PATH=/f /bin/sh -c "$3""#);
        let captured = capture(Some(fish), Duration::from_secs(5)).unwrap();
        assert_eq!(captured.path.as_deref(), Some("/f"));
        // csh: `-l` and the command on stdin.
        let csh = fake_shell("csh", "PATH=/c /bin/sh");
        let captured = capture(Some(csh), Duration::from_secs(5)).unwrap();
        assert_eq!(captured.path.as_deref(), Some("/c"));
    }

    #[test]
    fn the_shell_comes_from_shell_then_passwd() {
        assert_eq!(
            shell_from(Some("/bin/zsh".into())).as_deref(),
            Some("/bin/zsh")
        );
        assert_eq!(shell_from(Some(String::new())), None);
        assert_eq!(shell_from(None), None);
        let passwd = passwd_shell().unwrap();
        assert!(std::path::Path::new(&passwd).exists(), "{passwd}");
        let expected = shell_from(std::env::var("SHELL").ok()).or_else(passwd_shell);
        assert_eq!(super::login_shell(), expected);
        assert!(expected.is_some());
    }

    #[test]
    fn tests_skip_the_capture_and_cache_the_answer() {
        // `.cargo/config.toml` sets the opt-out for every test run.
        assert_eq!(*shell_env(), ShellEnv::default());
        assert!(std::ptr::eq(captured_shell_env().unwrap(), shell_env()));
    }

    #[test]
    fn empty_values_are_not_taken() {
        let output = format!("{MARKER}PATH=\0KUBECONFIG=\0{MARKER}");
        assert_eq!(ShellEnv::parse(output.as_bytes()), ShellEnv::default());
    }

    #[test]
    fn a_shell_that_hangs_past_the_timeout_is_an_error() {
        let shell = fake_shell("hang", "exec sleep 30");
        let started = std::time::Instant::now();
        let error = capture(Some(shell), Duration::from_millis(200)).unwrap_err();
        assert!(error.contains("did not finish"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_shell_that_fails_is_an_error() {
        let shell = fake_shell("bad", "exit 3");
        let error = capture(Some(shell), Duration::from_secs(5)).unwrap_err();
        assert!(error.contains("exited with"), "{error}");
        assert!(capture(None, Duration::from_secs(1)).is_err());
    }

    fn kubeconfig(exec_env: &str) -> kube::config::Kubeconfig {
        kube::config::Kubeconfig::from_yaml(&format!(
            r"apiVersion: v1
kind: Config
users:
- name: plugin
  user:
    exec:
      apiVersion: client.authentication.k8s.io/v1
      command: kubelogin
{exec_env}
- name: static
  user:
    token: abc
"
        ))
        .unwrap()
    }

    fn shell(path: &str) -> ShellEnv {
        ShellEnv::parse(format!("{MARKER}PATH={path}\0{MARKER}").as_bytes())
    }

    fn env_of(config: &kube::config::Kubeconfig, user: &str) -> Option<Vec<(String, String)>> {
        let exec = config
            .auth_infos
            .iter()
            .find(|named| named.name == user)?
            .auth_info
            .as_ref()?
            .exec
            .as_ref()?;
        Some(
            exec.env
                .as_ref()?
                .iter()
                .map(|entry| (entry["name"].clone(), entry["value"].clone()))
                .collect(),
        )
    }

    #[test]
    fn path_is_added_to_every_exec_block() {
        let mut config = kubeconfig("");
        shell("/opt/homebrew/bin:/usr/bin").apply(&mut config);
        assert_eq!(
            env_of(&config, "plugin").unwrap(),
            [("PATH".to_owned(), "/opt/homebrew/bin:/usr/bin".to_owned())]
        );
    }

    #[test]
    fn an_exec_block_that_names_path_keeps_its_own() {
        let mut config = kubeconfig(
            "      env:\n      - name: PATH\n        value: /mine\n      - name: A\n        value: b",
        );
        shell("/opt/homebrew/bin").apply(&mut config);
        assert_eq!(
            env_of(&config, "plugin").unwrap(),
            [
                ("PATH".to_owned(), "/mine".to_owned()),
                ("A".to_owned(), "b".to_owned())
            ]
        );
    }

    #[test]
    fn other_exec_variables_are_kept_beside_the_added_path() {
        let mut config = kubeconfig("      env:\n      - name: A\n        value: b");
        shell("/p").apply(&mut config);
        assert_eq!(
            env_of(&config, "plugin").unwrap(),
            [
                ("A".to_owned(), "b".to_owned()),
                ("PATH".to_owned(), "/p".to_owned())
            ]
        );
    }

    #[test]
    fn users_without_an_exec_block_are_untouched() {
        let mut config = kubeconfig("");
        let before = config.auth_infos[1].clone();
        shell("/p").apply(&mut config);
        assert_eq!(env_of(&config, "static"), None);
        assert_eq!(config.auth_infos[1].name, before.name);
        assert!(
            config.auth_infos[1]
                .auth_info
                .as_ref()
                .unwrap()
                .exec
                .is_none()
        );
    }

    #[test]
    fn an_empty_capture_changes_nothing() {
        let mut config = kubeconfig("");
        ShellEnv::default().apply(&mut config);
        assert_eq!(env_of(&config, "plugin"), None);
        assert!(ShellEnv::parse(b"").kubeconfig().is_none());
    }

    #[test]
    fn parse_ignores_noise_outside_the_markers() {
        let output = format!(
            "motd\nPATH=/noise\n{MARKER}HOME=/h\0PATH=/a:/b\0KUBECONFIG=/k1:/k2\0{MARKER}PATH=/after\n"
        );
        let parsed = ShellEnv::parse(output.as_bytes());
        assert_eq!(parsed.path.as_deref(), Some("/a:/b"));
        assert_eq!(parsed.kubeconfig(), Some("/k1:/k2"));
    }

    #[test]
    fn parse_keeps_newlines_and_equals_in_values() {
        let output = format!("{MARKER}A=x\ny\0PATH=/p=q\nr\0{MARKER}");
        assert_eq!(
            ShellEnv::parse(output.as_bytes()).path.as_deref(),
            Some("/p=q\nr")
        );
    }

    #[test]
    fn parse_without_both_markers_is_empty() {
        let only_one = format!("{MARKER}PATH=/p\0");
        assert_eq!(ShellEnv::parse(only_one.as_bytes()), ShellEnv::default());
        assert_eq!(ShellEnv::parse(b"PATH=/p\0"), ShellEnv::default());
    }
}
