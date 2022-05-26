use anyhow::Result;
use futures_util::StreamExt;
use signal_hook::{consts::signal, low_level};
use signal_hook_tokio::Signals;
use tokio_seqpacket::UnixSeqpacket;

use std::path::Path;

use crate::channel::{Channel, DetachableChannel};
use crate::proto::{self, Request, Response, SessionId, SessionRequest, SessionResponse, Str};

pub struct Client {
    signals: Signals,
    channel: Channel<Request, Response>,
}

pub struct SessionClient {
    signals: Signals,
    channel: DetachableChannel<SessionRequest, SessionResponse>,
    sid: SessionId,
    run: bool,
}

impl SessionClient {
    pub async fn run(&mut self) -> Result<i32> {
        while self.run {
            tokio::select! {
                biased;

                Some(signal) = self.signals.next() => return self.handle_signal(signal).await,

                rsp = self.channel.recv() => {
                    println!("SESSION RSP: {:?}", rsp);
                    match rsp {
                        Ok(None) => continue,
                        Ok(Some(SessionResponse::Terminated(forced))) => return self.terminate(Ok(forced)),
                        Ok(Some(rsp)) => self.handle_response(rsp).await?,
                        Err(e) => return self.terminate(Err(e)),
                    }
                }
            }
        }

        Ok(0)
    }

    pub async fn detach(&mut self) -> Result<()> {
        self.channel.send(&proto::SessionRequest::Detach).await?;
        let _ = self.channel.shutdown();
        Ok(())
    }

    async fn handle_signal(&mut self, signal: i32) -> Result<i32> {
        match signal {
            signal::SIGINT | signal::SIGTERM => {
                self.channel.send(&proto::SessionRequest::Terminate).await?;
                // let _ = self.channel.shutdown();
                low_level::emulate_default_handler(signal).unwrap();
                Ok(0)
            }
            _ => unreachable!(),
        }
    }

    async fn handle_response(&mut self, rsp: SessionResponse) -> Result<()> {
        match rsp {
            SessionResponse::Err => {}
            SessionResponse::Ok => {}
            _ => unreachable!(),
        }

        Ok(())
    }

    pub fn terminate(&mut self, reason: Result<bool>) -> Result<i32> {
        match reason {
            Ok(forced) => {
                let _ = self.channel.shutdown();
                // FIXME: prompt user to save if not forced
                Ok(if forced { 1 } else { 0 })
            }
            Err(e) => Err(e),
        }
    }
}

impl Client {
    pub async fn connect<P: AsRef<Path>>(addr: P) -> Result<Self> {
        let signals = Signals::new(&[signal::SIGINT, signal::SIGTERM])?;
        let conn = UnixSeqpacket::connect(addr).await?;

        let channel = Channel::new(conn);

        Ok(Self { signals, channel })
    }

    pub async fn start_session(mut self) -> Result<SessionClient> {
        self.channel.send(&proto::Request::NewSession).await?;

        let sid = match self.channel.recv().await? {
            proto::Response::NewSession(sid) => sid,
            proto::Response::Err => anyhow::bail!("server returned an error"),
            _ => anyhow::bail!("unexpected server response"),
        };

        Ok(SessionClient {
            signals: self.signals,
            channel: self.channel.reuse().into_detachable(),
            sid,
            run: true,
        })
    }

    pub async fn attach_session(mut self, sid: SessionId) -> Result<SessionClient> {
        self.channel
            .send(&proto::Request::AttachSession(sid))
            .await?;

        let sid = match self.channel.recv().await? {
            proto::Response::Err => anyhow::bail!("server returned an error"),
            proto::Response::NewSession(sid) => sid,
            _ => anyhow::bail!("unexpected server response"),
        };

        Ok(SessionClient {
            signals: self.signals,
            channel: self.channel.reuse().into_detachable(),
            sid,
            run: true,
        })
    }

    pub async fn attach_session_by_alias(mut self, alias: Str) -> Result<SessionClient> {
        self.channel
            .send(&proto::Request::AttachSessionByAlias(alias))
            .await?;
        let sid = match self.channel.recv().await? {
            proto::Response::NewSession(sid) => sid,
            proto::Response::Err => anyhow::bail!("server returned an error"),
            _ => anyhow::bail!("unexpected server response"),
        };

        Ok(SessionClient {
            signals: self.signals,
            channel: self.channel.reuse().into_detachable(),
            sid,
            run: true,
        })
    }

    pub async fn alias_session(mut self, sid: SessionId, alias: Str) -> Result<()> {
        self.channel
            .send(&proto::Request::AliasSession { sid, alias })
            .await?;
        let _ = match self.channel.recv().await? {
            proto::Response::Ok => (),
            proto::Response::Err => anyhow::bail!("server returned an error"),
            _ => anyhow::bail!("unexpected server response"),
        };
        let _ = self.channel.shutdown();

        Ok(())
    }

    pub async fn list_sessions(mut self) -> Result<Vec<(SessionId, std::time::SystemTime, Str)>> {
        self.channel.send(&proto::Request::ListSessions).await?;
        let sessions = match self.channel.recv().await? {
            proto::Response::ListSessions(sessions) => sessions,
            proto::Response::Err => anyhow::bail!("server returned an error"),
            _ => anyhow::bail!("unexpected server response"),
        };
        let _ = self.channel.shutdown();

        Ok(sessions)
    }

    pub async fn kill_session(mut self, sid: SessionId, force: bool) -> Result<()> {
        self.channel
            .send(&proto::Request::KillSession { sid, force })
            .await?;
        let _ = match self.channel.recv().await? {
            proto::Response::Ok => (),
            proto::Response::Err => anyhow::bail!("server returned an error"),
            _ => anyhow::bail!("unexpected server response"),
        };
        let _ = self.channel.shutdown();

        Ok(())
    }

    pub async fn kill_session_by_alias(mut self, alias: Str, force: bool) -> Result<()> {
        self.channel
            .send(&proto::Request::KillSessionByAlias { alias, force })
            .await?;
        let _ = match self.channel.recv().await? {
            proto::Response::Ok => (),
            proto::Response::Err => anyhow::bail!("server returned an error"),
            _ => anyhow::bail!("unexpected server response"),
        };
        let _ = self.channel.shutdown();

        Ok(())
    }

    pub async fn stop_server(mut self, force: bool) -> Result<()> {
        self.channel
            .send(&proto::Request::StopServer { force })
            .await?;
        let _ = match self.channel.recv().await? {
            proto::Response::Ok => (),
            _ => anyhow::bail!("unexpected server response"),
        };
        let _ = self.channel.shutdown();

        Ok(())
    }
}
