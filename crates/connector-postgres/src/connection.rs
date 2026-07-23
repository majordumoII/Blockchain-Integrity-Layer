//! A minimal hand-rolled Postgres wire protocol client: just enough to
//! open a `replication=database` connection, authenticate (SCRAM-SHA-256
//! or plaintext/MD5), and hand control to the caller for `START_REPLICATION`.
//!
//! `tokio-postgres` cannot be reused for this: its startup/auth handshake
//! is a private (`pub(crate)`) implementation detail, and it has no
//! support at all for `CopyBothResponse` (the message type replication
//! streaming uses), only one-directional `COPY OUT`. Building on
//! `postgres-protocol` (the same crate `tokio-postgres` itself uses for
//! wire encoding/decoding) keeps this free of a third client
//! implementation while still allowing direct control of the socket.

use bytes::{Buf, BytesMut};
use fallible_iterator::FallibleIterator;
use postgres_protocol::authentication::sasl::{ChannelBinding, ScramSha256};
use postgres_protocol::message::backend::Message as BackendMessage;
use postgres_protocol::message::frontend;
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Connection parameters for a replication connection.
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub dbname: String,
}

/// Errors from establishing or running the raw connection.
#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error("i/o error talking to postgres: {0}")]
    Io(#[from] io::Error),

    #[error("postgres reported an error: {0}")]
    ServerError(String),

    #[error("postgres requested an unsupported authentication method: {0}")]
    UnsupportedAuth(String),

    #[error("received an unexpected message from postgres during {phase}")]
    UnexpectedMessage { phase: &'static str },

    #[error("connection closed unexpectedly during {phase}")]
    ConnectionClosed { phase: &'static str },
}

/// A connected, authenticated Postgres socket, positioned right after
/// startup — ready for the caller to issue `START_REPLICATION` or any
/// other simple query.
///
/// Deliberately does not implement the full frontend/backend protocol
/// (no prepared statements, no extended query protocol) — replication
/// only ever needs the simple query subset, and keeping this narrow
/// keeps the amount of hand-rolled wire protocol code bounded to what
/// this crate actually exercises against a live server.
pub struct RawConnection {
    stream: TcpStream,
    read_buf: BytesMut,
}

impl RawConnection {
    /// Opens a TCP connection to Postgres, performs startup and
    /// authentication, and returns a connection ready for
    /// [`Self::send_query`]/[`Self::read_message`].
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionError`] if the TCP connection fails, Postgres
    /// rejects the credentials, or Postgres requests an authentication
    /// method other than SCRAM-SHA-256, MD5, or "trust" (no password).
    pub async fn connect(config: &ConnectionConfig) -> Result<Self, ConnectionError> {
        let stream = TcpStream::connect((config.host.as_str(), config.port)).await?;
        let mut conn = Self {
            stream,
            read_buf: BytesMut::new(),
        };
        conn.startup(config).await?;
        Ok(conn)
    }

    async fn startup(&mut self, config: &ConnectionConfig) -> Result<(), ConnectionError> {
        let mut buf = BytesMut::new();
        // `replication=database` is what puts the backend into replication
        // mode, enabling START_REPLICATION and logical decoding commands
        // on this connection instead of normal SQL.
        frontend::startup_message(
            [
                ("user", config.user.as_str()),
                ("database", config.dbname.as_str()),
                ("replication", "database"),
                ("client_encoding", "UTF8"),
            ],
            &mut buf,
        )
        .map_err(ConnectionError::Io)?;
        self.stream.write_all(&buf).await?;

        loop {
            let message = self.read_backend_message("startup").await?;
            match message {
                BackendMessage::AuthenticationCleartextPassword => {
                    self.send_password(config.password.as_bytes()).await?;
                }
                BackendMessage::AuthenticationMd5Password(body) => {
                    let hashed = postgres_protocol::authentication::md5_hash(
                        config.user.as_bytes(),
                        config.password.as_bytes(),
                        body.salt(),
                    );
                    self.send_password(hashed.as_bytes()).await?;
                }
                BackendMessage::AuthenticationSasl(body) => {
                    self.do_scram_sha256(config, body).await?;
                }
                BackendMessage::AuthenticationKerberosV5
                | BackendMessage::AuthenticationScmCredential
                | BackendMessage::AuthenticationGss
                | BackendMessage::AuthenticationGssContinue(_)
                | BackendMessage::AuthenticationSspi => {
                    return Err(ConnectionError::UnsupportedAuth(
                        "only trust, cleartext, md5, and SCRAM-SHA-256 are supported".to_string(),
                    ));
                }
                // AuthenticationOk means we're done authenticating;
                // BackendKeyData/ParameterStatus are expected informational
                // messages during startup. None require action from a
                // replication-only connection (no cancel support), so all
                // three are no-ops here — the loop simply continues until
                // ReadyForQuery.
                BackendMessage::AuthenticationOk
                | BackendMessage::BackendKeyData(_)
                | BackendMessage::ParameterStatus(_) => {}
                BackendMessage::ReadyForQuery(_) => return Ok(()),
                BackendMessage::ErrorResponse(body) => {
                    return Err(ConnectionError::ServerError(error_body_to_string(&body)));
                }
                _ => return Err(ConnectionError::UnexpectedMessage { phase: "startup" }),
            }
        }
    }

    async fn do_scram_sha256(
        &mut self,
        config: &ConnectionConfig,
        _initial: postgres_protocol::message::backend::AuthenticationSaslBody,
    ) -> Result<(), ConnectionError> {
        let mut scram = ScramSha256::new(config.password.as_bytes(), ChannelBinding::unsupported());

        let mut buf = BytesMut::new();
        frontend::sasl_initial_response("SCRAM-SHA-256", scram.message(), &mut buf)
            .map_err(ConnectionError::Io)?;
        self.stream.write_all(&buf).await?;

        let server_first = match self.read_backend_message("scram first").await? {
            BackendMessage::AuthenticationSaslContinue(body) => body.data().to_vec(),
            BackendMessage::ErrorResponse(body) => {
                return Err(ConnectionError::ServerError(error_body_to_string(&body)));
            }
            _ => {
                return Err(ConnectionError::UnexpectedMessage {
                    phase: "scram first",
                });
            }
        };
        scram.update(&server_first).map_err(ConnectionError::Io)?;

        let mut buf = BytesMut::new();
        frontend::sasl_response(scram.message(), &mut buf).map_err(ConnectionError::Io)?;
        self.stream.write_all(&buf).await?;

        let server_final = match self.read_backend_message("scram final").await? {
            BackendMessage::AuthenticationSaslFinal(body) => body.data().to_vec(),
            BackendMessage::ErrorResponse(body) => {
                return Err(ConnectionError::ServerError(error_body_to_string(&body)));
            }
            _ => {
                return Err(ConnectionError::UnexpectedMessage {
                    phase: "scram final",
                });
            }
        };
        scram.finish(&server_final).map_err(ConnectionError::Io)?;

        match self.read_backend_message("scram ok").await? {
            BackendMessage::AuthenticationOk => Ok(()),
            BackendMessage::ErrorResponse(body) => {
                Err(ConnectionError::ServerError(error_body_to_string(&body)))
            }
            _ => Err(ConnectionError::UnexpectedMessage { phase: "scram ok" }),
        }
    }

    async fn send_password(&mut self, password: &[u8]) -> Result<(), ConnectionError> {
        let mut buf = BytesMut::new();
        frontend::password_message(password, &mut buf).map_err(ConnectionError::Io)?;
        self.stream.write_all(&buf).await?;
        match self.read_backend_message("password auth").await? {
            BackendMessage::AuthenticationOk => Ok(()),
            BackendMessage::ErrorResponse(body) => {
                Err(ConnectionError::ServerError(error_body_to_string(&body)))
            }
            _ => Err(ConnectionError::UnexpectedMessage {
                phase: "password auth",
            }),
        }
    }

    /// Sends a simple query (e.g. `START_REPLICATION ...`, `IDENTIFY_SYSTEM`).
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionError::Io`] if writing to the socket fails.
    pub async fn send_query(&mut self, query: &str) -> Result<(), ConnectionError> {
        let mut buf = BytesMut::new();
        frontend::query(query, &mut buf).map_err(ConnectionError::Io)?;
        self.stream.write_all(&buf).await?;
        Ok(())
    }

    /// Sends a `CopyData` frontend message — used to reply to keepalive
    /// requests and send periodic status updates during replication.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionError::Io`] if writing to the socket fails.
    pub async fn send_copy_data(&mut self, data: &[u8]) -> Result<(), ConnectionError> {
        let mut buf = BytesMut::new();
        frontend::CopyData::new(data)
            .map_err(ConnectionError::Io)?
            .write(&mut buf);
        self.stream.write_all(&buf).await?;
        Ok(())
    }

    /// Reads one full backend message, parsed via `postgres-protocol`.
    ///
    /// `phase` is purely diagnostic — it's threaded through into any
    /// resulting error so a caller/log line can say *where* an unexpected
    /// message showed up instead of just "something unexpected happened."
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionError`] if the socket errors or closes.
    pub async fn read_backend_message(
        &mut self,
        phase: &'static str,
    ) -> Result<BackendMessage, ConnectionError> {
        loop {
            if let Some(message) =
                BackendMessage::parse(&mut self.read_buf).map_err(ConnectionError::Io)?
            {
                return Ok(message);
            }
            self.fill_buf(phase).await?;
        }
    }

    /// Reads exactly one raw backend message frame (tag byte + length +
    /// body) without interpreting it via `postgres-protocol`.
    ///
    /// Needed because `postgres-protocol::message::backend::Message`
    /// does not recognize the `W` (`CopyBothResponse`) tag that
    /// replication connections use to begin streaming — it only knows
    /// the one-directional `H` (`CopyOutResponse`). Everything after
    /// `CopyBothResponse` is `CopyData` (`d`), which *is* recognized, so
    /// this raw path is only needed for that single message.
    pub(crate) async fn read_raw_message(
        &mut self,
        phase: &'static str,
    ) -> Result<(u8, BytesMut), ConnectionError> {
        loop {
            if self.read_buf.len() >= 5 {
                let tag = self.read_buf[0];
                let len = u32::from_be_bytes(self.read_buf[1..5].try_into().unwrap()) as usize;
                // `len` includes itself (4 bytes) but not the 1-byte tag.
                if self.read_buf.len() > len {
                    let mut frame = self.read_buf.split_to(1 + len);
                    frame.advance(5); // drop tag + length prefix
                    return Ok((tag, frame));
                }
            }
            self.fill_buf(phase).await?;
        }
    }

    async fn fill_buf(&mut self, phase: &'static str) -> Result<(), ConnectionError> {
        let mut chunk = [0u8; 8192];
        let n = self.stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(ConnectionError::ConnectionClosed { phase });
        }
        self.read_buf.extend_from_slice(&chunk[..n]);
        Ok(())
    }
}

fn error_body_to_string(body: &postgres_protocol::message::backend::ErrorResponseBody) -> String {
    let mut fields = body.fields();
    let mut message = String::new();
    while let Ok(Some(field)) = fields.next() {
        if field.type_() == b'M' {
            message = String::from_utf8_lossy(field.value_bytes()).into_owned();
            break;
        }
    }
    if message.is_empty() {
        "unknown server error".to_string()
    } else {
        message
    }
}
