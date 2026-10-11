//! 下行生命周期独立于手机麦克风上行；所有发布检查连接与流代际。
#[cfg(test)]
mod route_tests;
mod routing;
#[cfg(test)]
pub(crate) mod tests;
mod worker;
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
    pub supports_playback_routing: bool,
    pub phone_supports_playback_routing: bool,
    pub route_state: String,
    pub route_error: Option<String>,
}

struct Session {
    connection_id: u64,
    session_id: i64,
    request_id: i64,
    stream_id: i64,
    control: mpsc::Sender<MessageWrapper>,
    audio: mpsc::Sender<MessageWrapper>,
    cancel: CancellationToken,
    supports_routing: bool,
}
pub struct DownlinkPeer {
    pub session_id: i64,
    pub supports_downlink: bool,
    pub supports_routing: bool,
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
    route: Arc<dyn routing::RouteLease>,
}
impl DownlinkController {
    pub fn new(events: SharedEvents, output: Vec<Arc<RwLock<Option<String>>>>) -> Arc<Self> {
        let route = routing::new_lease();
        Self::with_route(events, output, route)
    }
    fn with_route(
        events: SharedEvents,
        output: Vec<Arc<RwLock<Option<String>>>>,
        route: Arc<dyn routing::RouteLease>,
    ) -> Arc<Self> {
        let supports_playback_routing = route.supported();
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
                    supports_playback_routing,
                    phone_supports_playback_routing: false,
                    route_state: "idle".into(),
                    route_error: None,
                },
                session: None,
                stop: None,
                thread: None,
            }),
            operation: tokio::sync::Mutex::new(()),
            events,
            output,
            route,
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
                supports_playback_routing: inner.status.supports_playback_routing,
                route_state: inner.status.route_state.clone(),
                route_error: inner.status.route_error.clone().unwrap_or_default(),
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
            // 停止也结束流代际：关闭请求不启动新worker，旧完成回调仍不能污染它。
            if inner.stop.is_some() || inner.thread.is_some() || inner.status.active {
                if let Some(session) = inner.session.as_mut() {
                    if let Some(stream) = session.stream_id.checked_add(1) {
                        session.stream_id = stream;
                    } else {
                        session.cancel.cancel();
                    }
                }
            }
            if let Some(stop) = inner.stop.take() {
                stop.store(true, Ordering::Release);
            }
            inner.status.active = false;
            inner.capture_source = None;
        }
        // 先阻断当前代并恢复路由；采集线程停止超时不能把系统留在虚拟输出。
        if self.status().route_state != "restore_failed" {
            self.release_route().await;
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
        let result = match thread {
            Some(thread) => thread.join().map_err(|_| "capture_failed".to_string()),
            None => Ok(()),
        };
        result?;
        if self.status().route_state == "restore_failed" {
            Err("route_restore_required".into())
        } else {
            Ok(())
        }
    }
    pub async fn attach(
        self: &Arc<Self>,
        connection_id: u64,
        peer: DownlinkPeer,
        control: mpsc::Sender<MessageWrapper>,
        audio: mpsc::Sender<MessageWrapper>,
        cancel: CancellationToken,
    ) {
        let _gate = self.operation.lock().await;
        let _ = self.stop_worker().await;
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.session = Some(Session {
            connection_id,
            session_id: peer.session_id,
            request_id: 0,
            stream_id: 0,
            control,
            audio,
            cancel,
            supports_routing: peer.supports_routing,
        });
        inner.status.phone_supported = peer.supports_downlink && peer.session_id > 0;
        inner.status.phone_supports_playback_routing =
            peer.supports_routing && inner.status.phone_supported;
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
        inner.status.phone_supports_playback_routing = false;
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
        inner.status.phone_supports_playback_routing = false;
        inner.status.requested = false;
        inner.status.error = result.as_ref().err().cloned();
        self.publish(&inner);
        result
    }
    pub async fn request(self: &Arc<Self>, connection_id: u64, request: DownlinkRequestMessage) {
        // reader future可被断线select取消；已经开始的路由事务必须完整结束。
        let controller = self.clone();
        let _ = tokio::spawn(async move { controller.apply_request(connection_id, request).await })
            .await;
    }
    async fn apply_request(self: &Arc<Self>, connection_id: u64, request: DownlinkRequestMessage) {
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
            if request.enabled && !request.playback_error.is_empty() {
                return;
            }
            session.request_id = request.request_id;
            if request.enabled && inner.status.requested && inner.status.active {
                self.publish(&inner);
                return;
            }
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
            inner.status.error = routing::playback_error(&request.playback_error);
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
    pub fn transport_failed(self: &Arc<Self>, connection: u64) {
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
            let stream = inner.session.as_ref().unwrap().stream_id;
            drop(inner);
            let controller = self.clone();
            tokio::spawn(async move {
                controller
                    .finish_worker(connection, stream, Some("transport_stalled".into()))
                    .await;
            });
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
}
