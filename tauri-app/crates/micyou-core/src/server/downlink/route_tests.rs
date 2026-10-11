use super::*;
use crate::platform::playback_route::{RouteOutcome, RouteState};
use routing::RouteLease;

struct FakeLease {
    outcome: Mutex<RouteOutcome>,
    calls: Mutex<Vec<String>>,
    fail_restore: AtomicBool,
    fail_acquire: AtomicBool,
    acquire_gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    acquiring: tokio::sync::Notify,
    release_gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    releasing: tokio::sync::Notify,
}
impl FakeLease {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            outcome: Mutex::new(RouteOutcome {
                state: RouteState::Idle,
                error: None,
            }),
            calls: Mutex::new(Vec::new()),
            fail_restore: AtomicBool::new(false),
            fail_acquire: AtomicBool::new(false),
            acquire_gate: Mutex::new(None),
            acquiring: tokio::sync::Notify::new(),
            release_gate: Mutex::new(None),
            releasing: tokio::sync::Notify::new(),
        })
    }
}
impl RouteLease for FakeLease {
    fn supported(&self) -> bool {
        true
    }
    fn acquire(&self, target: &str) -> Result<RouteOutcome, String> {
        self.calls.lock().unwrap().push(format!("acquire:{target}"));
        self.acquiring.notify_one();
        if let Some(wait) = self.acquire_gate.lock().unwrap().take() {
            wait.recv().unwrap();
        }
        let mut outcome = self.outcome.lock().unwrap();
        if self.fail_acquire.load(Ordering::Acquire) {
            outcome.state = RouteState::Restored;
            outcome.error = Some("route_acquire_failed".into());
            Err("route_acquire_failed".into())
        } else {
            outcome.state = RouteState::Held;
            outcome.error = None;
            Ok(outcome.clone())
        }
    }
    fn release(&self) -> RouteOutcome {
        self.calls.lock().unwrap().push("release".into());
        if let Some(wait) = self.release_gate.lock().unwrap().take() {
            self.releasing.notify_one();
            wait.recv().unwrap();
        }
        let mut outcome = self.outcome.lock().unwrap();
        if matches!(outcome.state, RouteState::Held | RouteState::RestoreFailed) {
            if self.fail_restore.load(Ordering::Acquire) {
                outcome.state = RouteState::RestoreFailed;
                outcome.error = Some("route_restore_failed".into());
            } else {
                outcome.state = RouteState::Restored;
                outcome.error = None;
            }
        }
        outcome.clone()
    }
    fn external_change(&self) -> bool {
        false
    }
    fn recover_stale(&self) -> RouteOutcome {
        self.release()
    }
}

async fn attached(
    routing_peer: bool,
) -> (
    Arc<DownlinkController>,
    Arc<FakeLease>,
    mpsc::Receiver<MessageWrapper>,
) {
    let lease = FakeLease::new();
    let controller = DownlinkController::with_route(
        Arc::new(tests::Events::default()),
        vec![Arc::new(RwLock::new(None))],
        lease.clone(),
    );
    let (control, mut rx) = mpsc::channel(50);
    let (audio, _) = mpsc::channel(5);
    controller
        .attach(
            1,
            DownlinkPeer {
                session_id: 10,
                supports_downlink: true,
                supports_routing: routing_peer,
            },
            control,
            audio,
            CancellationToken::new(),
        )
        .await;
    rx.recv().await.unwrap();
    {
        let mut inner = controller.inner.lock().unwrap();
        inner.status.supported = true;
        inner.status.requested = true;
        inner.session.as_mut().unwrap().stream_id = 1;
        inner.stop = Some(Arc::new(AtomicBool::new(false)));
    }
    lease.calls.lock().unwrap().clear();
    (controller, lease, rx)
}

fn request(id: i64, enabled: bool, error: &str) -> DownlinkRequestMessage {
    DownlinkRequestMessage {
        session_id: 10,
        request_id: id,
        enabled,
        playback_error: error.into(),
    }
}

