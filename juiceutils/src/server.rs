//! QUIC/HTTP/3 server setup shared between juiceback and juicehost.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, Response};
use bytes::{Buf, Bytes};
use h3_quinn::Connection as H3Connection;
use http_body_util::{channel::Channel, BodyExt};
use quinn::Endpoint;
use quinn_proto::crypto::rustls::QuicServerConfig;
use quinn_proto::VarInt;
use rcgen::generate_simple_self_signed;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::sync::{Notify, Semaphore};
use tower::Service;

/// Generate an ephemeral self-signed TLS certificate for QUIC.
pub fn generate_self_signed_cert() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let certified_key = generate_simple_self_signed(vec!["juicebox.local".into()]).unwrap();
    let cert_der = certified_key.cert.der().clone();
    let key_der = PrivateKeyDer::try_from(certified_key.key_pair.serialize_der()).unwrap();
    (cert_der, key_der)
}

/// Generate or load a self-signed certificate used for certificate pinning.
pub fn get_or_generate_cert(
    cert_path: &std::path::Path,
) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    if let Ok(der_bytes) = std::fs::read(cert_path) {
        if let Ok(key_bytes) = std::fs::read(cert_path.with_extension("key")) {
            let cert_der = CertificateDer::from(der_bytes);
            let key_der =
                PrivateKeyDer::try_from(key_bytes).expect("failed to parse saved QUIC private key");
            tracing::info!("loaded QUIC cert from {}", cert_path.display());
            return (cert_der, key_der);
        }
    }
    let certified_key = generate_simple_self_signed(vec!["juicebox.local".into()]).unwrap();
    let cert_der = certified_key.cert.der().clone();
    let key_der_bytes = certified_key.key_pair.serialize_der();
    let key_der =
        PrivateKeyDer::try_from(key_der_bytes.clone()).expect("failed to create PrivateKeyDer");
    if let Some(parent) = cert_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!("failed to create cert directory {}: {e}", parent.display());
        }
    }
    if let Err(e) = std::fs::write(cert_path, cert_der.as_ref()) {
        tracing::warn!("failed to save cert to {}: {e}", cert_path.display());
    }
    if let Err(e) = write_private_key(&cert_path.with_extension("key"), &key_der_bytes) {
        tracing::warn!(
            "failed to save cert key to {}: {e}",
            cert_path.with_extension("key").display()
        );
    }
    tracing::info!("generated and saved QUIC cert to {}", cert_path.display());
    (cert_der, key_der)
}

fn write_private_key(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    std::io::Write::write_all(&mut options.open(path)?, bytes)
}

/// Load only the certificate from disk for client-side pinning.
pub fn load_cert_for_pinning(
    cert_path: &std::path::Path,
) -> Result<CertificateDer<'static>, String> {
    let der_bytes = std::fs::read(cert_path)
        .map_err(|e| format!("failed to read QUIC cert at {}: {}", cert_path.display(), e))?;
    Ok(CertificateDer::from(der_bytes))
}

/// Start a QUIC/HTTP/3 server and proxy requests through Axum.
/// `cert_path` persists the self-signed certificate for client pinning.
#[derive(Debug, Clone)]
pub struct QuicServerLimits {
    pub max_connections: usize,
    pub max_requests: usize,
    pub handshake_timeout: Duration,
    pub idle_timeout: Duration,
    pub request_timeout: Duration,
}

impl Default for QuicServerLimits {
    fn default() -> Self {
        Self {
            max_connections: 256,
            max_requests: 256,
            handshake_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(600),
        }
    }
}

pub async fn start_quic_server(
    router: axum::Router,
    addr: std::net::SocketAddr,
    shutdown: Arc<Notify>,
    service_name: &str,
    cert_path: Option<std::path::PathBuf>,
) {
    start_quic_server_with_limits(
        router,
        addr,
        shutdown,
        service_name,
        cert_path,
        QuicServerLimits::default(),
    )
    .await
}

