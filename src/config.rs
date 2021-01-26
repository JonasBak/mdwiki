use crate::utils::*;

use async_std::fs;
use async_std::path::{Path, PathBuf};
use async_std::prelude::*;

use serde::{Deserialize, Serialize};

use once_cell::sync::Lazy;

use figment::providers::{Env, Format, Toml};
use figment::value::{Dict, Map};
use figment::{Error, Figment, Metadata, Profile, Provider};

pub const MDWIKI_USER: Lazy<User> = Lazy::new(|| User {
    username: String::from("mdwiki"),
    password: "".into(),
});

#[derive(Debug)]
pub enum WikiTree {
    File(Box<Path>),
    Directory(Box<Path>, Vec<WikiTree>),
}

impl WikiTree {
    pub fn path(&self) -> &Path {
        match self {
            WikiTree::File(path) => &path,
            WikiTree::Directory(path, _) => &path,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct User {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub path: String,
    pub users: Vec<User>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            path: "./mdwiki".into(),
            users: Vec::new(),
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

    pub async fn can_edit(&self, path: &Path) -> bool {
        if !path_is_simple(path) {
            return false;
        } else if path.extension().map(|ext| ext != "md").unwrap_or(true) {
            return false;
        } else if is_reserved_name(path) {
            return false;
        }

        let full_path = Path::new(&self.path).join("src").join(&path);

        if !full_path.is_file().await {
            return false;
        }
        true
    }
    pub async fn can_create(&self, path: &Path) -> bool {
        if !path_is_simple(path) {
            return false;
        } else if path.extension().map(|ext| ext != "md").unwrap_or(true) {
            return false;
        } else if is_reserved_name(path) {
            return false;
        } else if path.ancestors().count() > 5 {
            return false;
        }

        let full_path = Path::new(&self.path).join("src").join(&path);

        if full_path.is_file().await {
            return false;
        }
        true
    }
    pub async fn get_wiki_tree(&self) -> WikiTree {
        use rocket::futures::future::{BoxFuture, FutureExt};
        fn visit(prefix: PathBuf, path: PathBuf) -> BoxFuture<'static, Option<WikiTree>> {
            async move {
                let relative_path = path.strip_prefix(&prefix).unwrap();
                if path.is_dir().await {
                    if relative_path.starts_with("images") {
                        return None;
                    }
                    let mut children = Vec::new();
                    let mut entries = fs::read_dir(&path).await.unwrap();
                    while let Some(entry) = entries.next().await {
                        if let Ok(entry) = entry {
                            if let Some(path) = visit(prefix.clone(), entry.path()).await {
                                children.push(path);
                            }
                        }
                    }

                    children.sort_by(|a, b| a.path().cmp(b.path()));
                    return Some(WikiTree::Directory(
                        relative_path.to_path_buf().into_boxed_path(),
                        children,
                    ));
                } else {
                    if path.extension().map(|ext| ext != "md").unwrap_or(true) {
                        return None;
                    } else if path.file_stem().map(|ext| ext == "README").unwrap_or(true) {
                        return None;
                    } else if is_reserved_name(relative_path) {
                        return None;
                    }
                    return Some(WikiTree::File(
                        relative_path.to_path_buf().into_boxed_path(),
                    ));
                }
            }
            .boxed()
        }
        let prefix = Path::new(&self.path).join("src");
        visit(
            prefix.to_path_buf(),
            Path::new(&self.path).join("src").to_path_buf(),
        )
        .await
        .unwrap()
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
