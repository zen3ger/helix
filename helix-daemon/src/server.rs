use anyhow::{Context, Result};
use futures_util::StreamExt;
use signal_hook::{consts::signal, low_level};
use signal_hook_tokio::Signals;
use tokio::sync::mpsc;
use tokio_seqpacket::{UnixSeqpacket, UnixSeqpacketListener};

use std::collections::HashMap;
use std::path::Path;

use crate::channel::Channel;
use crate::proto::{self, SessionId, SessionRequest, SessionResponse};
use crate::session::{self, Session, SessionHandle};

pub struct Server {
    listen: UnixSeqpacketListener,
    signals: Signals,
    next_sid: u64,
    sessions: HashMap<SessionId, SessionHandle>,
    events: (
        mpsc::Sender<session::SessionEvent>,
        mpsc::Receiver<session::SessionEvent>,
    ),
    run: bool,
}

impl Server {
    pub fn new<P: AsRef<Path>>(addr: P) -> Result<Self> {
        let listen =
            UnixSeqpacketListener::bind(addr).context("failed to bind to server address")?;
        let signals = Signals::new(&[signal::SIGINT, signal::SIGTERM])?;
        let sessions = HashMap::new();
        // TODO: make this configurable
        let events = mpsc::channel(10);

        Ok(Self {
            listen,
            signals,
            next_sid: 0,
            sessions,
            events,
            run: true,
        })
    }

    pub async fn run(&mut self) -> Result<i32> {
        while self.run {
            tokio::select! {
                biased;

                Some(signal) = self.signals.next() =>
                    return self.handle_signal(signal).await,

                conn = self.listen.accept() => {
                    match conn {
                        Err(e) => log::error!("failed to accept new connection: {}", e),
                        Ok(conn) => self.handle_connection(conn).await,
                    }
                }

                Some(event) = self.events.1.recv() => {
                    self.handle_event(event).await;
                }
            }
        }

        self.cleanup();
        Ok(0)
    }

    fn cleanup(&self) {
        if let Ok(addr) = self.listen.local_addr() {
            if let Err(_) = std::fs::remove_file(addr.clone()) {
                log::error!("failed to unlink socket file ({})", addr.display());
            }
        }
    }

    async fn handle_signal(&mut self, signal: i32) -> Result<i32> {
        match signal {
            signal::SIGINT | signal::SIGTERM => {
                for (sid, mut session) in self.sessions.drain() {
                    let _ = session.terminate(true).await;
                    if let Err(e) = session.join().await {
                        log::error!("{}: force terminated with: {}", sid, e);
                    }
                }
                self.cleanup();
                low_level::emulate_default_handler(signal).unwrap();
                Ok(0)
            }
            _ => unreachable!(),
        }
    }

