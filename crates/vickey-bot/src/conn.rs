//! Blocking connection helpers for the one-shot commands (`status`,
//! `replay`, `setup`). The daemon manages its own connection lifecycle in
//! `engine.rs` but shares the reducer-outcome plumbing defined here.
//!
//! SDK 2.13 threading model, verified against `spacetimedb-sdk` source:
//! `DbConnection` wraps `Arc`-shared state. `run_threaded()` spawns a thread
//! that `tokio::select!`s over incoming WebSocket messages and a queue of
//! "pending mutations". Calling a reducer (`conn.reducers.x_then(..)`) from
//! any thread only enqueues a mutation, which wakes that thread and is sent
//! immediately. Row callbacks run on the SDK thread after the whole
//! transaction has been applied to the client cache, so a callback (or any
//! later cache read) sees the post-transaction state.

use crate::module_bindings::{DbConnection, ReducerEventContext, SubscriptionHandle};
use anyhow::{anyhow, Context, Result};
use spacetimedb_sdk::__codegen::InternalError;
use spacetimedb_sdk::{DbContext, Identity};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub type ReducerOutcome = std::result::Result<std::result::Result<(), String>, InternalError>;

/// Flattens the SDK's nested reducer outcome into `Ok` or the server's message.
pub fn flatten_outcome(o: ReducerOutcome) -> std::result::Result<(), String> {
    match o {
        Ok(Ok(())) => Ok(()),
        Ok(Err(msg)) => Err(msg),
        Err(e) => Err(format!("internal error: {e}")),
    }
}

pub struct Session {
    pub conn: DbConnection,
    pub identity: Identity,
    pub token: String,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.conn.disconnect();
    }
}

/// Connects (anonymously when `token` is `None`) and waits for the identity.
pub fn connect(host: &str, db: &str, token: Option<String>, timeout: Duration) -> Result<Session> {
    let (tx, rx) = mpsc::channel::<std::result::Result<(Identity, String), String>>();
    let tx_err = tx.clone();
    let conn = DbConnection::builder()
        .with_uri(host)
        .with_database_name(db)
        .with_token(token)
        .on_connect(move |_, identity, token| {
            let _ = tx.send(Ok((identity, token.to_string())));
        })
        .on_connect_error(move |_, e| {
            let _ = tx_err.send(Err(e.to_string()));
        })
        .build()
        .with_context(|| format!("connecting to {db} at {host}"))?;
    conn.run_threaded();
    let (identity, token) = rx
        .recv_timeout(timeout)
        .map_err(|_| anyhow!("timed out connecting to {db} at {host}"))?
        .map_err(|e| anyhow!("connection refused: {e}"))?;
    Ok(Session { conn, identity, token })
}

/// Subscribes to `queries` and blocks until the initial rows are in the cache.
pub fn subscribe_wait(conn: &DbConnection, queries: &[String], timeout: Duration) -> Result<SubscriptionHandle> {
    let (tx, rx) = mpsc::channel::<std::result::Result<(), String>>();
    let tx_err = tx.clone();
    let handle = conn
        .subscription_builder()
        .on_applied(move |_| {
            let _ = tx.send(Ok(()));
        })
        .on_error(move |_, e| {
            let _ = tx_err.send(Err(e.to_string()));
        })
        .subscribe(queries.to_vec());
    rx.recv_timeout(timeout)
        .map_err(|_| anyhow!("timed out waiting for subscription"))?
        .map_err(|e| anyhow!("subscription failed: {e}"))?;
    Ok(handle)
}

/// Calls a reducer through its `_then` variant and waits for the server's verdict.
pub fn call_wait<F>(invoke: F, timeout: Duration) -> Result<std::result::Result<(), String>>
where
    F: FnOnce(Box<dyn FnOnce(&ReducerEventContext, ReducerOutcome) + Send>) -> spacetimedb_sdk::Result<()>,
{
    let (tx, rx) = mpsc::channel();
    invoke(Box::new(move |_, outcome| {
        let _ = tx.send(flatten_outcome(outcome));
    }))
    .map_err(|e| anyhow!("could not send reducer call: {e}"))?;
    rx.recv_timeout(timeout).map_err(|_| anyhow!("timed out waiting for reducer result"))
}

/// Polls `cond` every 25 ms until it returns `Some` or `timeout` passes.
pub fn wait_for<T>(timeout: Duration, mut cond: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    loop {
        if let Some(v) = cond() {
            return Some(v);
        }
        if start.elapsed() >= timeout {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_flatten() {
        assert_eq!(flatten_outcome(Ok(Ok(()))), Ok(()));
        assert_eq!(flatten_outcome(Ok(Err("Auction closed".into()))), Err("Auction closed".into()));
    }

    #[test]
    fn wait_for_times_out_and_succeeds() {
        assert_eq!(wait_for(Duration::from_millis(30), || None::<u8>), None);
        let mut n = 0;
        assert_eq!(
            wait_for(Duration::from_secs(1), || {
                n += 1;
                (n > 2).then_some(n)
            }),
            Some(3)
        );
    }
}
