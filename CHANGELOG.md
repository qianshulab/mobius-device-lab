# Changelog

## 0.1.0 — Initial preview

- Added a unified Android/iOS device workbench with compact device switching.
- Added Android ADB discovery, connect/pair, forward/reverse, proxy controls, file management, package management, screenshots, user-stopped recording, and embedded scrcpy streaming.
- Added jailbreak iOS SSH file management, password/private-key sessions, libimobiledevice diagnostics, IPA analysis/install/export, screenshots, managed Frida Server sessions, and loopback-only iproxy/SSH tunnels.
- Added package metadata analysis for APK and IPA files, including identity, version, icon, hashes, and permissions where available.
- Added dependency health checks and configurable tool locations without silently downloading third-party binaries.
- Added Windows, Linux, Intel macOS, and Apple Silicon macOS CI/release pipelines.

This preview is intended for devices the operator owns or is explicitly authorized to test. Third-party device tools are not bundled. Release packages produced without platform signing secrets are development builds and may trigger operating-system trust warnings.
