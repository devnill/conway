//! `PrefixKey` computation (architecture §5.3): the cache/slot dedup key
//! over the fixed static+inherited boundary, stable across sibling agents
//! forked at the same point.

use conway_core::canon::canonical_json_bytes;
use conway_core::ids::{ModelId, PrefixKey};
use conway_core::provenance::SegmentTier;
use conway_core::segment::PromptSegment;

/// Index of the last segment whose provenance tier is not `Volatile` —
/// architecture §5.3's boundary `B`: the end of the inherited prefix if one
/// exists, else the end of the static prefix (`ToolSchemas`). `None` only
/// if every segment is volatile, which does not happen in practice since
/// `ToolSchemas` is unconditional.
fn boundary_index(segments: &[PromptSegment]) -> Option<usize> {
    segments
        .iter()
        .rposition(|segment| segment.provenance.tier() != SegmentTier::Volatile)
}

/// `blake3(model_id ‖ canonical_bytes(segments[0..=B]))`, where `B` is
/// `boundary_index` (architecture §5.3).
///
/// Deliberately excludes each segment's `id` and `cache_hint`: `id` is
/// derived per-agent (so sibling agents forked at the same point get
/// distinct ids despite byte-identical static+inherited content), and
/// `cache_hint` is attached *after* this key is computed. Excluding both is
/// what makes the key stable across siblings and neutral to caching
/// never correctness-bearing.
pub fn prefix_key(model: &ModelId, segments: &[PromptSegment]) -> PrefixKey {
    let slice: &[PromptSegment] = match boundary_index(segments) {
        Some(boundary) => &segments[..=boundary],
        None => &[],
    };

    let mut hasher = blake3::Hasher::new();
    hasher.update(model.as_str().as_bytes());
    hasher.update(&canonical_segment_bytes(slice));
    PrefixKey::from_blake3(hasher.finalize())
}

/// Canonical `(role, content, provenance)` bytes for a slice of segments.
fn canonical_segment_bytes(segments: &[PromptSegment]) -> Vec<u8> {
    let projected: Vec<serde_json::Value> = segments
        .iter()
        .map(|segment| {
            serde_json::json!({
                "role": &segment.role,
                "content": &segment.content,
                "provenance": &segment.provenance,
            })
        })
        .collect();
    canonical_json_bytes(&serde_json::Value::Array(projected))
}
