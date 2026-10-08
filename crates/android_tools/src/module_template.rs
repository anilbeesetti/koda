// Default module layout adapted from Android Studio's GradleAndroidModuleTemplate
// and AndroidModulePaths at a84efec3ba9542d9bfa1255103f0dc94833a3796.
// Copyright (C) 2017, 2021 The Android Open Source Project.
// Licensed under the Apache License, Version 2.0.

use std::{
    ffi::OsString,
    path::{MAIN_SEPARATOR, MAIN_SEPARATOR_STR, Path, PathBuf},
};

/// A default Android module layout, including paths for fields still being edited.
/// Construction only creates values; it does not validate or create files.
#[derive(Clone, Debug)]
pub struct DefaultModuleTemplate {
    module_root: PathBuf,
    manifest_directory: PathBuf,
    source_root: PathBuf,
    unit_test_root: PathBuf,
    test_root: PathBuf,
    aidl_root: PathBuf,
    resource_directories: [PathBuf; 1],
    ml_model_directories: [PathBuf; 1],
}

impl DefaultModuleTemplate {
    /// Preserves the supplied lexical root, including temporarily invalid values.
    /// The root is an existing path value, rather than a Java `File(String)` parser.
    pub fn at(module_root: impl Into<PathBuf>) -> Self {
        let module_root = module_root.into();
        let sources = append_child(&module_root, "src");
        let main = append_child(&sources, "main");
        Self {
            source_root: append_child(&main, "java"),
            unit_test_root: append_child(&sources, "test/java"),
            test_root: append_child(&sources, "androidTest/java"),
            aidl_root: append_child(&main, "aidl"),
            resource_directories: [append_child(&main, "res")],
            ml_model_directories: [append_child(&main, "ml")],
            manifest_directory: main,
            module_root,
        }
    }

    pub fn name(&self) -> &str {
        "main"
    }

    pub fn module_root(&self) -> &Path {
        &self.module_root
    }

    pub fn manifest_directory(&self) -> &Path {
        &self.manifest_directory
    }

    pub fn source_directory(&self, package_name: Option<&str>) -> PathBuf {
        append_package(&self.source_root, package_name)
    }

    pub fn test_directory(&self, package_name: Option<&str>) -> PathBuf {
        append_package(&self.test_root, package_name)
    }

    pub fn unit_test_directory(&self, package_name: Option<&str>) -> PathBuf {
        append_package(&self.unit_test_root, package_name)
    }

    pub fn aidl_directory(&self, package_name: Option<&str>) -> PathBuf {
        append_package(&self.aidl_root, package_name)
    }

    pub fn resource_directories(&self) -> &[PathBuf] {
        &self.resource_directories
    }

    pub fn ml_model_directories(&self) -> &[PathBuf] {
        &self.ml_model_directories
    }
}

fn append_package(root: &Path, package_name: Option<&str>) -> PathBuf {
    match package_name {
        Some(package_name) => append_child(root, &package_name.replace('.', MAIN_SEPARATOR_STR)),
        None => root.to_owned(),
    }
}

fn append_child(parent: &Path, child: &str) -> PathBuf {
    append_child_with_separator(parent, child, MAIN_SEPARATOR)
}

fn append_child_with_separator(parent: &Path, child: &str, separator: char) -> PathBuf {
    let child = normalize_child(child, separator);
    let mut path = if parent.as_os_str().is_empty() {
        separator.to_string().into()
    } else {
        parent.as_os_str().to_owned()
    };
    if child.is_empty() {
        return path.into();
    }

    // File(parent, child) keeps rooted children under the parent, unlike Path::join.
    let child = child.as_str();
    let directory_relative = {
        let bytes = path.as_encoded_bytes();
        separator == '\\'
            && bytes.len() == 2
            && bytes.first().is_some_and(u8::is_ascii_alphabetic)
            && bytes.get(1) == Some(&b':')
    };

    let child = if separator == '\\' {
        if child.starts_with("\\\\") {
            let child = child.trim_start_matches("\\\\");
            if child.is_empty() {
                if path.as_encoded_bytes().last() == Some(&b'\\') {
                    path = strip_trailing_separator(path, b'\\');
                }
                return path.into();
            }
            child
        } else if child.len() > 1 && !directory_relative {
            child.strip_prefix('\\').unwrap_or(child)
        } else {
            child
        }
    } else {
        child
    };

    if path.as_encoded_bytes().last() != Some(&(separator as u8))
        && !child.starts_with(separator)
        && !directory_relative
    {
        path.push(separator.to_string());
    }
    path.push(child);

    let bytes = path.as_encoded_bytes();
    if bytes.len() > 1
        && bytes.last() == Some(&(separator as u8))
        && (separator != '\\' || bytes.get(bytes.len() - 2) != Some(&b':'))
    {
        path = strip_trailing_separator(path, separator as u8);
    }
    path.into()
}

