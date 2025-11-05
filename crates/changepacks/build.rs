#[cfg(windows)]
use embed_manifest::{embed_manifest, new_manifest};

#[cfg(windows)]
fn main() {
    embed_manifest(new_manifest("Changepacks.Changepacks")).expect("unable to embed manifest file");
}

#[cfg(not(windows))]
fn main() {}
