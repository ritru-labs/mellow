//! Product and maker names in one place, so renaming the product or
//! rewording its credit touches one file.

use std::env;

pub const PRODUCT: &str = "Mellow";
/// Shown with the maker's name wherever Mellow introduces itself.
pub const CREDIT: &str = "crafted by Ritru Labs";
/// The promise in one line: no modes to learn, nothing to memorise.
pub const TAGLINE: &str = "No modes, no manual: just start typing";

/// `mellow --help` summary and footer. `--version` stays `mellow <version>`
/// because release checks compare it exactly.
pub const ABOUT: &str = "The calm terminal editor. No modes, no manual: just start typing.";
pub const AFTER_HELP: &str = "Crafted by Ritru Labs · https://github.com/ritru-labs/mellow · F1 inside Mellow shows the everyday keys";

/// Folder under the config and state directories (`~/.config/mellow`).
pub const DIR_NAME: &str = "mellow";

/// Prefix of Mellow's environment variables (`MELLOW_SETTINGS`, ...).
pub const ENV_PREFIX: &str = "MELLOW_";

/// The value of `MELLOW_<name>`.
pub fn env_var_os(name: &str) -> Option<std::ffi::OsString> {
    env::var_os(format!("{ENV_PREFIX}{name}"))
}

/// The value of `MELLOW_<name>`.
pub fn env_var(name: &str) -> Result<String, env::VarError> {
    env::var(format!("{ENV_PREFIX}{name}"))
}
