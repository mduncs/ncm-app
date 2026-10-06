//! Font resolution performs no terminal or GUI I/O.
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// The file selected for the thumbnail atlas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFont {
    pub family: String,
    pub path: PathBuf,
}

/// Resolve Ghostty's configured family, then SF Mono, then Menlo.
pub fn resolve_font() -> Result<ResolvedFont> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(home) = home {
        if let Ok(config) = std::fs::read_to_string(home.join(".config/ghostty/config")) {
            for family in configured_families(&config) {
                if let Some(path) = family_path(&family).filter(|p| p.is_file()) {
                    return Ok(ResolvedFont { family, path });
                }
            }
        }
    }
    for (family, path) in [
        ("SF Mono", "/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts/SF-Mono-Regular.otf"),
        ("Menlo", "/System/Library/Fonts/Menlo.ttc"),
    ] {
        if Path::new(path).is_file() {
            return Ok(ResolvedFont { family: family.into(), path: path.into() });
        }
    }
    if let Some(path) = family_path("Menlo").filter(|p| p.is_file()) {
        return Ok(ResolvedFont {
            family: "Menlo".into(),
            path,
        });
    }
    bail!("no Ghostty, SF Mono or Menlo font file resolved; use ThumbnailRenderer::with_font")
}

fn configured_families(config: &str) -> Vec<String> {
    config
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            if key.trim() != "font-family" {
                return None;
            }
            let value = value.trim();
            let value = if let Some(rest) = value.strip_prefix('"') {
                rest.split_once('"')?.0
            } else {
                value.split('#').next()?.trim()
            };
            (!value.is_empty()).then(|| value.to_owned())
        })
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn family_path(family: &str) -> Option<PathBuf> {
    let path = PathBuf::from(family);
    path.is_file().then_some(path)
}

#[cfg(target_os = "macos")]
fn family_path(family: &str) -> Option<PathBuf> {
    let direct = PathBuf::from(family);
    if direct.is_file() {
        return Some(direct);
    }
    let mut directories = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        directories.push(PathBuf::from(home).join("Library/Fonts"));
    }
    directories.extend(
        [
            "/Library/Fonts",
            "/System/Library/Fonts",
            "/System/Library/Fonts/Supplemental",
            "/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts",
        ]
        .map(PathBuf::from),
    );
    let mut candidates: Vec<_> = directories
        .into_iter()
        .filter_map(|directory| std::fs::read_dir(directory).ok())
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|extension| {
                matches!(
                    extension.to_string_lossy().to_ascii_lowercase().as_str(),
                    "ttf" | "otf" | "ttc"
                )
            })
        })
        .collect();
    let wanted = normalized_family(family);
    candidates.sort_by_key(|path| {
        (
            !normalized_family(&path.file_stem().unwrap_or_default().to_string_lossy())
                .starts_with(&wanted),
            path.clone(),
        )
    });
    find_family(family, &candidates)
}

#[cfg(any(target_os = "macos", test))]
fn normalized_family(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

// Read just the SFNT directory and bounded name table, not glyph data or CoreText caches.
// Face zero matches the renderer's collection index. TTC table offsets are file-relative.
#[cfg(any(target_os = "macos", test))]
fn font_names(path: &Path) -> Option<Vec<(u16, String)>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let mut header = [0u8; 12];
    file.read_exact(&mut header).ok()?;
    if &header[..4] == b"ttcf" {
        let mut offset = [0u8; 4];
        file.read_exact(&mut offset).ok()?;
        file.seek(SeekFrom::Start(u32::from_be_bytes(offset) as u64))
            .ok()?;
        file.read_exact(&mut header).ok()?;
    }
    let tables = u16::from_be_bytes([header[4], header[5]]);
    if tables > 256 {
        return None;
    }
    for _ in 0..tables {
        let mut table = [0u8; 16];
        file.read_exact(&mut table).ok()?;
        if &table[..4] != b"name" {
            continue;
        }
        let offset = u32::from_be_bytes(table[8..12].try_into().ok()?) as u64;
        let length = u32::from_be_bytes(table[12..16].try_into().ok()?) as usize;
        if length > 1024 * 1024 {
            return None;
        }
        file.seek(SeekFrom::Start(offset)).ok()?;
        let mut bytes = vec![0; length];
        file.read_exact(&mut bytes).ok()?;
        return Some(
            ttf_parser::name::Table::parse(&bytes)?
                .names
                .into_iter()
                .filter_map(|name| name.to_string().map(|text| (name.name_id, text)))
                .collect(),
        );
    }
    None
}

#[cfg(any(target_os = "macos", test))]
fn find_family(family: &str, candidates: &[PathBuf]) -> Option<PathBuf> {
    let wanted = normalized_family(family);
    let mut fallback = None;
    for path in candidates {
        let Some(names) = font_names(path) else {
            continue;
        };
        if !names
            .iter()
            .any(|(id, text)| matches!(*id, 1 | 16) && normalized_family(text) == wanted)
        {
            continue;
        }
        let regular = names.iter().any(|(id, text)| {
            matches!(*id, 2 | 17)
                && matches!(
                    text.to_lowercase().as_str(),
                    "regular" | "book" | "normal" | "roman"
                )
        });
        if regular {
            return Some(path.clone());
        }
        fallback.get_or_insert_with(|| path.clone());
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ghostty_family_config() {
        assert_eq!(configured_families("# font-family = fake\nfont-family = \"Google Sans Code\" # comment\nfont-family-bold = Other\nfont-family = Menlo # comment"), ["Google Sans Code", "Menlo"]);
    }
    #[test]
    fn name_tables_resolve_family_and_regular_face_independent_of_filename() {
        fn fixture(path: &Path, family: &str, style: &str) {
            let values = [(1u16, family), (2, style)];
            let mut strings = Vec::new();
            let mut name = Vec::new();
            name.extend_from_slice(&[0, 0, 0, 2, 0, 30]);
            for (id, value) in values {
                let encoded: Vec<_> = value.encode_utf16().flat_map(u16::to_be_bytes).collect();
                for n in [
                    3u16,
                    1,
                    0x409,
                    id,
                    encoded.len() as u16,
                    strings.len() as u16,
                ] {
                    name.extend_from_slice(&n.to_be_bytes());
                }
                strings.extend(encoded);
            }
            name.extend(strings);
            let mut font = vec![0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
            font.extend_from_slice(b"name");
            font.extend_from_slice(&0u32.to_be_bytes());
            font.extend_from_slice(&28u32.to_be_bytes());
            font.extend_from_slice(&(name.len() as u32).to_be_bytes());
            font.extend(name);
            std::fs::write(path, font).unwrap();
        }
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/font-fixtures")
            .join(std::process::id().to_string());
        std::fs::create_dir_all(&directory).unwrap();
        let bold = directory.join("wrong-file-name.ttf");
        let regular = directory.join("another-name.otf");
        let other = directory.join("WantedFamily.ttf");
        fixture(&bold, "Wanted Family", "Bold");
        fixture(&regular, "Wanted Family", "Regular");
        fixture(&other, "Different Family", "Regular");
        let candidates = [other, bold, regular.clone()];
        assert_eq!(find_family("Wanted Family", &candidates), Some(regular));
        assert!(find_family("Unknown Family", &candidates).is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
