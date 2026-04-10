use crate::proxy::credentials::CredentialManager;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

// SOCKS5 constants (RFC 1928)
const SOCKS5_VERSION: u8 = 0x05;
const AUTH_USERNAME_PASSWORD: u8 = 0x02;
const AUTH_NO_ACCEPTABLE: u8 = 0xFF;
const CMD_CONNECT: u8 = 0x01;
const ATYP_IPV4: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const ATYP_IPV6: u8 = 0x04;

// SOCKS5 reply codes
const REP_SUCCESS: u8 = 0x00;
const REP_GENERAL_FAILURE: u8 = 0x01;
const REP_CONNECTION_REFUSED: u8 = 0x05;
const REP_COMMAND_NOT_SUPPORTED: u8 = 0x07;
const REP_ADDRESS_TYPE_NOT_SUPPORTED: u8 = 0x08;

// Auth sub-negotiation (RFC 1929)
const AUTH_SUBNEG_VERSION: u8 = 0x01;
const AUTH_SUCCESS: u8 = 0x00;
const AUTH_FAILURE: u8 = 0x01;

/// Handshake + auth + request timeout
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Upstream target connect timeout.
const TARGET_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Run the embedded SOCKS5 server.
pub async fn run_socks5_server(
    bind_addr: SocketAddr,
    credentials: Arc<CredentialManager>,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(bind_addr).await?;
    info!("SOCKS5 server listening on {}", bind_addr);

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("SOCKS5 server shutting down");
                break;
            }
            accept = listener.accept() => {
                match accept {
                    Ok((stream, peer)) => {
                        info!("Accepted SOCKS5 connection from {}", peer);
                        let creds = credentials.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, peer, creds).await {
                                warn!("SOCKS5 connection from {} error: {}", peer, e);
                            }
                        });
                    }
                    Err(e) => {
                        warn!("SOCKS5 accept error: {}", e);
                    }
                }
            }
        }
    }

    Ok(())
}

