use std::{env, fs, io, path::PathBuf};

fn main() -> io::Result<()> {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../strategies");
    let mut files = Vec::new();
    for folder in [&directory, &directory.join("costs")] {
        println!("cargo:rerun-if-changed={}", folder.display());
        for entry in fs::read_dir(folder)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_file()
                && matches!(
                    path.extension().and_then(|ext| ext.to_str()),
                    Some("md" | "py" | "json")
                )
            {
                files.push(path);
            }
        }
    }
    files.sort();
    let mut source = String::from("&[\n");
    for path in files {
        let path = path.canonicalize()?;
        let relative = path
            .strip_prefix(directory.canonicalize()?)
            .expect("strategy directory");
        source.push_str(&format!("    ({relative:?}, include_bytes!({path:?})),\n"));
    }
    source.push_str("]\n");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("reference_strategies.rs"),
        source,
    )
}
