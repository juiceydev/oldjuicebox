//! the QUIC/HTTP/3 client for juiceback

use std::sync::Arc;
use std::time::Duration;

use axum::http::{Request, Uri};

use bytes::{Buf, Bytes};
use futures::future;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::{DigitallySignedStruct, Error as RustlsError, SignatureScheme};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};

use crate::state::AppState;
use quinn_proto::VarInt;

#[derive(Debug)]
struct PinnedCertVerifier {
    pinned_cert_der: Vec<u8>,
}

impl ServerCertVerifier for PinnedCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        if self.pinned_cert_der.is_empty() {
            return Err(RustlsError::General(
                "no QUIC certificate pinned - set QUIC_CERT_PATH to enable QUIC".into(),
            ));
        }
        if end_entity.as_ref() == self.pinned_cert_der.as_slice() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(RustlsError::General(
                "server certificate does not match pinned QUIC certificate".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        let verifier = rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        );
        verifier.map_err(|e| {
            RustlsError::General(format!("TLS 1.2 signature verification failed: {}", e))
        })
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        let verifier = rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        );
        verifier.map_err(|e| {
            RustlsError::General(format!("TLS 1.3 signature verification failed: {}", e))
        })
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::ED448,
        ]
    }
}

/// Create a QUIC endpoint, pinning the certificate when configured.
fn create_quic_endpoint(
    cert_path: Option<&std::path::Path>,
) -> Result<h3_quinn::quinn::Endpoint, String> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let verifier: Arc<dyn ServerCertVerifier> = match cert_path {
        Some(path) => {
            let pinned_der = juiceutils::load_cert_for_pinning(path)?;
            tracing::info!("QUIC client: pinning against cert at {}", path.display());
            Arc::new(PinnedCertVerifier {
                pinned_cert_der: pinned_der.to_vec(),
            })
        }
        None => {
            let default_path = std::path::PathBuf::from("./quic-cert.der");
            tracing::info!(
                "QUIC_CERT_PATH not set, auto-generating cert at {}",
                default_path.display()
            );
            let (cert_der, _) = juiceutils::get_or_generate_cert(&default_path);
            Arc::new(PinnedCertVerifier {
                pinned_cert_der: cert_der.as_ref().to_vec(),
            })
        }
    };

    let mut tls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    tls_config.alpn_protocols = vec![b"h3".to_vec()];
    tls_config.enable_early_data = true;

    let quic_client_config = quinn::crypto::rustls::QuicClientConfig::try_from(tls_config)
        .map_err(|e| format!("QUIC client config: {}", e))?;

    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_bidi_streams(64u32.into());
    transport.max_concurrent_uni_streams(128u32.into());
    transport.stream_receive_window(VarInt::from_u32(16 * 1024 * 1024)); // 16 MiB per stream
    transport.receive_window(VarInt::from_u32(64 * 1024 * 1024)); // 64 MiB connection-level
    transport.send_window(64 * 1024 * 1024); // 64 MiB send window

    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_client_config));
    client_config.transport_config(Arc::new(transport));

    let mut endpoint = h3_quinn::quinn::Endpoint::client(std::net::SocketAddr::new(
        std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
        0,
    ))
    .map_err(|e| format!("QUIC endpoint: {}", e))?;

    endpoint.set_default_client_config(client_config);

    Ok(endpoint)
}

/// Get or create the persistent QUIC client endpoint.
async fn get_endpoint(state: &Arc<AppState>) -> Result<h3_quinn::quinn::Endpoint, String> {
    state
        .quic_endpoint
        .get_or_try_init(|| async {
            let cert_path = state.quic_cert_path.as_deref();
            create_quic_endpoint(cert_path)
        })
        .await
        .cloned()
}

/// Turn a juicehost URL into a QUIC socket addr. port+1 baby.
async fn resolve_quic_addr(juicehost_url: &str) -> Result<(std::net::SocketAddr, String), String> {
    let uri: Uri = juicehost_url
        .parse()
        .map_err(|e| format!("invalid juicehost URL: {}", e))?;
    let hostname = uri
        .host()
        .ok_or("no hostname in juicehost URL")?
        .to_string();
    let tcp_port = uri.port_u16().unwrap_or(6402);
    let quic_port = tcp_port + 1;

    let addr = if let Ok(ip) = hostname.parse::<std::net::IpAddr>() {
        std::net::SocketAddr::new(ip, quic_port)
    } else {
        let mut addrs = tokio::net::lookup_host((hostname.as_str(), quic_port))
            .await
            .map_err(|e| format!("DNS resolution failed for {}: {}", hostname, e))?;
        addrs
            .find(|a| a.is_ipv4() || a.is_ipv6())
            .ok_or_else(|| format!("no IP address found for {}", hostname))?
    };

    Ok((addr, hostname))
}

