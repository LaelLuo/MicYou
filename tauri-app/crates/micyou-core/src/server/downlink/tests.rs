use super::*;
use crate::events::{AecStatus, DownloadProgress, ServerEvents, SpectrumPayload};
use crate::stats::AudioMetrics;
use crate::transport::tcp::DeviceInfo;
#[derive(Default)]
pub(crate) struct Events(Mutex<Vec<DownlinkStatus>>);
impl ServerEvents for Events {
    fn downlink_status_changed(&self, status: DownlinkStatus) {
        self.0.lock().unwrap().push(status);
    }
    fn device_connected(&self, _: DeviceInfo) {}
    fn device_disconnected(&self) {}
    fn audio_metrics(&self, _: AudioMetrics) {}
    fn udp_audio_warning(&self) {}
    fn mute_state_changed(&self, _: bool) {}
    fn audio_level(&self, _: u32) {}
    fn audio_spectrum(&self, _: SpectrumPayload) {}
    fn server_stopped(&self) {}
    fn web_client_count(&self, _: u32) {}
    fn install_progress(&self, _: String) {}
    fn aec_status_changed(&self, _: AecStatus) {}
    fn monitoring_state_changed(&self, _: bool) {}
    fn plugin_download_progress(&self, _: DownloadProgress) {}
}
fn controller() -> Arc<DownlinkController> {
    let mut controller = DownlinkController::new(
        Arc::new(Events::default()),
        vec![Arc::new(RwLock::new(None))],
    );
    Arc::get_mut(&mut controller).unwrap().identity = |id| {
        Ok(micyou_audio::endpoint::OutputEndpoint {
            id: id.into(),
            name: id.into(),
            component: micyou_audio::endpoint::Component::Physical,
            backing_id: None,
        })
    };
    controller
}

#[test]
fn virtual_line_admission_and_physical_endpoint_policy_share_one_check() {
    use micyou_audio::endpoint::{Component, OutputEndpoint};
    let mut controller = controller();
    Arc::get_mut(&mut controller).unwrap().identity = |id| {
        let (component, backing) = match id {
            "cable2" | "cable16" => (Component::Cable, Some("ordinary")),
            "hifi" => (Component::Hifi, Some("hifi")),
            "unknown" => (Component::UnknownVirtual, Some("unverified")),
            _ => (Component::Physical, Some("physical-card")),
        };
        Ok(OutputEndpoint {
            id: id.into(),
            name: id.into(),
            component,
            backing_id: backing.map(str::to_string),
        })
    };
    *controller.output[0].write().unwrap() = Some("cable2".into());
    assert_eq!(controller.source_error("cable16"), Some("feedback_source"));
    assert_eq!(controller.source_error("hifi"), None);
    assert_eq!(
        controller.source_error("unknown"),
        Some("identity_unavailable")
    );
    *controller.output[0].write().unwrap() = Some("unknown".into());
    assert_eq!(
        controller.source_error("hifi"),
        Some("identity_unavailable")
    );
    assert_eq!(controller.source_error("speakerA"), None);
    *controller.output[0].write().unwrap() = Some("speakerA".into());
    assert_eq!(controller.source_error("speakerB"), None);
    assert_eq!(controller.source_error("speakerA"), Some("feedback_source"));
}
pub(crate) fn unavailable_controller(events: SharedEvents) -> Arc<DownlinkController> {
    let controller = DownlinkController::new(events, vec![Arc::new(RwLock::new(None))]);
    controller.inner.lock().unwrap().status.source_id =
        Some("micyou-test-nonexistent-endpoint".into());
    controller
}

#[tokio::test]
async fn failed_source_persistence_keeps_old_source_and_publishes_stopped_error() {
    let controller = controller();
    {
        let mut inner = controller.inner.lock().unwrap();
        inner.status.source_id = Some("old-source".into());
        inner.status.active = true;
    }
    assert_eq!(
        controller
            .replace_source(
                Some("new-source".into()),
                |_| Err("disk unavailable".into())
            )
            .await
            .unwrap_err(),
        "disk unavailable"
    );
    let status = controller.status();
    assert!(!status.active);
    assert_eq!(status.source_id.as_deref(), Some("old-source"));
    assert_eq!(status.error.as_deref(), Some("source_unavailable"));
}

#[tokio::test]
async fn worker_panic_is_bounded_and_shutdown_still_clears_session() {
    let controller = controller();
    let (control, _rx) = mpsc::channel(10);
    let (audio, _audio_rx) = mpsc::channel(5);
    controller
        .attach(1, 10, true, control, audio, CancellationToken::new())
        .await;
    {
        let mut inner = controller.inner.lock().unwrap();
        inner.status.requested = true;
        inner.status.active = true;
        inner.stop = Some(Arc::new(AtomicBool::new(false)));
        inner.thread = Some(std::thread::spawn(|| panic!("simulated capture panic")));
    }
    assert_eq!(controller.shutdown().await.unwrap_err(), "capture_failed");
    let status = controller.status();
    assert!(!status.active && !status.requested && !status.phone_supported);
    assert_eq!(status.error.as_deref(), Some("capture_failed"));
}

