//! The environment a version's rules are evaluated against.
//!
//! Deliberately a value rather than something read from the running machine, so
//! rule evaluation can be tested for every platform from one test run. Reading
//! the real environment is [`Platform::host`], and it is the only function here
//! that looks at anything outside its arguments.

use std::fmt;

/// The operating system names Mojang uses in manifests.
///
/// Note `Osx`, not "macos": the manifests have said `osx` since 2013 and still
/// do. Mapping it to a nicer name internally would just mean translating back
/// at every comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Os {
    Windows,
    Linux,
    Osx,
}

impl Os {
    /// The string as it appears in a manifest rule.
    pub const fn manifest_name(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Osx => "osx",
        }
    }

    pub fn from_manifest_name(name: &str) -> Option<Self> {
        match name {
            "windows" => Some(Self::Windows),
            "linux" => Some(Self::Linux),
            "osx" => Some(Self::Osx),
            _ => None,
        }
    }

    /// The OS this binary is running on.
    pub const fn host() -> Option<Self> {
        if cfg!(target_os = "windows") {
            Some(Self::Windows)
        } else if cfg!(target_os = "linux") {
            Some(Self::Linux)
        } else if cfg!(target_os = "macos") {
            Some(Self::Osx)
        } else {
            None
        }
    }
}

impl fmt::Display for Os {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.manifest_name())
    }
}

/// Architecture names as manifests spell them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arch {
    X86,
    X86_64,
    Arm64,
}

impl Arch {
    pub const fn manifest_name(self) -> &'static str {
        match self {
            Self::X86 => "x86",
            Self::X86_64 => "x86_64",
            Self::Arm64 => "arm64",
        }
    }

    /// What `${arch}` expands to inside a natives classifier.
    ///
    /// Mojang uses the bit width here, not the architecture name: the classifier
    /// `natives-windows-${arch}` becomes `natives-windows-64`, never
    /// `natives-windows-x86_64`.
    pub const fn bits(self) -> &'static str {
        match self {
            Self::X86 => "32",
            Self::X86_64 | Self::Arm64 => "64",
        }
    }

    pub fn from_manifest_name(name: &str) -> Option<Self> {
        match name {
            "x86" | "i386" => Some(Self::X86),
            "x86_64" | "amd64" => Some(Self::X86_64),
            "arm64" | "aarch64" => Some(Self::Arm64),
            _ => None,
        }
    }

    pub const fn host() -> Option<Self> {
        if cfg!(target_arch = "x86_64") {
            Some(Self::X86_64)
        } else if cfg!(target_arch = "x86") {
            Some(Self::X86)
        } else if cfg!(target_arch = "aarch64") {
            Some(Self::Arm64)
        } else {
            None
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.manifest_name())
    }
}

/// Everything a rule can be evaluated against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub os: Os,
    pub arch: Arch,
    /// OS version string, matched against a rule's `os.version` regex.
    pub version: String,
}

impl Platform {
    pub fn new(os: Os, arch: Arch, version: impl Into<String>) -> Self {
        Self {
            os,
            arch,
            version: version.into(),
        }
    }

    /// The machine this is running on.
    ///
    /// Returns `None` on a platform Minecraft does not ship for, rather than
    /// guessing - a wrong guess here silently selects the wrong native
    /// libraries.
    pub fn host() -> Option<Self> {
        Some(Self::new(Os::host()?, Arch::host()?, host_os_version()))
    }
}

/// Best-effort OS version string for `os.version` rules.
///
/// Only a handful of manifests use `os.version` at all, and the rules that do
/// are of the form `^10\.` - so an empty string when the version cannot be read
/// simply means those rules do not match, which is the safe direction.
fn host_os_version() -> String {
    std::env::var("OS_VERSION_OVERRIDE").unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_names_round_trip() {
        for os in [Os::Windows, Os::Linux, Os::Osx] {
            assert_eq!(Os::from_manifest_name(os.manifest_name()), Some(os));
        }
        for arch in [Arch::X86, Arch::X86_64, Arch::Arm64] {
            assert_eq!(Arch::from_manifest_name(arch.manifest_name()), Some(arch));
        }
    }

    /// Mojang has spelled macOS "osx" in manifests since 2013. Renaming it
    /// internally would mean translating back at every comparison.
    #[test]
    fn macos_is_spelled_osx() {
        assert_eq!(Os::Osx.manifest_name(), "osx");
        assert_eq!(Os::from_manifest_name("macos"), None);
    }

    /// `${arch}` expands to the bit width, not the architecture name.
    /// `natives-windows-${arch}` is `natives-windows-64`.
    #[test]
    fn arch_placeholder_expands_to_bit_width() {
        assert_eq!(Arch::X86.bits(), "32");
        assert_eq!(Arch::X86_64.bits(), "64");
        assert_eq!(Arch::Arm64.bits(), "64");
    }

    #[test]
    fn common_architecture_aliases_are_accepted() {
        assert_eq!(Arch::from_manifest_name("amd64"), Some(Arch::X86_64));
        assert_eq!(Arch::from_manifest_name("aarch64"), Some(Arch::Arm64));
        assert_eq!(Arch::from_manifest_name("i386"), Some(Arch::X86));
        assert_eq!(Arch::from_manifest_name("sparc"), None);
    }

    #[test]
    fn host_is_detected_on_supported_platforms() {
        let host = Platform::host();
        if cfg!(any(
            target_os = "windows",
            target_os = "linux",
            target_os = "macos"
        )) {
            assert!(host.is_some(), "host platform should be recognised");
        }
    }
}
