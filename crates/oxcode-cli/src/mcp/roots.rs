//! Client MCP workspace roots, fetched outside tool handlers.
//!
//! Nested `roots/list` during a tool call can hang some hosts. This cache is
//! populated from `on_initialized` / `on_roots_list_changed` only; tool
//! handlers wait for a `Ready` publication.

use std::{path::PathBuf, sync::Arc, time::Duration};

use rmcp::{Peer, RoleServer, model::Root};
use tokio::sync::watch;

use super::project_root::file_uri_to_path;

/// How long an omitted-`path` resolve will wait for a roots fetch before
/// falling through without using a possibly-stale cache.
const ROOTS_READY_WAIT: Duration = Duration::from_millis(500);

/// Published state of the client's MCP workspace roots.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RootsState {
    /// A fetch has not completed yet, or a refresh is in progress.
    Pending,
    /// Last completed fetch. `None` means the client has no usable root.
    Ready(Option<PathBuf>),
}

/// Outcome of one `roots/list` attempt.
enum LoadOutcome {
    /// Client advertised no roots capability, or the RPC failed.
    Unavailable,
    /// Client returned an empty roots array.
    Empty,
    /// Client returned URIs but none parsed to a filesystem path.
    Unparseable,
    /// First usable filesystem path.
    Parsed(PathBuf),
}

/// Single watch-backed cache for MCP roots.
#[derive(Clone)]
pub(crate) struct RootsCache {
    tx: Arc<watch::Sender<RootsState>>,
    rx: watch::Receiver<RootsState>,
}

impl RootsCache {
    #[must_use]
    pub(crate) fn new() -> Self {
        let (tx, rx) = watch::channel(RootsState::Pending);
        Self {
            tx: Arc::new(tx),
            rx,
        }
    }

    /// Waits briefly for a completed fetch and returns the ready root, if any.
    ///
    /// Never issues `roots/list`. On timeout while still `Pending` (including an
    /// in-flight refresh), returns `None` so the caller does not keep using a
    /// stale workspace after a folder switch.
    pub(crate) async fn wait_ready(&self) -> Option<PathBuf> {
        if let RootsState::Ready(root) = &*self.rx.borrow() {
            return root.clone();
        }
        let mut rx = self.rx.clone();
        let finished = tokio::time::timeout(
            ROOTS_READY_WAIT,
            rx.wait_for(|state| matches!(state, RootsState::Ready(_))),
        )
        .await;
        match finished {
            Ok(Ok(state)) => match &*state {
                RootsState::Ready(root) => root.clone(),
                RootsState::Pending => None,
            },
            _ => None,
        }
    }

    /// Fetches `roots/list` and publishes [`RootsState::Ready`].
    ///
    /// Marks [`RootsState::Pending`] first so concurrent waiters observe the
    /// refresh. RPC failures and unparseable URI lists restore the previous
    /// ready root; an explicitly empty list clears it.
    pub(crate) async fn fetch(&self, peer: &Peer<RoleServer>) {
        let previous = match &*self.rx.borrow() {
            RootsState::Ready(root) => root.clone(),
            RootsState::Pending => None,
        };
        let _ = self.tx.send(RootsState::Pending);
        let published = match self.load_root(peer).await {
            LoadOutcome::Empty => None,
            LoadOutcome::Parsed(path) => Some(path),
            LoadOutcome::Unavailable | LoadOutcome::Unparseable => previous,
        };
        let _ = self.tx.send(RootsState::Ready(published));
    }

    async fn load_root(&self, peer: &Peer<RoleServer>) -> LoadOutcome {
        if peer
            .peer_info()
            .and_then(|info| info.capabilities.roots.as_ref())
            .is_none()
        {
            return LoadOutcome::Unavailable;
        }
        let Ok(result) = peer.list_roots().await else {
            return LoadOutcome::Unavailable;
        };
        classify_roots_list(result.roots)
    }
}

/// Classifies a `roots/list` payload.
fn classify_roots_list(roots: Vec<Root>) -> LoadOutcome {
    if roots.is_empty() {
        return LoadOutcome::Empty;
    }
    match roots
        .iter()
        .filter_map(|root| file_uri_to_path(&root.uri))
        .next()
    {
        Some(path) => LoadOutcome::Parsed(std::fs::canonicalize(&path).unwrap_or(path)),
        None => LoadOutcome::Unparseable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_roots_list_empty_unparseable_and_parsed() {
        assert!(matches!(classify_roots_list(vec![]), LoadOutcome::Empty));
        assert!(matches!(
            classify_roots_list(vec![Root::new("https://example.com")]),
            LoadOutcome::Unparseable
        ));
        let project = tempfile::TempDir::new().expect("temp");
        let uri = format!("file://{}", project.path().display());
        assert!(matches!(
            classify_roots_list(vec![Root::new(uri)]),
            LoadOutcome::Parsed(path) if path == project.path()
        ));
    }
}
