//! A mock apiserver behind the `Cluster` handle, shared by the watch
//! tests. Every payload here is synthetic: hand-written minimal shapes,
//! never captured from a real apiserver.
#![allow(dead_code)]

use std::convert::Infallible;
use std::io::Write;
use std::sync::{Arc, Mutex};

use astrolabe_kube::cluster::{Cluster, ClusterId};
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::kube::Client;
use astrolabe_kube::kube::client::Body;
use astrolabe_kube::kube::runtime::watcher::Event;
use astrolabe_kube::watch::NamespaceWatchMessage;
use futures::channel::mpsc;
use futures::future::{self, BoxFuture};
use futures::{FutureExt as _, StreamExt as _};

/// One canned answer to a namespace LIST or WATCH.
pub enum Answer {
    Ok(String),
    Status(u16, &'static str),
    /// The request never completes: a watch that stays open and silent.
    Hang,
}

/// The mock apiserver's script: answers in order per request type, and
/// every request's path and query in arrival order.
#[derive(Default)]
pub struct Apiserver {
    lists: Mutex<Vec<Answer>>,
    watches: Mutex<Vec<Answer>>,
    seen_lists: Mutex<Vec<String>>,
    seen_watches: Mutex<Vec<String>>,
}

impl Apiserver {
    pub fn new(lists: Vec<Answer>, watches: Vec<Answer>) -> Arc<Self> {
        Arc::new(Self {
            lists: Mutex::new(lists),
            watches: Mutex::new(watches),
            ..Self::default()
        })
    }

    pub fn lists(&self) -> Vec<String> {
        self.seen_lists.lock().unwrap().clone()
    }

    pub fn watches(&self) -> Vec<String> {
        self.seen_watches.lock().unwrap().clone()
    }

    pub fn answer(&self, target: String) -> BoxFuture<'static, http::Response<Body>> {
        let (answers, seen) = if target.contains("watch=true") {
            (&self.watches, &self.seen_watches)
        } else {
            (&self.lists, &self.seen_lists)
        };
        seen.lock().unwrap().push(target);
        let mut answers = answers.lock().unwrap();
        let answer = if answers.is_empty() {
            Answer::Hang
        } else {
            answers.remove(0)
        };
        match answer {
            Answer::Ok(body) => future::ready(reply(200, &body)).boxed(),
            Answer::Status(code, reason) => future::ready(status(code, reason)).boxed(),
            Answer::Hang => future::pending().boxed(),
        }
    }
}

pub fn reply(code: u16, body: &str) -> http::Response<Body> {
    http::Response::builder()
        .status(code)
        .header("content-type", "application/json")
        .body(Body::from(body.as_bytes().to_vec()))
        .unwrap()
}

pub fn status_json(code: u16, reason: &str) -> String {
    format!(
        r#"{{"kind":"Status","apiVersion":"v1","status":"Failure","code":{code},"reason":"{reason}","message":"{reason} by the mock"}}"#
    )
}

pub fn status(code: u16, reason: &str) -> http::Response<Body> {
    reply(code, &status_json(code, reason))
}

pub fn namespace_list(names: &[&str], resource_version: &str) -> Answer {
    let items = names
        .iter()
        .map(|name| {
            format!(r#"{{"apiVersion":"v1","kind":"Namespace","metadata":{{"name":"{name}"}}}}"#)
        })
        .collect::<Vec<_>>()
        .join(",");
    Answer::Ok(format!(
        r#"{{"apiVersion":"v1","kind":"NamespaceList","metadata":{{"resourceVersion":"{resource_version}"}},"items":[{items}]}}"#
    ))
}

/// A watch that sends one ERROR event, then ends.
pub fn failing_watch() -> Answer {
    Answer::Ok(format!(
        "{{\"type\":\"ERROR\",\"object\":{}}}\n",
        status_json(500, "InternalError")
    ))
}

/// `Cluster` over `apiserver`, on the test's runtime.
pub fn mock_cluster(apiserver: &Arc<Apiserver>) -> Cluster {
    let apiserver = apiserver.clone();
    let service = tower::service_fn(move |request: http::Request<Body>| {
        let target = request.uri().to_string();
        apiserver.answer(target).map(Ok::<_, Infallible>)
    });
    Cluster::from_client(Client::new(service, "default"), ClusterId::named("mock"))
}

pub async fn next(watch: &mut mpsc::Receiver<NamespaceWatchMessage>) -> NamespaceWatchMessage {
    watch.next().await.expect("the watch ended")
}

pub fn name(namespace: &Namespace) -> &str {
    namespace.metadata.name.as_deref().unwrap()
}

/// Asserts the next messages are one complete LIST of `names`.
pub async fn assert_list(watch: &mut mpsc::Receiver<NamespaceWatchMessage>, names: &[&str]) {
    assert!(matches!(next(watch).await, Ok(Event::Init)));
    for expected in names {
        match next(watch).await {
            Ok(Event::InitApply(namespace)) => assert_eq!(name(&namespace), *expected),
            other => panic!("expected InitApply({expected}), got {other:?}"),
        }
    }
    assert!(matches!(next(watch).await, Ok(Event::InitDone)));
}

/// Captures every thread's logs for the rest of the test binary. A
/// thread-scoped subscriber would race other tests' threads over tracing's
/// per-callsite cache, so only one test per binary installs it, and no
/// other test in that binary reaches the callsites it asserts.
#[derive(Clone, Default)]
pub struct Logs(Arc<Mutex<Vec<u8>>>);

impl Logs {
    pub fn install(&self) {
        tracing_subscriber::fmt()
            .with_env_filter("astrolabe_kube=debug,kube_runtime=debug")
            .with_ansi(false)
            .with_writer(self.clone())
            .init();
    }

    pub fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
    type Writer = Logs;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
