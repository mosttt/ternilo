#![forbid(unsafe_code)]

use std::{fs, path::PathBuf, sync::Arc};

use ternilo_kernel::{HostPolicy, compose_profiles};
use ternilo_local::LocalApplication;
use ternilo_protocol::{HarnessError, Profile, RunLimits};

mod node;
pub mod service;
pub mod web;

/// Compose the built-in local profile with zero or more JSON profile layers.
pub fn load_local_profile(paths: &[PathBuf]) -> Result<Profile, HarnessError> {
    let mut layers = vec![ternilo_local::local_profile()];
    for path in paths {
        let text = fs::read_to_string(path).map_err(|error| {
            HarnessError::invalid(format!("read profile {}: {error}", path.display()))
        })?;
        let layer = serde_json::from_str(&text).map_err(|error| {
            HarnessError::invalid(format!("parse profile {}: {error}", path.display()))
        })?;
        layers.push(layer);
    }
    Ok(compose_profiles(layers))
}

/// Open the shared local application used by CLI, browser, node and desktop adapters.
pub async fn open_local_application(
    profile: Profile,
    data_dir: PathBuf,
) -> Result<Arc<LocalApplication>, HarnessError> {
    open_local_application_with_limits(
        profile,
        data_dir,
        RunLimits {
            max_tool_calls: 0,
            ..RunLimits::default()
        },
    )
    .await
}

/// Open the shared local application with explicit per-turn run limits.
pub async fn open_local_application_with_limits(
    profile: Profile,
    data_dir: PathBuf,
    limits: RunLimits,
) -> Result<Arc<LocalApplication>, HarnessError> {
    open_local_application_with_options(
        profile,
        data_dir,
        limits,
        ternilo_local::LocalApplicationOpenOptions::default(),
    )
    .await
}

pub async fn open_local_application_with_options(
    profile: Profile,
    data_dir: PathBuf,
    limits: RunLimits,
    options: ternilo_local::LocalApplicationOpenOptions,
) -> Result<Arc<LocalApplication>, HarnessError> {
    Ok(Arc::new(
        LocalApplication::open_with_options(
            ternilo_local::catalog()?,
            profile,
            HostPolicy::local(limits),
            data_dir,
            options,
        )
        .await?,
    ))
}
