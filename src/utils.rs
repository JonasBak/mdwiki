use async_std::path::{Component, Path};

use rand::Rng;

#[macro_export]
macro_rules! try_response {
    ( $resp:expr ) => {{
        if !$resp.is_ok() {
            return $resp;
        }
        $resp
    }};
}

pub const RESERVED_NAMES: &[&str] = &["SUMMARY.md", "index.md"];
pub const RESERVED_PREFIXES: &[&str] = &["new", "edit", "upload", "images"];

pub fn log_warn<T: std::fmt::Display>(err: T) -> T {
    warn!("{}", err);
    err
}

pub fn is_reserved_name(path: &Path) -> bool {
    RESERVED_NAMES
        .iter()
        .find(|reserved| path.ends_with(reserved))
        .is_some()
        || RESERVED_PREFIXES
            .iter()
            .find(|reserved| path.starts_with(reserved))
            .is_some()
}

pub fn path_is_simple(path: &Path) -> bool {
    path.components()
        .find(|comp| match comp {
            Component::Normal(_) => false,
            _ => true,
        })
        .is_none()
}

pub fn rand_safe_string(length: usize) -> String {
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz";

    let mut rng = rand::thread_rng();

    (0..length)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}
