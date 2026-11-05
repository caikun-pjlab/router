//! Sticky routing with least-running-load placement for new sessions.
//!
//! Existing sessions stay on their assigned replica. New sessions are placed on
//! the healthy replica with the fewest in-flight requests; ties are resolved
//! with rendezvous hashing. Requests without an explicit session ID are
//! load-balanced but are not recorded as sessions.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use super::hash_key;
use super::{
    get_healthy_worker_indices, ConsistentHashPolicy, LoadBalancingPolicy, RequestHeaders,
};
use crate::core::Worker;
use crate::metrics::RouterMetrics;

pub const DEFAULT_SESSION_EXPIRATION_SECS: u64 = 7200;
pub const SESSION_EXPIRATION_ENV: &str = "STICKY_LEAST_LOADED_SESSION_EXPIRATION_IN_S";
const SWEEP_INTERVAL_SECS: u64 = 60;

#[derive(Debug)]
struct SessionEntry {
    worker_url: String,
    last_access: Instant,
}

#[derive(Debug)]
struct SessionState {
    sessions: HashMap<String, SessionEntry>,
    last_sweep: Instant,
}

#[derive(Debug)]
struct LeastLoadedSelection {
    index: usize,
    minimum_running_requests: usize,
    tie_break: &'static str,
    tie_break_hash: u64,
}

/// A sticky routing policy whose load signal is the worker running-request count.
#[derive(Debug)]
pub struct StickyLeastLoadedPolicy {
    state: Mutex<SessionState>,
    session_expiration: Duration,
}

impl StickyLeastLoadedPolicy {
    pub fn new() -> Self {
        Self::with_expiration_secs(Self::expiration_secs_from_env())
    }

    pub fn with_expiration_secs(secs: u64) -> Self {
        Self {
            state: Mutex::new(SessionState {
                sessions: HashMap::new(),
                last_sweep: Instant::now(),
            }),
            session_expiration: Duration::from_secs(secs),
        }
    }

    fn expiration_secs_from_env() -> u64 {
        match std::env::var(SESSION_EXPIRATION_ENV) {
            Ok(value) => value.trim().parse::<u64>().unwrap_or_else(|_| {
                warn!(
                    env = SESSION_EXPIRATION_ENV,
                    value = %value,
                    default = DEFAULT_SESSION_EXPIRATION_SECS,
                    "invalid session expiration, using default"
                );
                DEFAULT_SESSION_EXPIRATION_SECS
            }),
            Err(_) => DEFAULT_SESSION_EXPIRATION_SECS,
        }
    }

    fn sweep_expired(state: &mut SessionState, expiration: Duration, now: Instant) {
        if now.duration_since(state.last_sweep) < Duration::from_secs(SWEEP_INTERVAL_SECS) {
            return;
        }

        state.sessions.retain(|session_id, entry| {
            let expired = now.duration_since(entry.last_access) >= expiration;
            if expired {
                debug!(
                    session_id = session_id,
                    worker_url = %entry.worker_url,
                    "expired sticky session"
                );
            }
            !expired
        });
        state.last_sweep = now;
    }

    fn select_least_loaded(
        workers: &[Arc<dyn Worker>],
        healthy_indices: &[usize],
        tie_break_key: &str,
    ) -> LeastLoadedSelection {
        let loads: Vec<(usize, usize)> = healthy_indices
            .iter()
            .map(|&index| (index, workers[index].load()))
            .collect();

        let minimum_load = loads
            .iter()
            .map(|&(_, load)| load)
            .min()
            .expect("healthy worker indices are non-empty");

        let mut minimum_candidates = 0;
        let mut selected = None;
        for &(index, load) in &loads {
            if load != minimum_load {
                continue;
            }

            minimum_candidates += 1;
            let hash_key = format!("{}:{}", tie_break_key, workers[index].url());
            let hash = ConsistentHashPolicy::fbi_hash(&hash_key);
            match selected {
                Some((_, selected_hash)) if hash < selected_hash => {}
                _ => selected = Some((index, hash)),
            }
        }

        let (selected_index, tie_break_hash) =
            selected.expect("at least one worker has the minimum load");

        LeastLoadedSelection {
            index: selected_index,
            minimum_running_requests: minimum_load,
            tie_break: if minimum_candidates == 1 {
                "unique_minimum"
            } else {
                "rendezvous_hash"
            },
            tie_break_hash,
        }
    }

    fn record_selection(&self, worker: &Arc<dyn Worker>) {
        worker.increment_load();
        RouterMetrics::set_running_requests(worker.url(), worker.load());
        worker.increment_processed();
        RouterMetrics::record_processed_request(worker.url());
        RouterMetrics::record_policy_decision(self.name(), worker.url());
    }

