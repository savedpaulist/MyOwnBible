use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn collect_json_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_json_files(&path, files);
        } else if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
            && path
                .components()
                .any(|component| component.as_os_str().to_string_lossy() == "themes")
        {
            files.push(path);
        }
    }
}

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let themes_dir = manifest_dir.join("../../themes ZED");
    println!("cargo:rerun-if-changed={}", themes_dir.display());

    let mut files = Vec::new();
    collect_json_files(&themes_dir, &mut files);
    files.sort();

    let mut generated = String::from("pub const EMBEDDED_ZED_THEMES: &[(&str, &str)] = &[\n");
    for path in files {
        let Ok(contents) = fs::read_to_string(&path) else {
            println!("cargo:warning=Cannot read Zed theme {}", path.display());
            continue;
        };
        let relative = path.strip_prefix(&themes_dir).unwrap_or(&path);
        generated.push_str(&format!(
            "    ({:?}, {:?}),\n",
            relative.to_string_lossy(),
            contents
        ));
    }
    generated.push_str("];\n");

    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("embedded_zed_themes.rs");
    fs::write(output, generated).expect("write embedded Zed theme catalogue");
}
