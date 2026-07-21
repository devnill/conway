//! The `SessionStore` port (architecture §4.4).
//!
//! MVP impl: `JsonlSessionStore` in `conway-session` — one `.jsonl` per
//! session, first line the header. Debuggable with `jq`, greppable,
//! diffable, and trivially inspectable by a human (decision 9).

use async_trait::async_trait;

use crate::error::StoreError;
use crate::ids::{LogSeq, SeqRange, SessionId};
use crate::log::{LogRecord, SessionFilter, SessionMeta};

#[async_trait]
pub trait SessionStore: Send + Sync + 'static {
    async fn create(&self, meta: SessionMeta) -> Result<SessionId, StoreError>;

    async fn append(&self, sid: &SessionId, rec: LogRecord) -> Result<LogSeq, StoreError>;

    async fn read(&self, sid: &SessionId, range: SeqRange) -> Result<Vec<LogRecord>, StoreError>;

    async fn head(&self, sid: &SessionId) -> Result<LogSeq, StoreError>;

    /// Writes exactly one header line; copies zero records. O(1) in parent
    /// transcript size regardless of how many records the parent holds —
    /// this is what makes tournament patterns (one fork → N spawned
    /// children) affordable (architecture §5.1, §8).
    async fn fork(
        &self,
        parent: &SessionId,
        at: LogSeq,
        meta: SessionMeta,
    ) -> Result<SessionId, StoreError>;

    async fn meta(&self, sid: &SessionId) -> Result<SessionMeta, StoreError>;

    async fn children(&self, sid: &SessionId) -> Result<Vec<SessionId>, StoreError>;

    async fn list(&self, filter: SessionFilter) -> Result<Vec<SessionMeta>, StoreError>;
}