    async fn handle_connection(&mut self, conn: UnixSeqpacket) {
        let mut channel = Channel::new(conn);

        let req = match channel.recv().await {
            Err(e) => {
                log::error!("initial message exchange failed: {}", e);
                let _ = channel.shutdown();
                return;
            }
            Ok(req) => req,
        };

        println!("REQ {:?}", req);

        match req {
            proto::Request::NewSession => {
                self.next_sid += 1;
                let sid = SessionId(self.next_sid);
                match channel.send(&proto::Response::NewSession(sid)).await {
                    Err(e) => {
                        log::error!("failed to send new session response: {}", e);
                        let _ = channel.shutdown();
                    }
                    Ok(()) => {
                        let event = self.events.0.clone();
                        let handle = Session::spawn(sid, channel.reuse().into_detachable(), event);
                        self.sessions.insert(sid, handle);
                    }
                }
            }

            proto::Request::AttachSession(sid) => match self.sessions.get_mut(&sid) {
                None => {
                    log::warn!("requeseted attach on {} which does not exist", sid);
                    let _ = channel.send(&proto::Response::Err).await;
                    let _ = channel.shutdown();
                }
                Some(session) if !session.is_detached() => {
                    log::warn!("requested attach on {} which is occupied", sid);
                    let _ = channel.send(&proto::Response::Err).await;
                    let _ = channel.shutdown();
                }
                Some(session) => {
                    match channel.send(&proto::Response::NewSession(sid)).await {
                        Err(e) => {
                            log::error!("failed to send attach response: {}", e);
                            let _ = channel.shutdown();
                        }
                        Ok(()) => {
                            if let Err(e) = session.attach(channel.reuse()).await {
                                // FIXME: if channel is not shutdown on drop this might cause hanging on client
                                log::error!("failed to send attach request to session: {}", e);
                                return;
                            }
                        }
                    }
                }
            },

            proto::Request::AttachSessionByAlias(alias) => {
                for (sid, session) in self.sessions.iter_mut() {
                    if session.alias() == &alias {
                        if !session.is_detached() {
                            log::warn!("requested attach on {} which is occupied", sid);
                            let _ = channel.send(&proto::Response::Err).await;
                            let _ = channel.shutdown();
                        } else {
                            if let Err(e) = channel
                                .send(&proto::Response::NewSession(sid.clone()))
                                .await
                            {
                                log::error!("failed to send attach response: {}", e);
                            } else {
                                if let Err(e) = session.attach(channel.reuse()).await {
                                    log::error!("failed to send attach request to session: {}", e);
                                }
                            }
                        }
                        return;
                    }
                }

                log::warn!("requested attach on \"{}\" which does not exist", alias);
                let _ = channel.send(&proto::Response::Err).await;
                let _ = channel.shutdown();
            }

            proto::Request::AliasSession { sid, alias } => {
                if let Some(session) = self.sessions.get_mut(&sid) {
                    session.set_alias(alias);
                    let _ = channel.send(&proto::Response::Ok).await;
                } else {
                    log::warn!("alias request for non-axisting session");
                    let _ = channel.send(&proto::Response::Err).await;
                }
                let _ = channel.shutdown();
            }

            proto::Request::ListSessions => {
                let mut sessions = vec![];
                for (sid, session) in self.sessions.iter() {
                    sessions.push((
                        sid.clone(),
                        session.timestamp().clone(),
                        session.alias().clone(),
                    ))
                }
                let _ = channel.send(&proto::Response::ListSessions(sessions)).await;
                let _ = channel.shutdown();
            }

            proto::Request::KillSession { sid, force } => match self.sessions.get_mut(&sid) {
                Some(session) => {
                    if let Err(e) = session.terminate(force).await {
                        log::error!("kill request for {} failed: {}", sid, e);
                        let _ = channel.send(&proto::Response::Err).await;
                    } else {
                        let _ = channel.send(&proto::Response::Ok).await;
                    }
                    let _ = channel.shutdown();
                }
                None => {
                    log::warn!("kill request for non-existing session");
                    let _ = channel.send(&proto::Response::Err).await;
                    let _ = channel.shutdown();
                }
            },

            proto::Request::KillSessionByAlias { alias, force } => {
                for (sid, session) in self.sessions.iter_mut() {
                    if session.alias() == &alias {
                        if let Err(e) = session.terminate(force).await {
                            log::error!("kill request for {} failed: {}", sid, e);
                            let _ = channel.send(&proto::Response::Err).await;
                        } else {
                            let _ = channel.send(&proto::Response::Ok).await;
                        }
                        let _ = channel.shutdown();
                        return;
                    }
                }
                log::warn!("kill request for non-existing alias \"{}\"", alias);
                let _ = channel.send(&proto::Response::Err).await;
                let _ = channel.shutdown();
            }

            proto::Request::StopServer { force } => {
                log::info!("stop request received");
                for (sid, mut session) in self.sessions.drain() {
                    if let Err(e) = session.terminate(force).await {
                        log::error!("failed to terminate {} on stop request: {}", sid, e);
                    }
                }
                let _ = channel.send(&proto::Response::Ok).await;
                let _ = channel.shutdown();
                self.run = false;
            }
        }
    }

    async fn handle_event(&mut self, event: session::SessionEvent) {
        match event.kind {
            session::SessionEventKind::Terminated { expected } => {
                let session = self.sessions.remove(&event.sid).unwrap();
                let info = session.join().await;
                if expected {
                    log::info!("{}: terminated", event.sid);
                    assert!(info.is_ok());
                } else {
                    log::error!(
                        "{}: terminated unexpectedly, reason: {}",
                        event.sid,
                        info.unwrap_err()
                    );
                }
            }
            session::SessionEventKind::ClientDetached => {
                let session = self.sessions.get_mut(&event.sid).unwrap();
                session.set_detached(true);
                log::info!("{}: client detached", event.sid);
            }
        }
    }
}

#[derive(Debug)]
pub enum ServerEvent {
    RequestTermination { forced: bool },
    AttachRequest(Channel<SessionResponse, SessionRequest>),
}
