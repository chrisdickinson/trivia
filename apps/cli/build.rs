use std::path::Path;
use std::process::Command;

const DEFAULT_WWW_URL: &str =
    "https://github.com/chrisdickinson/trivia/releases/latest/download/trivia-www.tar.gz";

fn main() {
    println!("cargo:rerun-if-changed=www/dist/index.html");
    println!("cargo:rerun-if-env-changed=TRIVIA_WWW_URL");
    println!("cargo:rerun-if-env-changed=TRIVIA_WWW_SKIP");

    let index = Path::new("www/dist/index.html");
    if index.exists() {
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

fn try_download() -> Result<(), ()> {
    let url = std::env::var("TRIVIA_WWW_URL").unwrap_or_else(|_| DEFAULT_WWW_URL.to_string());
    let tarball = Path::new("www/trivia-www.tar.gz");

    println!("cargo:warning=www/dist/ not found — downloading pre-built web UI from {url}");

    // Download
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

    // Extract
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

    // Verify
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
