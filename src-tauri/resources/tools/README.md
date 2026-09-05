# Managed tool resources

This directory is intentionally shipped without third-party executables. Mobius never downloads a
tool automatically. The resource rule in `tauri.conf.json` only makes this a controlled location for
future, separately reviewed distributions.

## Directory layout

Put a binary in the directory matching the build target, using its normal executable name:

```text
tools/
  README.md
  common/                 # portable tools only
  macos-aarch64/
  macos-x86_64/
  linux-aarch64/
  linux-x86_64/
  windows-aarch64/
  windows-x86_64/
```

The resolver also accepts the equivalent `tools/<os>/<arch>/` layout. Target-specific directories
take precedence over `common/`. Windows executables must use `.exe` or `.com`; Unix executables must
have at least one execute bit. Do not add symlinks, wrappers, shell scripts, installers, archives, or
download helpers here.

## Release gate for every binary

Before adding any executable, all of the following are required:

1. Confirm that its license permits redistribution in the intended installer and jurisdictions.
2. Add the complete license text and attribution to a nearby `THIRD_PARTY_NOTICES.txt`.
3. Record the upstream project, exact version, target, original download URL, file name, byte size,
   and lowercase SHA-256 digest in a reviewed `manifest.json`.
4. Verify the digest against an upstream-authenticated checksum or signature, then repeat the hash in
   CI before packaging.
5. Scan the exact release artifact and review its transitive/runtime dependencies.
6. Preserve executable permissions on macOS/Linux and perform the platform's normal signing and
   notarization steps after the binary set is frozen.

An example manifest entry (illustrative values only):

```json
{
  "name": "tool-name",
  "version": "exact-version",
  "target": "macos-aarch64",
  "file": "tool-name",
  "source": "https://vendor.example/release",
  "sha256": "64-lowercase-hex-characters",
  "license": "SPDX-identifier"
}
```

Absence from this directory is expected: users can select trusted local executables/directories, and
the resolver can use Android SDK locations or the system `PATH` after explicit and bundled locations.