pub async fn start_quic_server_with_limits(
    router: axum::Router,
    addr: std::net::SocketAddr,
    shutdown: Arc<Notify>,
    service_name: &str,
    cert_path: Option<std::path::PathBuf>,
    limits: QuicServerLimits,
) {
    let (cert_der, key_der) = match cert_path.as_deref() {
        Some(path) => get_or_generate_cert(path),
        None => generate_self_signed_cert(),
    };

    let tls_config = {
        static INSTALL_CRYPTO_PROVIDER: std::sync::Once = std::sync::Once::new();
        INSTALL_CRYPTO_PROVIDER.call_once(|| {
            rustls::crypto::aws_lc_rs::default_provider()
                .install_default()
                .expect("failed to install default crypto provider");
        });
        let mut tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)
            .unwrap();
        tls.alpn_protocols = vec![b"h3".to_vec()];
        tls
    };

    let quic_server_config =
        QuicServerConfig::try_from(Arc::new(tls_config)).expect("QuicServerConfig creation failed");

    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_bidi_streams(16u32.into());
    transport.max_concurrent_uni_streams(16u32.into());
    transport.stream_receive_window(VarInt::from_u32(2 * 1024 * 1024));
    transport.receive_window(VarInt::from_u32(8 * 1024 * 1024));
    transport.send_window(8 * 1024 * 1024);
    transport.max_idle_timeout(Some(
        limits
            .idle_timeout
            .try_into()
            .expect("bounded QUIC idle timeout"),
    ));

    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_server_config));
    server_config.transport_config(Arc::new(transport));

    let socket = std::net::UdpSocket::bind(addr).expect("Failed to bind QUIC UDP socket");
    socket
        .set_nonblocking(true)
        .expect("Failed to set socket nonblocking");
    let endpoint = Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(server_config),
        socket,
        Arc::new(quinn::TokioRuntime),
    )
    .expect("Failed to create QUIC endpoint");

    tracing::info!("{} QUIC listening on udp://{}", service_name, addr);

    let quic_shutdown = shutdown.clone();
    tokio::select! {
        biased;
        _ = quic_shutdown.notified() => {
            tracing::info!("{} QUIC shutting down...", service_name);
            endpoint.close(VarInt::from_u32(0), b"server shutdown");
            let _ = tokio::time::timeout(Duration::from_secs(5), endpoint.wait_idle()).await;
        }
        _ = run_quic_server(&endpoint, router, limits) => {
            endpoint.wait_idle().await;
        }
    }
}

async fn run_quic_server(endpoint: &Endpoint, router: axum::Router, limits: QuicServerLimits) {
    let connection_limit = Arc::new(Semaphore::new(limits.max_connections));
    let request_limit = Arc::new(Semaphore::new(limits.max_requests));
    loop {
        let incoming = match endpoint.accept().await {
            Some(conn) => conn,
            None => break,
        };
        if !incoming.remote_address_validated() && incoming.may_retry() {
            if let Err(error) = incoming.retry() {
                tracing::debug!("QUIC Retry failed: {error}");
            }
            continue;
        }
        let permit = match Arc::clone(&connection_limit).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                incoming.refuse();
                continue;
            }
        };

        let router = router.clone();
        let request_limit = Arc::clone(&request_limit);
        let limits = limits.clone();
        tokio::spawn(async move {
            let remote_addr = incoming.remote_address();
            let conn = match tokio::time::timeout(limits.handshake_timeout, incoming).await {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => {
                    tracing::warn!("QUIC connection handshake failed: {}", e);
                    return;
                }
                Err(_) => {
                    tracing::warn!("QUIC connection handshake timed out");
                    return;
                }
            };

            if let Err(e) = handle_h3_conn(
                H3Connection::new(conn),
                router,
                remote_addr,
                request_limit,
                limits,
            )
            .await
            {
                tracing::debug!("QUIC connection closed: {}", e);
            }
            drop(permit);
        });
    }
}

