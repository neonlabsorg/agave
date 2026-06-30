use std::process::Command;

fn main() {
    if let Ok(git_output) = Command::new("git").args(["rev-parse", "HEAD"]).output() {
        if git_output.status.success() {
            if let Ok(git_commit_hash) = String::from_utf8(git_output.stdout) {
                let trimmed_hash = git_commit_hash.trim().to_string();
                println!("cargo:rustc-env=AGAVE_GIT_COMMIT_HASH={trimmed_hash}");
            }
        }
    }

    // Re-run this build script (and therefore re-stamp the commit hash) whenever
    // the checked-out commit changes. `.git/HEAD` changes on branch switch and
    // its target ref file changes on new commits; `.git/packed-refs` covers the
    // packed case. Paths are relative to this crate's manifest directory.
    if let Ok(git_dir_output) = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
    {
        if git_dir_output.status.success() {
            if let Ok(git_dir) = String::from_utf8(git_dir_output.stdout) {
                let git_dir = git_dir.trim();
                println!("cargo:rerun-if-changed={git_dir}/HEAD");
                println!("cargo:rerun-if-changed={git_dir}/packed-refs");
                println!("cargo:rerun-if-changed={git_dir}/refs");
            }
        }
    }
}
