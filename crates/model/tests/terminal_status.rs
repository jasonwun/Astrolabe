use astrolabe_kube::cluster::ClusterError;
use astrolabe_model::notice::{Intent, Notice};
use astrolabe_model::terminal_status::{ExecState, LogState};

#[test]
fn log_states_say_how_the_stream_stands() {
    assert_eq!(
        LogState::Streaming.notice(),
        Notice::new("logs-status", "Streaming…", Intent::Muted)
    );
    assert_eq!(
        LogState::Ended.notice(),
        Notice::new("logs-status", "Stream ended", Intent::Muted)
    );
    assert_eq!(
        LogState::Failed(ClusterError::new("boom")).notice(),
        Notice::new("logs-status", "Stream failed: boom", Intent::Danger)
    );
}

#[test]
fn exec_states_say_how_the_session_stands() {
    assert_eq!(
        ExecState::Running.notice(),
        Notice::new("exec-status", "Session running", Intent::Muted)
    );
    assert_eq!(
        ExecState::Exited(3).notice(),
        Notice::new("exec-status", "Exited with code 3", Intent::Muted)
    );
    assert_eq!(
        ExecState::Failed(ClusterError::new("boom")).notice(),
        Notice::new("exec-status", "Session failed: boom", Intent::Danger)
    );
}
