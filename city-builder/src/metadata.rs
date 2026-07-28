//! Build provenance: which upstream inputs produced each `<city>.bin`.
//!
//! This is the record that answers "does this `.bin` need rebuilding?" — as
//! opposed to [`crate::http_cache`], whose sidecars answer "is this *download*
//! still current?". The two questions look identical on a developer machine,
//! where the cache and the outputs are always restored together, but they come
//! apart in CI: the workflow restores `transit-viz/public/data/` before the
//! `--check-only` run and the multi-GB `cache/` directory only afterwards, and
//! only when a rebuild was already decided on. A record stored beside the
//! payload therefore can't be consulted when the decision is made.
//!
//! So the record lives beside the artifact it describes. `metadata.json` sits
//! in the output directory, rides the same cache layer as the `.bin` files, and
//! lets the rebuild decision be made from validators alone — no payload on disk
//! and no age heuristics, except as a fallback for sources that expose neither
//! a hash nor an HTTP validator.
//!
//! The output directory is web-served, so this file is public. It records only
//! opaque validator tokens keyed by city id and feed id — never a source URL,
//! which for Interline would carry the API token.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::http_cache::Validators;

pub const METADATA_FILE: &str = "metadata.json";

/// Bumped when the schema changes in a way that invalidates existing records.
/// A mismatch is treated as "no metadata", which forces a full rebuild — the
/// safe direction, and cheaper to reason about than a migration.
const SCHEMA_VERSION: u32 = 1;

/// Identity of one input source as its origin reports it.
///
/// Every field is an **opaque token**: compared for equality, never parsed or
/// recomputed. `sha1` comes from the Transitland API, the other two from HTTP
/// headers. All three are optional because origins differ in what they expose,
/// and an entry with none of them is what "unverifiable" means here.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Eq, Debug)]
pub struct SourceId {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

impl SourceId {
    pub fn from_sha1(sha1: impl Into<String>) -> Self {
        Self {
            sha1: Some(sha1.into()),
            ..Default::default()
        }
    }

    /// True when the origin told us nothing we could compare later.
    pub fn is_empty(&self) -> bool {
        self.sha1.is_none() && self.etag.is_none() && self.last_modified.is_none()
    }

    /// Compare two identities of the same source, strongest signal first.
    ///
    /// `None` means "not comparable" — the two sides have no field in common,
    /// which happens for a never-recorded source or when an origin changes
    /// which validator it exposes. Callers must not read that as "unchanged".
    pub fn same_as(&self, other: &Self) -> Option<bool> {
        if let (Some(a), Some(b)) = (&self.sha1, &other.sha1) {
            return Some(a == b);
        }
        if let (Some(a), Some(b)) = (&self.etag, &other.etag) {
            return Some(a == b);
        }
        if let (Some(a), Some(b)) = (&self.last_modified, &other.last_modified) {
            return Some(a == b);
        }
        None
    }
}

impl From<&Validators> for SourceId {
    fn from(v: &Validators) -> Self {
        Self {
            sha1: None,
            etag: v.etag.clone(),
            last_modified: v.last_modified.clone(),
        }
    }
}

/// What produced one city's `.bin`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CityMetadata {
    /// RFC 3339 timestamp of the build. Content, not mtime, so it survives any
    /// archive/restore round trip; bounds staleness for unverifiable inputs.
    pub built_at: String,
    /// Feed id (Transitland onestop id or direct URL) → identity at build time.
    pub feeds: BTreeMap<String, SourceId>,
    /// OSM extract identity. Absent for the Overpass fallback, which is a POST
    /// query with no validators.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub osm: Option<SourceId>,
}

impl CityMetadata {
    /// How long ago this `.bin` was built, or `None` if the stamp is unparsable.
    pub fn age(&self) -> Option<Duration> {
        let built = chrono::DateTime::parse_from_rfc3339(&self.built_at).ok()?;
        chrono::Utc::now()
            .signed_duration_since(built.with_timezone(&chrono::Utc))
            .to_std()
            .ok()
    }

    pub fn age_days(&self) -> u64 {
        self.age().map(|a| a.as_secs() / 86_400).unwrap_or(0)
    }
}

#[derive(Serialize, Deserialize)]
pub struct Metadata {
    pub version: u32,
    pub cities: BTreeMap<String, CityMetadata>,
}

impl Default for Metadata {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            cities: BTreeMap::new(),
        }
    }
}

pub fn metadata_path(output_dir: &Path) -> PathBuf {
    output_dir.join(METADATA_FILE)
}

