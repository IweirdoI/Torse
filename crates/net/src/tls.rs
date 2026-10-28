//! Mutual TLS configuration and bounded nonblocking record I/O.
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, Connection, RootCertStore, ServerConfig};
use sha2::{Digest as _, Sha256};
use std::io::{self, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::Arc;
use torse_types::{Digest, Fault};

const MAX_PEM_BYTES: u64 = 1024 * 1024;
/// Conservative shared configuration allowance, independent of per-peer records.
pub fn configuration_reservation(paths: &[&Path]) -> Result<u64, Fault> {
    paths.iter().try_fold(65_536u64, |bytes, path| {
        let length = std::fs::metadata(path)
            .map_err(|_| Fault::invalid_data("TLS configuration metadata"))?
            .len();
        if length > MAX_PEM_BYTES {
            return Err(Fault::resource_limit("TLS credential file exceeds 1 MiB"));
        }
        bytes
            .checked_add(length.saturating_mul(8))
            .ok_or_else(|| Fault::resource_limit("TLS configuration reservation overflow"))
    })
}
pub(crate) const BUFFER_BYTES: usize = 65_536;
pub(crate) const RESERVATION_BYTES: u64 = 1024 * 1024;

/// A transport supplies this only after verifying the complete TLS handshake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportSecurity {
    Unprotected,
    Authenticating,
    MutualTls(Digest),
}

pub fn certificate_fingerprint(certificate: &[u8]) -> Digest {
    Digest(Sha256::digest(certificate).into())
}

fn read_pem(path: &Path) -> Result<BufReader<io::Take<std::fs::File>>, Fault> {
    let file = std::fs::File::open(path)
        .map_err(|_| Fault::invalid_data("cannot open TLS credential file"))?;
    if file
        .metadata()
        .map_err(|_| Fault::invalid_data("TLS file metadata"))?
        .len()
        > MAX_PEM_BYTES
    {
        return Err(Fault::resource_limit("TLS credential file exceeds 1 MiB"));
    }
    Ok(BufReader::new(file.take(MAX_PEM_BYTES + 1)))
}
pub fn certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>, Fault> {
    let certificates: Vec<_> = rustls_pemfile::certs(&mut read_pem(path)?)
        .collect::<Result<_, _>>()
        .map_err(|_| Fault::invalid_data("invalid TLS certificates"))?;
    if certificates.is_empty() || certificates.len() > 64 {
        return Err(Fault::invalid_data("TLS certificate count"));
    }
    Ok(certificates)
}
fn private_key(path: &Path) -> Result<PrivateKeyDer<'static>, Fault> {
    rustls_pemfile::private_key(&mut read_pem(path)?)
        .map_err(|_| Fault::invalid_data("invalid TLS private key"))?
        .ok_or_else(|| Fault::invalid_data("missing TLS private key"))
}
fn roots(path: &Path) -> Result<RootCertStore, Fault> {
    let mut roots = RootCertStore::empty();
    for certificate in certificates(path)? {
        roots
            .add(certificate)
            .map_err(|_| Fault::invalid_data("invalid TLS trust anchor"))?;
    }
    Ok(roots)
}
fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Client certificates are mandatory; no anonymous or early-data fallback exists.
pub fn server_config(
    cert: &Path,
    key: &Path,
    client_ca: &Path,
) -> Result<Arc<ServerConfig>, Fault> {
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots(client_ca)?),
        provider(),
    )
    .build()
    .map_err(|_| Fault::invalid_data("TLS client verification configuration"))?;
    let mut config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|_| Fault::invalid_data("TLS versions"))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(certificates(cert)?, private_key(key)?)
        .map_err(|_| Fault::invalid_data("TLS server key or certificate"))?;
    config.alpn_protocols = vec![b"torse/3".to_vec()];
    config.max_early_data_size = 0;
    config.send_tls13_tickets = 0;
    config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
    Ok(Arc::new(config))
}
pub fn client_config(ca: &Path, cert: &Path, key: &Path) -> Result<Arc<ClientConfig>, Fault> {
    let mut config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|_| Fault::invalid_data("TLS versions"))?
        .with_root_certificates(roots(ca)?)
        .with_client_auth_cert(certificates(cert)?, private_key(key)?)
        .map_err(|_| Fault::invalid_data("TLS client key or certificate"))?;
    config.alpn_protocols = vec![b"torse/3".to_vec()];
    config.enable_early_data = false;
    config.resumption = rustls::client::Resumption::disabled();
    Ok(Arc::new(config))
}

