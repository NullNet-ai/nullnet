use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=ui/src");
    println!("cargo:rerun-if-changed=ui/index.html");
    println!("cargo:rerun-if-changed=ui/package.json");
    println!("cargo:rerun-if-changed=ui/package-lock.json");

    let ui_dir = Path::new("ui");
    let node_modules = ui_dir.join("node_modules");
    // npm rewrites this to mirror package-lock.json on every successful
    // install; if package-lock.json is newer, node_modules is stale (e.g. a
    // dependency was added since the last install) and must be reinstalled —
    // not just "does node_modules exist at all".
    let installed_lock = node_modules.join(".package-lock.json");
    let needs_install =
        !node_modules.exists() || is_older(&installed_lock, &ui_dir.join("package-lock.json"));

    if needs_install {
        let status = Command::new("npm")
            .args(["install"])
            .current_dir(ui_dir)
            .status()
            .expect("failed to run npm install");
        assert!(status.success(), "npm install failed");
    }

    let status = Command::new("npm")
        .args(["run", "build"])
        .current_dir(ui_dir)
        .status()
        .expect("failed to run npm run build");
    assert!(status.success(), "npm run build failed");
}

/// `true` if `path` doesn't exist, or its mtime predates `reference`'s — in
/// either case treat it as stale rather than trusting it.
fn is_older(path: &Path, reference: &Path) -> bool {
    let (Ok(path_meta), Ok(ref_meta)) = (path.metadata(), reference.metadata()) else {
        return true;
    };
    let (Ok(path_mtime), Ok(ref_mtime)) = (path_meta.modified(), ref_meta.modified()) else {
        return true;
    };
    path_mtime < ref_mtime
}
