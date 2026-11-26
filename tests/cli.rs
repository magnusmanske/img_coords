use std::io::Write;
use std::process::{Command, Stdio};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_img_coords")
}

fn run<I, S>(args: I) -> std::process::Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new(binary()).args(args).output().unwrap()
}

#[test]
fn version_reports_package_version() {
    let output = run(["--version"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
}

#[test]
fn help_lists_subcommands() {
    let output = run(["--help"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("scan"), "{text}");
    assert!(text.contains("import"), "{text}");
}

#[test]
fn scan_emits_valid_geojson() {
    let output = run(["scan", "--dir", "test_files", "-q"]);
    assert!(output.status.success(), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["features"].as_array().unwrap().len(), 1);
}

#[test]
fn invalid_format_is_rejected() {
    let output = run(["scan", "--dir", "test_files", "--format", "xml"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("possible values"), "{stderr}");
}

#[test]
fn import_deduplicates_stdin() {
    let mut child = Command::new(binary())
        .args(["import", "-q"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"test_files/sunrise.jpg\ntest_files/sunrise.jpg\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["features"].as_array().unwrap().len(), 1);
}

#[test]
fn output_extension_selects_format() {
    let path = std::env::temp_dir().join(format!("img_coords_cli_test_{}.csv", std::process::id()));
    let output = Command::new(binary())
        .args(["scan", "--dir", "test_files", "-q", "-o"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        text.starts_with("file,latitude,longitude,altitude,direction,timestamp"),
        "{text}"
    );
}
