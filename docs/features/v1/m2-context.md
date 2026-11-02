# M2 context contract

`context::ContextStore` exposes create/show/update without supervisor, Git or
SQLite dependencies. Packages are schema version 1; revision IDs are lowercase
SHA-256 of serialized package bytes. Each artifact also has a SHA-256 content ID.
Immutable package files embed exact UTF-8 content and preserve canonical source
path, supplied source revision, kind, original manifest, resolved roles and prior
package identity. The source revision is caller-supplied provenance, not proof of
human approval. Issue bodies/comments must already be snapshotted into a file;
this module does not fetch GitHub or infer approval.

Collection is explicit: absolute files under explicit canonical directory roots,
64 input entries and 1 MiB aggregate content, no recursive discovery. Credential
path components and common key filenames are denied before and after symlink
resolution. Non-files and non-UTF-8 inputs fail. These guards prevent incidental
collection, not malicious same-user filesystem races or secrets disguised in
ordinary source files. Collection assumes sources are not concurrently replaced.

Roles are sorted by name; per-run overrides replace whole known roles, including
skill selections. No fallback, installation, canonical skill edits or dispatch
occurs. Explicit update requires acknowledgment equal to the old revision and
retains that revision. Collection completes before publication. Staging and an
exclusive hard link publish complete files without overwrite; file and directory
sync provide durability. Context directory is private on Unix. Retrieval validates
package and artifact hashes without consulting the original inputs.

M8 must deliver the selected package revision and record worker acknowledgment;
this milestone's update acknowledgment is the caller's explicit command. Human
knowledge promotion and approval enforcement belong to later authority controls.
M1 integration needs only the public API and context CLI branch; it should retain
one binary and keep this storage independent of workflow schema ownership.

Local process tests: `cargo test --locked --test context`. Linux behavior remains
subject to CI; no live agent acceptance is claimed.
