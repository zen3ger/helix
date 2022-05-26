use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use tokio_seqpacket::UnixSeqpacket;

// A type that can receive `Req` requests and answer with `Rsp` response types.
#[derive(Debug)]
pub struct Channel<Tx, Rx> {
    buf: std::io::Cursor<[u8; 1024]>,
    conn: UnixSeqpacket,
    _typ: std::marker::PhantomData<(Tx, Rx)>,
}

impl<Tx, Rx> Channel<Tx, Rx> {
    // Create a new `[Channel]` wrapping a `[UnixSeqpacket]`.
    pub fn new(conn: UnixSeqpacket) -> Self {
        Self {
            buf: std::io::Cursor::new([0u8; 1024]),
            conn,
            _typ: std::marker::PhantomData,
        }
    }

    // Shutdown the underlying connection.
    pub fn shutdown(&self) -> Result<()> {
        self.conn
            .shutdown(std::net::Shutdown::Both)
            .context("failed to shutdown connection")
    }

    // Transform an exsiting `[Channel]` to send and receive different type of
    // requests and responses.
    pub fn reuse<ForTx, ForRx>(self) -> Channel<ForTx, ForRx> {
        Channel {
            buf: self.buf,
            conn: self.conn,
            _typ: std::marker::PhantomData,
        }
    }

    // Create a `[DetachableChannel]` by consuming the original `[Channel]`.
    pub fn into_detachable(self) -> DetachableChannel<Tx, Rx> {
        DetachableChannel {
            channel: Some(self),
            attach_notify: tokio::sync::Notify::new(),
            detaching: false,
        }
    }
}

impl<Tx, Rx> Channel<Tx, Rx>
where
    Tx: Serialize,
    Rx: DeserializeOwned,
{
    // Send response to peer.
    pub async fn send(&mut self, msg: &Tx) -> Result<()> {
        bincode::serialize_into(&mut self.buf, msg)?;
        let bytes = &self.buf.get_ref()[..self.buf.position() as usize];
        self.conn.send(bytes).await?;
        Ok(())
    }

    // Receive request from peer.
    pub async fn recv(&mut self) -> Result<Rx> {
        let n = self.conn.recv(self.buf.get_mut()).await?;
        let bytes = &self.buf.get_ref()[..n];
        bincode::deserialize(bytes).context("failed to deserialize response")
    }
}

// A type that can receive `Req` requests and answer with `Rsp` response types, similar to `[Channel]`.
// This channel can also be detached, which closes the peer connection, but
// stays alive until either `[shutdown]` is called or another peer is connected
// with `[attach]`.
#[derive(Debug)]
pub struct DetachableChannel<Tx, Rx> {
    channel: Option<Channel<Tx, Rx>>,
    attach_notify: tokio::sync::Notify,
    detaching: bool,
}

impl<Tx, Rx> DetachableChannel<Tx, Rx> {
    // Shutdown the underlying connection.
    pub fn shutdown(&mut self) -> Result<()> {
        if let Some(channel) = self.channel.take() {
            self.detaching = true;
            channel.shutdown()
        } else {
            Ok(())
        }
    }

    // Attaches a new peer.
    pub fn attach(&mut self, channel: Channel<Tx, Rx>) -> Result<()> {
        if let Some(_) = self.channel {
            let _ = channel.shutdown();
            return Err(anyhow::anyhow!("channel is occupied"));
        }

        self.channel = Some(channel);
        self.attach_notify.notify_one();
        Ok(())
    }

    // Marks the channel to be detached.
    //
    // **Note**: It does not close the existing peer connection, instead it
    //           waits for the peer to close first.
    pub fn detach(&mut self) {
        self.detaching = true;
    }

    pub fn is_attached(&self) -> bool {
        self.channel.is_some()
    }
}

impl<Tx, Rx> DetachableChannel<Tx, Rx>
where
    Tx: Serialize,
    Rx: DeserializeOwned,
{
    // Send response to peer.
    pub async fn send(&mut self, msg: &Tx) -> Result<()> {
        if let Some(channel) = &mut self.channel {
            channel.send(msg).await
        } else {
            Err(anyhow::anyhow!("send on detached channel"))
        }
    }

    // Receive request from peer.
    pub async fn recv(&mut self) -> Result<Option<Rx>> {
        if let Some(channel) = &mut self.channel {
            match channel.conn.recv(channel.buf.get_mut()).await {
                Ok(0) => {
                    let _ = self.shutdown();
                    if self.detaching {
                        self.detaching = false;
                        Ok(None)
                    } else {
                        Err(anyhow::anyhow!("unexpected disconnect"))
                    }
                }
                Ok(n) => {
                    let bytes = &channel.buf.get_ref()[..n];
                    let msg =
                        bincode::deserialize(bytes).context("failed to deserialize response")?;
                    Ok(Some(msg))
                }
                Err(e) => Err(e).context("receive error"),
            }
        } else {
            self.attach_notify.notified().await;
            Ok(None)
        }
    }
}