    fn log_least_loaded_selection(
        session_id: Option<&str>,
        tie_break_key_source: &'static str,
        selection: &LeastLoadedSelection,
        worker: &Arc<dyn Worker>,
    ) {
        info!(
            session_id = session_id.unwrap_or(""),
            tie_break_key_source,
            selection = if session_id.is_some() {
                "new_session_min_running_request"
            } else {
                "no_session_min_running_request"
            },
            minimum_running_requests = selection.minimum_running_requests,
            tie_break = selection.tie_break,
            tie_break_hash = selection.tie_break_hash,
            worker_url = %worker.url(),
            running_requests_after_reservation = worker.load(),
            "sticky_least_loaded selected worker"
        );
    }

    pub fn session_count(&self) -> usize {
        self.state.lock().unwrap().sessions.len()
    }
}

impl Default for StickyLeastLoadedPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl LoadBalancingPolicy for StickyLeastLoadedPolicy {
    fn select_worker_with_headers(
        &self,
        workers: &[Arc<dyn Worker>],
        request_text: Option<&str>,
        headers: Option<&RequestHeaders>,
    ) -> Option<usize> {
        let session_id = hash_key::extract_session_id(request_text, headers);
        self.select_worker_with_session(workers, request_text, headers, session_id.as_deref())
    }

    fn select_worker_with_session(
        &self,
        workers: &[Arc<dyn Worker>],
        request_text: Option<&str>,
        _headers: Option<&RequestHeaders>,
        session_id: Option<&str>,
    ) -> Option<usize> {
        let healthy_indices = get_healthy_worker_indices(workers);
        if healthy_indices.is_empty() {
            return None;
        }

        let now = Instant::now();
        let mut state = self.state.lock().unwrap();
        Self::sweep_expired(&mut state, self.session_expiration, now);

        let Some(session_id) = session_id else {
            let tie_break_key = request_text.unwrap_or("");
            let selection = Self::select_least_loaded(workers, &healthy_indices, tie_break_key);
            let index = selection.index;
            let worker = workers[index].clone();
            drop(state);
            self.record_selection(&worker);
            Self::log_least_loaded_selection(None, "request_text", &selection, &worker);
            return Some(index);
        };

        if let Some(entry) = state.sessions.get_mut(session_id) {
            let expired = now.duration_since(entry.last_access) >= self.session_expiration;
            let worker_url = entry.worker_url.clone();
            let assigned = if expired {
                None
            } else {
                healthy_indices
                    .iter()
                    .copied()
                    .find(|&index| workers[index].url() == worker_url)
            };

            if let Some(index) = assigned {
                entry.last_access = now;
                let worker = workers[index].clone();
                drop(state);
                self.record_selection(&worker);
                info!(
                    session_id = session_id,
                    worker_url = %worker_url,
                    selection = "existing_session",
                    running_requests_after_reservation = worker.load(),
                    "sticky_least_loaded routed session"
                );
                return Some(index);
            }

            state.sessions.remove(session_id);
            info!(
                session_id = session_id,
                worker_url = %worker_url,
                selection = "reassign_session",
                reason = if expired { "expired" } else { "unavailable" },
                "sticky_least_loaded reassigning session"
            );
        }

        let selection = Self::select_least_loaded(workers, &healthy_indices, session_id);
        let index = selection.index;
        let worker_url = workers[index].url().to_string();
        state.sessions.insert(
            session_id.to_string(),
            SessionEntry {
                worker_url: worker_url.clone(),
                last_access: now,
            },
        );
        let worker = workers[index].clone();
        drop(state);
        self.record_selection(&worker);
        Self::log_least_loaded_selection(Some(session_id), "session_id", &selection, &worker);
        Some(index)
    }

