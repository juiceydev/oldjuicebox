//! File-type validation with tiered extension and signature denylists.

use std::path::Path;

/// Protection level parsed from the `DANGER_LEVEL` env var.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProtectionLevel {
    None = 0,
    Low = 1,
    Medium = 2,
    High = 3,
}

impl ProtectionLevel {
    /// Parse from a string like "none", "low", "medium", "high".
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "none" => Self::None,
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            _ => Self::High,
        }
    }

    /// Whether the given danger tier should be blocked at this protection level.
    pub fn blocks(self, tier: DangerTier) -> bool {
        self >= tier.level()
    }

    /// Return the string representation for API responses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Danger tier of a file type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DangerTier {
    Low,
    Medium,
    High,
}

impl DangerTier {
    /// The minimum protection level needed to block this tier.
    pub fn level(self) -> ProtectionLevel {
        match self {
            DangerTier::Low => ProtectionLevel::Low,
            DangerTier::Medium => ProtectionLevel::Medium,
            DangerTier::High => ProtectionLevel::High,
        }
    }
}

/// Low-tier dangerous extensions: Windows executables, installers, DLLs,
/// system files, disk images.
const LOW_TIER_EXTENSIONS: &[&str] = &[
    // Windows executables and installers
    "exe",
    "msi",
    "msp",
    "mst",
    "pif",
    "scr",
    "com",
    "cpl",
    "hta",
    "application",
    "gadget",
    // DLLs, libraries, system files
    "dll",
    "so",
    "dylib",
    "ko",
    "sys",
    "drv",
    // Disk images (can contain executable payloads)
    "iso",
    "img",
    "vhd",
    "vmdk",
    "vdi",
    // Other binary formats
    "pyc",
    "pyo",
    "class",
    "jar",
];

/// Medium-tier dangerous extensions: scripts, batch files, scripting languages.
const MEDIUM_TIER_EXTENSIONS: &[&str] = &[
    // Windows scripting
    "bat", "cmd", "inf", "jse", "lnk", "vbs", "vbe", "wsf", "wsh", "ws", "reg", "rgs", "sct", "shb",
    "shs", "ps1", "psm1", "psd1", "psc1", "psc2", "ps1xml", "psc1xml", // Shell scripts
    "sh", "bash", "csh", "ksh", "zsh", "fish", // macOS
    "app", "command", "terminal", // Other
    "url", "website", "xnk", "xbap",
];

/// High-tier dangerous extensions: JavaScript, HTML/SVG, PHP, Python, etc.
/// These are potential XSS/RFI vectors if served directly.
const HIGH_TIER_EXTENSIONS: &[&str] = &[
    // JavaScript variants
    "js", "mjs", "cjs", "jsx", "ts", "tsx", // HTML and variants (XSS risk)
    "html", "htm", "xhtml", "xht", "shtml", "svg", // PHP
    "php", "php3", "php4", "php5", "phtml", "phar", // Python
    "py", "pyw", "pyi", // Ruby
    "rb", "erb", "rake", // Perl
    "pl", "pm", "cgi", // ASP
    "asp", "aspx", "ascx", "ashx", "asmx", // ColdFusion
    "cfm", "cfc", // Other scripting
    "lua", "tcl", "groovy", "gradle", "jsp", "jspx", "wss",
];

