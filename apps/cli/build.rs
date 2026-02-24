fn main() {
    let dist = std::path::Path::new("www/dist");
    if !dist.exists() {
        println!("cargo:warning=www/dist/ not found — the web UI will show a placeholder. Run: cd apps/cli/www && npm run build");
    }
    println!("cargo:rerun-if-changed=www/dist");
}
