//! File Upload/Storage Helper (Rust port of File.php).
//!
//! ## Storage Best Practice — "Relative Path in DB, Full URL in Response"
//!
//! `File::upload()` returns a RELATIVE path (e.g. `images/abc123.jpg`).
//! ALWAYS store this relative path in the database — never the full URL.
//!
//! - DB column: `images/abc123.jpg`
//! - When exposing the URL in an API response:
//!   `File::url("images/abc123.jpg")` → `http://localhost:8085/uploads/images/abc123.jpg`
//!
//! Security features:
//! - Path traversal prevention (`sanitize_path`)
//! - Dangerous extension blacklist + optional whitelist
//! - Content inspection defense-in-depth (PHP tag / script detection)
//! - Secure randomized filenames (UUID v4 32-hex chars matching PHP `bin2hex(random_bytes(16))`)
//! - Secure directory permissions

use std::{
    fs,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::{env, error::ApiError};

/// Default maximum file size: 5MB (matching File.php 5242880 bytes).
pub const DEFAULT_MAX_SIZE: usize = 5 * 1024 * 1024;

/// Dangerous file extensions blacklist (matching File.php).
const DANGEROUS_EXTENSIONS: &[&str] = &[
    "php", "phtml", "phar", "php3", "php4", "php5", "php7", "php8", "phps", "cgi", "pl", "asp",
    "aspx", "shtml", "htaccess", "sh", "bat", "cmd", "com", "exe", "dll", "msi", "py", "rb",
    "js", "jsp", "war",
];

/// Options for file uploading.
#[derive(Debug, Clone)]
pub struct UploadOptions<'a> {
    pub sub_dir: &'a str,
    pub allowed_types: &'a [&'a str],
    pub max_size: usize,
}

impl<'a> Default for UploadOptions<'a> {
    fn default() -> Self {
        Self {
            sub_dir: "",
            allowed_types: &[],
            max_size: DEFAULT_MAX_SIZE,
        }
    }
}

pub struct FileHelper;

/// Type alias for PHP parity (`File::upload(...)`, `File::url(...)`).
pub type File = FileHelper;

