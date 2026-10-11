//! 下行生命周期独立于手机麦克风上行；所有发布检查连接与流代际。
#[cfg(test)]
pub(crate) mod tests;
use crate::events::SharedEvents;
use micyou_audio::stereo_loopback;
use micyou_protocol::micyou::{
    DownlinkAudioMessage, DownlinkRequestMessage, DownlinkStateMessage, MessageWrapper,
};
use serde::Serialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, RwLock,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const AUDIO_QUEUE_FRAMES: usize = 5;
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownlinkSource {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub is_feedback: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownlinkStatus {
    pub supported: bool,
    pub phone_supported: bool,
    pub requested: bool,
    pub active: bool,
    pub source_id: Option<String>,
    pub error: Option<String>,
    pub dropped_frames: u64,
}

struct Session {
    connection_id: u64,
    session_id: i64,
    request_id: i64,
    stream_id: i64,
    control: mpsc::Sender<MessageWrapper>,
    audio: mpsc::Sender<MessageWrapper>,
    cancel: CancellationToken,
}
struct Inner {
    capture_source: Option<String>,
    status: DownlinkStatus,
    session: Option<Session>,
    stop: Option<Arc<AtomicBool>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
pub struct DownlinkController {
    identity: fn(&str) -> Result<micyou_audio::endpoint::OutputEndpoint, String>,
    inner: Mutex<Inner>,
    operation: tokio::sync::Mutex<()>,
    events: SharedEvents,
    output: Vec<Arc<RwLock<Option<String>>>>,
}
impl DownlinkController {
    pub fn new(events: SharedEvents, output: Vec<Arc<RwLock<Option<String>>>>) -> Arc<Self> {
        Arc::new(Self {
            identity: micyou_audio::endpoint::resolve_endpoint_identity,
            inner: Mutex::new(Inner {
                capture_source: None,
                status: DownlinkStatus {
                    supported: cfg!(windows),
                    phone_supported: false,
                    requested: false,
                    active: false,
                    source_id: crate::config::load_server_prefs().downlink_source_id,
                    error: None,
                    dropped_frames: 0,
                },
                session: None,
                stop: None,
                thread: None,
            }),
            operation: tokio::sync::Mutex::new(()),
            events,
            output,
        })
    }
    pub fn status(&self) -> DownlinkStatus {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .status
            .clone()
    }
    pub fn sources(&self) -> Result<Vec<DownlinkSource>, String> {
        stereo_loopback::list_render_endpoints().map(|sources| {
            sources
                .into_iter()
                .map(|source| DownlinkSource {
                    is_feedback: self.is_feedback(&source.id),
                    id: source.id,
                    name: source.name,
                    is_default: source.is_default,
                })
                .collect()
        })
    }
    fn is_feedback(&self, id: &str) -> bool {
        self.source_error(id) == Some("feedback_source")
    }
    fn source_error(&self, id: &str) -> Option<&'static str> {
        use micyou_audio::endpoint::{same_line, Component};
        let source = (self.identity)(id).ok();
        if source
            .as_ref()
            .is_none_or(|e| e.component == Component::UnknownVirtual)
        {
            return Some("identity_unavailable");
        }
        for output in &self.output {
            let actual = output.read().unwrap_or_else(|p| p.into_inner()).clone();
            if let Some(actual) = actual {
                if actual.eq_ignore_ascii_case(id) {
                    return Some("feedback_source");
                }
                let primary = (self.identity)(&actual).ok();
                if source
                    .as_ref()
                    .is_some_and(|e| e.component != Component::Physical)
                    && primary
                        .as_ref()
                        .is_none_or(|e| e.component == Component::UnknownVirtual)
                {
                    return Some("identity_unavailable");
                }
                if source
                    .as_ref()
                    .zip(primary.as_ref())
                    .is_some_and(|(source, primary)| same_line(source, primary))
                {
                    return Some("feedback_source");
                }
            }
        }
        None
    }
    fn publish(&self, inner: &Inner) {
        self.events.downlink_status_changed(inner.status.clone());
        let Some(session) = &inner.session else {
            return;
        };
        if !inner.status.phone_supported {
            return;
        }
        let message = MessageWrapper {
            downlink_state: Some(DownlinkStateMessage {
                session_id: session.session_id,
                request_id: session.request_id,
                stream_id: session.stream_id,
                supported: inner.status.supported,
                active: inner.status.active,
                error: inner.status.error.clone().unwrap_or_default(),
                sample_rate: micyou_protocol::DOWNLINK_SAMPLE_RATE,
                channel_count: micyou_protocol::DOWNLINK_CHANNEL_COUNT,
                audio_format: micyou_protocol::DOWNLINK_AUDIO_FORMAT,
            }),
            ..Default::default()
        };
        // 控制满载不能默默丢状态，否则手机可能一直播放已结束的流。
        if session.control.try_send(message).is_err() {
            session.cancel.cancel();
        }
    }
    async fn stop_worker(&self) -> Result<(), String> {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(stop) = inner.stop.take() {
                stop.store(true, Ordering::Release);
            }
            inner.status.active = false;
            inner.capture_source = None;
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            let finished = self
                .inner
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .thread
                .as_ref()
                .is_none_or(|thread| thread.is_finished());
            if finished {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("capture_failed".into());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // 句柄留在共享状态直到退出；停止 future 被连接取消时仍可继续 join。
        let thread = self
            .inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .thread
            .take();
        match thread {
            Some(thread) => thread.join().map_err(|_| "capture_failed".into()),
            None => Ok(()),
        }
    }
    pub async fn attach(
        self: &Arc<Self>,
        connection_id: u64,
        session_id: i64,
        supported: bool,
        control: mpsc::Sender<MessageWrapper>,
        audio: mpsc::Sender<MessageWrapper>,
        cancel: CancellationToken,
    ) {
        let _gate = self.operation.lock().await;
        let _ = self.stop_worker().await;
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.session = Some(Session {
            connection_id,
            session_id,
            request_id: 0,
            stream_id: 0,
            control,
            audio,
            cancel,
        });
        inner.status.phone_supported = supported && session_id > 0;
        inner.status.requested = false;
        inner.status.dropped_frames = 0;
        inner.status.error = if !inner.status.supported {
            Some("unsupported_platform".into())
        } else {
            None
        };
        self.publish(&inner);
    }
    pub async fn detach(&self, connection_id: u64) {
        let _gate = self.operation.lock().await;
        if self
            .inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .session
            .as_ref()
            .map(|s| s.connection_id)
            != Some(connection_id)
        {
            return;
        }
        let _ = self.stop_worker().await;
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.session = None;
        inner.status.phone_supported = false;
        inner.status.requested = false;
        inner.status.error = None;
        self.publish(&inner);
    }
    pub async fn shutdown(&self) -> Result<(), String> {
        let _gate = self.operation.lock().await;
        let result = self.stop_worker().await;
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.session = None;
        inner.status.phone_supported = false;
        inner.status.requested = false;
        inner.status.error = result.as_ref().err().cloned();
        self.publish(&inner);
        result
    }
    pub async fn request(self: &Arc<Self>, connection_id: u64, request: DownlinkRequestMessage) {
        let _gate = self.operation.lock().await;
        {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if !inner.status.phone_supported {
                return;
            }
            let Some(session) = &mut inner.session else {
                return;
            };
            if session.connection_id != connection_id
                || request.session_id != session.session_id
                || request.request_id <= session.request_id
            {
                return;
            }
            session.request_id = request.request_id;
            inner.status.requested = request.enabled;
        }
        if let Err(error) = self.stop_worker().await {
            self.fail_current(error);
            return;
        }
        if request.enabled {
            self.start_worker();
        } else {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            inner.status.error = None;
            self.publish(&inner);
        }
    }
    pub async fn set_source(
        self: &Arc<Self>,
        source: Option<String>,
    ) -> Result<DownlinkStatus, String> {
        let _gate = self.operation.lock().await;
        let sources = self.sources()?;
        let selected = sources
            .iter()
            .find(|s| source.as_ref().map_or(s.is_default, |id| id == &s.id))
            .ok_or("source_unavailable")?;
        if let Some(error) = self.source_error(&selected.id) {
            return Err(error.into());
        }
        self.replace_source(source, |source| {
            crate::config::update_server_prefs(|prefs| prefs.downlink_source_id = source.clone())
        })
        .await
    }
    async fn replace_source(
        self: &Arc<Self>,
        source: Option<String>,
        persist: impl FnOnce(&Option<String>) -> Result<(), String>,
    ) -> Result<DownlinkStatus, String> {
        if let Err(error) = self.stop_worker().await {
            self.fail_current(error.clone());
            return Err(error);
        }
        if let Err(error) = persist(&source) {
            self.fail_current("source_unavailable".into());
            return Err(error);
        }
        let requested = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            inner.status.source_id = source;
            inner.status.error = None;
            inner.status.requested
        };
        if requested {
            self.start_worker();
        } else {
            self.publish(&self.inner.lock().unwrap_or_else(|p| p.into_inner()));
        }
        Ok(self.status())
    }
    fn fail_current(&self, error: String) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.status.active = false;
        inner.status.error = Some(error);
        self.publish(&inner);
    }
    pub fn transport_failed(&self, connection: u64) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if inner
            .session
            .as_ref()
            .is_some_and(|s| s.connection_id == connection)
        {
            if let Some(stop) = &inner.stop {
                stop.store(true, Ordering::Release);
            }
            inner.status.active = false;
            inner.status.error = Some("transport_stalled".into());
            self.events.downlink_status_changed(inner.status.clone());
        }
    }
    fn start_worker(self: &Arc<Self>) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if !inner.status.supported {
            inner.status.error = Some("unsupported_platform".into());
            self.publish(&inner);
            return;
        }
        let source = inner.status.source_id.clone();
        let Some(session) = inner.session.as_mut() else {
            return;
        };
        let Some(stream_id) = session.stream_id.checked_add(1) else {
            session.cancel.cancel();
            return;
        };
        session.stream_id = stream_id;
        let connection_id = session.connection_id;
        let session_id = session.session_id;
        let audio = session.audio.clone();
        let cancel = session.cancel.clone();
        inner.status.active = false;
        inner.status.error = None;
        let stop = Arc::new(AtomicBool::new(false));
        inner.stop = Some(stop.clone());
        let controller = self.clone();
        let handle = std::thread::Builder::new()
            .name("micyou-downlink".into())
            .spawn(move || {
                controller.capture_worker(
                    connection_id,
                    session_id,
                    stream_id,
                    source,
                    stop,
                    cancel,
                    audio,
                );
            });
        match handle {
            Ok(thread) => inner.thread = Some(thread),
            Err(error) => {
                log::error!("[Downlink] spawn failed: {error}");
                inner.status.error = Some("capture_failed".into());
                self.publish(&inner);
            }
        }
    }
    fn generation_matches(inner: &Inner, connection: u64, stream: i64) -> bool {
        inner
            .session
            .as_ref()
            .is_some_and(|s| s.connection_id == connection && s.stream_id == stream)
            && inner
                .stop
                .as_ref()
                .is_some_and(|s| !s.load(Ordering::Acquire))
    }
    pub fn accepts_audio(&self, connection: u64, message: &DownlinkAudioMessage) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.status.active
            && Self::generation_matches(&inner, connection, message.stream_id)
            && inner
                .session
                .as_ref()
                .is_some_and(|s| s.session_id == message.session_id)
            && inner
                .capture_source
                .as_ref()
                .is_some_and(|id| self.source_error(id).is_none())
    }
    #[allow(clippy::too_many_arguments)]
    fn capture_worker(
        self: Arc<Self>,
        connection: u64,
        session: i64,
        stream: i64,
        source: Option<String>,
        stop: Arc<AtomicBool>,
        cancel: CancellationToken,
        audio: mpsc::Sender<MessageWrapper>,
    ) {
        let endpoints = stereo_loopback::list_render_endpoints();
        let endpoint = endpoints.ok().and_then(|sources| {
            sources
                .into_iter()
                .find(|s| source.as_ref().map_or(s.is_default, |id| id == &s.id))
        });
        let result = if let Some(endpoint) = endpoint {
            let mut sequence = 0i64;
            stereo_loopback::capture(
                &endpoint.id,
                stop.clone(),
                || {
                    if stop.load(Ordering::Acquire) || cancel.is_cancelled() {
                        return Err("cancelled".into());
                    }
                    if let Some(error) = self.source_error(&endpoint.id) {
                        return Err(error.into());
                    }
                    if source.is_none() {
                        let default = micyou_audio::endpoint::default_render_endpoint_id()?;
                        if !default.eq_ignore_ascii_case(&endpoint.id) {
                            return Err("source_unavailable".into());
                        }
                    }
                    Ok(())
                },
                || {
                    let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
                    if Self::generation_matches(&inner, connection, stream) {
                        inner.capture_source = Some(endpoint.id.clone());
                        inner.status.active = true;
                        inner.status.error = None;
                        self.publish(&inner);
                    }
                },
                |pcm| {
                    let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
                    if !Self::generation_matches(&inner, connection, stream) {
                        return;
                    }
                    let message = MessageWrapper {
                        downlink_audio: Some(DownlinkAudioMessage {
                            session_id: session,
                            stream_id: stream,
                            sequence_number: sequence,
                            pcm,
                        }),
                        ..Default::default()
                    };
                    sequence = sequence.saturating_add(1);
                    match audio.try_send(message) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            inner.status.dropped_frames =
                                inner.status.dropped_frames.saturating_add(1);
                            // 每秒至多发布一次统计，避免音频队列挤压控制队列。
                            if inner.status.dropped_frames % 50 == 1 {
                                self.events.downlink_status_changed(inner.status.clone());
                            }
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            stop.store(true, Ordering::Release);
                            cancel.cancel();
                        }
                    }
                },
            )
        } else {
            Err("source_unavailable".into())
        };
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if Self::generation_matches(&inner, connection, stream) {
            inner.status.active = false;
            inner.status.error = result.err().filter(|e| e != "cancelled").map(|error| {
                log::warn!("[Downlink] capture ended: {error}");
                if error == "feedback_source"
                    || error == "source_unavailable"
                    || error == "identity_unavailable"
                {
                    error
                } else {
                    "capture_failed".into()
                }
            });
            stop.store(true, Ordering::Release);
            self.publish(&inner);
        }
    }
}
