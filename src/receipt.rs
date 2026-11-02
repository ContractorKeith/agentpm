//! Guardian-owned, durable completion evidence, published only after worker ownership ends.
use serde::{Deserialize, Serialize};
use std::{io::Write, path::Path};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub token: String,
    pub snapshot_hash: String,
    pub complete: bool,
    pub elapsed_ms: u64,
    pub result: Result<String, String>,
}
impl Receipt {
    pub fn publish(&self, path: &Path) -> Result<(), String> {
        let temp = path.with_extension("pending");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec(self).map_err(|e| e.to_string())?)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        if path.exists() {
            return Err("guardian receipt already exists".into());
        }
        std::fs::rename(&temp, path).map_err(|e| e.to_string())?;
        std::fs::File::open(path.parent().ok_or("receipt parent missing")?)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    }
    pub fn read(path: &Path, token: &str, hash: &str) -> Result<Option<Self>, String> {
        use std::io::Read;
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let mut bytes = Vec::new();
        file.take(1_048_577)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 1_048_576 {
            return Err("guardian receipt exceeds limit".into());
        }
        let receipt: Self = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if receipt.token != token || receipt.snapshot_hash != hash {
            return Err("guardian receipt identity mismatch".into());
        }
        Ok(Some(receipt))
    }
}
