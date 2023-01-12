use std::{
    convert::TryInto,
    io::{self, ErrorKind},
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};

use byte_string::ByteStr;
use bytes::Buf;
use kcp::{Error as KcpError, KcpResult};
use log::{debug, error, trace};
use tokio::{
    net::{ToSocketAddrs, UdpSocket},
    sync::mpsc,
    task::JoinHandle,
    time,
};

use crate::{
    config::KcpConfig,
    handshake::{HandshakePacket, HANDSHAKE_RET_CODE, HANDSHAKE_RET_MAGIC},
    session::KcpSessionManager,
    stream::KcpStream,
};

pub struct KcpListener {
    udp: Arc<UdpSocket>,
    accept_rx: mpsc::Receiver<(KcpStream, SocketAddr)>,
    task_watcher: JoinHandle<()>,
    pub sessions: KcpSessionManager,
}

impl Drop for KcpListener {
    fn drop(&mut self) {
        self.task_watcher.abort();
    }
}

impl KcpListener {
    /// Create an `KcpListener` bound to `addr`
    pub async fn bind<A: ToSocketAddrs>(config: KcpConfig, addr: A) -> KcpResult<KcpListener> {
        let udp = UdpSocket::bind(addr).await?;
        KcpListener::from_socket(config, udp).await
    }

    /// Create a `KcpListener` from an existed `UdpSocket`
    pub async fn from_socket(config: KcpConfig, udp: UdpSocket) -> KcpResult<KcpListener> {
        let udp = Arc::new(udp);
        let server_udp = udp.clone();

        let (accept_tx, accept_rx) = mpsc::channel(1024 /* backlogs */);
        let sessions = KcpSessionManager::new();
        // KcpSessionManager has Arc internally
        let sessions_clone = KcpSessionManager::clone(&sessions);

        let task_watcher = tokio::spawn(async move {
            let (close_tx, mut close_rx) = mpsc::channel(64);

            let mut packet_buffer = [0u8; 65536];
            loop {
                tokio::select! {
                    peer_addr = close_rx.recv() => {
                        let peer_addr = peer_addr.expect("close_tx closed unexpectly");
                        sessions_clone.close_peer(peer_addr);
                        trace!("session peer_addr: {} removed", peer_addr);
                    }

                    recv_res = udp.recv_from(&mut packet_buffer) => {
                        match recv_res {
                            Err(err) => {
                                error!("udp.recv_from failed, error: {}", err);
                                time::sleep(Duration::from_secs(1)).await;
                            }
                            Ok((n, peer_addr)) => {
                                let token;
                                let mut conv;
                                let packet = &mut packet_buffer[..n];
                                let mut sn = 0;

                                if n == 20 {
                                    conv = (&packet[4..]).get_u32_le();
                                    token = (&packet[8..]).get_u32_le();
                                } else {
                                    conv = kcp::get_conv(packet);
                                    token = kcp::get_token(packet);
                                    sn = kcp::get_sn(packet);
                                    if conv == 0 {
                                        // Allocate a conv for client.
                                        conv = sessions_clone.alloc_conv();
                                        debug!("allocate {} conv for peer: {}", conv, peer_addr);

                                        kcp::set_conv(packet, conv);
                                    }
                                }

                                log::trace!("received peer: {}, {:?}", peer_addr, ByteStr::new(packet));

                                let session = match sessions_clone.get_or_create(&config, conv, token, sn, &udp, peer_addr, &close_tx).await {
                                    Ok((s, created)) => {
                                        if created {
                                            // Created a new session, constructed a new accepted client
                                            let stream = KcpStream::with_session(s.clone());
                                            if let Err(..) = accept_tx.try_send((stream, peer_addr)) {
                                                debug!("failed to create accepted stream due to channel failure");

                                                // remove it from session
                                                sessions_clone.close_peer(peer_addr);
                                                continue;
                                            }
                                        } else {
                                            let session_conv = s.conv().await;
                                            if session_conv != conv {
                                                debug!("received peer: {} with conv: {} not match with session conv: {}",
                                                       peer_addr,
                                                       conv,
                                                       session_conv);
                                                continue;
                                            }
                                        }

                                        s
                                    },
                                    Err(err) => {
                                        error!("failed to create session, error: {}, peer: {}, conv: {}", err, peer_addr, conv);
                                        continue;
                                    }
                                };

                                if n == 20 {
                                    let mut handshake_packet = HandshakePacket::parse(
                                        packet[..20].try_into().unwrap()
                                    );
                                    if handshake_packet.is_handshake() {
                                        handshake_packet.code = HANDSHAKE_RET_CODE;
                                        handshake_packet.end_magic = HANDSHAKE_RET_MAGIC;
                                        session.input(&handshake_packet.to_bytes()).await;
                                    }
                                    if handshake_packet.is_disconnect() {
                                        sessions_clone.close_peer(peer_addr);
                                    }
                                } else {
                                    session.input(packet).await;
                                }
                            }
                        }
                    }
                }
            }
        });

        Ok(KcpListener {
            udp: server_udp,
            accept_rx,
            task_watcher,
            sessions,
        })
    }

    /// Accept a new connected `KcpStream`
    pub async fn accept(&mut self) -> KcpResult<(KcpStream, SocketAddr)> {
        match self.accept_rx.recv().await {
            Some(s) => Ok(s),
            None => Err(KcpError::IoError(io::Error::new(
                ErrorKind::Other,
                "accept channel closed unexpectly",
            ))),
        }
    }

    /// Get the local address of the underlying socket
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.udp.local_addr()
    }
}

#[cfg(unix)]
impl std::os::unix::io::AsRawFd for KcpListener {
    fn as_raw_fd(&self) -> std::os::unix::prelude::RawFd {
        self.udp.as_raw_fd()
    }
}

#[cfg(windows)]
impl std::os::windows::io::AsRawSocket for KcpListener {
    fn as_raw_socket(&self) -> std::os::windows::prelude::RawSocket {
        self.udp.as_raw_socket()
    }
}

#[cfg(test)]
mod test {
    use super::KcpListener;
    use crate::{config::KcpConfig, stream::KcpStream};
    use futures::future;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn multi_echo() {
        let _ = env_logger::try_init();

        let config = KcpConfig::default();

        let mut listener = KcpListener::bind(config, "127.0.0.1:0").await.unwrap();
        let server_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();

                tokio::spawn(async move {
                    let mut buffer = [0u8; 8192];
                    while let Ok(n) = stream.read(&mut buffer).await {
                        if n == 0 {
                            break;
                        }

                        let data = &buffer[..n];
                        stream.write_all(data).await.unwrap();
                        stream.flush().await.unwrap();
                    }
                });
            }
        });

        let mut vfut = Vec::new();

        for _ in 0..100 {
            vfut.push(async move {
                let mut stream = KcpStream::connect(&config, server_addr).await.unwrap();

                for _ in 0..20 {
                    const SEND_BUFFER: &[u8] = b"HELLO WORLD";
                    stream.write_all(SEND_BUFFER).await.unwrap();
                    stream.flush().await.unwrap();

                    let mut buffer = [0u8; 1024];
                    let n = stream.recv(&mut buffer).await.unwrap();
                    assert_eq!(SEND_BUFFER, &buffer[..n]);
                }
            });
        }

        future::join_all(vfut).await;
    }
}
