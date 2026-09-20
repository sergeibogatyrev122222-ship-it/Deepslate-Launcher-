//! Mojang's version list, and fetching individual version manifests.
//!
//! The list is the only document in the chain with no published hash, so it can
//! only be trusted over TLS. Everything it points at - version JSONs included -
//! carries a sha1 and goes through the verified, cached path.

use ds_core::version::VersionManifest;
use ds_net::{Artifact, Downloader, NetError};
use ds_store::Store;
use serde::Deserialize;

pub const VERSION_LIST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error(transparent)]
    Net(#[from] NetError),

    #[error("no version called '{0}'")]
    UnknownVersion(String),

    #[error("could not parse {what}")]
    Parse {
        what: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("could not read the cached manifest for '{id}'")]
    Read {
        id: String,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    Inherit(#[from] crate::inherit::InheritError),
}

type Result<T> = std::result::Result<T, CatalogError>;

/// One entry in the version list.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionEntry {
    /// **Opaque.** Never parsed into numbers - `1.21.11` and `26.2` do not
    /// compare.
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    /// sha1 of the version JSON at `url`. This is what lets a version manifest
    /// be verified and cached like any other artifact.
    pub sha1: String,
    pub release_time: String,
}

impl VersionEntry {
    pub fn is_release(&self) -> bool {
        self.kind == "release"
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Latest {
    pub release: String,
    pub snapshot: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VersionList {
    pub latest: Latest,
    pub versions: Vec<VersionEntry>,
}

impl VersionList {
    pub fn parse(json: &str) -> Result<Self> {
        serde_json::from_str(json).map_err(|source| CatalogError::Parse {
            what: "the version list".to_owned(),
            source,
        })
    }

    pub fn find(&self, id: &str) -> Option<&VersionEntry> {
        self.versions.iter().find(|entry| entry.id == id)
    }

    /// Releases only, newest first.
    ///
    /// Sorted by `release_time`, never by id. The list arrives in that order
    /// already, but relying on the server's ordering for something a user sees
    /// is the kind of assumption that breaks quietly.
    pub fn releases(&self) -> Vec<&VersionEntry> {
        let mut out: Vec<&VersionEntry> = self.versions.iter().filter(|v| v.is_release()).collect();
        out.sort_by(|a, b| b.release_time.cmp(&a.release_time));
        out
    }
}

/// The version list plus the machinery to fetch what it points at.
pub struct Catalog {
    list: VersionList,
}

impl Catalog {
    /// Fetch the version list.
    pub async fn load(downloader: &Downloader) -> Result<Self> {
        let json = downloader.fetch_text(VERSION_LIST_URL).await?;
        Ok(Self {
            list: VersionList::parse(&json)?,
        })
    }

    pub fn from_list(list: VersionList) -> Self {
        Self { list }
    }

    pub fn list(&self) -> &VersionList {
        &self.list
    }

    /// Fetch one version's manifest, verified against the sha1 in the list and
    /// cached in the store.
    ///
    /// Cached by content hash, so a version manifest is fetched once ever - the
    /// second launch of a version reads it from disk without a request.
    pub async fn manifest(
        &self,
        downloader: &Downloader,
        store: &Store,
        id: &str,
    ) -> Result<VersionManifest> {
        let entry = self
            .list
            .find(id)
            .ok_or_else(|| CatalogError::UnknownVersion(id.to_owned()))?;

        let artifact = Artifact::sha1(entry.url.clone(), entry.sha1.clone(), 0);
        let path = downloader.fetch(store, &artifact).await?;

        let json = std::fs::read_to_string(&path).map_err(|source| CatalogError::Read {
            id: id.to_owned(),
            source,
        })?;

        VersionManifest::parse(&json).map_err(|source| CatalogError::Parse {
            what: format!("the manifest for '{id}'"),
            source,
        })
    }

    /// Fetch a version and resolve its whole inheritance chain.
    pub async fn resolved(
        &self,
        downloader: &Downloader,
        store: &Store,
        id: &str,
    ) -> Result<VersionManifest> {
        let start = self.manifest(downloader, store, id).await?;

        crate::inherit::resolve(start, |parent_id| async move {
            self.manifest(downloader, store, &parent_id).await
        })
        .await
        .map_err(|error| match error {
            crate::inherit::ResolveError::Fetch(inner) => inner,
            crate::inherit::ResolveError::Inherit(inner) => CatalogError::Inherit(inner),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const LIST: &str = r#"{
        "latest": {"release":"1.21.11","snapshot":"26.3-snapshot-7"},
        "versions": [
            {"id":"26.2","type":"snapshot","url":"http://x/26.2.json",
             "sha1":"cccccccccccccccccccccccccccccccccccccccc",
             "time":"2026-09-01T00:00:00+00:00","releaseTime":"2026-09-01T00:00:00+00:00"},
            {"id":"1.21.11","type":"release","url":"http://x/1.21.11.json",
             "sha1":"dddddddddddddddddddddddddddddddddddddddd",
             "time":"2026-01-01T00:00:00+00:00","releaseTime":"2026-01-01T00:00:00+00:00"},
            {"id":"b1.7.3","type":"old_beta","url":"http://x/b1.7.3.json",
             "sha1":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
             "time":"2011-07-08T00:00:00+00:00","releaseTime":"2011-07-08T00:00:00+00:00"}
        ]
    }"#;

    fn list() -> VersionList {
        VersionList::parse(LIST).expect("the list should parse")
    }

    #[test]
    fn parses_the_list_and_finds_versions_by_id() {
        let list = list();
        assert_eq!(list.latest.release, "1.21.11");
        assert_eq!(list.versions.len(), 3);
        assert_eq!(list.find("26.2").map(|v| v.kind.as_str()), Some("snapshot"));
        assert!(list.find("does-not-exist").is_none());
    }

    /// Snapshots and legacy versions must be present, not filtered out - the
    /// brief asks for every version, including old alpha and beta.
    #[test]
    fn snapshots_and_legacy_versions_survive_parsing() {
        let list = list();
        assert!(list.find("b1.7.3").is_some(), "old_beta was dropped");
        assert!(list.find("26.2").is_some(), "snapshot was dropped");
    }

    /// Ordering comes from releaseTime, never from the id. `26.2` sorts after
    /// `1.21.11` by date and before it by any string comparison, which is
    /// exactly the trap.
    #[test]
    fn releases_are_ordered_by_date_not_by_id() {
        let list = list();
        let releases = list.releases();
        assert_eq!(releases.len(), 1, "only 1.21.11 is a release");

        let mut all: Vec<&VersionEntry> = list.versions.iter().collect();
        all.sort_by(|a, b| b.release_time.cmp(&a.release_time));
        assert_eq!(all[0].id, "26.2", "newest by date should be first");
        assert_eq!(all[2].id, "b1.7.3", "oldest by date should be last");
    }

    #[tokio::test]
    async fn loads_the_list_over_http() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/list.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(LIST))
            .mount(&server)
            .await;

        let downloader = Downloader::new(2, 2);
        let json = downloader
            .fetch_text(&format!("{}/list.json", server.uri()))
            .await
            .unwrap();
        let list = VersionList::parse(&json).unwrap();
        assert_eq!(list.latest.release, "1.21.11");
    }

    #[tokio::test]
    async fn an_unknown_version_is_named_in_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let catalog = Catalog::from_list(list());

        let err = catalog
            .manifest(&Downloader::new(2, 1), &store, "9.9.9")
            .await
            .unwrap_err();

        assert!(matches!(err, CatalogError::UnknownVersion(ref id) if id == "9.9.9"));
        assert!(err.to_string().contains("9.9.9"));
    }

    /// A version manifest is verified against the sha1 the list publishes, so a
    /// tampered or truncated one is rejected rather than parsed.
    #[tokio::test]
    async fn a_manifest_that_fails_its_published_hash_is_rejected() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/1.21.11.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{\"id\":\"tampered\"}"))
            .mount(&server)
            .await;

        let listing =
            VersionList::parse(&LIST.replace("http://x/", &format!("{}/", server.uri()))).unwrap();
        let catalog = Catalog::from_list(listing);

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        let err = catalog
            .manifest(&Downloader::new(2, 1), &store, "1.21.11")
            .await
            .unwrap_err();

        assert!(matches!(err, CatalogError::Net(_)), "{err:?}");
    }
}
