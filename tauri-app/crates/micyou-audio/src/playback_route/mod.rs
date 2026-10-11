//! 三个 render 默认角色的独占、可恢复事务；不提供 capture 写接口。
#[cfg(windows)]
mod journal;
#[cfg(any(windows, test))]
mod lease;
#[cfg(windows)]
mod native;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteState {
    Idle,
    Held,
    Restored,
    RestoreFailed,
    ReleasedExternal,
}

impl RouteState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Held => "held",
            Self::Restored => "restored",
            Self::RestoreFailed => "restore_failed",
            Self::ReleasedExternal => "released_external",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteOutcome {
    pub state: RouteState,
    pub error: Option<String>,
}

impl RouteOutcome {
    fn new(state: RouteState, error: Option<String>) -> Self {
        Self { state, error }
    }
}

#[cfg(windows)]
enum Command {
    Acquire(
        String,
        std::sync::mpsc::Sender<Result<RouteOutcome, String>>,
    ),
    Release(std::sync::mpsc::Sender<RouteOutcome>),
    Recover(std::sync::mpsc::Sender<RouteOutcome>),
    External(std::sync::mpsc::Sender<bool>),
    Status(std::sync::mpsc::Sender<RouteOutcome>),
}

/// Send+Sync 门面；COM 对象与线程所有权 mutex 只由单个 MTA actor 持有。
pub struct PlaybackRouteController {
    #[cfg(windows)]
    tx: std::sync::mpsc::Sender<Command>,
}

impl PlaybackRouteController {
    pub fn supported() -> bool {
        cfg!(windows)
    }

    #[cfg(windows)]
    pub fn new() -> Result<Self, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let (ready, startup) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("micyou-playback-route".into())
            .spawn(move || {
                let backend = match native::NativeBackend::new() {
                    Ok(backend) => backend,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                let journal = match journal::FileJournal::new() {
                    Ok(journal) => journal,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                let mut machine = lease::LeaseMachine::new(backend, journal);
                let _ = ready.send(Ok(()));
                while let Ok(command) = rx.recv() {
                    match command {
                        Command::Acquire(target, reply) => {
                            let _ = reply.send(machine.acquire(&target));
                        }
                        Command::Release(reply) => {
                            let _ = reply.send(machine.release());
                        }
                        Command::Recover(reply) => {
                            let _ = reply.send(machine.recover_stale());
                        }
                        Command::External(reply) => {
                            let _ = reply.send(machine.external_change());
                        }
                        Command::Status(reply) => {
                            let _ = reply.send(machine.status());
                        }
                    }
                }
                machine.release();
            })
            .map_err(|error| error.to_string())?;
        startup
            .recv()
            .map_err(|_| "routing actor initialization failed".to_string())??;
        Ok(Self { tx })
    }

    #[cfg(not(windows))]
    pub fn new() -> Result<Self, String> {
        Err("automatic playback routing unsupported".into())
    }

    #[cfg(windows)]
    fn request<T>(
        &self,
        command: impl FnOnce(std::sync::mpsc::Sender<T>) -> Command,
    ) -> Result<T, String> {
        let (reply, response) = std::sync::mpsc::channel();
        self.tx
            .send(command(reply))
            .map_err(|_| "routing actor unavailable".to_string())?;
        response
            .recv()
            .map_err(|_| "routing actor unavailable".to_string())
    }

    pub fn acquire(&self, target_id: &str) -> Result<RouteOutcome, String> {
        #[cfg(windows)]
        {
            self.request(|reply| Command::Acquire(target_id.into(), reply))?
                .map_err(|error| {
                    if error.starts_with("route_") {
                        error
                    } else {
                        format!("route_acquire_failed: {error}")
                    }
                })
        }
        #[cfg(not(windows))]
        {
            let _ = target_id;
            Err("automatic playback routing unsupported".into())
        }
    }

    pub fn release(&self) -> RouteOutcome {
        #[cfg(windows)]
        {
            self.request(Command::Release)
                .unwrap_or_else(|error| RouteOutcome::new(RouteState::RestoreFailed, Some(error)))
        }
        #[cfg(not(windows))]
        {
            RouteOutcome::new(RouteState::Idle, None)
        }
    }

    pub fn recover_stale(&self) -> RouteOutcome {
        #[cfg(windows)]
        {
            self.request(Command::Recover)
                .unwrap_or_else(|error| RouteOutcome::new(RouteState::RestoreFailed, Some(error)))
        }
        #[cfg(not(windows))]
        {
            RouteOutcome::new(RouteState::Idle, None)
        }
    }

    pub fn external_change(&self) -> bool {
        #[cfg(windows)]
        {
            self.request(Command::External).unwrap_or(true)
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    pub fn status(&self) -> RouteOutcome {
        #[cfg(windows)]
        {
            self.request(Command::Status)
                .unwrap_or_else(|error| RouteOutcome::new(RouteState::RestoreFailed, Some(error)))
        }
        #[cfg(not(windows))]
        {
            RouteOutcome::new(RouteState::Idle, None)
        }
    }
}
