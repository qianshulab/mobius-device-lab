# Rebuilding the scrcpy 4.1 portable client

Mobius redistributes the unmodified client, Server, ADB, and Windows runtime
files from the official scrcpy 4.1 portable archives. The accompanying source
archive contains the exact scrcpy source and the source releases selected by
scrcpy 4.1's own `app/deps/*.sh` scripts:

- FFmpeg 8.1.2
- SDL 3.4.12
- libusb 1.0.30
- dav1d 1.5.3

It also contains the zlib source corresponding to the release environment when
zlib code is linked into a distributed file: 1.2.11 for Linux x86_64, 1.3.1
for Windows x86_64, and 1.3.2 for macOS arm64. The macOS x86_64 portable client
uses the operating system libz and does not distribute a zlib binary.
For the Windows build, the archive additionally contains MinGW-w64 11.0.1
source and its complete runtime notice because portions of that runtime are
statically linked into the official portable files.

## Upstream build controls

After extracting `sources/scrcpy-4.1.tar.gz`, the complete controlling scripts
are available inside the source tree:

- `.github/workflows/release.yml` records the native runner and system packages.
- `release/build_linux.sh`, `release/build_macos.sh`, and
  `release/build_windows.sh` build the portable clients.
- `release/build_server.sh` builds the matching Android Server.
- `app/deps/*.sh` records the dependency versions, configure options, linkage,
  upstream URLs, and checksums.
- `meson.build`, `app/meson.build`, the cross files under `release/`, and the
  dependency projects' own build files control compilation and linking.

The upstream entry points are:

```text
release/build_linux.sh x86_64
release/build_macos.sh aarch64
release/build_macos.sh x86_64
release/build_windows.sh 64
```

Use the operating-system packages and runner versions recorded by the upstream
workflow. To reuse the included dependency archives without downloading them
again, place `ffmpeg-8.1.2.tar.xz`, `sdl-3.4.12.tar.gz`,
`libusb-1.0.30.tar.gz`, and `dav1d-1.5.3.tar.gz` in
`app/deps/work/sources/` before invoking the relevant build script. The names
match the names expected by scrcpy's scripts.

The exact source and binary archive identities are recorded in
`packaging/toolchain.lock.json`. The Mobius preparation script independently
downloads the matching Android Platform Tools archive for each target and
requires every distributed ADB file to match it byte for byte before packaging.

## LGPL relinking

The Windows package keeps FFmpeg, libusb, and SDL as adjacent replaceable DLLs;
dav1d and zlib are linked into FFmpeg, and MinGW runtime portions are statically
linked into the portable files. The Linux and macOS portable clients statically
link FFmpeg, libusb, and SDL; dav1d and the target-specific zlib are linked into
FFmpeg. This source package therefore includes the complete scrcpy client
source, all fixed dependency sources, applicable zlib and MinGW sources, and the
upstream scripts needed to compile and relink a modified version. Compiler and
system-library differences may prevent a locally rebuilt file from being
byte-identical to the official portable artifact; byte identity is not required
to exercise the relinking rights granted by the LGPL.

The Windows toolchain's MinGW runtime source is included as
`sources/mingw-w64-11.0.1.tar.bz2`. Reproducing the original Windows compiler
environment also requires the Ubuntu runner and package versions recorded by
scrcpy's upstream release workflow; the source archive alone does not claim a
bit-for-bit reconstruction of that compiler toolchain.

No Mobius patch is applied to scrcpy or its portable dependencies.
