use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

const DEFAULT_WWW_URL: &str =
    "https://github.com/chrisdickinson/trivia/releases/latest/download/trivia-www.tar.gz";

fn main() {
    // Tracking dist/index.html ensures the crate is recompiled when dist/
    // changes (because `include_dir!` embeds it without telling cargo about
    // the dependency). Tracking source files ensures we rebuild dist/ when
    // the TypeScript/HTML/config inputs change.
    println!("cargo:rerun-if-changed=www/dist/index.html");
    println!("cargo:rerun-if-changed=www/src");
    println!("cargo:rerun-if-changed=www/index.html");
    println!("cargo:rerun-if-changed=www/package.json");
    println!("cargo:rerun-if-changed=www/package-lock.json");
    println!("cargo:rerun-if-changed=www/vite.config.ts");
    println!("cargo:rerun-if-changed=www/tsconfig.json");
    println!("cargo:rerun-if-changed=www/tsconfig.app.json");
    println!("cargo:rerun-if-changed=www/tsconfig.node.json");
    println!("cargo:rerun-if-env-changed=TRIVIA_WWW_URL");
    println!("cargo:rerun-if-env-changed=TRIVIA_WWW_SKIP");

    let dist_index = Path::new("www/dist/index.html");
    let src_dir = Path::new("www/src");
    let pkg = Path::new("www/package.json");

    // Prefer building from source whenever the source tree is present, npm is
    // available, and the source is newer than dist/. Without the staleness
    // check, `npm run build` would touch dist/index.html, which is itself
    // tracked, and cargo would loop into rebuilding every invocation.
    if src_dir.exists() && pkg.exists() && needs_npm_build() && npm_available() {
        if try_npm_build().is_ok() {
            return;
        }
        println!(
            "cargo:warning=npm build failed — falling back to existing dist/ or download"
        );
    }

    if dist_index.exists() {
        return;
    }

    if std::env::var("TRIVIA_WWW_SKIP").is_ok() {
        write_placeholder();
        return;
    }

    if try_download().is_err() {
        write_placeholder();
    }
}

/// Returns true if dist/ is missing or any tracked source input is newer than
/// dist/index.html.
fn needs_npm_build() -> bool {
    let dist_mtime = match mtime("www/dist/index.html") {
        Some(t) => t,
        None => return true,
    };

    let inputs = [
        "www/src",
        "www/index.html",
        "www/package.json",
        "www/package-lock.json",
        "www/vite.config.ts",
        "www/tsconfig.json",
        "www/tsconfig.app.json",
        "www/tsconfig.node.json",
    ];
    inputs
        .iter()
        .filter_map(|p| max_mtime_in(Path::new(p)))
        .any(|t| t > dist_mtime)
}

/// Latest modification time of `path`, recursing into directories.
fn max_mtime_in(path: &Path) -> Option<SystemTime> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.is_dir() {
        let mut latest = meta.modified().ok();
        for entry in std::fs::read_dir(path).ok()?.flatten() {
            if let Some(t) = max_mtime_in(&entry.path()) {
                latest = Some(latest.map_or(t, |cur| cur.max(t)));
            }
        }
        latest
    } else {
        meta.modified().ok()
    }
}

fn mtime(path: &str) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn npm_available() -> bool {
    Command::new("npm")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn try_npm_build() -> Result<(), ()> {
    let www_dir = Path::new("www");

    if !www_dir.join("node_modules").exists() {
        println!("cargo:warning=www/node_modules missing — running 'npm ci'...");
        let status = Command::new("npm")
            .args(["ci"])
            .current_dir(www_dir)
            .status()
            .map_err(|e| println!("cargo:warning=npm ci failed to start: {e}"))?;
        if !status.success() {
            return Err(());
        }
    }

    println!("cargo:warning=building web UI from source (npm run build)");
    let status = Command::new("npm")
        .args(["run", "build"])
        .current_dir(www_dir)
        .status()
        .map_err(|e| println!("cargo:warning=npm run build failed to start: {e}"))?;

    if !status.success() {
        return Err(());
    }

    if !Path::new("www/dist/index.html").exists() {
        println!("cargo:warning=npm build did not produce dist/index.html");
        return Err(());
    }

    Ok(())
}

fn try_download() -> Result<(), ()> {
    let url = std::env::var("TRIVIA_WWW_URL").unwrap_or_else(|_| DEFAULT_WWW_URL.to_string());
    let tarball = Path::new("www/trivia-www.tar.gz");

    println!("cargo:warning=www/dist/ not found — downloading pre-built web UI from {url}");

    let status = Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(tarball)
        .arg(&url)
        .status()
        .map_err(|e| {
            println!("cargo:warning=curl failed to start: {e}");
        })?;

    if !status.success() {
        let _ = std::fs::remove_file(tarball);
        println!("cargo:warning=download failed (HTTP error or network issue)");
        return Err(());
    }

    std::fs::create_dir_all("www/dist").map_err(|e| {
        println!("cargo:warning=failed to create www/dist: {e}");
    })?;

    let status = Command::new("tar")
        .args(["xzf"])
        .arg(tarball)
        .args(["-C", "www/dist"])
        .status()
        .map_err(|e| {
            println!("cargo:warning=tar failed to start: {e}");
        })?;

    let _ = std::fs::remove_file(tarball);

    if !status.success() {
        println!("cargo:warning=tar extraction failed");
        return Err(());
    }

    if !Path::new("www/dist/index.html").exists() {
        println!("cargo:warning=downloaded archive did not contain index.html");
        return Err(());
    }

    Ok(())
}

fn write_placeholder() {
    println!(
        "cargo:warning=www/dist/ not found — the web UI will show a placeholder. \
         Run: cd apps/cli/www && npm run build"
    );
    std::fs::create_dir_all("www/dist").expect("failed to create www/dist placeholder");
    std::fs::write(
        "www/dist/index.html",
        "<html><body><p>Web UI not built. \
         Run: <code>cd apps/cli/www &amp;&amp; npm run build</code></p></body></html>",
    )
    .expect("failed to write placeholder index.html");
}