#[tokio::test]
async fn capture_ready_acquires_before_active_and_runtime_failure_restores_without_phone_ack() {
    let (controller, lease, mut rx) = attached(true).await;
    controller.worker_ready(1, 1, "hifi".into()).await;
    let acquiring = rx.recv().await.unwrap().downlink_state.unwrap();
    assert!(!acquiring.active);
    assert_eq!(acquiring.route_state, "acquiring");
    let held = rx.recv().await.unwrap().downlink_state.unwrap();
    assert!(held.active && held.supports_playback_routing);
    assert_eq!(held.route_state, "held");
    controller
        .finish_worker(1, 1, Some("capture_failed".into()))
        .await;
    let status = controller.status();
    assert!(!status.active);
    assert_eq!(status.route_state, "restored");
    assert_eq!(status.error.as_deref(), Some("capture_failed"));
    assert_eq!(
        *lease.calls.lock().unwrap(),
        vec!["acquire:hifi", "release"]
    );
}

#[tokio::test]
async fn old_peer_never_acquires_and_old_generation_cannot_release_current_route() {
    let (controller, lease, _rx) = attached(false).await;
    controller.worker_ready(1, 1, "hifi".into()).await;
    assert!(controller.status().active);
    assert!(lease.calls.lock().unwrap().is_empty());
    let (controller, lease, _rx) = attached(true).await;
    controller.worker_ready(1, 1, "hifi".into()).await;
    controller.worker_ready(1, 0, "obsolete".into()).await;
    controller
        .finish_worker(0, 1, Some("capture_failed".into()))
        .await;
    controller
        .finish_worker(1, 0, Some("capture_failed".into()))
        .await;
    assert!(controller.status().active);
    assert_eq!(*lease.calls.lock().unwrap(), vec!["acquire:hifi"]);
}

#[tokio::test]
async fn failure_to_acquire_never_publishes_active_and_retry_uses_a_new_generation() {
    let (controller, lease, mut rx) = attached(true).await;
    lease.fail_acquire.store(true, Ordering::Release);
    controller.worker_ready(1, 1, "hifi".into()).await;
    assert!(!controller.status().active);
    assert_eq!(
        controller.status().error.as_deref(),
        Some("route_acquire_failed")
    );
    while let Ok(message) = rx.try_recv() {
        assert!(!message.downlink_state.unwrap().active);
    }
    lease.fail_acquire.store(false, Ordering::Release);
    {
        let mut inner = controller.inner.lock().unwrap();
        inner.session.as_mut().unwrap().stream_id = 2;
        inner.stop = Some(Arc::new(AtomicBool::new(false)));
    }
    controller.worker_ready(1, 2, "hifi".into()).await;
    assert!(controller.status().active);
}

#[tokio::test]
async fn restoration_failure_blocks_new_request_until_explicit_retry() {
    let (controller, lease, _rx) = attached(true).await;
    controller.worker_ready(1, 1, "hifi".into()).await;
    lease.fail_restore.store(true, Ordering::Release);
    controller
        .request(1, request(1, false, "playback_failed"))
        .await;
    assert_eq!(controller.status().route_state, "restore_failed");
    lease.fail_restore.store(false, Ordering::Release);
    controller.request(1, request(2, true, "")).await;
    assert!(!controller.status().active);
    assert_eq!(controller.status().route_state, "restore_failed");
    assert_eq!(
        controller.status().error.as_deref(),
        Some("route_restore_required")
    );
    assert_eq!(
        lease
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("acquire"))
            .count(),
        1
    );
    assert_eq!(
        controller.retry_route_restore().await.route_state,
        "restored"
    );
}