async fn handle_connection(
    mut stream: TcpStream,
    peer: SocketAddr,
    credentials: Arc<CredentialManager>,
) -> anyhow::Result<()> {
    // Set TCP keepalive matching Dante config
    set_keepalive(&stream)?;

    // Wrap handshake + auth + request in timeout
    let target = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        // 1. Greeting
        let version = stream.read_u8().await?;
        if version != SOCKS5_VERSION {
            anyhow::bail!("Invalid SOCKS version: {}", version);
        }

        let nmethods = stream.read_u8().await?;
        let mut methods = vec![0u8; nmethods as usize];
        stream.read_exact(&mut methods).await?;

        if !methods.contains(&AUTH_USERNAME_PASSWORD) {
            stream
                .write_all(&[SOCKS5_VERSION, AUTH_NO_ACCEPTABLE])
                .await?;
            anyhow::bail!("Client does not support username/password auth");
        }

        // Select username/password auth
        stream
            .write_all(&[SOCKS5_VERSION, AUTH_USERNAME_PASSWORD])
            .await?;

        // 2. Authentication (RFC 1929)
        let auth_version = stream.read_u8().await?;
        if auth_version != AUTH_SUBNEG_VERSION {
            anyhow::bail!("Invalid auth sub-negotiation version: {}", auth_version);
        }

        let ulen = stream.read_u8().await? as usize;
        let mut username = vec![0u8; ulen];
        stream.read_exact(&mut username).await?;
        let username = String::from_utf8_lossy(&username).to_string();

        let plen = stream.read_u8().await? as usize;
        let mut password = vec![0u8; plen];
        stream.read_exact(&mut password).await?;
        let password = String::from_utf8_lossy(&password).to_string();

        if !credentials.authenticate(&username, &password) {
            stream
                .write_all(&[AUTH_SUBNEG_VERSION, AUTH_FAILURE])
                .await?;
            anyhow::bail!("Authentication failed for user: {}", username);
        }

        stream
            .write_all(&[AUTH_SUBNEG_VERSION, AUTH_SUCCESS])
            .await?;
        info!("SOCKS5 auth success for {} from {}", username, peer);

        // 3. Request
        let ver = stream.read_u8().await?;
        if ver != SOCKS5_VERSION {
            anyhow::bail!("Invalid SOCKS version in request: {}", ver);
        }

        let cmd = stream.read_u8().await?;
        let _rsv = stream.read_u8().await?;
        let atyp = stream.read_u8().await?;

        if cmd != CMD_CONNECT {
            send_reply(&mut stream, REP_COMMAND_NOT_SUPPORTED).await?;
            anyhow::bail!("Unsupported command: {}", cmd);
        }

        // Parse target address
        let target_addr = match atyp {
            ATYP_IPV4 => {
                let mut addr = [0u8; 4];
                stream.read_exact(&mut addr).await?;
                let port = stream.read_u16().await?;
                let ip = std::net::Ipv4Addr::new(addr[0], addr[1], addr[2], addr[3]);
                format!("{}:{}", ip, port)
            }
            ATYP_DOMAIN => {
                let len = stream.read_u8().await? as usize;
                let mut domain = vec![0u8; len];
                stream.read_exact(&mut domain).await?;
                let port = stream.read_u16().await?;
                let domain = String::from_utf8_lossy(&domain).to_string();
                format!("{}:{}", domain, port)
            }
            ATYP_IPV6 => {
                let mut addr = [0u8; 16];
                stream.read_exact(&mut addr).await?;
                let port = stream.read_u16().await?;
                let ip = std::net::Ipv6Addr::from(addr);
                format!("[{}]:{}", ip, port)
            }
            _ => {
                send_reply(&mut stream, REP_ADDRESS_TYPE_NOT_SUPPORTED).await?;
                anyhow::bail!("Unsupported address type: {}", atyp);
            }
        };

        Ok(target_addr)
    })
    .await;

    let target_addr = match target {
        Ok(Ok(addr)) => addr,
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            anyhow::bail!("SOCKS5 handshake timed out for {}", peer);
        }
    };

    // 4. Connect to target
    info!("SOCKS5 connecting {} -> {}", peer, target_addr);
    let target_stream = match tokio::time::timeout(
        TARGET_CONNECT_TIMEOUT,
        TcpStream::connect(&target_addr),
    )
    .await
    {
        Ok(Ok(s)) => {
            set_keepalive(&s)?;
            send_reply(&mut stream, REP_SUCCESS).await?;
            info!("SOCKS5 connected {} -> {}", peer, target_addr);
            s
        }
        Ok(Err(e)) => {
            let code = match e.kind() {
                std::io::ErrorKind::ConnectionRefused => REP_CONNECTION_REFUSED,
                _ => REP_GENERAL_FAILURE,
            };
            send_reply(&mut stream, code).await?;
            anyhow::bail!("Failed to connect to {}: {}", target_addr, e);
        }
        Err(_) => {
            send_reply(&mut stream, REP_GENERAL_FAILURE).await?;
            anyhow::bail!("Timed out connecting to {}", target_addr);
        }
    };

    // 5. Bidirectional relay
    let (mut client_read, mut client_write) = stream.into_split();
    let (mut target_read, mut target_write) = target_stream.into_split();

    let c2t = tokio::io::copy(&mut client_read, &mut target_write);
    let t2c = tokio::io::copy(&mut target_read, &mut client_write);

    tokio::select! {
        r = c2t => { debug!("Client→Target finished: {:?}", r); }
        r = t2c => { debug!("Target→Client finished: {:?}", r); }
    }

    Ok(())
}

/// Send a SOCKS5 reply with BND.ADDR = 0.0.0.0:0
async fn send_reply(stream: &mut TcpStream, reply: u8) -> std::io::Result<()> {
    let response = [
        SOCKS5_VERSION,
        reply,
        0x00, // RSV
        ATYP_IPV4,
        0,
        0,
        0,
        0, // BND.ADDR
        0,
        0, // BND.PORT
    ];
    stream.write_all(&response).await
}

/// Set TCP keepalive matching Dante's configuration.
fn set_keepalive(stream: &TcpStream) -> anyhow::Result<()> {
    let sock = socket2::SockRef::from(stream);
    let mut keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(7200))
        .with_interval(Duration::from_secs(75));
    #[cfg(target_os = "linux")]
    {
        keepalive = keepalive.with_retries(9);
    }
    sock.set_tcp_keepalive(&keepalive)?;
    Ok(())
}
