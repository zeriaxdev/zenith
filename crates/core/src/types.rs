//! Core domain types shared across crates.

use serde::Deserialize;

// ---- mod loaders --------------------------------------------------------
#[derive(Clone, Copy, PartialEq)]
pub enum Loader {
    Vanilla,
    Fabric,
    Quilt,
    Forge,
    NeoForge,
}

impl Loader {
    pub fn label(self) -> &'static str {
        match self {
            Loader::Vanilla => "Vanilla",
            Loader::Fabric => "Fabric",
            Loader::Quilt => "Quilt",
            Loader::Forge => "Forge",
            Loader::NeoForge => "NeoForge",
        }
    }
    pub fn all() -> [Loader; 5] {
        [
            Loader::Vanilla,
            Loader::Fabric,
            Loader::Quilt,
            Loader::Forge,
            Loader::NeoForge,
        ]
    }
}

// ---- account passed to the game -----------------------------------------
#[derive(Clone)]
pub struct Account {
    pub username: String,
    pub uuid: String,
    pub access_token: String,
    pub user_type: String, // "msa" for real accounts, "legacy"/"offline" otherwise
}

impl Account {
    /// An offline account, using the same name-based UUID scheme servers use.
    pub fn offline(username: &str) -> Self {
        let digest = md5::compute(format!("OfflinePlayer:{username}").as_bytes());
        let mut b = digest.0;
        b[6] = (b[6] & 0x0f) | 0x30; // version 3
        b[8] = (b[8] & 0x3f) | 0x80; // RFC 4122 variant
        let h = |r: std::ops::Range<usize>| {
            b[r].iter().map(|x| format!("{x:02x}")).collect::<String>()
        };
        let uuid = format!("{}-{}-{}-{}-{}", h(0..4), h(4..6), h(6..8), h(8..10), h(10..16));
        Account {
            username: username.to_string(),
            uuid,
            access_token: "0".to_string(),
            user_type: "legacy".to_string(),
        }
    }
}

// ---- version manifest entry ---------------------------------------------
#[derive(Deserialize, Clone)]
pub struct VersionEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
}

// ---- a successful Microsoft sign-in --------------------------------------
#[derive(Debug, Clone)]
pub struct Session {
    pub username: String,
    pub uuid: String,
    pub access_token: String,
}
