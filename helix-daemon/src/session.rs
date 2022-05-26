use std::future::Future;

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::channel::{Channel, DetachableChannel};
use crate::proto::{SessionId, SessionRequest, SessionResponse, Str};
use crate::server::ServerEvent;

pub struct Session {
    sid: SessionId,
    // connection towards the actual client process
    channel: DetachableChannel<SessionResponse, SessionRequest>,
    // receiver for server generated events
    rx: mpsc::Receiver<ServerEvent>,
    // sender for session events that should be propagated
    // to the server
    event: mpsc::Sender<SessionEvent>,
    run: bool,
}

impl Session {
    // Spawn a new future to handle client session
    pub fn spawn(
        sid: SessionId,
        channel: DetachableChannel<SessionResponse, SessionRequest>,
        event: mpsc::Sender<SessionEvent>,
    ) -> SessionHandle {
        let (tx, rx) = mpsc::channel(2);

        let session = Self {
            sid,
            channel,
            rx,
            event,
            run: true,
        };

        let handle = SessionHandle {
            join: tokio::spawn(session.run()),
            tx,
            timestamp: std::time::SystemTime::now(),
            alias: Str::with_capacity(32),
            detached: false,
        };
        log::debug!("{}: new session started", sid);
        handle
    }

    fn run(mut self) -> impl Future<Output = Result<()>> {
        async move {
            while self.run {
                tokio::select! {
                    Some(event) = self.rx.recv() => {
                        match event {
                            ServerEvent::RequestTermination { forced } if forced  => {
                                let reason = Err(anyhow::anyhow!("requested forced termination"));
                                return self.terminate(reason).await;
                            }
                            ServerEvent::RequestTermination { .. } => {
                                println!("SERVER REQ TERMINATE");
                                return self.terminate(Ok(())).await;
                            }
                            event => self.handle_event(event).await?,
                        }
                    }

                    req = self.channel.recv() => {
                        println!("SESSION REQ: {:?}", req);
                        match req {
                            Ok(None) => continue,
                            Ok(Some(SessionRequest::Terminate)) => return self.terminate(Ok(())).await,
                            Ok(Some(req)) => self.handle_request(req).await?,
                            Err(e) => return self.terminate(Err(e)).await,
                        }
                    }
                }
            }

            Ok(())
        }
    }

    async fn handle_event(&mut self, event: ServerEvent) -> Result<()> {
        match event {
            ServerEvent::AttachRequest(channel) if self.channel.is_attached() => {
                log::error!("{}: attach request on occupied session", self.sid);
                let _ = channel.shutdown();
            }
            ServerEvent::AttachRequest(channel) => {
                self.channel.attach(channel)?;
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    async fn handle_request(&mut self, req: SessionRequest) -> Result<()> {
        match req {
            SessionRequest::Detach => {
                self.channel.detach();
                self.send_event(SessionEventKind::ClientDetached).await?;
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    async fn send_event(&mut self, kind: SessionEventKind) -> Result<()> {
        self.event
            .send(SessionEvent {
                sid: self.sid,
                kind,
            })
            .await
            .context("failed to send event to server")
    }

    async fn terminate(&mut self, reason: Result<()>) -> Result<()> {
        let expected = reason.is_ok();
        let terminated = SessionEventKind::Terminated { expected };

        let _ = self
            .channel
            .send(&SessionResponse::Terminated(!expected))
            .await;
        self.channel.detach();
        if let Err(e) = self.channel.shutdown() {
            log::error!("{}: failed to shutdown session socket: {}", self.sid, e);
        }

        if let Err(e) = self.send_event(terminated).await {
            log::error!(
                "{}: failed to notify server about termination: {}",
                self.sid,
                e
            );
        }

        self.run = false;

        reason
    }
}

// Events reported by the `[Session]` towards the `[Server]`
#[derive(Debug)]
pub struct SessionEvent {
    pub sid: SessionId,
    pub kind: SessionEventKind,
}

// Types of `[SessionEvent]`s that the server can receive.
#[derive(Debug)]
pub enum SessionEventKind {
    // sent if client closed the connection
    Terminated { expected: bool },
    ClientDetached,
}

// A handle to the async `[Session]`
pub struct SessionHandle {
    join: JoinHandle<Result<()>>,
    tx: mpsc::Sender<ServerEvent>,
    timestamp: std::time::SystemTime,
    alias: Str,
    detached: bool,
}

impl SessionHandle {
    // Request the termination of the `[Session]` associated with this handle.
    pub async fn terminate(&mut self, forced: bool) -> Result<()> {
        self.tx
            .send(ServerEvent::RequestTermination { forced })
            .await
            .context("failed to send termination request")
    }

    pub fn is_detached(&self) -> bool {
        self.detached
    }

    pub fn timestamp(&self) -> &std::time::SystemTime {
        &self.timestamp
    }

    pub fn alias(&self) -> &Str {
        &self.alias
    }

    pub fn set_alias(&mut self, alias: Str) {
        self.alias = alias;
    }

    pub fn set_detached(&mut self, detached: bool) {
        self.detached = detached;
    }

    // Attach a new `[Channel]` to an existing `[Session]`.
    pub async fn attach(
        &mut self,
        channel: Channel<SessionResponse, SessionRequest>,
    ) -> Result<()> {
        assert!(self.detached);
        self.tx
            .send(ServerEvent::AttachRequest(channel))
            .await
            .context("failed to send attach request")?;
        self.detached = false;
        Ok(())
    }

    // Wait for the termination of the `[Session]` associated with this handle.
    pub async fn join(self) -> Result<()> {
        match self.join.await {
            Ok(result) => result,
            Err(e) => Err(e).context("failed to gracefully terminate session"),
        }
    }
}
