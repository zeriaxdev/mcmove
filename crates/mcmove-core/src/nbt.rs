//! NBT for the playerdata command.
//!
//! `level.dat` = gzipped NBT, root → `Data` → `Player`. A server's
//! `playerdata/<uuid>.dat` = gzipped NBT with the Player compound as the root
//! (empty root name). Working on `fastnbt::Value` keeps every tag intact —
//! including mod data attachments like `neoforge:attachments`.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fastnbt::Value;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::{Error, Result};

/// Read a (gzipped or raw) NBT file into a Value.
pub fn load(path: &Path) -> Result<Value> {
    let raw = fs::read(path)?;
    let bytes = if raw.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        GzDecoder::new(&raw[..]).read_to_end(&mut out)?;
        out
    } else {
        raw
    };
    fastnbt::from_bytes(&bytes)
        .map_err(|e| Error::Other(format!("{}: bad NBT: {e}", path.display())))
}

/// Extract the Player compound from a single-player level.dat.
pub fn extract_player(level_path: &Path) -> Result<Value> {
    let root = load(level_path)?;
    let player = root
        .as_compound()
        .and_then(|c| c.get("Data"))
        .and_then(|d| d.as_compound())
        .and_then(|d| d.get("Player"))
        .cloned();
    player.ok_or_else(|| {
        Error::Other(format!(
            "{}: no Data/Player tag — is this a single-player level.dat?",
            level_path.display()
        ))
    })
}

/// Write a Player compound as `<out_dir>/<uuid>.dat` (gzipped, empty root name).
pub fn write_playerdata(player: &Value, uuid: &str, out_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(out_dir)?;
    let path = out_dir.join(format!("{uuid}.dat"));
    let bytes = fastnbt::to_bytes(player).map_err(|e| Error::Other(format!("NBT encode: {e}")))?;
    let mut enc = GzEncoder::new(fs::File::create(&path)?, Compression::default());
    enc.write_all(&bytes)?;
    enc.finish()?;
    Ok(path)
}

trait AsCompound {
    fn as_compound(&self) -> Option<&std::collections::HashMap<String, Value>>;
}

impl AsCompound for Value {
    fn as_compound(&self) -> Option<&std::collections::HashMap<String, Value>> {
        match self {
            Value::Compound(map) => Some(map),
            _ => None,
        }
    }
}
