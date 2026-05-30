//! Server lifecycle types: `Server` and `ServerHandle`.

use std::sync::Arc;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::OpcUaConfig;
use crate::error::ServerError;
use crate::LoginCallback;
use core_model::TagRegistry;

/// Handle returned when the server is started. It can be used to stop the server
/// and await until it exits.
pub struct ServerHandle {
    pub(crate) shutdown_tx: watch::Sender<bool>,
    pub(crate) join_handle: JoinHandle<Result<(), ServerError>>,
    pub(crate) write_bridge_thread: Option<std::thread::JoinHandle<()>>,
}

impl ServerHandle {
    /// Construct a new ServerHandle from internal pieces.
    pub(crate) fn new(
        shutdown_tx: watch::Sender<bool>,
        join_handle: JoinHandle<Result<(), ServerError>>,
        write_bridge_thread: std::thread::JoinHandle<()>,
    ) -> Self {
        Self {
            shutdown_tx,
            join_handle,
            write_bridge_thread: Some(write_bridge_thread),
        }
    }

    /// Request server shutdown. This method is non-blocking.
    pub fn shutdown(&self) -> Result<(), ServerError> {
        let _ = self.shutdown_tx.send(true);
        Ok(())
    }

    /// Wait for the server task to finish. Returns any backend error if present.
    ///
    /// Before awaiting the server task, this joins background threads (write bridge
    /// and tag event bridge) to detect any panics and ensure clean shutdown.
    pub async fn wait(self) -> Result<(), ServerError> {
        if let Some(h) = self.write_bridge_thread {
            let _ = h.join();
        }
        match self.join_handle.await {
            Ok(res) => res,
            Err(join_err) => Err(ServerError::Other(format!(
                "Server task panicked or was cancelled: {}",
                join_err
            ))),
        }
    }
}

/// Public server facade. Construct with `Server::new(...)` and call `start`.
///
/// The concrete server implementation is backed by a native open62541 instance.
#[derive(Clone)]
pub struct Server {
    cfg: Arc<OpcUaConfig>,
    registry: Arc<TagRegistry>,
    write_handler: crate::writes::WriteHandlerArc,
}

impl Server {
    /// Create a new server facade instance.
    pub fn new(
        cfg: Arc<OpcUaConfig>,
        registry: Arc<TagRegistry>,
        write_handler: crate::writes::WriteHandlerArc,
    ) -> Self {
        Self {
            cfg,
            registry,
            write_handler,
        }
    }

    /// Start the server asynchronously. Returns a `ServerHandle` to control lifecycle.
    #[tracing::instrument(skip(self))]
    pub fn start(&self) -> Result<ServerHandle, ServerError> {
        self.start_with_login_cb(None)
    }

    /// Start the server with an optional username/password login callback.
    #[tracing::instrument(skip(self, login_cb))]
    pub fn start_with_login_cb(
        &self,
        login_cb: Option<LoginCallback>,
    ) -> Result<ServerHandle, ServerError> {
        if login_cb.is_some() && !self.cfg.username_password_enabled {
            return Err(ServerError::Config(
                "username/password authentication disabled in config".into(),
            ));
        }

        let cfg_owned: OpcUaConfig = (*self.cfg).clone();
        crate::native::start_native_server(
            cfg_owned,
            self.registry.clone(),
            self.write_handler.clone(),
            login_cb,
        )
    }
}
