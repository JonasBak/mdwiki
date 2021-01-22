use serde::{Deserialize, Serialize};

use figment::providers::{Env, Format, Toml};
use figment::value::{Dict, Map};
use figment::{Error, Figment, Metadata, Profile, Provider};

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub path: String,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            path: "./mdwiki".into(),
        }
    }
}

impl Config {
    #[cfg(debug_assertions)]
    pub const DEFAULT_PROFILE: Profile = Profile::const_new("debug");
    #[cfg(not(debug_assertions))]
    pub const DEFAULT_PROFILE: Profile = Profile::const_new("release");

    pub fn figment() -> Figment {
        Figment::from(Config::default())
            .merge(Toml::file("mdwiki.toml").nested())
            .merge(Env::prefixed("MDWIKI_").global())
    }
}

impl Provider for Config {
    fn metadata(&self) -> Metadata {
        Metadata::named("mdwiki config")
    }

    fn data(&self) -> Result<Map<Profile, Dict>, Error> {
        figment::providers::Serialized::defaults(self).data()
    }

    fn profile(&self) -> Option<Profile> {
        Some(Self::DEFAULT_PROFILE)
    }
}
