//! Offline answers (docs/offline-plan.md): `--offline-pack <folder>` makes a game play
//! without its servers. The folder (the game's recorded server data and mirrored files, with
//! a `pack.txt` saying which host is answered how; see the bridge's src/offline/mod.rs) goes
//! next to the bridge as one file, the game's Apache HTTP client becomes Game Bridge's
//! (dex.rs fix 7) and Game Bridge's HTTP classes are added as the next classesN.dex.
//! Packs hold the game maker's data, so they live outside this repository.

use std::path::Path;

/// Game Bridge's offline HTTP classes (offline/java, built by tools/build-offline-dex.sh).
pub const OFFLINE_DEX: &[u8] = include_bytes!("../assets/offline.dex");
/// The pack's file name next to the bridge (lib*.so, so Android unpacks it).
pub const PACK_FILE: &str = "libarm32_bridge_offline.so";
const MAGIC: &[u8; 8] = b"GBOFFLN1";

/// The folder as one pack: MAGIC, u32 file count, per file u32 name length, the name
/// (folder-relative, `/`-separated), u64 offset and u64 size from the pack's start; then the
/// files' bytes. Little-endian; names sorted. The bridge reads it (src/offline/mod.rs Pack).
pub fn pack_folder(folder: &Path) -> std::io::Result<Vec<u8>> {
    let mut files = Vec::new();
    collect(folder, folder, &mut files)?;
    files.sort();
    if !files.iter().any(|(name, _)| name == "pack.txt") {
        return Err(std::io::Error::other("the folder has no pack.txt"));
    }
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&(files.len() as u32).to_le_bytes());
    let mut offset = 12 + files.iter().map(|(name, _)| 4 + name.len() + 16).sum::<usize>();
    let mut data = Vec::new();
    for (name, path) in &files {
        let bytes = std::fs::read(path)?;
        out.extend_from_slice(&(name.len() as u32).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(offset as u64).to_le_bytes());
        out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        offset += bytes.len();
        data.extend(bytes);
    }
    out.extend(data);
    Ok(out)
}

fn collect(root: &Path, folder: &Path, files: &mut Vec<(String, std::path::PathBuf)>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(folder)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(root, &path, files)?;
        } else {
            let name = path.strip_prefix(root).unwrap().components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            files.push((name, path));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn folder_becomes_one_pack() {
        let folder = std::env::temp_dir().join(format!("gb-offline-pack-{}", std::process::id()));
        std::fs::create_dir_all(folder.join("cdn/h")).unwrap();
        std::fs::write(folder.join("pack.txt"), "h files cdn/h\n").unwrap();
        std::fs::write(folder.join("cdn/h/a.png"), "PNG").unwrap();
        let pack = super::pack_folder(&folder).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();
        // Index: "cdn/h/a.png" (11) then "pack.txt" (8); data starts after it.
        let data = 12 + (4 + 11 + 16) + (4 + 8 + 16);
        let mut expected = b"GBOFFLN1\x02\0\0\0".to_vec();
        for (name, offset, size) in [("cdn/h/a.png", data, 3), ("pack.txt", data + 3, 14)] {
            expected.extend_from_slice(&(name.len() as u32).to_le_bytes());
            expected.extend_from_slice(name.as_bytes());
            expected.extend_from_slice(&(offset as u64).to_le_bytes());
            expected.extend_from_slice(&(size as u64).to_le_bytes());
        }
        expected.extend_from_slice(b"PNGh files cdn/h\n");
        assert_eq!(pack, expected);
    }

    #[test]
    fn a_folder_needs_its_host_list() {
        let folder = std::env::temp_dir().join(format!("gb-offline-empty-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let result = super::pack_folder(&folder);
        std::fs::remove_dir_all(&folder).unwrap();
        assert!(result.is_err());
    }
}
