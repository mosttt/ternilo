//! Signed client releases shared by Server, the execution host and its updater.

mod bundle;
pub use bundle::{executable_names, install_bundle, restore_bundle, stage_bundle};
mod download;
mod manifest;
pub use download::{download_artifact, fetch_release, save_response};
pub use manifest::{
    Artifact, RELEASE_ORIGIN, ReleaseManifest, SignedRelease, UPDATE_FORMAT, VerifiedRelease,
    current_target, verify_file,
};