/// Magic byte signatures mapped to danger tiers.
const DANGEROUS_MAGIC: &[(&[u8], &str, DangerTier)] = &[
    // PE executable (Windows) -- Low tier
    (&[0x4D, 0x5A], "PE executable", DangerTier::Low),
    // ELF executable (Linux) -- Low tier
    (&[0x7F, 0x45, 0x4C, 0x46], "ELF executable", DangerTier::Low),
    // Mach-O executable (macOS) -- Low tier
    (
        &[0xFE, 0xED, 0xFA, 0xCE],
        "Mach-O executable",
        DangerTier::Low,
    ),
    (
        &[0xFE, 0xED, 0xFA, 0xCF],
        "Mach-O 64-bit executable",
        DangerTier::Low,
    ),
    (
        &[0xCE, 0xFA, 0xED, 0xFE],
        "Mach-O executable",
        DangerTier::Low,
    ),
    (
        &[0xCF, 0xFA, 0xED, 0xFE],
        "Mach-O 64-bit executable",
        DangerTier::Low,
    ),
    // Java class file -- Low tier
    (
        &[0xCA, 0xFE, 0xBA, 0xBE],
        "Java class file",
        DangerTier::Low,
    ),
    // Batch file (@echo off) -- Medium tier
    (
        &[0x40, 0x65, 0x63, 0x68, 0x6F],
        "batch script",
        DangerTier::Medium,
    ),
    // Shebang line (#!/...) -- Medium tier
    (&[0x23, 0x21], "shell script", DangerTier::Medium),
    // JavaScript (common patterns) -- High tier
    (
        &[0x66, 0x75, 0x6E, 0x63, 0x74, 0x69, 0x6F, 0x6E],
        "JavaScript",
        DangerTier::High,
    ),
    // HTML DOCTYPE -- High tier
    (
        &[0x3C, 0x21, 0x44, 0x4F, 0x43, 0x54, 0x59, 0x50, 0x45],
        "HTML document",
        DangerTier::High,
    ),
    // HTML <html -- High tier
    (
        &[0x3C, 0x68, 0x74, 0x6D, 0x6C],
        "HTML document",
        DangerTier::High,
    ),
    // SVG -- High tier
    (
        &[0x3C, 0x3F, 0x78, 0x6D, 0x6C],
        "XML/SVG document",
        DangerTier::High,
    ),
    // PHP -- High tier
    (
        &[0x3C, 0x3F, 0x70, 0x68, 0x70],
        "PHP script",
        DangerTier::High,
    ),
];

/// Result of file type validation.
#[derive(Debug)]
pub enum FileValidation {
    /// File is safe to accept.
    Allowed,
    /// File was rejected because of a dangerous extension.
    BlockedExtension { ext: String, tier: DangerTier },
    /// File was rejected because its magic bytes indicate a dangerous type.
    BlockedMagic {
        description: String,
        tier: DangerTier,
    },
    /// File was rejected because it's empty.
    Empty,
}

pub fn tier_name(tier: DangerTier) -> &'static str {
    match tier {
        DangerTier::Low => "low",
        DangerTier::Medium => "medium",
        DangerTier::High => "high",
    }
}

/// User-friendly message for why a file type was blocked.
pub fn friendly_block_reason(tier: DangerTier) -> String {
    match tier {
        DangerTier::Low => "Executable files, installers, and disk images are not allowed".into(),
        DangerTier::Medium => "Script files (batch, shell, PowerShell) are not allowed".into(),
        DangerTier::High => "HTML, JavaScript, and scripting files are not allowed".into(),
    }
}

/// Validate a filename against the configured extension policy.
pub fn validate_filename(filename: &str, level: ProtectionLevel) -> FileValidation {
    if level == ProtectionLevel::None {
        return FileValidation::Allowed;
    }

    if let Some(ext) = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
    {
        if level.blocks(DangerTier::Low) && LOW_TIER_EXTENSIONS.contains(&ext.as_str()) {
            return FileValidation::BlockedExtension {
                ext,
                tier: DangerTier::Low,
            };
        }
        if level.blocks(DangerTier::Medium) && MEDIUM_TIER_EXTENSIONS.contains(&ext.as_str()) {
            return FileValidation::BlockedExtension {
                ext,
                tier: DangerTier::Medium,
            };
        }
        if level.blocks(DangerTier::High) && HIGH_TIER_EXTENSIONS.contains(&ext.as_str()) {
            return FileValidation::BlockedExtension {
                ext,
                tier: DangerTier::High,
            };
        }
    }

    FileValidation::Allowed
}

