//! [`proof_connectors::RecordSource`] implementation backed by a GCS
//! Pub/Sub pull subscription receiving object-change notifications.
//!
//! Only `OBJECT_FINALIZE` events are surfaced as
//! [`proof_connectors::SourceRecord`]s — GCS only fires this once an
//! object is fully and durably written, which is exactly the "commit"
//! signal a raw filesystem watcher cannot provide (see
//! `GAPS-DATA-SOURCES.md`'s file-systems section for why that gap made
//! filesystems the weakest `RecordSource` target of the three
//! considered, and why object storage's own event model sidesteps it).
//! `OBJECT_DELETE`/`OBJECT_METADATA_UPDATE`/`OBJECT_ARCHIVE` events and
//! any event type this connector doesn't recognize are acknowledged and
//! skipped — there is no object content to hash for those.
//!
//! Requires the subscription to have **exactly-once delivery** enabled.
//! Pub/Sub's plain `ack()` is fire-and-forget with no confirmation
//! signal at all; only `confirmed_ack()` (available on an exactly-once
//! subscription's handler) gives the real awaitable success/failure
//! result [`proof_connectors::AckToken::ack`]'s contract requires. Using
//! plain `ack()` here would make this connector's acknowledgment
//! silently weaker than every other `RecordSource` in this workspace
//! (`connector-postgres`'s `LsnAckToken` genuinely confirms its flush
//! position was sent) — see the project's own gap-analysis discussion
//! for why that tradeoff was rejected rather than accepted quietly.

use crate::notification::{self, EventType};
use crate::object_change::ObjectChange;
use google_cloud_pubsub::client::Subscriber;
use google_cloud_pubsub::model::Message;
use google_cloud_pubsub::subscriber::handler::Handler;
use google_cloud_storage::client::Storage;
use proof_connectors::{AckError, AckToken, SourceError, SourceId, SourceRecord};
use proof_core::hash::HashAlgorithm;

/// A [`proof_connectors::RecordSource`] that watches one GCS Pub/Sub pull
/// subscription and yields each `OBJECT_FINALIZE` notification as a
/// [`SourceRecord`].
///
/// The object's content digest is computed by streaming its bytes
/// straight from GCS through a `proof_core::hash::Hasher` (`hash_algorithm`),
/// never buffering the whole object in memory — the gap `proof-core`'s
/// streaming hash API was added to close. `SourceRecord::bytes` is *not*
/// the object's raw content (that would defeat the point of streaming);
/// it's a canonical encoding (`crate::object_change::ObjectChange`) of
/// the object's identity plus that content digest, which is what
/// `proof-service`'s pipeline then hashes again with its own fixed BLAKE3
/// step (a cheap hash over a few hundred bytes, independent of
/// `hash_algorithm`, which only controls the potentially-large object's
/// own content hash) — see `object_change`'s module docs for why this
/// two-step shape is still fully independently re-derivable by a
/// verifier, not a shortcut.
pub struct GcsSource {
    source_id: SourceId,
    storage: Storage,
    subscription_path: String,
    stream: google_cloud_pubsub::subscriber::MessageStream,
    hash_algorithm: HashAlgorithm,
}

impl GcsSource {
    /// Connects to Pub/Sub and GCS using Application Default Credentials,
    /// and starts pulling from `subscription_path` (the full resource
    /// name, `projects/{project}/subscriptions/{subscription}`).
    ///
    /// `source_id` labels every [`SourceRecord`] and metric this source
    /// produces — e.g. `"gcs:corporate-raw-docs"`.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Disconnected`] if either client cannot be
    /// built (e.g. credentials cannot be resolved).
    pub async fn connect(
        source_id: SourceId,
        subscription_path: impl Into<String>,
        hash_algorithm: HashAlgorithm,
    ) -> Result<Self, SourceError> {
        let subscriber =
            Subscriber::builder()
                .build()
                .await
                .map_err(|e| SourceError::Disconnected {
                    source_id: source_id.clone(),
                    reason: format!("building Pub/Sub subscriber client: {e}"),
                })?;
        let storage = Storage::builder()
            .build()
            .await
            .map_err(|e| SourceError::Disconnected {
                source_id: source_id.clone(),
                reason: format!("building GCS storage client: {e}"),
            })?;

        let subscription_path = subscription_path.into();
        let stream = subscriber.subscribe(subscription_path.clone()).build();

        Ok(Self {
            source_id,
            storage,
            subscription_path,
            stream,
            hash_algorithm,
        })
    }
}

#[async_trait::async_trait]
impl proof_connectors::RecordSource for GcsSource {
    fn source_id(&self) -> &SourceId {
        &self.source_id
    }