#[tokio::test]
async fn worker_stop_retains_timed_out_handle_and_rejects_old_publication() {
    let controller = controller();
    let (release, wait) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut inner = controller.inner.lock().unwrap();
        inner.stop = Some(stop.clone());
        inner.thread = Some(std::thread::spawn(move || {
            wait.recv().unwrap();
        }));
    }
    assert_eq!(
        controller.stop_worker().await.unwrap_err(),
        "capture_failed"
    );
    assert!(stop.load(Ordering::Acquire));
    assert!(controller.inner.lock().unwrap().thread.is_some());
    assert!(!DownlinkController::generation_matches(
        &controller.inner.lock().unwrap(),
        1,
        1
    ));
    release.send(()).unwrap();
    controller.stop_worker().await.unwrap();
    assert!(controller.inner.lock().unwrap().thread.is_none());
}

#[tokio::test]
async fn cancelled_stop_future_retains_handle_for_later_shutdown() {
    let controller = controller();
    let (release, wait) = std::sync::mpsc::channel();
    {
        let mut inner = controller.inner.lock().unwrap();
        inner.stop = Some(Arc::new(AtomicBool::new(false)));
        inner.thread = Some(std::thread::spawn(move || {
            wait.recv().unwrap();
        }));
    }
    assert!(tokio::time::timeout(
        std::time::Duration::from_millis(20),
        controller.stop_worker()
    )
    .await
    .is_err());
    assert!(controller.inner.lock().unwrap().thread.is_some());
    release.send(()).unwrap();
    controller.shutdown().await.unwrap();
    assert!(controller.inner.lock().unwrap().thread.is_none());
}
#[tokio::test]
async fn legacy_never_receives_new_frames_and_reconnect_resets_request() {
    let controller = controller();
    let (control, mut rx) = mpsc::channel(10);
    let (audio, _) = mpsc::channel(5);
    controller
        .attach(
            1,
            1,
            false,
            control.clone(),
            audio.clone(),
            CancellationToken::new(),
        )
        .await;
    assert!(rx.try_recv().is_err());
    controller
        .request(
            1,
            DownlinkRequestMessage {
                session_id: 1,
                request_id: 1,
                enabled: true,
            },
        )
        .await;
    assert!(!controller.status().requested);
    controller
        .attach(2, 2, true, control, audio, CancellationToken::new())
        .await;
    let state = rx.recv().await.unwrap().downlink_state.unwrap();
    assert_eq!(
        (
            state.session_id,
            state.request_id,
            state.stream_id,
            state.active
        ),
        (2, 0, 0, false)
    );
    assert!(!controller.status().requested);
}
#[tokio::test]
async fn stale_request_and_old_cleanup_cannot_change_new_connection() {
    let controller = controller();
    let (control, mut rx) = mpsc::channel(10);
    let (audio, _) = mpsc::channel(5);
    controller
        .attach(2, 20, true, control, audio, CancellationToken::new())
        .await;
    rx.recv().await.unwrap();
    controller
        .request(
            2,
            DownlinkRequestMessage {
                session_id: 20,
                request_id: 2,
                enabled: false,
            },
        )
        .await;
    let stopped = rx.recv().await.unwrap().downlink_state.unwrap();
    assert_eq!(stopped.request_id, 2);
    controller
        .request(
            2,
            DownlinkRequestMessage {
                session_id: 20,
                request_id: 1,
                enabled: true,
            },
        )
        .await;
    controller
        .request(
            2,
            DownlinkRequestMessage {
                session_id: 19,
                request_id: 3,
                enabled: true,
            },
        )
        .await;
    controller.detach(1).await;
    assert!(rx.try_recv().is_err());
    assert!(controller.status().phone_supported);
    assert!(!controller.status().requested);
    controller.shutdown().await.unwrap();
    assert!(!controller.status().phone_supported);
}
#[tokio::test]
async fn old_generation_and_changed_output_endpoint_cannot_send_audio() {
    let controller = controller();
    let (control, _) = mpsc::channel(10);
    let (audio, _) = mpsc::channel(5);
    controller
        .attach(2, 20, true, control, audio, CancellationToken::new())
        .await;
    {
        let mut inner = controller.inner.lock().unwrap();
        inner.session.as_mut().unwrap().stream_id = 4;
        inner.stop = Some(Arc::new(AtomicBool::new(false)));
        inner.status.active = true;
        inner.capture_source = Some("endpoint-A".into());
    }
    let mut message = DownlinkAudioMessage {
        session_id: 20,
        stream_id: 4,
        sequence_number: 0,
        pcm: vec![0; 3840],
    };
    assert!(controller.accepts_audio(2, &message));
    message.stream_id = 3;
    assert!(!controller.accepts_audio(2, &message));
    message.stream_id = 4;
    assert!(!controller.accepts_audio(1, &message));
    *controller.output[0].write().unwrap() = Some("endpoint-A".into());
    assert!(!controller.accepts_audio(2, &message));
    controller.transport_failed(1);
    assert!(controller.status().active);
    controller.transport_failed(2);
    assert!(!controller.status().active);
    assert_eq!(
        controller.status().error.as_deref(),
        Some("transport_stalled")
    );
}
