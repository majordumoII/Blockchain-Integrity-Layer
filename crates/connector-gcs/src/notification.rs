//! Parsing for GCS Pub/Sub object-change notifications.
//!
//! GCS delivers these as a Pub/Sub message with the event type in the
//! `eventType` message *attribute* (not the body) and — only when the
//! notification config was created with `--payload-format=json`
//! (`JSON_API_V1`) — the full `storage#object` resource as the message
//! body. This connector requires `JSON_API_V1` specifically so richer
//! object metadata (size, content type, generation, hashes) is available
//! without a separate `storage.objects.get` round trip per event.

use serde::Deserialize;

/// GCS notification event types, as sent in the `eventType` Pub/Sub
/// message attribute. Only `ObjectFinalize` carries a fully-written,
/// hashable object — see [`crate::source`]'s module docs for why the
/// other variants are acknowledged and skipped rather than surfaced as
/// [`proof_connectors::SourceRecord`]s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventType {
    /// A new object was created, or an existing object's content was
    /// overwritten. This is GCS's own "fully and durably written" signal
    /// — the object is guaranteed complete by the time this fires, unlike
    /// a raw filesystem write which can be observed mid-write.
    ObjectFinalize,
    /// An object (or a specific generation of it) was deleted.
    ObjectDelete,
    /// An object's metadata (not content) changed.
    ObjectMetadataUpdate,
    /// An object's storage class changed via Object Lifecycle Management.
    ObjectArchive,
    /// Some event type this connector doesn't recognize (GCS may add new
    /// ones over time); carried through rather than treated as an error,
    /// since an unrecognized event should be safely ignorable, not fatal.
    Other(String),
}

impl EventType {
    /// Parses the raw `eventType` attribute value GCS sends
    /// (`OBJECT_FINALIZE`, `OBJECT_DELETE`, `OBJECT_METADATA_UPDATE`,
    /// `OBJECT_ARCHIVE`).
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw {
            "OBJECT_FINALIZE" => Self::ObjectFinalize,
            "OBJECT_DELETE" => Self::ObjectDelete,
            "OBJECT_METADATA_UPDATE" => Self::ObjectMetadataUpdate,
            "OBJECT_ARCHIVE" => Self::ObjectArchive,
            other => Self::Other(other.to_string()),
        }
    }
}

/// The subset of GCS's `storage#object` resource this connector needs,
/// as delivered in a `JSON_API_V1`-format notification's message body.
///
/// Deliberately not exhaustive of every field GCS can return — only what
/// this connector actually threads into [`proof_connectors::SourceRecord`]
/// metadata or uses to address the object for download.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectResource {
    /// The bucket containing this object.
    pub bucket: String,
    /// The object's name (its full path within the bucket).
    pub name: String,
    /// The object generation, as a decimal string (GCS represents this as
    /// a JSON string, not a number, since it can exceed a safe JSON
    /// integer's range). Uniquely identifies this exact version of the
    /// object's content.
    pub generation: String,
    /// The object's size in bytes, as a decimal string (same rationale as
    /// `generation`).
    pub size: Option<String>,
    /// MIME content type, if set.
    pub content_type: Option<String>,
}

/// Parses a `JSON_API_V1` notification message body as an
/// [`ObjectResource`].
///
/// # Errors
///
/// Returns the underlying [`serde_json::Error`] if `body` is not valid
/// JSON, or not shaped like a `storage#object` resource.
pub fn parse_object_resource(body: &[u8]) -> Result<ObjectResource, serde_json::Error> {
    serde_json::from_slice(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_known_event_types() {
        assert_eq!(
            EventType::parse("OBJECT_FINALIZE"),
            EventType::ObjectFinalize
        );
        assert_eq!(EventType::parse("OBJECT_DELETE"), EventType::ObjectDelete);
        assert_eq!(
            EventType::parse("OBJECT_METADATA_UPDATE"),
            EventType::ObjectMetadataUpdate
        );
        assert_eq!(EventType::parse("OBJECT_ARCHIVE"), EventType::ObjectArchive);
    }

    #[test]
    fn unrecognized_event_type_is_carried_through_not_rejected() {
        let parsed = EventType::parse("SOME_FUTURE_EVENT_TYPE");
        assert_eq!(
            parsed,
            EventType::Other("SOME_FUTURE_EVENT_TYPE".to_string())
        );
    }

    /// A trimmed but realistic `storage#object` resource, matching what a
    /// real GCS `JSON_API_V1` notification body looks like — including
    /// several real fields (`kind`, `id`, `selfLink`, `md5Hash`, `etag`,
    /// `timeCreated`) this connector doesn't model, to confirm unknown
    /// fields are tolerated rather than rejected.
    const SAMPLE_NOTIFICATION_BODY: &str = r#"{
        "kind": "storage#object",
        "id": "corporate-raw-docs/reports/q3.pdf/1234567890123456",
        "selfLink": "https://www.googleapis.com/storage/v1/b/corporate-raw-docs/o/reports%2Fq3.pdf",
        "name": "reports/q3.pdf",
        "bucket": "corporate-raw-docs",
        "generation": "1234567890123456",
        "metageneration": "1",
        "contentType": "application/pdf",
        "timeCreated": "2026-07-24T12:00:00.000Z",
        "updated": "2026-07-24T12:00:00.000Z",
        "size": "48213",
        "md5Hash": "sQqNsWTgdUEFt6mb5y4/5Q==",
        "etag": "CJf2/aXQ//8CEAE="
    }"#;

    #[test]
    fn parses_a_realistic_object_resource_ignoring_unknown_fields() {
        let object = parse_object_resource(SAMPLE_NOTIFICATION_BODY.as_bytes())
            .expect("should parse a realistic notification body");

        assert_eq!(object.bucket, "corporate-raw-docs");
        assert_eq!(object.name, "reports/q3.pdf");
        assert_eq!(object.generation, "1234567890123456");
        assert_eq!(object.content_type.as_deref(), Some("application/pdf"));
        assert_eq!(object.size.as_deref(), Some("48213"));
    }

    #[test]
    fn rejects_non_json_body() {
        let result = parse_object_resource(b"not json at all");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_json_missing_required_fields() {
        let result = parse_object_resource(br#"{"kind": "storage#object"}"#);
        assert!(result.is_err());
    }
}
