use std::fs;
use std::path::{Path, PathBuf};

fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

#[test]
fn controller_has_no_ocp_runtime_or_configuration_dependency() {
    let root = package_root();
    let manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let source = [
        "config.rs",
        "lib.rs",
        "main.rs",
        "planner.rs",
        "shadow.rs",
        "store.rs",
    ]
    .into_iter()
    .map(|name| {
        let source = fs::read_to_string(root.join("src").join(name)).unwrap();
        source
            .split("#[cfg(test)]")
            .next()
            .expect("source prefix")
            .to_string()
    })
    .collect::<Vec<_>>()
    .join("\n");

    assert!(!manifest.contains("openab-control-plane"));
    assert!(!source.contains("openab_control_plane"));
    assert!(!source.contains("crate::state::AppState"));
    assert!(!source.contains("OABCP_"));
    assert!(!source.contains("ControllerAction"));
    assert!(!source.contains("OABCP_CONTROLLER_ACTION"));
    assert!(!source.contains("GH_TOKEN"));
    assert!(!manifest.contains("reqwest"));
}

#[test]
fn ocp_default_build_does_not_select_the_controller() {
    let workspace = fs::read_to_string(package_root().join("../../Cargo.toml")).unwrap();
    let default_members = workspace
        .split("default-members = ")
        .nth(1)
        .and_then(|tail| tail.lines().next())
        .expect("workspace must declare explicit default members");
    assert!(default_members.contains("controller-protocol"));
    assert!(!default_members.contains("github-pr-controller"));
}

#[test]
fn independent_image_does_not_copy_ocp_sources() {
    let dockerfile =
        fs::read_to_string(package_root().join("../../Dockerfile.github-controller")).unwrap();
    assert!(dockerfile.contains("COPY crates/github-pr-controller"));
    assert!(!dockerfile
        .lines()
        .any(|line| line.trim() == "COPY src ./src"));
    assert!(!dockerfile.contains("openab-control-plane"));
}
