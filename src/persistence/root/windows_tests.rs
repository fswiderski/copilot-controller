use super::*;

#[test]
fn synthetic_drive_paths_reject_unsafe_spellings() {
    for path in [
        r"C:relative",
        r"\rooted-without-drive",
        r"\\server\share",
        r"\\?\C:\extended-input",
        r"C:\one\..\two",
        r"C:\one\.\two",
        r"C:\one\\two",
        r"C:\one\name:stream",
        r"C:\one\NUL",
    ] {
        assert_eq!(
            absolute_paths(Path::new(path)).err().unwrap().kind(),
            ErrorKind::InvalidPath,
            "{path}"
        );
    }
    for path in [r"C:\", r"c:\Users\Private Root", "D:/some/root/"] {
        let parsed = absolute_paths(Path::new(path)).unwrap();
        assert!(!parsed.ancestors.is_empty());
        assert_eq!(*parsed.root.last().unwrap(), 0);
    }
}
