# Changelog

## 0.3.0 — Copy-first package analysis

- Added offline APK/DEX protector, packer, and obfuscator signature detection with the reviewed Detect It Easy 3.21 CLI and rule database bundled for every release target.
- Added three-state protection results (`detected`, `not detected`, and `inconclusive`) so missing tools, partial APK/DEX rule catalogs, non-APK scan contexts, timeouts, truncated output, invalid JSON, and engine failures never become false “not protected” conclusions.
- Redesigned APK/IPA results around copyable application name, package or Bundle ID, version, build, ABI, file name, local path, and MD5 fields, plus one-click summary, permission, and protection-result copying.
- Fixed overlapping package-analysis requests, stale install actions, duplicate iOS privacy entries, and unnecessary installed-app loading outside its tab. Android APK export is now a visible row action, and iOS app rows can copy their Bundle ID directly.
- Kept user-started Android recordings active across page and device navigation until the user stops them or exits the application, with globally serialized start/stop state and the recording device named even after switching targets. Fixed read-only Shell presets to execute immediately, made remote folders open with one click, and kept file-target symbolic links selectable and downloadable.
- Added real APK, browser interaction, full Rust test, lint, and four-platform tool-bundle verification coverage for this release.

## 0.2.0 — Bundled toolchain

- Added verified per-platform bundles for ADB 37.0.0, scrcpy/client Server 4.1, a minimal LGPL FFmpeg 9.0.1 build, and AAPT2 9.4.0-15978811, so installed releases no longer require Android command-line tools on `PATH`.
- Added go-ios 1.3.2-mobius.1 as the primary iOS host adapter for discovery, information, application listing and installation, screenshots, bounded logs, and USB forwarding. The published Mobius patch binds forwarding to loopback; local libimobiledevice and `iproxy` tools remain optional compatibility fallbacks.
- Removed the unused host Frida CLI health dependency. Frida Server remains a user-supplied, device/ABI-specific file managed through the existing version slots and loopback forwarding.
- Added an immutable toolchain lock, archive size and SHA-256 verification, safe extraction, target/architecture checks, native smoke tests, per-file manifests, third-party notices, and a companion source/build-script archive in each Release.
- Added a CGO-free, restricted Mobius SSH/SFTP client to every platform bundle for password/private-key commands, single-file transfers, and loopback-only local/reverse forwarding. It uses pinned Go modules, one-time password delivery, and accept-new host-key pinning with changed-key rejection, so jailbreak workflows no longer require system OpenSSH or `PATH` configuration.
- Windows still requires Apple Mobile Device support for iOS USB; Linux still requires usbmuxd and suitable udev permissions.

This preview remains limited to devices the operator owns or is explicitly authorized to test. Unsigned packages may trigger operating-system trust warnings.

## 0.1.0 — Initial preview

- Added a unified Android/iOS device workbench with compact device switching.
- Added Android ADB discovery, connect/pair, forward/reverse, proxy controls, file management, package management, screenshots, user-stopped recording, and embedded scrcpy streaming.
- Added jailbreak iOS SSH file management, password/private-key sessions, libimobiledevice diagnostics, IPA analysis/install/export, screenshots, managed Frida Server sessions, and loopback-only iproxy/SSH tunnels.
- Added package metadata analysis for APK and IPA files, including identity, version, icon, hashes, and permissions where available.
- Added dependency health checks and configurable tool locations.
- Added Windows, Linux, Intel macOS, and Apple Silicon macOS CI/release pipelines.

This preview is intended for devices the operator owns or is explicitly authorized to test. Release packages produced without platform signing secrets are development builds and may trigger operating-system trust warnings.
