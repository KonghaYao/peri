use super::*;

#[test]
fn test_resolve_path_relative() {
    let directory = tempfile::tempdir().unwrap();
    let result = resolve_path(directory.path().to_str().unwrap(), "new.txt");
    assert_eq!(
        result,
        directory.path().canonicalize().unwrap().join("new.txt")
    );
}

#[test]
fn test_resolve_path_absolute() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("absolute.txt");
    std::fs::write(&file, "preserved content").unwrap();
    let result = resolve_path(directory.path().to_str().unwrap(), file.to_str().unwrap());
    assert_eq!(result, file.canonicalize().unwrap());
    assert_eq!(
        std::fs::read_to_string(result).unwrap(),
        "preserved content"
    );
}

#[test]
fn test_resolve_path_traversal_canonicalized() {
    let directory = tempfile::tempdir().unwrap();
    let child = directory.path().join("child");
    std::fs::create_dir(&child).unwrap();
    let result = resolve_path(child.to_str().unwrap(), "../new.txt");
    assert_eq!(
        result,
        directory.path().canonicalize().unwrap().join("new.txt")
    );
}