/// Validate a filename and the available leading file bytes.
pub fn validate_file(filename: &str, data: &[u8], level: ProtectionLevel) -> FileValidation {
    let filename_result = validate_filename(filename, level);
    if !matches!(filename_result, FileValidation::Allowed) {
        return filename_result;
    }

    if level == ProtectionLevel::None {
        return FileValidation::Allowed;
    }

    if data.is_empty() {
        return FileValidation::Empty;
    }

    // Check leading signatures against danger tiers.
    for (magic, desc, tier) in DANGEROUS_MAGIC {
        if level.blocks(*tier) && data.len() >= magic.len() && &data[..magic.len()] == *magic {
            return FileValidation::BlockedMagic {
                description: desc.to_string(),
                tier: *tier,
            };
        }
    }

    FileValidation::Allowed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_allows_everything() {
        assert!(matches!(
            validate_file("malware.exe", b"MZ\x90\x00", ProtectionLevel::None),
            FileValidation::Allowed
        ));
    }

    #[test]
    fn low_blocks_exe() {
        assert!(matches!(
            validate_file("app.exe", b"MZ\x90\x00", ProtectionLevel::Low),
            FileValidation::BlockedExtension {
                tier: DangerTier::Low,
                ..
            }
        ));
    }

    #[test]
    fn filename_only_validation_blocks_dangerous_extensions() {
        assert!(matches!(
            validate_filename("app.exe", ProtectionLevel::Low),
            FileValidation::BlockedExtension {
                tier: DangerTier::Low,
                ..
            }
        ));
    }

    #[test]
    fn low_allows_sh() {
        assert!(matches!(
            validate_file("script.sh", b"#!/bin/bash", ProtectionLevel::Low),
            FileValidation::Allowed
        ));
    }

    #[test]
    fn medium_blocks_sh() {
        assert!(matches!(
            validate_file("script.sh", b"#!/bin/bash", ProtectionLevel::Medium),
            FileValidation::BlockedExtension {
                tier: DangerTier::Medium,
                ..
            }
        ));
    }

    #[test]
    fn medium_blocks_bat() {
        assert!(matches!(
            validate_file("autoexec.bat", b"@echo off", ProtectionLevel::Medium),
            FileValidation::BlockedExtension {
                tier: DangerTier::Medium,
                ..
            }
        ));
    }

    #[test]
    fn medium_allows_js() {
        assert!(matches!(
            validate_file("app.js", b"console.log('hi')", ProtectionLevel::Medium),
            FileValidation::Allowed
        ));
    }

    #[test]
    fn high_blocks_js() {
        assert!(matches!(
            validate_file("app.js", b"function() {", ProtectionLevel::High),
            FileValidation::BlockedExtension {
                tier: DangerTier::High,
                ..
            }
        ));
    }

    #[test]
    fn high_blocks_html() {
        assert!(matches!(
            validate_file("page.html", b"<!DOCTYPE html>", ProtectionLevel::High),
            FileValidation::BlockedExtension {
                tier: DangerTier::High,
                ..
            }
        ));
    }

    #[test]
    fn high_blocks_svg() {
        assert!(matches!(
            validate_file("image.svg", b"<svg xmlns", ProtectionLevel::High),
            FileValidation::BlockedExtension {
                tier: DangerTier::High,
                ..
            }
        ));
    }

    #[test]
    fn blocks_pe_magic_at_low() {
        assert!(matches!(
            validate_file("renamed.txt", b"MZ\x90\x00\x03", ProtectionLevel::Low),
            FileValidation::BlockedMagic {
                tier: DangerTier::Low,
                ..
            }
        ));
    }

    #[test]
    fn blocks_elf_magic_at_low() {
        assert!(matches!(
            validate_file("renamed.txt", b"\x7FELF", ProtectionLevel::Low),
            FileValidation::BlockedMagic {
                tier: DangerTier::Low,
                ..
            }
        ));
    }

    #[test]
    fn blocks_shebang_magic_at_medium() {
        assert!(matches!(
            validate_file("renamed.txt", b"#!/bin/bash", ProtectionLevel::Medium),
            FileValidation::BlockedMagic {
                tier: DangerTier::Medium,
                ..
            }
        ));
    }

    #[test]
    fn shebang_allowed_at_low() {
        assert!(matches!(
            validate_file("renamed.txt", b"#!/bin/bash", ProtectionLevel::Low),
            FileValidation::Allowed
        ));
    }

    #[test]
    fn allows_safe_file() {
        assert!(matches!(
            validate_file("photo.jpg", b"\xFF\xD8\xFF", ProtectionLevel::High),
            FileValidation::Allowed
        ));
    }

    #[test]
    fn rejects_empty_file() {
        assert!(matches!(
            validate_file("empty.txt", b"", ProtectionLevel::Low),
            FileValidation::Empty
        ));
    }

    #[test]
    fn tier_blocks_include_lower() {
        // Medium should also block Low-tier files
        assert!(matches!(
            validate_file("app.exe", b"MZ", ProtectionLevel::Medium),
            FileValidation::BlockedExtension {
                tier: DangerTier::Low,
                ..
            }
        ));
        // High should also block Low and Medium-tier files
        assert!(matches!(
            validate_file("script.sh", b"#!/bin/bash", ProtectionLevel::High),
            FileValidation::BlockedExtension {
                tier: DangerTier::Medium,
                ..
            }
        ));
        assert!(matches!(
            validate_file("app.exe", b"MZ", ProtectionLevel::High),
            FileValidation::BlockedExtension {
                tier: DangerTier::Low,
                ..
            }
        ));
    }
}