    fn name(&self) -> &'static str {
        "sticky_least_loaded"
    }

    fn needs_request_text(&self) -> bool {
        true
    }

    fn needs_headers(&self) -> bool {
        true
    }

    fn starts_running_load(&self) -> bool {
        true
    }

    fn reset(&self) {
        self.state.lock().unwrap().sessions.clear();
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::core::worker::WorkerMetadata;
    use crate::core::{
        BasicWorker, CircuitBreaker, ConnectionMode, Worker, WorkerResult, WorkerType,
    };

    fn make_workers(urls: &[&str]) -> Vec<Arc<dyn Worker>> {
        urls.iter()
            .map(|url| {
                Arc::new(BasicWorker::new((*url).to_string(), WorkerType::Regular))
                    as Arc<dyn Worker>
            })
            .collect()
    }

    fn session_headers(session_id: &str) -> RequestHeaders {
        RequestHeaders::from([("x-session-id".to_string(), session_id.to_string())])
    }

    #[derive(Debug)]
    struct ChangingLoadWorker {
        inner: BasicWorker,
        load_reads: AtomicUsize,
    }

    #[async_trait]
    impl Worker for ChangingLoadWorker {
        fn url(&self) -> &str {
            self.inner.url()
        }

        fn worker_type(&self) -> WorkerType {
            self.inner.worker_type()
        }

        fn connection_mode(&self) -> ConnectionMode {
            self.inner.connection_mode()
        }

        fn is_healthy(&self) -> bool {
            self.inner.is_healthy()
        }

        fn set_healthy(&self, healthy: bool) {
            self.inner.set_healthy(healthy);
        }

        async fn check_health_async(&self) -> WorkerResult<()> {
            self.inner.check_health_async().await
        }

        fn load(&self) -> usize {
            match self.load_reads.fetch_add(1, Ordering::SeqCst) {
                0 => 1,
                _ => 0,
            }
        }

        fn increment_load(&self) {
            self.inner.increment_load();
        }

        fn decrement_load(&self) {
            self.inner.decrement_load();
        }

        fn processed_requests(&self) -> usize {
            self.inner.processed_requests()
        }

        fn increment_processed(&self) {
            self.inner.increment_processed();
        }

        fn metadata(&self) -> &WorkerMetadata {
            self.inner.metadata()
        }

        fn circuit_breaker(&self) -> &CircuitBreaker {
            self.inner.circuit_breaker()
        }
    }

    #[test]
    fn selection_uses_one_load_snapshot() {
        let policy = StickyLeastLoadedPolicy::with_expiration_secs(7200);
        let worker = Arc::new(ChangingLoadWorker {
            inner: BasicWorker::new("http://worker:8080".to_string(), WorkerType::Regular),
            load_reads: AtomicUsize::new(0),
        });
        let workers: Vec<Arc<dyn Worker>> = vec![worker];

        let selected = policy
            .select_worker_with_session(&workers, None, None, Some("session-1"))
            .unwrap();
        assert_eq!(selected, 0);
    }

    #[test]
    fn new_sessions_use_minimum_running_load_and_hash_ties() {
        let policy = StickyLeastLoadedPolicy::with_expiration_secs(7200);
        let workers = make_workers(&["http://worker1", "http://worker2", "http://worker3"]);
        workers[0].increment_load();
        workers[1].increment_load();

        let headers = session_headers("session-1");
        let selected = policy
            .select_worker_with_headers(&workers, None, Some(&headers))
            .unwrap();
        assert_eq!(selected, 2);
        assert_eq!(workers[selected].load(), 1);

        let headers = session_headers("session-2");
        let expected = (0..workers.len())
            .max_by_key(|&index| {
                ConsistentHashPolicy::fbi_hash(&format!("session-2:{}", workers[index].url()))
            })
            .unwrap();
        let tied = policy
            .select_worker_with_headers(&workers, None, Some(&headers))
            .unwrap();
        assert_eq!(tied, expected);
    }

    #[test]
    fn existing_sessions_remain_sticky_and_unavailable_workers_reassign() {
        let policy = StickyLeastLoadedPolicy::with_expiration_secs(7200);
        let workers = make_workers(&["http://worker1", "http://worker2"]);
        let headers = session_headers("session-1");

        let first = policy
            .select_worker_with_headers(&workers, None, Some(&headers))
            .unwrap();
        workers[first].decrement_load();
        let second = policy
            .select_worker_with_headers(&workers, None, Some(&headers))
            .unwrap();
        assert_eq!(first, second);

        workers[first].set_healthy(false);
        let reassigned = policy
            .select_worker_with_headers(&workers, None, Some(&headers))
            .unwrap();
        assert_ne!(reassigned, first);
    }

    #[test]
    fn requests_without_session_do_not_create_mappings() {
        let policy = StickyLeastLoadedPolicy::with_expiration_secs(7200);
        let workers = make_workers(&["http://worker1", "http://worker2"]);

        let selected = policy
            .select_worker_with_headers(&workers, Some("prompt"), None)
            .unwrap();
        assert_eq!(policy.session_count(), 0);
        assert_eq!(workers[selected].load(), 1);
    }

    #[test]
    fn expired_sessions_are_reassigned() {
        let policy = StickyLeastLoadedPolicy::with_expiration_secs(0);
        let workers = make_workers(&["http://worker1", "http://worker2"]);
        let headers = session_headers("session-1");

        let first = policy
            .select_worker_with_headers(&workers, None, Some(&headers))
            .unwrap();
        let second = policy
            .select_worker_with_headers(&workers, None, Some(&headers))
            .unwrap();
        assert_ne!(first, second);
    }
}
