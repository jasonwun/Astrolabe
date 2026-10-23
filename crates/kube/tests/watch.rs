use astrolabe_kube::kube::core::Status;
use astrolabe_kube::kube::runtime::watcher::Error;
use astrolabe_kube::watch::is_forbidden;

fn status(code: u16) -> Box<Status> {
    Box::new(Status {
        code,
        ..Status::default()
    })
}

#[test]
fn only_403_responses_count_as_rbac_denial() {
    let api = |code| astrolabe_kube::kube::Error::Api(status(code));
    assert!(is_forbidden(&Error::InitialListFailed(api(403))));
    assert!(is_forbidden(&Error::WatchStartFailed(api(403))));
    assert!(is_forbidden(&Error::WatchFailed(api(403))));
    assert!(is_forbidden(&Error::WatchError(status(403))));

    assert!(!is_forbidden(&Error::InitialListFailed(api(500))));
    assert!(!is_forbidden(&Error::WatchStartFailed(api(401))));
    assert!(!is_forbidden(&Error::WatchFailed(api(410))));
    assert!(!is_forbidden(&Error::WatchError(status(410))));
    assert!(!is_forbidden(&Error::NoResourceVersion));
}