async fn handle_h3_conn(
    conn: H3Connection,
    router: axum::Router,
    remote_addr: std::net::SocketAddr,
    request_limit: Arc<Semaphore>,
    limits: QuicServerLimits,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut builder = h3::server::builder();
    builder.max_field_section_size(32 * 1024);
    let mut h3_conn = builder.build(conn).await?;

    loop {
        let resolver = match h3_conn.accept().await {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => {
                tracing::debug!("h3 accept error: {}", e);
                break;
            }
        };

        let mut router = router.clone();
        let permit = match Arc::clone(&request_limit).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => continue,
        };
        let request_timeout = limits.request_timeout;
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + request_timeout;
            let (req, stream) =
                match tokio::time::timeout_at(deadline, resolver.resolve_request()).await {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        tracing::debug!("h3 resolve_request error: {}", e);
                        return;
                    }
                    Err(_) => return,
                };

            if header_bytes(&req) > 32 * 1024 {
                tracing::warn!("rejected oversized H3 request headers");
                return;
            }
            match tokio::time::timeout_at(
                deadline,
                proxy_axum(&mut router, req, stream, remote_addr, request_timeout),
            )
            .await
            {
                Err(_) => tracing::debug!("H3 request deadline exceeded"),
                Ok(Err(e)) => tracing::debug!("h3->axum proxy error: {e}"),
                Ok(Ok(())) => {}
            }
            drop(permit);
        });
    }

    Ok(())
}

async fn proxy_axum(
    router: &mut axum::Router,
    req: Request<()>,
    stream: h3::server::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>,
    remote_addr: std::net::SocketAddr,
    request_timeout: Duration,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let uri = req.uri().clone();
    let method = req.method().clone();
    let headers = req.headers().clone();
    let method_str = method.to_string();
    let uri_str = uri.to_string();

    let (mut send_stream, mut recv_stream) = stream.split();
    let (mut body_tx, body) = Channel::<Bytes, std::io::Error>::new(4);
    let request_body_task = tokio::spawn(async move {
        loop {
            let chunk = match tokio::time::timeout(
                request_timeout.min(Duration::from_secs(30)),
                recv_stream.recv_data(),
            )
            .await
            {
                Err(_) => {
                    body_tx.abort(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "timeout reading H3 request body",
                    ));
                    return;
                }
                Ok(Err(error)) => {
                    body_tx.abort(std::io::Error::other(error));
                    return;
                }
                Ok(Ok(chunk)) => chunk,
            };
            let Some(mut chunk) = chunk else {
                return;
            };
            let bytes = chunk.copy_to_bytes(chunk.remaining());
            if body_tx.send_data(bytes).await.is_err() {
                return;
            }
        }
    });

    let mut axum_req = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::new(body))
        .unwrap();
    *axum_req.headers_mut() = headers;
    axum_req
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(remote_addr));

    let response = Service::call(router, axum_req).await?;
    request_body_task.abort();

    let status = response.status();
    let resp_headers = response.headers().clone();
    let mut resp_body = response.into_body();

    if !status.is_success() {
        tracing::warn!("proxy_axum: {} {} -> {}", method_str, uri_str, status,);
    }

    let mut builder = Response::builder().status(status);
    for (k, v) in resp_headers.iter() {
        builder = builder.header(k, v);
    }
    send_stream.send_response(builder.body(()).unwrap()).await?;
    while let Some(frame) = resp_body.frame().await {
        let frame = frame?;
        if let Some(data) = frame.data_ref() {
            send_stream.send_data(data.clone()).await?;
        } else if let Some(trailers) = frame.trailers_ref() {
            send_stream.send_trailers(trailers.clone()).await?;
        }
    }
    send_stream.finish().await?;

    Ok(())
}

fn header_bytes(req: &Request<()>) -> usize {
    req.method().as_str().len()
        + req.uri().to_string().len()
        + req
            .headers()
            .iter()
            .map(|(name, value)| name.as_str().len() + value.as_bytes().len())
            .sum::<usize>()
}