pub(crate) struct Tls {
    pub connection: Connection,
}
impl Tls {
    pub fn server(config: Arc<ServerConfig>) -> Result<Self, Fault> {
        Self::new(
            rustls::ServerConnection::new(config)
                .map(Connection::Server)
                .map_err(|_| Fault::invalid_data("TLS server connection"))?,
        )
    }
    pub fn client(config: Arc<ClientConfig>, name: &str) -> Result<Self, Fault> {
        let name = ServerName::try_from(name.to_owned())
            .map_err(|_| Fault::invalid_data("TLS server name"))?;
        Self::new(
            rustls::ClientConnection::new(config, name)
                .map(Connection::Client)
                .map_err(|_| Fault::invalid_data("TLS client connection"))?,
        )
    }
    fn new(mut connection: Connection) -> Result<Self, Fault> {
        connection.set_buffer_limit(Some(BUFFER_BYTES));
        Ok(Self { connection })
    }
    pub fn security(&self) -> TransportSecurity {
        if self.connection.is_handshaking() {
            return TransportSecurity::Authenticating;
        }
        if self.connection.alpn_protocol() != Some(b"torse/3") {
            return TransportSecurity::Unprotected;
        }
        self.connection
            .peer_certificates()
            .and_then(|certificates| certificates.first())
            .map_or(TransportSecurity::Unprotected, |certificate| {
                TransportSecurity::MutualTls(certificate_fingerprint(certificate.as_ref()))
            })
    }
    /// Bound reads and writes independently. TLS parsing stays inside these byte
    /// and operation limits even when a socket remains continuously readable.
    pub fn pump(&mut self, stream: &mut TcpStream, bytes: usize) -> Result<(), Fault> {
        let mut io = LimitedIo {
            stream,
            reads: bytes,
            writes: bytes,
            operations: 64,
        };
        while io.operations > 0 {
            let mut progress = false;
            if self.connection.wants_write() && io.writes > 0 {
                match self.connection.write_tls(&mut io) {
                    Ok(0) => return Err(Fault::invalid_data("TLS socket closed")),
                    Ok(_) => progress = true,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => return Err(Fault::invalid_data("TLS record write failed")),
                }
            }
            if self.connection.wants_read() && io.reads > 0 && io.operations > 0 {
                match self.connection.read_tls(&mut io) {
                    Ok(0) => return Err(Fault::invalid_data("TLS socket closed")),
                    Ok(_) => {
                        self.connection.process_new_packets().map_err(|_| {
                            Fault::incompatible(
                                "TLS peer authentication or record verification failed",
                            )
                        })?;
                        progress = true;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => return Err(Fault::invalid_data("TLS record read failed")),
                }
            }
            if !progress {
                break;
            }
        }
        if !self.connection.is_handshaking()
            && !matches!(self.security(), TransportSecurity::MutualTls(_))
        {
            return Err(Fault::incompatible(
                "TLS requires a verified peer and application protocol",
            ));
        }
        Ok(())
    }
}
struct LimitedIo<'a> {
    stream: &'a mut TcpStream,
    reads: usize,
    writes: usize,
    operations: usize,
}
impl Read for LimitedIo<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.reads == 0 || self.operations == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.operations -= 1;
        let length = buffer.len().min(self.reads).min(16_384);
        let read = self.stream.read(&mut buffer[..length])?;
        self.reads -= read;
        Ok(read)
    }
}
impl Write for LimitedIo<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.writes == 0 || self.operations == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.operations -= 1;
        let written = self
            .stream
            .write(&buffer[..buffer.len().min(self.writes).min(16_384)])?;
        self.writes -= written;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
