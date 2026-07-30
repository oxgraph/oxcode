//! Client MCP workspace roots, fetched outside tool handlers.
//!
//! Nested `roots/list` during a tool call can hang some hosts. This cache is
//! populated from `on_initialized` / `on_roots_list_changed` only; tool
//! handlers wait for a completed publication.

use std::{path::PathBuf, sync::Arc, time::Duration};

use rmcp::{Peer, RoleServer, model::Root};
use tokio::sync::{Mutex, watch};

use super::project_root::file_uri_to_path;

/// How long an omitted-`path` resolve will wait for a roots fetch before
/// falling through.
const ROOTS_READY_WAIT: Duration = Duration::from_millis(500);

/// Published state of the client's MCP workspace roots.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RootsState {
    /// No fetch has completed yet (cold start).
    Cold,
    /// A refresh is in flight; holds the last completed root for failure restore.
    Refreshing(Option<PathBuf>),
    /// Last completed fetch. `None` means the client has no usable root.
    Ready(Option<PathBuf>),
}

/// Outcome of waiting for the roots cache from a tool handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RootsWait {
    /// A completed fetch is available (`None` = client has no usable root).
    Ready(Option<PathBuf>),
    /// Timed out while replacing a previously ready root. Callers must not fall
    /// back to sticky host env snapshots (those can be older than the root being
    /// replaced). Cold-start / `Refreshing(None)` timeouts are [`Ready`]`(None)`
    /// instead, so host env remains available.
    RefreshInFlight,
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
///
/// Fetches are serialized so overlapping `initialized` / `roots/list_changed`
/// notifications cannot wipe or restore the wrong root.
#[derive(Clone)]
pub(crate) struct RootsCache {
    tx: Arc<watch::Sender<RootsState>>,
    rx: watch::Receiver<RootsState>,
    fetch_lock: Arc<Mutex<()>>,
}

impl RootsCache {
    #[must_use]
    pub(crate) fn new() -> Self {
        let (tx, rx) = watch::channel(RootsState::Cold);
        Self {
            tx: Arc::new(tx),
            rx,
            fetch_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Waits briefly for a completed fetch.
    ///
    /// Never issues `roots/list`. On timeout during a refresh, returns
    /// [`RootsWait::RefreshInFlight`] so callers do not use a sticky host env
    /// snapshot that may predate the workspace being refreshed.
    pub(crate) async fn wait_ready(&self) -> RootsWait {
        if let RootsState::Ready(root) = &*self.rx.borrow() {
            return RootsWait::Ready(root.clone());
        }
        let mut rx = self.rx.clone();
        let finished = tokio::time::timeout(
            ROOTS_READY_WAIT,
            rx.wait_for(|state| matches!(state, RootsState::Ready(_))),
        )
        .await;
        match finished {
            Ok(Ok(state)) => match &*state {
                RootsState::Ready(root) => RootsWait::Ready(root.clone()),
                RootsState::Cold | RootsState::Refreshing(_) => RootsWait::Ready(None),
            },
            _ => match &*self.rx.borrow() {
                RootsState::Ready(root) => RootsWait::Ready(root.clone()),
                // Only suppress host env when replacing a previously ready root.
                // Cold start / Refreshing(None) still allows CLAUDE_PROJECT_DIR
                // and WORKSPACE_FOLDER_PATHS if roots/list is slow.
                RootsState::Refreshing(Some(_)) => RootsWait::RefreshInFlight,
                RootsState::Refreshing(None) | RootsState::Cold => RootsWait::Ready(None),
            },
        }
    }

    /// Fetches `roots/list` and publishes [`RootsState::Ready`].
    ///
    /// Serialized: only one fetch runs at a time. Marks
    /// [`RootsState::Refreshing`] first so concurrent waiters observe the
    /// refresh. RPC failures and unparseable URI lists restore the previous
    /// ready root; an explicitly empty list clears it.
    pub(crate) async fn fetch(&self, peer: &Peer<RoleServer>) {
        let _guard = self.fetch_lock.lock().await;
        let previous = match &*self.rx.borrow() {
            RootsState::Ready(root) | RootsState::Refreshing(root) => root.clone(),
            RootsState::Cold => None,
        };
        let _ = self.tx.send(RootsState::Refreshing(previous.clone()));
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

    #[tokio::test]
    async fn wait_ready_timeout_during_refresh_is_in_flight() {
        let cache = RootsCache::new();
        let _ = cache
            .tx
            .send(RootsState::Refreshing(Some(PathBuf::from("/old"))));
        assert_eq!(cache.wait_ready().await, RootsWait::RefreshInFlight);
    }

    #[tokio::test]
    async fn wait_ready_timeout_on_first_fetch_allows_host_env() {
        let cache = RootsCache::new();
        let _ = cache.tx.send(RootsState::Refreshing(None));
        assert_eq!(
            cache.wait_ready().await,
            RootsWait::Ready(None),
            "cold-start Refreshing(None) must not suppress host env"
        );
    }

    #[tokio::test]
    async fn wait_ready_timeout_while_cold_allows_host_env_fallback() {
        let cache = RootsCache::new();
        assert_eq!(cache.wait_ready().await, RootsWait::Ready(None));
    }

    #[tokio::test]
    async fn wait_ready_returns_ready_root() {
        let cache = RootsCache::new();
        let root = PathBuf::from("/project");
        let _ = cache.tx.send(RootsState::Ready(Some(root.clone())));
        assert_eq!(cache.wait_ready().await, RootsWait::Ready(Some(root)));
    }
}
