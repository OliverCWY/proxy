use std::sync::Arc;
use tokio::io::{AsyncWriteExt, copy_bidirectional};

use crate::{
    error::{InboundError, OutboundError, OutboundErrorKind, PoolError},
    inbound::{http::PendingHttpSession, socks5::Socks5Handshake},
    pool::ProxyPool,
    protocol::Credentials,
};

pub struct ForwardHandler {
    pool: Arc<ProxyPool>,
    security: Option<Arc<crate::security::SecurityFilter>>,
    auth_credentials: Option<Credentials>,
}

impl ForwardHandler {
    pub fn new(pool: Arc<ProxyPool>, auth_credentials: Option<Credentials>) -> Self {
        Self {
            pool,
            security: None,
            auth_credentials,
        }
    }

    pub fn with_security(mut self, security: Arc<crate::security::SecurityFilter>) -> Self {
        self.security = Some(security);
        self
    }

    /// Handle incoming forward SOCKS5 session
    pub async fn handle_socks5(
        &self,
        handshake: Socks5Handshake,
    ) -> Result<(), InboundError> {
        let require_auth = self.auth_credentials.is_some();
        let session = handshake.negotiate(require_auth).await?;

        // Verify credentials if required
        if let Some(expected) = &self.auth_credentials {
            let matches = match session.credentials() {
                Some(provided) => expected.matches(&provided.user, &provided.pass),
                None => false,
            };

            if !matches {
                session.reject_auth().await?;
                return Err(InboundError::AuthFailed);
            }
        }

        // Read destination target from client
        let (committed_state, target) = session.read_request().await?;

        // Fast Security Check (SSRF & Domain Blacklist)
        if let Some(sec) = &self.security {
            if let Err(reason) = sec.check_target(&target) {
                let err = OutboundError::new(
                    OutboundErrorKind::TargetBlocked(reason.to_string()),
                    None,
                );
                committed_state.commit_fail(&err).await?;
                return Err(InboundError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    reason,
                )));
            }
        }

        // Connect to upstream proxy pool with retry BEFORE sending success reply!
        match self.pool.connect_with_retry(&target).await {
            Ok(mut conn) => {
                let mut client_reader = committed_state.commit_success().await?;

                if !conn.residual.is_empty() {
                    client_reader.get_mut().write_all(&conn.residual).await?;
                }

                // Forward any buffered client reader data to upstream
                let unconsumed = client_reader.buffer();
                if !unconsumed.is_empty() {
                    conn.stream.write_all(unconsumed).await?;
                }

                let mut client_socket = client_reader.into_inner();
                let relay_res = copy_bidirectional(&mut client_socket, &mut conn.stream).await;
                conn.record_relay_result(&relay_res, 3, std::time::Duration::from_secs(10));
                Ok(())
            }
            Err(PoolError::AllRetriesFailed(errors)) => {
                let last_err = errors.into_iter().last().unwrap_or_else(|| {
                    OutboundError::new(
                        OutboundErrorKind::TcpConnectFailed("All upstream proxies failed".into()),
                        None,
                    )
                });
                committed_state.commit_fail(&last_err).await?;
                Err(InboundError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    last_err.to_string(),
                )))
            }
            Err(PoolError::NoHealthyProxy) => {
                let err = OutboundError::new(
                    OutboundErrorKind::TcpConnectFailed("No proxy available in pool".into()),
                    None,
                );
                committed_state.commit_fail(&err).await?;
                Err(InboundError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "No proxy available in pool",
                )))
            }
        }
    }

    /// Handle incoming forward HTTP CONNECT session
    pub async fn handle_http_connect(
        &self,
        session: PendingHttpSession,
    ) -> Result<(), InboundError> {
        // Verify credentials if required
        if let Some(expected) = &self.auth_credentials {
            let matches = match session.credentials() {
                Some(provided) => expected.matches(&provided.user, &provided.pass),
                None => false,
            };

            if !matches {
                session.reject_auth().await?;
                return Err(InboundError::AuthFailed);
            }
        }

        let target = session.target.clone();

        // Fast Security Check (SSRF & Domain Blacklist)
        if let Some(sec) = &self.security {
            if let Err(reason) = sec.check_target(&target) {
                let err = OutboundError::new(
                    OutboundErrorKind::TargetBlocked(reason.to_string()),
                    None,
                );
                session.commit_fail(&err).await?;
                return Err(InboundError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    reason,
                )));
            }
        }

        // Connect to upstream proxy pool with retry BEFORE committing 200 Connection Established!
        match self.pool.connect_with_retry(&target).await {
            Ok(mut conn) => {
                let (mut client_reader, client_early_data) = session.commit_success().await?;

                if !conn.residual.is_empty() {
                    client_reader.get_mut().write_all(&conn.residual).await?;
                }

                // If downstream client sent early data after CONNECT headers
                if !client_early_data.is_empty() {
                    conn.stream.write_all(&client_early_data).await?;
                }

                // Forward any buffered client reader bytes to upstream
                let unconsumed = client_reader.buffer();
                if !unconsumed.is_empty() {
                    conn.stream.write_all(unconsumed).await?;
                }

                let mut client_socket = client_reader.into_inner();
                let relay_res = copy_bidirectional(&mut client_socket, &mut conn.stream).await;
                conn.record_relay_result(&relay_res, 3, std::time::Duration::from_secs(10));
                Ok(())
            }
            Err(PoolError::AllRetriesFailed(errors)) => {
                let last_err = errors.into_iter().last().unwrap_or_else(|| {
                    OutboundError::new(
                        OutboundErrorKind::TcpConnectFailed("All upstream proxies failed".into()),
                        None,
                    )
                });
                session.commit_fail(&last_err).await?;
                Err(InboundError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    last_err.to_string(),
                )))
            }
            Err(PoolError::NoHealthyProxy) => {
                let err = OutboundError::new(
                    OutboundErrorKind::TcpConnectFailed("No proxy available in pool".into()),
                    None,
                );
                session.commit_fail(&err).await?;
                Err(InboundError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "No proxy available in pool",
                )))
            }
        }
    }
}