impl Metadata {
    /// Read the record, or an empty one if it's missing, unreadable, malformed,
    /// or from another schema version. Every one of those means "we can't prove
    /// anything about the existing `.bin` files", and an empty record makes the
    /// caller rebuild — so a corrupt file costs time, never correctness.
    pub fn load(output_dir: &Path) -> Self {
        let path = metadata_path(output_dir);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(m) if m.version == SCHEMA_VERSION => m,
            Ok(m) => {
                eprintln!(
                    "  {} is schema v{} (expected v{}) — treating as absent",
                    METADATA_FILE, m.version, SCHEMA_VERSION
                );
                Self::default()
            }
            Err(e) => {
                eprintln!("  WARNING: could not parse {}: {}", METADATA_FILE, e);
                Self::default()
            }
        }
    }

    pub fn save(&self, output_dir: &Path) -> Result<()> {
        let path = metadata_path(output_dir);
        let text = serde_json::to_string_pretty(self).context("failed to serialise metadata")?;
        crate::http_cache::write_atomic(&path, format!("{}\n", text).as_bytes())
            .with_context(|| format!("failed to write {:?}", path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_outranks_http_validators() {
        let a = SourceId {
            sha1: Some("aaa".into()),
            etag: Some("\"1\"".into()),
            last_modified: None,
        };
        let b = SourceId {
            sha1: Some("aaa".into()),
            etag: Some("\"2\"".into()),
            last_modified: None,
        };
        assert_eq!(a.same_as(&b), Some(true));
    }

    #[test]
    fn etag_outranks_last_modified() {
        let a = SourceId {
            sha1: None,
            etag: Some("\"1\"".into()),
            last_modified: Some("Mon, 01 Jan 2026 00:00:00 GMT".into()),
        };
        let b = SourceId {
            sha1: None,
            etag: Some("\"2\"".into()),
            last_modified: Some("Mon, 01 Jan 2026 00:00:00 GMT".into()),
        };
        assert_eq!(a.same_as(&b), Some(false));
    }

    #[test]
    fn nothing_in_common_is_not_comparable() {
        // The dangerous case: must be None, never Some(true). A never-recorded
        // source compared against a live one has to force a rebuild.
        let recorded = SourceId::default();
        let live = SourceId {
            sha1: None,
            etag: Some("\"1\"".into()),
            last_modified: None,
        };
        assert_eq!(recorded.same_as(&live), None);
        assert_eq!(
            SourceId::from_sha1("aaa").same_as(&live),
            None,
            "sha1 vs etag share no field"
        );
    }

    #[test]
    fn round_trips_through_json() {
        let mut m = Metadata::default();
        m.cities.insert(
            "chicago".into(),
            CityMetadata {
                built_at: "2026-07-27T02:00:00Z".into(),
                feeds: BTreeMap::from([
                    ("f-dp3-cta".into(), SourceId::from_sha1("abc123")),
                    (
                        "https://example.com/feed.zip".into(),
                        SourceId {
                            sha1: None,
                            etag: Some("\"32f4b0ef-6\"".into()),
                            last_modified: None,
                        },
                    ),
                ]),
                osm: Some(SourceId {
                    sha1: None,
                    etag: Some("\"deadbeef\"".into()),
                    last_modified: Some("Fri, 17 Jul 2026 08:26:43 GMT".into()),
                }),
            },
        );

        let dir = std::env::temp_dir().join("city-builder-metadata-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        m.save(&dir).unwrap();

        let back = Metadata::load(&dir);
        assert_eq!(back.version, SCHEMA_VERSION);
        let city = back.cities.get("chicago").unwrap();
        assert_eq!(city.feeds["f-dp3-cta"].sha1.as_deref(), Some("abc123"));
        assert_eq!(
            city.osm.as_ref().unwrap().etag.as_deref(),
            Some("\"deadbeef\"")
        );

        // Omitted fields must not round-trip as Some("").
        assert!(
            city.feeds["https://example.com/feed.zip"]
                .last_modified
                .is_none()
        );
    }

    #[test]
    fn a_future_schema_version_reads_as_absent() {
        let dir = std::env::temp_dir().join("city-builder-metadata-version-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            metadata_path(&dir),
            r#"{"version": 999, "cities": {"chicago": {"built_at": "2026-01-01T00:00:00Z", "feeds": {}}}}"#,
        )
        .unwrap();
        assert!(Metadata::load(&dir).cities.is_empty());
    }

    #[test]
    fn a_corrupt_file_reads_as_absent() {
        let dir = std::env::temp_dir().join("city-builder-metadata-corrupt-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(metadata_path(&dir), "{ not json").unwrap();
        assert!(Metadata::load(&dir).cities.is_empty());
    }

    #[test]
    fn age_is_measured_from_the_recorded_stamp() {
        let recent = CityMetadata {
            built_at: chrono::Utc::now().to_rfc3339(),
            feeds: BTreeMap::new(),
            osm: None,
        };
        assert_eq!(recent.age_days(), 0);

        let old = CityMetadata {
            built_at: (chrono::Utc::now() - chrono::Duration::days(20)).to_rfc3339(),
            feeds: BTreeMap::new(),
            osm: None,
        };
        assert_eq!(old.age_days(), 20);
        assert!(old.age().unwrap() > Duration::from_secs(19 * 86_400));

        let broken = CityMetadata {
            built_at: "not a timestamp".into(),
            feeds: BTreeMap::new(),
            osm: None,
        };
        assert!(broken.age().is_none());
    }
}