#[tokio::test]
async fn thread_timeout_still_restores_and_retains_handle() {
    let (controller, lease, _rx) = attached(true).await;
    controller.worker_ready(1, 1, "hifi".into()).await;
    let (release, wait) = std::sync::mpsc::channel();
    controller.inner.lock().unwrap().thread =
        Some(std::thread::spawn(move || wait.recv().unwrap()));
    assert_eq!(
        controller.stop_worker().await.unwrap_err(),
        "capture_failed"
    );
    assert_eq!(controller.status().route_state, "restored");
    assert!(controller.inner.lock().unwrap().thread.is_some());
    assert_eq!(
        *lease.calls.lock().unwrap(),
        vec!["acquire:hifi", "release"]
    );
    release.send(()).unwrap();
    controller.shutdown().await.unwrap();
}

#[tokio::test]
async fn close_serializes_with_inflight_acquire_and_never_leaves_the_route_held() {
    let (controller, lease, _rx) = attached(true).await;
    let (release, wait) = std::sync::mpsc::channel();
    *lease.acquire_gate.lock().unwrap() = Some(wait);
    let ready_controller = controller.clone();
    let ready =
        tokio::spawn(async move { ready_controller.worker_ready(1, 1, "hifi".into()).await });
    lease.acquiring.notified().await;
    assert_eq!(controller.status().route_state, "acquiring");
    assert!(!controller.status().active);
    let close_controller = controller.clone();
    let closed =
        tokio::spawn(async move { close_controller.request(1, request(1, false, "")).await });
    release.send(()).unwrap();
    ready.await.unwrap();
    closed.await.unwrap();
    assert!(!controller.status().active && !controller.status().requested);
    assert_eq!(controller.status().route_state, "restored");
}

#[tokio::test]
async fn playback_error_is_bounded_and_restore_retry_does_not_stop_a_held_stream() {
    let (controller, lease, _rx) = attached(true).await;
    controller.worker_ready(1, 1, "hifi".into()).await;
    assert!(controller.retry_route_restore().await.active);
    assert_eq!(*lease.calls.lock().unwrap(), vec!["acquire:hifi"]);
    controller
        .request(1, request(1, true, "playback_failed"))
        .await;
    assert!(controller.status().active);
    controller
        .request(1, request(1, false, "untrusted error text"))
        .await;
    assert_eq!(
        controller.status().error.as_deref(),
        Some("playback_failed")
    );
    assert_eq!(controller.status().route_state, "restored");
}

#[tokio::test]
async fn repeated_enable_and_late_startup_recovery_preserve_the_held_lease() {
    let (controller, lease, _rx) = attached(true).await;
    controller.worker_ready(1, 1, "hifi".into()).await;
    controller.request(1, request(1, true, "")).await;
    controller.recover_route().await;
    assert!(controller.status().active);
    assert_eq!(*lease.calls.lock().unwrap(), vec!["acquire:hifi"]);
}

#[tokio::test]
async fn external_route_change_and_disconnect_release_independently_of_status_delivery() {
    let (controller, lease, rx) = attached(true).await;
    controller.worker_ready(1, 1, "hifi".into()).await;
    lease.outcome.lock().unwrap().state = RouteState::ReleasedExternal;
    controller
        .finish_worker(1, 1, Some("route_changed_external".into()))
        .await;
    assert!(!controller.status().active);
    assert_eq!(controller.status().route_state, "released_external");
    assert_eq!(
        controller.status().error.as_deref(),
        Some("route_changed_external")
    );
    assert_eq!(
        *lease.calls.lock().unwrap(),
        vec!["acquire:hifi", "release"]
    );
    drop(rx);
    controller.detach(1).await;
    assert!(!controller.status().phone_supported);
    assert_eq!(controller.status().route_state, "released_external");
}