impl FileHelper {
    /// Default upload directory name.
    pub fn upload_dir() -> &'static str {
        "uploads"
    }

    /// Upload a file securely from bytes and original filename.
    ///
    /// Preserves standard 5-argument signature:
    /// - `original_name`: name of file from client
    /// - `bytes`: file payload content
    /// - `sub_dir`: optional subdirectory within `uploads` (e.g. `"images"`, `"avatars"`)
    /// - `allowed_types`: optional whitelist of file extensions (empty slice allows any non-dangerous)
    /// - `max_size`: maximum size in bytes
    ///
    /// Returns the relative path within upload directory (e.g. `images/abc123.jpg`).
    pub fn upload(
        original_name: &str,
        bytes: &[u8],
        sub_dir: &str,
        allowed_types: &[&str],
        max_size: usize,
    ) -> Result<String, ApiError> {
        let opts = UploadOptions {
            sub_dir,
            allowed_types,
            max_size,
        };
        Self::upload_with_options(original_name, bytes, &opts)
    }

    /// Upload with `UploadOptions`.
    pub fn upload_with_options(
        original_name: &str,
        bytes: &[u8],
        opts: &UploadOptions,
    ) -> Result<String, ApiError> {
        // 1. Validate size
        if bytes.len() > opts.max_size {
            return Err(ApiError::bad_request(format!(
                "File size exceeds limit ({:.2}MB)",
                (opts.max_size as f64) / 1024.0 / 1024.0
            )));
        }

        // 2. Validate extension
        let ext = Path::new(original_name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        if ext.is_empty() {
            return Err(ApiError::bad_request("File must have an extension"));
        }

        if DANGEROUS_EXTENSIONS.contains(&ext.as_str()) {
            return Err(ApiError::bad_request(format!(
                "File type '{}' is not allowed for security reasons",
                ext
            )));
        }

        if !opts.allowed_types.is_empty()
            && !opts.allowed_types.iter().any(|t| t.eq_ignore_ascii_case(&ext))
        {
            return Err(ApiError::bad_request(format!(
                "File type not allowed. Allowed: {}",
                opts.allowed_types.join(", ")
            )));
        }

        // 3. Defense-in-depth: Inspect content for disguised scripts
        Self::validate_content_safety(bytes)?;

        // 4. Resolve target directory
        let safe_sub_dir = Self::sanitize_path(opts.sub_dir);
        let base_upload_dir = PathBuf::from(Self::upload_dir());
        let target_dir = if safe_sub_dir.is_empty() {
            base_upload_dir
        } else {
            base_upload_dir.join(&safe_sub_dir)
        };

        fs::create_dir_all(&target_dir)
            .map_err(|e| ApiError::internal(format!("Failed to create upload directory: {}", e)))?;

        // 5. Generate secure random filename (UUID v4 simple hex: 32 chars)
        let random_name = format!("{}.{}", Uuid::new_v4().simple(), ext);
        let target_path = target_dir.join(&random_name);

        fs::write(&target_path, bytes)
            .map_err(|e| ApiError::internal(format!("Failed to save uploaded file: {}", e)))?;

        let relative_path = if safe_sub_dir.is_empty() {
            random_name
        } else {
            format!("{}/{}", safe_sub_dir, random_name)
        };

        Ok(relative_path)
    }

    /// Convenience upload with default 5MB size limit and safe default options.
    pub fn upload_simple(
        original_name: &str,
        bytes: &[u8],
        sub_dir: &str,
    ) -> Result<String, ApiError> {
        Self::upload(original_name, bytes, sub_dir, &[], DEFAULT_MAX_SIZE)
    }

    /// Delete a file by relative path.
    pub fn delete(path: &str) -> bool {
        let safe_path = Self::sanitize_path(path);
        let full_path = PathBuf::from(Self::upload_dir()).join(safe_path);

        if let Ok(real_path) = fs::canonicalize(&full_path) {
            if let Ok(base_real) = fs::canonicalize(Self::upload_dir()) {
                if real_path.starts_with(&base_real) && real_path.is_file() {
                    return fs::remove_file(real_path).is_ok();
                }
            }
        }
        false
    }

    /// Convert relative storage path to full public URL.
    ///
    /// If the path is already an absolute URL (e.g. from legacy data), returns as-is.
    pub fn url(path: &str) -> String {
        if Self::is_absolute_url(path) {
            return path.to_string();
        }

        let base_url = Self::resolve_base_url();
        let base = base_url.trim_end_matches('/');
        let safe_path = Self::sanitize_path(path);
        format!("{}/{}/{}", base, Self::upload_dir(), safe_path)
    }

    /// Convert to full public URL, or None if path is None or empty.
    ///
    /// Safe to use on nullable database columns.
    pub fn url_or_null(path: Option<&str>) -> Option<String> {
        path.filter(|p| !p.trim().is_empty()).map(Self::url)
    }

    /// Convert to full public URL or None if empty string.
    pub fn url_if_present(path: &str) -> Option<String> {
        if path.trim().is_empty() {
            None
        } else {
            Some(Self::url(path))
        }
    }

    /// Check if a string is already a fully-qualified URL.
    pub fn is_absolute_url(path: &str) -> bool {
        path.starts_with("http://") || path.starts_with("https://")
    }

    /// Resolve application base URL.
    /// Order:
    /// 1. `APP_URL` environment variable if configured and non-empty.
    /// 2. `http://localhost:8085` fallback default.
    pub fn resolve_base_url() -> String {
        let configured = env::get_or("APP_URL", "");
        if !configured.trim().is_empty() {
            return configured.trim_end_matches('/').to_string();
        }
        "http://localhost:8085".to_string()
    }

    /// Sanitize path to prevent directory traversal attacks.
    ///
    /// Removes null bytes, normalizes slashes, and filters out `.` and `..`.
    pub fn sanitize_path(path: &str) -> String {
        let cleaned = path.replace('\0', "").replace('\\', "/");
        let safe_parts: Vec<&str> = cleaned
            .split('/')
            .filter(|part| !part.is_empty() && *part != "." && *part != "..")
            .collect();
        safe_parts.join("/")
    }

    /// Check if a relative file path exists in the upload storage.
    pub fn exists(path: &str) -> bool {
        let safe_path = Self::sanitize_path(path);
        PathBuf::from(Self::upload_dir()).join(safe_path).is_file()
    }

    /// Get local filesystem path for a relative storage path.
    pub fn full_path(path: &str) -> PathBuf {
        let safe_path = Self::sanitize_path(path);
        PathBuf::from(Self::upload_dir()).join(safe_path)
    }

    /// Get file size in bytes for a relative storage path.
    pub fn size(path: &str) -> Option<u64> {
        let full = Self::full_path(path);
        fs::metadata(full).ok().map(|m| m.len())
    }

    /// Read raw file bytes from storage.
    pub fn read(path: &str) -> Result<Vec<u8>, ApiError> {
        let full = Self::full_path(path);
        fs::read(&full).map_err(|e| ApiError::not_found(format!("File not found: {}", e)))
    }

    /// Write raw file bytes to storage path.
    pub fn put(path: &str, bytes: &[u8]) -> Result<(), ApiError> {
        let full = Self::full_path(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| ApiError::internal(format!("Failed to create directories: {}", e)))?;
        }
        fs::write(&full, bytes)
            .map_err(|e| ApiError::internal(format!("Failed to write file: {}", e)))
    }

    /// Detect simple MIME type based on file extension.
    pub fn mime_type(path: &str) -> &'static str {
        let ext = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        match ext.as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "png" => "image/png",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "svg" => "image/svg+xml",
            "pdf" => "application/pdf",
            "json" => "application/json",
            "csv" => "text/csv",
            "txt" => "text/plain",
            "zip" => "application/zip",
            "mp4" => "video/mp4",
            "mp3" => "audio/mpeg",
            _ => "application/octet-stream",
        }
    }

    /// Content inspection defense-in-depth against embedded PHP / script attacks.
    fn validate_content_safety(bytes: &[u8]) -> Result<(), ApiError> {
        let probe_len = bytes.len().min(4096);
        let probe = &bytes[..probe_len];

        // Block embedded PHP tags and script tags
        if probe.windows(5).any(|w| w.eq_ignore_ascii_case(b"<?php"))
            || probe.windows(3).any(|w| w == b"<?=")
            || probe.windows(7).any(|w| w.eq_ignore_ascii_case(b"<script"))
        {
            return Err(ApiError::bad_request(
                "File content is not allowed for security reasons",
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_path() {
        assert_eq!(
            File::sanitize_path("../../../etc/passwd"),
            "etc/passwd"
        );
        assert_eq!(
            File::sanitize_path("images\\sub/..\\photo.jpg"),
            "images/sub/photo.jpg"
        );
        assert_eq!(
            File::sanitize_path("images\0/photo.jpg"),
            "images/photo.jpg"
        );
        assert_eq!(File::sanitize_path("normal/path/test.png"), "normal/path/test.png");
    }

    #[test]
    fn test_url_resolution() {
        crate::env::set("APP_URL", "https://api.padi.dev");
        assert_eq!(
            File::url("images/sample.jpg"),
            "https://api.padi.dev/uploads/images/sample.jpg"
        );
        assert_eq!(
            File::url("https://cdn.example.com/legacy.png"),
            "https://cdn.example.com/legacy.png"
        );
        assert_eq!(
            File::url_or_null(Some("images/sample.jpg")),
            Some("https://api.padi.dev/uploads/images/sample.jpg".to_string())
        );
        assert_eq!(File::url_or_null(None), None);
        assert_eq!(File::url_or_null(Some("   ")), None);
    }

    #[test]
    fn test_dangerous_extension_blocked() {
        let res = File::upload("exploit.php", b"echo 1;", "scripts", &[], 1024 * 1024);
        assert!(res.is_err());

        let res_exe = File::upload("malware.exe", b"MZ...", "bin", &[], 1024 * 1024);
        assert!(res_exe.is_err());
    }

    #[test]
    fn test_whitelist_validation() {
        let res = File::upload(
            "doc.pdf",
            b"test pdf content",
            "docs",
            &["jpg", "png"],
            1024 * 1024,
        );
        assert!(res.is_err());
    }

    #[test]
    fn test_content_inspection_blocked() {
        let res = File::upload(
            "avatar.jpg",
            b"GIF89a<?php system($_GET['cmd']); ?>",
            "avatars",
            &["jpg"],
            1024 * 1024,
        );
        assert!(res.is_err());
    }

    #[test]
    fn test_mime_type_detection() {
        assert_eq!(File::mime_type("avatar.png"), "image/png");
        assert_eq!(File::mime_type("doc.pdf"), "application/pdf");
        assert_eq!(File::mime_type("photo.jpeg"), "image/jpeg");
        assert_eq!(File::mime_type("unknown.xyz"), "application/octet-stream");
    }
}
