use crate::proxy::credentials::CredentialManager;
use base64::Engine;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Maximum header size to prevent abuse.
const MAX_HEADER_SIZE: usize = 16 * 1024;

/// Run the embedded HTTP CONNECT proxy server.
pub async fn run_http_connect_server(
    bind_addr: SocketAddr,
    credentials: Arc<CredentialManager>,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(bind_addr).await?;
    info!("HTTP CONNECT proxy listening on {}", bind_addr);

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("HTTP CONNECT proxy shutting down");
                break;
            }
            accept = listener.accept() => {
                match accept {
                    Ok((stream, peer)) => {
                        let creds = credentials.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, peer, creds).await {
                                debug!("HTTP CONNECT from {} error: {}", peer, e);
                            }
                        });
                    }
                    Err(e) => {
                        warn!("HTTP CONNECT accept error: {}", e);
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
    // Set TCP keepalive
    set_keepalive(&stream)?;

    // Read headers with timeout
    let headers = tokio::time::timeout(Duration::from_secs(30), async {
        read_headers(&mut stream).await
    })
    .await
    .map_err(|_| anyhow::anyhow!("Header read timeout"))??;

    let headers_str = String::from_utf8_lossy(&headers);

    // Parse request line
    let first_line = headers_str.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();

    if parts.len() < 3 || parts[0].to_uppercase() != "CONNECT" {
        stream
            .write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n")
            .await?;
        anyhow::bail!("Invalid request: {}", first_line);
    }

    let target = parts[1].to_string();

    // Extract Proxy-Authorization header
    let auth_value = headers_str
        .lines()
        .find(|line| line.to_lowercase().starts_with("proxy-authorization:"))
        .and_then(|line| line.split_once(':'))
        .map(|(_, value)| value.trim().to_string());

    // Parse Basic auth
    let (username, password) = match parse_basic_auth(auth_value.as_deref()) {
        Some(creds) => creds,
        None => {
            stream
                .write_all(
                    b"HTTP/1.1 407 Proxy Authentication Required\r\n\
                      Proxy-Authenticate: Basic realm=\"TPN Proxy\"\r\n\
                      \r\n",
                )
                .await?;
            anyhow::bail!("Missing or invalid Proxy-Authorization from {}", peer);
        }
    };

    // Authenticate
    if !credentials.authenticate(&username, &password) {
        stream
            .write_all(
                b"HTTP/1.1 407 Proxy Authentication Required\r\n\
                  Proxy-Authenticate: Basic realm=\"TPN Proxy\"\r\n\
                  \r\n",
            )
            .await?;
        anyhow::bail!("Authentication failed for {} from {}", username, peer);
    }

    debug!("HTTP CONNECT auth success for {} from {}", username, peer);

    // Connect to target
    let target_stream = match TcpStream::connect(&target).await {
        Ok(s) => {
            set_keepalive(&s)?;
            s
        }
        Err(e) => {
            stream
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                .await?;
            anyhow::bail!("Failed to connect to {}: {}", target, e);
        }
    };

    // Send success response
    stream
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .await?;

    // Bidirectional relay
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

/// Read HTTP headers until \r\n\r\n, with size limit.
async fn read_headers(stream: &mut TcpStream) -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];

    loop {
        stream.read_exact(&mut byte).await?;
        buf.push(byte[0]);

        if buf.len() > MAX_HEADER_SIZE {
            anyhow::bail!("Headers exceed {} bytes", MAX_HEADER_SIZE);
        }

        // Check for \r\n\r\n
        if buf.len() >= 4 && &buf[buf.len() - 4..] == b"\r\n\r\n" {
            break;
        }
    }

    Ok(buf)
}

/// Parse "Basic <base64(user:pass)>" into (username, password).
fn parse_basic_auth(value: Option<&str>) -> Option<(String, String)> {
    let value = value?;
    let value = value
        .strip_prefix("Basic ")
        .or_else(|| value.strip_prefix("basic "))?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (user, pass) = decoded.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

/// Set TCP keepalive matching Dante configuration.
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
