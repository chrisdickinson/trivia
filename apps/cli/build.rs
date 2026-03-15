fn main() {
    let dist = std::path::Path::new("www/dist");
    if !dist.exists() {
        println!("cargo:warning=www/dist/ not found — the web UI will show a placeholder. Run: cd apps/cli/www && npm run build");
        std::fs::create_dir_all(dist).expect("failed to create www/dist placeholder");
        std::fs::write(
            dist.join("index.html"),
            "<html><body><p>Web UI not built. Run: <code>cd apps/cli/www &amp;&amp; npm run build</code></p></body></html>",
        )
        .expect("failed to write placeholder index.html");
    }
    println!("cargo:rerun-if-changed=www/dist");
}