/// Set up a QUIC connection to juicehost and return the h3 send_request handle.
/// Reuses the persistent endpoint from AppState.
#[tracing::instrument(skip_all)]
async fn quic_connect(
    state: &Arc<AppState>,
    juicehost_url: &str,
) -> Result<h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>, String> {
    if juicehost_url.is_empty() {
        return Err("JUICEHOST_URL not set".into());
    }

    let (addr, hostname) = resolve_quic_addr(juicehost_url).await?;
    let endpoint = get_endpoint(state).await?;

    tracing::debug!("QUIC connecting: addr={} hostname={}", addr, hostname);

    let conn = endpoint
        .connect(addr, &hostname)
        .map_err(|e| format!("QUIC connect error: {}", e))?
        .await
        .map_err(|e| format!("QUIC handshake failed: {}", e))?;

    tracing::debug!("QUIC handshake complete: addr={}", addr);

    let quinn_conn = h3_quinn::Connection::new(conn);
    let (mut driver, send_request) = h3::client::new(quinn_conn)
        .await
        .map_err(|e| format!("h3 client setup failed: {}", e))?;

    tokio::spawn(async move {
        let err = future::poll_fn(|cx| driver.poll_close(cx)).await;
        if !err.is_h3_no_error() {
            tracing::warn!("h3 client connection error: {}", err);
        }
    });

    Ok(send_request)
}

/// Build an h3 request to push a file over QUIC. good enough.
fn quic_stream_request(
    juicehost_url: &str,
    id: &str,
    filename: &str,
    mime_type: &str,
    state: &Arc<AppState>,
) -> Result<Request<()>, String> {
    let encoded_fn =
        percent_encoding::utf8_percent_encode(filename, percent_encoding::NON_ALPHANUMERIC)
            .to_string();
    let req_uri = format!(
        "{}/internal/file/stream/{}/{}",
        juicehost_url.trim_end_matches('/'),
        id,
        encoded_fn
    );
    let mut req_builder = Request::post(&req_uri);
    req_builder = req_builder.header("x-mime-type", mime_type);
    let headers = crate::juicehost::juicehost_headers(state);
    for (name, value) in &headers {
        req_builder = req_builder.header(name.as_str(), value.as_bytes());
    }
    req_builder
        .body(())
        .map_err(|e| format!("failed to build request: {}", e))
}

/// After sending all data and calling finish(), wait for the server response.
async fn quic_recv_response(
    stream: &mut h3::client::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>,
) -> Result<(u16, Vec<u8>), String> {
    let (status, body_bytes) = tokio::time::timeout(Duration::from_secs(60), async {
        stream
            .finish()
            .await
            .map_err(|e| format!("finish failed: {}", e))?;

        let resp = stream
            .recv_response()
            .await
            .map_err(|e| format!("recv_response failed: {}", e))?;

        let mut body_bytes = Vec::new();
        while let Some(chunk) = stream
            .recv_data()
            .await
            .map_err(|e| format!("recv_data failed: {}", e))?
        {
            body_bytes.extend_from_slice(chunk.chunk());
        }

        Ok::<_, String>((resp.status().as_u16(), body_bytes))
    })
    .await
    .map_err(|_| "QUIC push timed out waiting for juicehost response".to_string())??;

    Ok((status, body_bytes))
}

fn quic_result(id: &str, url: &str, status: u16, body_bytes: Vec<u8>) -> Result<(), String> {
    if (200..300).contains(&status) {
        tracing::info!("juicehost QUIC push: id={} ok", id);
        Ok(())
    } else {
        let body_text = String::from_utf8_lossy(&body_bytes).to_string();
        Err(format!(
            "{} (url={}, status={}, body_len={})",
            crate::juicehost::format_error_response(status, body_text.clone()),
            url,
            status,
            body_bytes.len(),
        ))
    }
}

/// Push to juicehost over QUIC while streaming chunks from a receiver.
/// Also buffers each chunk so the caller can fall back to HTTP on failure.
pub async fn push_file_streaming_quic_streamed(
    state: &Arc<AppState>,
    id: &str,
    filename: &str,
    mime_type: &str,
    rx: &mut tokio::sync::mpsc::Receiver<Result<Bytes, String>>,
    chunks: &mut Vec<Bytes>,
) -> Result<(), String> {
    let juicehost_url = &state.config.juicehost_url;
    let mut send_request = quic_connect(state, juicehost_url).await?;
    let req = quic_stream_request(juicehost_url, id, filename, mime_type, state)?;
    let url = format!(
        "{}/internal/file/stream/{}/{}",
        juicehost_url.trim_end_matches('/'),
        id,
        percent_encoding::utf8_percent_encode(filename, percent_encoding::NON_ALPHANUMERIC)
    );
    tracing::debug!("QUIC push: id={} url={}", id, url);
    let mut stream = send_request
        .send_request(req)
        .await
        .map_err(|e| format!("send_request failed: {}", e))?;

    let mut has_data = false;
    while let Some(chunk) = rx.recv().await {
        has_data = true;
        let data = chunk.map_err(|e| format!("chunk error: {}", e))?;
        chunks.push(data.clone());
        stream
            .send_data(data)
            .await
            .map_err(|e| format!("send_data failed: {}", e))?;
    }
    if !has_data {
        return Err("no data received for QUIC push".into());
    }

    let (status, body_bytes) = quic_recv_response(&mut stream).await?;
    quic_result(id, &url, status, body_bytes)
}