fn strip_trailing_separator(path: OsString, separator: u8) -> OsString {
    match path.as_encoded_bytes().split_last() {
        Some((last, prefix)) if separator.is_ascii() && *last == separator => {
            let prefix = prefix.to_vec();
            // SAFETY: these bytes came from an OsString on this platform; removing its
            // trailing ASCII separator preserves the platform encoding boundary.
            unsafe { OsString::from_encoded_bytes_unchecked(prefix) }
        }
        _ => path,
    }
}

fn normalize_child(child: &str, separator: char) -> String {
    let child = if separator == '\\' {
        normalize_windows_prefix(child)
    } else {
        child.to_owned()
    };

    let mut normalized = String::with_capacity(child.len());
    for character in child.chars() {
        if character != separator || !normalized.ends_with(separator) {
            normalized.push(character);
        } else if separator == '\\' && normalized == "\\" {
            normalized.push(character);
        }
    }
    while normalized.ends_with(separator)
        && normalized.len() > 1
        && !(separator == '\\'
            && (normalized == "\\\\"
                || (normalized.len() == 3 && normalized.as_bytes().get(1) == Some(&b':'))))
    {
        normalized.truncate(normalized.len() - 1);
    }
    normalized
}

fn normalize_windows_prefix(child: &str) -> String {
    let child = if let Some(child) = child.strip_prefix("\\\\?\\") {
        if let Some(child) = child.strip_prefix("UNC\\") {
            format!("\\\\{child}")
        } else if child == "UNC" {
            "\\\\".to_owned()
        } else {
            child.to_owned()
        }
    } else {
        child.to_owned()
    };
    let child = child.replace('/', "\\");
    let after_slashes = child.trim_start_matches('\\');
    let bytes = after_slashes.as_bytes();
    if bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.get(1) == Some(&b':') {
        after_slashes.to_owned()
    } else {
        child
    }
}

#[cfg(test)]
mod tests {
    use super::append_child_with_separator;
    use std::path::Path;

    #[test]
    fn windows_drive_relative_and_rooted_children_are_lexical() {
        for (parent, child, expected) in [
            ("C:", "src", "C:src"),
            ("C:", "\\src", "C:\\src"),
            ("C:\\", "src", "C:\\src"),
            ("C:\\module", "\\package", "C:\\module\\package"),
            ("C:\\module", "\\\\host\\share", "C:\\module\\host\\share"),
            ("", "src", "\\src"),
        ] {
            assert_eq!(
                append_child_with_separator(Path::new(parent), child, '\\').as_os_str(),
                Path::new(expected).as_os_str()
            );
        }
    }

    #[test]
    fn windows_child_normalization_preserves_roots_and_invalid_units() {
        for (child, expected) in [
            ("my//package/", "module\\my\\package"),
            ("//C:/package/", "module\\C:\\package"),
            ("\\\\?\\UNC\\host\\share", "module\\host\\share"),
            ("\\\\?\\C:\\package", "module\\C:\\package"),
            ("my\\\0", "module\\my\\\0"),
            ("", "module"),
        ] {
            assert_eq!(
                append_child_with_separator(Path::new("module"), child, '\\').as_os_str(),
                Path::new(expected).as_os_str()
            );
        }
    }

    #[test]
    fn windows_unicode_prefix_lengths_follow_utf16() {
        for (child, expected) in [
            ("😀:\\", "module\\😀:"),
            ("C:\\", "module\\C:\\"),
            ("é:\\", "module\\é:\\"),
            ("\0:\\", "module\\\0:\\"),
        ] {
            assert_eq!(
                append_child_with_separator(Path::new("module"), child, '\\').as_os_str(),
                Path::new(expected).as_os_str()
            );
        }
    }

    #[test]
    fn unix_backslashes_are_not_separators() {
        assert_eq!(
            append_child_with_separator(Path::new("module"), "my\\package", '/').as_os_str(),
            Path::new("module/my\\package").as_os_str()
        );
    }
}