#[tokio::test]
async fn late_worker_error_cannot_overwrite_user_close_or_source_persistence_failure() {
    let (controller, _, mut rx) = attached(true).await;
    controller.worker_ready(1, 1, "hifi".into()).await;
    controller.request(1, request(1, false, "")).await;
    while rx.try_recv().is_ok() {}
    controller
        .finish_worker(1, 1, Some("capture_failed".into()))
        .await;
    assert!(controller.status().error.is_none());
    assert!(!controller.status().requested);
    assert!(rx.try_recv().is_err());

    let (controller, _, mut rx) = attached(true).await;
    controller.worker_ready(1, 1, "hifi".into()).await;
    controller
        .replace_source(Some("another".into()), |_| Err("disk unavailable".into()))
        .await
        .unwrap_err();
    while rx.try_recv().is_ok() {}
    controller
        .finish_worker(1, 1, Some("capture_failed".into()))
        .await;
    assert_eq!(
        controller.status().error.as_deref(),
        Some("source_unavailable")
    );
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn detach_transport_failure_and_normal_shutdown_all_release_a_held_lease() {
    for reason in ["detach", "transport", "shutdown"] {
        let (controller, lease, mut rx) = attached(true).await;
        controller.worker_ready(1, 1, "hifi".into()).await;
        while rx.try_recv().is_ok() {}
        match reason {
            "detach" => controller.detach(1).await,
            "shutdown" => controller.shutdown().await.unwrap(),
            "transport" => {
                controller.transport_failed(1);
                let stopped = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
                    .await
                    .unwrap()
                    .unwrap()
                    .downlink_state
                    .unwrap();
                assert!(!stopped.active);
                assert_eq!(stopped.route_state, "restored");
            }
            _ => unreachable!(),
        }
        assert!(!controller.status().active);
        assert_eq!(controller.status().route_state, "restored", "{reason}");
        assert_eq!(
            *lease.calls.lock().unwrap(),
            vec!["acquire:hifi", "release"],
            "{reason}"
        );
    }
}

#[tokio::test]
async fn cancelled_request_waiter_does_not_cancel_inflight_route_release() {
    let (controller, lease, mut rx) = attached(true).await;
    controller.worker_ready(1, 1, "hifi".into()).await;
    while rx.try_recv().is_ok() {}
    let (release, wait) = std::sync::mpsc::channel();
    *lease.release_gate.lock().unwrap() = Some(wait);
    let request_controller = controller.clone();
    let waiter =
        tokio::spawn(async move { request_controller.request(1, request(1, false, "")).await });
    lease.releasing.notified().await;
    waiter.abort();
    release.send(()).unwrap();
    let stopped = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap()
        .downlink_state
        .unwrap();
    assert!(!stopped.active);
    assert_eq!(
        (stopped.request_id, stopped.route_state.as_str()),
        (1, "restored")
    );
    assert_eq!(controller.status().route_state, "restored");
}

#[tokio::test]
async fn changing_source_releases_before_persisting_the_new_binding() {
    let (controller, lease, _rx) = attached(true).await;
    controller.worker_ready(1, 1, "hifi".into()).await;
    controller.inner.lock().unwrap().status.requested = false;
    controller
        .replace_source(Some("new-hifi".into()), |_| {
            assert_eq!(lease.outcome.lock().unwrap().state, RouteState::Restored);
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(controller.status().source_id.as_deref(), Some("new-hifi"));
    assert_eq!(controller.status().route_state, "restored");
    controller
        .finish_worker(1, 1, Some("capture_failed".into()))
        .await;
    assert!(controller.status().error.is_none());
}

#[tokio::test]
async fn routing_actor_failure_stops_and_releases_without_claiming_external_change() {
    let (controller, lease, mut rx) = attached(true).await;
    controller.worker_ready(1, 1, "hifi".into()).await;
    while rx.try_recv().is_ok() {}
    controller
        .finish_worker(1, 1, Some("route_unavailable".into()))
        .await;
    let message = rx.recv().await.unwrap().downlink_state.unwrap();
    assert!(!message.active);
    assert_eq!(message.error, "route_unavailable");
    assert_eq!(message.route_state, "restored");
    assert_eq!(
        *lease.calls.lock().unwrap(),
        vec!["acquire:hifi", "release"]
    );
}
