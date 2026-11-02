//! Immutable, explicitly collected context; no source discovery or dispatch authority.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const LIMIT: usize = 1024 * 1024;
const PACKAGE_LIMIT: usize = 16 * LIMIT;
const MAX_FILES: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub path: PathBuf,
    /// Caller-supplied source revision (for example an issue snapshot receipt).
    pub source_revision: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Role {
    pub backend: String,
    pub model: String,
    pub skills: Vec<Input>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    /// Absolute collection roots, including explicit existing user skill roots.
    pub roots: Vec<PathBuf>,
    pub approved: Input,
    pub brief: Input,
    pub instructions: Vec<Input>,
    pub dependencies: Vec<Input>,
    pub roles: BTreeMap<String, Role>,
    /// Whole-role replacements; there is no implicit provider fallback.
    pub overrides: BTreeMap<String, Role>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub kind: String,
    pub path: PathBuf,
    pub source_revision: String,
    pub content_id: String,
    pub content: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub version: u32,
    pub previous: Option<String>,
    pub manifest: Manifest,
    pub artifacts: Vec<Artifact>,
    pub selected_roles: BTreeMap<String, Role>,
}

fn identity(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn credential(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy().to_lowercase();
        s == ".env"
            || s.starts_with(".env.")
            || matches!(
                s.as_str(),
                ".git"
                    | "gcloud"
                    | "keychains"
                    | ".ssh"
                    | ".aws"
                    | ".azure"
                    | ".gnupg"
                    | "credentials"
                    | "credentials.json"
                    | ".credentials.json"
                    | ".netrc"
                    | ".npmrc"
                    | "auth.json"
                    | "hosts.yml"
                    | "id_rsa"
                    | "id_ed25519"
            )
            || s.ends_with(".pem")
            || s.ends_with(".key")
    })
}
fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    // Opening a FIFO can block before descriptor metadata is available.
    if !fs::metadata(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .is_file()
    {
        return Err("input must be a regular file".into());
    }
    let file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("input must be a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("context collection exceeds byte limit".into());
    }
    Ok(bytes)
}

/// Parse a bounded manifest. Relative input paths are rejected, independent of cwd.
pub fn read_manifest(path: &Path) -> Result<Manifest, String> {
    serde_json::from_slice(&bounded_read(path, LIMIT)?).map_err(|e| e.to_string())
}

/// Read-only snapshot construction; all required inputs validate before storage.
pub fn collect(manifest: Manifest, previous: Option<String>) -> Result<Package, String> {
    // Library callers must obey the same metadata bound as manifest-file callers.
    if serde_json::to_vec(&manifest)
        .map_err(|e| e.to_string())?
        .len()
        > LIMIT
    {
        return Err("context manifest exceeds byte limit".into());
    }
    if manifest.version != 1 || manifest.roots.is_empty() {
        return Err("unsupported manifest version or missing collection roots".into());
    }
    let roots = manifest
        .roots
        .iter()
        .map(|p| {
            if !p.is_absolute() || credential(p) {
                return Err("invalid collection root".to_string());
            }
            let root = p.canonicalize().map_err(|e| e.to_string())?;
            if !root.is_dir() || root.parent().is_none() || credential(&root) {
                return Err("invalid collection root".into());
            }
            Ok(root)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut roles = manifest.roles.clone();
    for (name, role) in &manifest.overrides {
        if !roles.contains_key(name) {
            return Err(format!("override for unknown role: {name}"));
        }
        roles.insert(name.clone(), role.clone());
    }
    let mut inputs = vec![
        ("approved".to_string(), &manifest.approved),
        ("brief".to_string(), &manifest.brief),
    ];
    inputs.extend(
        manifest
            .instructions
            .iter()
            .map(|v| ("instruction".into(), v)),
    );
    inputs.extend(
        manifest
            .dependencies
            .iter()
            .map(|v| ("dependency".into(), v)),
    );
    for (name, role) in &roles {
        if name.is_empty() || role.backend.trim().is_empty() || role.model.trim().is_empty() {
            return Err("role name, backend and model are required".into());
        }
        inputs.extend(role.skills.iter().map(|v| (format!("skill:{name}"), v)));
    }
    if inputs.len() > MAX_FILES {
        return Err("context exceeds 64 inputs".into());
    }
    let mut remaining = LIMIT;
    let mut artifacts = Vec::new();
    for (kind, input) in inputs {
        if !input.path.is_absolute()
            || credential(&input.path)
            || input.source_revision.trim().is_empty()
        {
            return Err(format!(
                "invalid or credential input: {}",
                input.path.display()
            ));
        }
        let path = input
            .path
            .canonicalize()
            .map_err(|e| format!("required input {}: {e}", input.path.display()))?;
        if credential(&path) || !roots.iter().any(|r| path.starts_with(r)) {
            return Err(format!(
                "out-of-scope or credential input: {}",
                path.display()
            ));
        }
        let bytes = bounded_read(&path, remaining)?;
        remaining -= bytes.len();
        let content_id = identity(&bytes);
        let content =
            String::from_utf8(bytes).map_err(|_| "context inputs must be UTF-8".to_string())?;
        artifacts.push(Artifact {
            kind,
            path,
            source_revision: input.source_revision.clone(),
            content_id,
            content,
        });
    }
    Ok(Package {
        version: 1,
        previous,
        manifest,
        artifacts,
        selected_roles: roles,
    })
}

/// Filesystem content store, isolated from the supervisor database.
pub struct ContextStore {
    root: PathBuf,
}
impl ContextStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    fn path(&self, id: &str) -> Result<PathBuf, String> {
        if id.len() != 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err("invalid context revision".into());
        }
        Ok(self.root.join(format!("{id}.json")))
    }
    pub fn show(&self, id: &str) -> Result<Package, String> {
        let bytes = bounded_read(&self.path(id)?, PACKAGE_LIMIT)?;
        if identity(&bytes) != id {
            return Err("context integrity check failed".into());
        }
        let package: Package = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if package.version != 1 {
            return Err("unsupported package version".into());
        }
        for artifact in &package.artifacts {
            if identity(artifact.content.as_bytes()) != artifact.content_id {
                return Err("artifact integrity check failed".into());
            }
        }
        Ok(package)
    }
    pub fn create(&self, manifest: Manifest) -> Result<String, String> {
        self.save(collect(manifest, None)?)
    }
    /// Acknowledgment pins exactly the prior content; changed source is read only here.
    pub fn update(
        &self,
        previous: &str,
        acknowledgment: &str,
        manifest: Manifest,
    ) -> Result<String, String> {
        if acknowledgment != previous {
            return Err("update must acknowledge the exact previous revision".into());
        }
        self.show(previous)?;
        self.save(collect(manifest, Some(previous.into()))?)
    }
    fn save(&self, package: Package) -> Result<String, String> {
        use std::io::Write;
        let bytes = serde_json::to_vec(&package).map_err(|e| e.to_string())?;
        if bytes.len() > PACKAGE_LIMIT {
            return Err("serialized context package exceeds byte limit".into());
        }
        let id = identity(&bytes);
        fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        // Stage then hard-link: readers never observe partial data; publication never overwrites.
        let mut temp = tempfile::NamedTempFile::new_in(&self.root).map_err(|e| e.to_string())?;
        temp.write_all(&bytes).map_err(|e| e.to_string())?;
        temp.as_file().sync_all().map_err(|e| e.to_string())?;
        match fs::hard_link(temp.path(), self.path(&id)?) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                self.show(&id)?;
            }
            Err(e) => return Err(e.to_string()),
        }
        fs::File::open(&self.root)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(id)
    }
}