    async fn next(&mut self) -> Result<(SourceRecord, Box<dyn AckToken>), SourceError> {
        loop {
            let (message, handler) = self
                .stream
                .next()
                .await
                .ok_or_else(|| SourceError::Disconnected {
                    source_id: self.source_id.clone(),
                    reason: "Pub/Sub message stream ended".to_string(),
                })?
                .map_err(|e| SourceError::Disconnected {
                    source_id: self.source_id.clone(),
                    reason: format!("Pub/Sub stream error: {e}"),
                })?;

            let Handler::ExactlyOnce(handler) = handler else {
                // Fail loudly rather than silently downgrading to a
                // fire-and-forget ack — see this module's docs for why
                // that tradeoff was rejected.
                return Err(SourceError::Disconnected {
                    source_id: self.source_id.clone(),
                    reason: format!(
                        "subscription {} does not have exactly-once delivery enabled; GcsSource requires it",
                        self.subscription_path
                    ),
                });
            };

            let Some(event_type) = message_event_type(&message) else {
                return Err(SourceError::UndecodableRecord {
                    source_id: self.source_id.clone(),
                    reason: "Pub/Sub message missing eventType attribute".to_string(),
                });
            };

            if !matches!(event_type, EventType::ObjectFinalize) {
                // No content to hash for a delete/metadata/archive event
                // — acknowledge it so the subscription doesn't redeliver
                // it forever, and move on to the next message.
                handler
                    .confirmed_ack()
                    .await
                    .map_err(|e| SourceError::Disconnected {
                        source_id: self.source_id.clone(),
                        reason: format!("failed to ack a skipped non-finalize event: {e}"),
                    })?;
                continue;
            }

            let object = notification::parse_object_resource(&message.data).map_err(|e| {
                SourceError::UndecodableRecord {
                    source_id: self.source_id.clone(),
                    reason: format!("undecodable OBJECT_FINALIZE notification body: {e}"),
                }
            })?;

            let content_digest = self.hash_object(&object).await?;

            let change = ObjectChange {
                bucket: object.bucket.clone(),
                object_name: object.name.clone(),
                generation: object.generation.clone(),
                content_digest: content_digest.into(),
            };
            let bytes =
                change
                    .to_canonical_bytes()
                    .map_err(|e| SourceError::UndecodableRecord {
                        source_id: self.source_id.clone(),
                        reason: format!("failed to encode object change: {e}"),
                    })?;

            let record = SourceRecord {
                bytes,
                metadata: vec![
                    ("bucket".to_string(), object.bucket.clone()),
                    ("object_name".to_string(), object.name.clone()),
                    ("generation".to_string(), object.generation.clone()),
                    (
                        "content_type".to_string(),
                        object.content_type.clone().unwrap_or_default(),
                    ),
                    (
                        "size_bytes".to_string(),
                        object.size.clone().unwrap_or_default(),
                    ),
                ],
                source_position: message.message_id.clone(),
            };

            let ack = Box::new(ExactlyOnceAckToken { handler });
            return Ok((record, ack));
        }
    }
}

impl GcsSource {
    /// Streams `object`'s content straight from GCS through a
    /// [`proof_core::hash::Hasher`], never buffering the whole object in
    /// memory — the gap `proof-core`'s streaming hash API was added to
    /// close.
    async fn hash_object(
        &self,
        object: &notification::ObjectResource,
    ) -> Result<proof_core::hash::Digest, SourceError> {
        let bucket_path = format!("projects/_/buckets/{}", object.bucket);
        let mut response = self
            .storage
            .read_object(bucket_path, &object.name)
            .send()
            .await
            .map_err(|e| SourceError::Disconnected {
                source_id: self.source_id.clone(),
                reason: format!(
                    "opening read of gs://{}/{}: {e}",
                    object.bucket, object.name
                ),
            })?;

        let mut hasher = self.hash_algorithm.hasher();
        while let Some(chunk) = response.next().await {
            let chunk = chunk.map_err(|e| SourceError::UndecodableRecord {
                source_id: self.source_id.clone(),
                reason: format!(
                    "reading gs://{}/{} (generation {}): {e}",
                    object.bucket, object.name, object.generation
                ),
            })?;
            hasher.update(&chunk);
        }

        Ok(hasher.finalize())
    }
}

fn message_event_type(message: &Message) -> Option<EventType> {
    message
        .attributes
        .get("eventType")
        .map(|raw| EventType::parse(raw))
}

/// Acknowledges a GCS notification by confirming the Pub/Sub message's
/// exactly-once ack, giving a real awaitable success/failure result —
/// see this module's docs for why exactly-once delivery is required
/// rather than optional.
struct ExactlyOnceAckToken {
    handler: google_cloud_pubsub::subscriber::handler::ExactlyOnce,
}

impl std::fmt::Debug for ExactlyOnceAckToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExactlyOnceAckToken")
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl AckToken for ExactlyOnceAckToken {
    async fn ack(self: Box<Self>) -> Result<(), AckError> {
        self.handler
            .confirmed_ack()
            .await
            .map_err(|e| AckError(e.to_string()))
    }
}
