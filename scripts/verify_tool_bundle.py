#!/usr/bin/env python3
"""Verify a staged Mobius tool bundle before Tauri packages it."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import platform
import re
import stat
import struct
import subprocess
import sys
from pathlib import Path
from typing import Any


TARGETS = {
    "windows-x86_64",
    "linux-x86_64",
    "macos-aarch64",
    "macos-x86_64",
}
# A generated 16x16, single-frame H.264 elementary stream. Keeping this fixture
# inline makes the release verifier exercise the exact stdin/stdout transcoding
# path without downloading test media or adding an opaque binary to the repo.
H264_SMOKE_FRAME = (
    "AAAAAWdCwAraewEQAAADABAAAAMAKPEiagAAAAFozg/IAAABZYiEOhGKAAI4scAAQaI4ABXA"
)
MACOS_MINIMUM_VERSION = (12, 0, 0)
# Google still ships the Windows platform-tools 37.0.0 ADB client and its two
# companion DLLs as PE32/i386 binaries. They are the only 32-bit payloads we
# intentionally permit in the x86_64 package; Windows x64 runs them through
# WoW64, and their exact size/SHA-256 values are pinned in the toolchain lock.
WINDOWS_X86_VENDOR_FILES = {
    "adb.exe",
    "AdbWinApi.dll",
    "AdbWinUsbApi.dll",
}
BANNED_SUFFIXES = {
    ".7z",
    ".apk",
    ".bat",
    ".cmd",
    ".dmg",
    ".gz",
    ".jar",
    ".msi",
    ".ps1",
    ".sh",
    ".tar",
    ".vbs",
    ".xz",
    ".zip",
}


class VerifyError(RuntimeError):
    pass


def sha256_file(file_path: Path) -> str:
    digest = hashlib.sha256()
    with file_path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def load_json(file_path: Path) -> dict[str, Any]:
    try:
        value = json.loads(file_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise VerifyError(f"Unable to load {file_path}: {error}") from error
    if not isinstance(value, dict):
        raise VerifyError(f"Expected a JSON object in {file_path}")
    return value


def native_target() -> str | None:
    system = platform.system().lower()
    machine = platform.machine().lower()
    if (
        system == "windows"
        or system.startswith("msys")
        or system.startswith("mingw")
        or system.startswith("cygwin")
    ) and machine in {"amd64", "x86_64"}:
        return "windows-x86_64"
    if system == "linux" and machine in {"amd64", "x86_64"}:
        return "linux-x86_64"
    if system == "darwin" and machine in {"arm64", "aarch64"}:
        return "macos-aarch64"
    if system == "darwin" and machine in {"amd64", "x86_64"}:
        return "macos-x86_64"
    return None


def expected_programs(target: str) -> dict[str, list[str]]:
    suffix = ".exe" if target.startswith("windows-") else ""
    return {
        f"adb{suffix}": ["version"],
        f"scrcpy{suffix}": ["--version"],
        f"ffmpeg{suffix}": ["-hide_banner", "-version"],
        f"aapt2{suffix}": ["version"],
        f"ios{suffix}": ["version"],
        f"ssh{suffix}": ["-V"],
        f"scp{suffix}": ["-V"],
    }


def expected_version_markers(target: str) -> dict[str, list[str]]:
    suffix = ".exe" if target.startswith("windows-") else ""
    return {
        f"adb{suffix}": [
            "Android Debug Bridge version 1.0.41",
            "Version 37.0.0-",
        ],
        f"scrcpy{suffix}": ["scrcpy 4.1"],
        f"ffmpeg{suffix}": ["ffmpeg version 9.0.1"],
        f"aapt2{suffix}": ["15978811"],
        f"ios{suffix}": ['"version":"1.3.2-mobius.1"'],
        f"ssh{suffix}": ["Mobius SSH/SFTP Client 0.2.0", "x/crypto/ssh"],
        f"scp{suffix}": ["Mobius SSH/SFTP Client 0.2.0", "x/crypto/ssh"],
    }


def decode_macos_version(value: int) -> tuple[int, int, int]:
    return (value >> 16, (value >> 8) & 0xFF, value & 0xFF)


def verify_macos_minimum_version(
    file_path: Path, target: str, expected_cpu: int
) -> None:
    with file_path.open("rb") as stream:
        magic = stream.read(4)
        slice_offset = 0
        if magic in {b"\xca\xfe\xba\xbe", b"\xca\xfe\xba\xbf"}:
            stream.seek(4)
            count_data = stream.read(4)
            if len(count_data) != 4:
                raise VerifyError(f"Truncated Mach-O fat header: {file_path}")
            count = struct.unpack(">I", count_data)[0]
            is_fat64 = magic == b"\xca\xfe\xba\xbf"
            entry_size = 32 if is_fat64 else 20
            selected_offset = None
            for _ in range(count):
                entry = stream.read(entry_size)
                if len(entry) != entry_size:
                    raise VerifyError(f"Truncated Mach-O architecture table: {file_path}")
                cpu = struct.unpack_from(">I", entry, 0)[0]
                if cpu == expected_cpu:
                    selected_offset = struct.unpack_from(">Q" if is_fat64 else ">I", entry, 8)[0]
            if selected_offset is None:
                raise VerifyError(f"Mach-O lacks the requested target slice: {file_path}")
            slice_offset = selected_offset
            stream.seek(slice_offset)
            magic = stream.read(4)

        if magic == b"\xcf\xfa\xed\xfe":
            endian = "<"
        elif magic == b"\xfe\xed\xfa\xcf":
            endian = ">"
        else:
            raise VerifyError(f"Expected a 64-bit Mach-O slice: {file_path}")

        stream.seek(slice_offset)
        header = stream.read(32)
        if len(header) != 32:
            raise VerifyError(f"Truncated Mach-O header: {file_path}")
        cpu, command_count, command_bytes = (
            struct.unpack_from(f"{endian}I", header, 4)[0],
            struct.unpack_from(f"{endian}I", header, 16)[0],
            struct.unpack_from(f"{endian}I", header, 20)[0],
        )
        if cpu != expected_cpu:
            raise VerifyError(f"Unexpected Mach-O architecture: {file_path}")
        if command_bytes > 4 * 1024 * 1024:
            raise VerifyError(f"Unreasonable Mach-O load command size: {file_path}")
        commands = stream.read(command_bytes)
        if len(commands) != command_bytes:
            raise VerifyError(f"Truncated Mach-O load commands: {file_path}")

    versions: list[tuple[int, int, int]] = []
    position = 0
    for _ in range(command_count):
        if position + 8 > len(commands):
            raise VerifyError(f"Invalid Mach-O load commands: {file_path}")
        command, command_size = struct.unpack_from(f"{endian}II", commands, position)
        if command_size < 8 or position + command_size > len(commands):
            raise VerifyError(f"Invalid Mach-O load command size: {file_path}")
        if command == 0x32 and command_size >= 24:  # LC_BUILD_VERSION
            platform_id, minimum = struct.unpack_from(
                f"{endian}II", commands, position + 8
            )
            if platform_id == 1:  # PLATFORM_MACOS
                versions.append(decode_macos_version(minimum))
        elif command == 0x24 and command_size >= 16:  # LC_VERSION_MIN_MACOSX
            minimum = struct.unpack_from(f"{endian}I", commands, position + 8)[0]
            versions.append(decode_macos_version(minimum))
        position += command_size

    if not versions:
        raise VerifyError(f"Mach-O does not declare a macOS minimum version: {file_path}")
    minimum = max(versions)
    if minimum > MACOS_MINIMUM_VERSION:
        actual = ".".join(str(part) for part in minimum)
        supported = ".".join(str(part) for part in MACOS_MINIMUM_VERSION)
        raise VerifyError(
            f"{file_path.name} requires macOS {actual}, above the supported {supported} floor"
        )


def verify_machine(file_path: Path, target: str) -> None:
    with file_path.open("rb") as stream:
        data = stream.read(4096)
    if target.startswith("windows-"):
        if data[:2] != b"MZ" or len(data) < 0x40:
            raise VerifyError(f"Expected a PE executable: {file_path}")
        offset = struct.unpack_from("<I", data, 0x3C)[0]
        if offset + 6 > len(data):
            with file_path.open("rb") as stream:
                stream.seek(offset)
                header = stream.read(6)
        else:
            header = data[offset : offset + 6]
        if header[:4] != b"PE\0\0":
            raise VerifyError(f"Invalid PE header: {file_path}")
        machine = struct.unpack_from("<H", header, 4)[0]
        expected_machines = (
            {0x014C, 0x8664}
            if file_path.name in WINDOWS_X86_VENDOR_FILES
            else {0x8664}
        )
        if machine not in expected_machines:
            expected = (
                "Windows x86 or x86_64"
                if len(expected_machines) == 2
                else "Windows x86_64"
            )
            raise VerifyError(
                f"Expected a {expected} executable: {file_path} "
                f"(PE machine 0x{machine:04x})"
            )
        return

    if target.startswith("linux-"):
        if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
            raise VerifyError(f"Expected a little-endian ELF64 executable: {file_path}")
        if struct.unpack_from("<H", data, 18)[0] != 62:
            raise VerifyError(f"Expected a Linux x86_64 executable: {file_path}")
        return

    expected_cpu = 0x0100000C if target == "macos-aarch64" else 0x01000007
    magic = data[:4]
    if magic == b"\xcf\xfa\xed\xfe":
        cpu = struct.unpack_from("<I", data, 4)[0]
        if cpu != expected_cpu:
            raise VerifyError(f"Unexpected Mach-O architecture: {file_path}")
        verify_macos_minimum_version(file_path, target, expected_cpu)
        return
    if magic in {b"\xca\xfe\xba\xbe", b"\xca\xfe\xba\xbf"}:
        count = struct.unpack_from(">I", data, 4)[0]
        entry_size = 20 if magic == b"\xca\xfe\xba\xbe" else 32
        cpus = {
            struct.unpack_from(">I", data, 8 + index * entry_size)[0]
            for index in range(count)
            if 8 + index * entry_size + 4 <= len(data)
        }
        if expected_cpu not in cpus:
            raise VerifyError(f"Universal Mach-O lacks the target architecture: {file_path}")
        verify_macos_minimum_version(file_path, target, expected_cpu)
        return
    raise VerifyError(f"Expected a Mach-O executable: {file_path}")


def verify_manifest(root: Path, target: str, lock: dict[str, Any]) -> dict[str, Any]:
    target_dir = root / target
    if target_dir.is_symlink() or not target_dir.is_dir():
        raise VerifyError(f"Missing target bundle directory: {target_dir}")
    manifest = load_json(target_dir / "manifest.json")
    if manifest.get("schemaVersion") != 1 or manifest.get("target") != target:
        raise VerifyError("Bundle manifest schema or target does not match")
    if manifest.get("bundleRevision") != lock.get("bundleRevision"):
        raise VerifyError("Bundle revision does not match toolchain lock")

    entries = manifest.get("files")
    if not isinstance(entries, list) or not entries:
        raise VerifyError("Bundle manifest has no files")
    recorded: set[str] = set()
    for entry in entries:
        if not isinstance(entry, dict):
            raise VerifyError("Bundle manifest contains an invalid file entry")
        relative = entry.get("path")
        if not isinstance(relative, str) or relative in recorded:
            raise VerifyError(f"Invalid or duplicate manifest path: {relative!r}")
        recorded.add(relative)
        file_path = target_dir / relative
        if file_path.is_symlink() or not file_path.is_file():
            raise VerifyError(f"Manifest file is missing or is a link: {relative}")
        if file_path.stat().st_size != entry.get("size"):
            raise VerifyError(f"Size mismatch: {relative}")
        if sha256_file(file_path) != entry.get("sha256"):
            raise VerifyError(f"SHA-256 mismatch: {relative}")
        expected_executable = bool(entry.get("executable"))
        actual_executable = bool(file_path.stat().st_mode & 0o111)
        if os.name != "nt" and expected_executable != actual_executable:
            raise VerifyError(f"Executable permission mismatch: {relative}")

    actual = {
        file_path.relative_to(target_dir).as_posix()
        for file_path in target_dir.rglob("*")
        if file_path.is_file() and file_path.name != "manifest.json"
    }
    if recorded != actual:
        raise VerifyError(
            f"Manifest file set mismatch; missing={sorted(recorded - actual)}, "
            f"unexpected={sorted(actual - recorded)}"
        )
    for file_path in target_dir.rglob("*"):
        if file_path.is_symlink():
            raise VerifyError(f"Bundle contains a symlink: {file_path}")
        if file_path.is_file() and file_path.suffix.lower() in BANNED_SUFFIXES:
            if file_path.suffix.lower() == ".patch" or "licenses" in file_path.parts:
                continue
            raise VerifyError(f"Bundle contains a forbidden payload type: {file_path}")

    expected_components = {"scrcpy", "aapt2", "go-ios", "mobius-ssh", "ffmpeg"}
    components = manifest.get("components")
    if not isinstance(components, list):
        raise VerifyError("Bundle component list is invalid")
    actual_components = {item.get("name") for item in components if isinstance(item, dict)}
    if actual_components != expected_components:
        raise VerifyError(f"Unexpected component set: {sorted(actual_components)}")
    for item in components:
        locked = lock["components"].get(item["name"])
        if (
            not locked
            or item.get("version") != locked.get("version")
            or item.get("license") != locked.get("license")
        ):
            raise VerifyError(f"Component does not match lock: {item.get('name')}")
    return manifest


def run_smoke_checks(target_dir: Path, target: str) -> None:
    environment = os.environ.copy()
    for name in (
        "ADB_SERVER_SOCKET",
        "ANDROID_ADB_SERVER_ADDRESS",
        "ANDROID_ADB_SERVER_PORT",
        "ENABLE_GO_IOS_AGENT",
        "GO_IOS_AGENT_HOST",
        "GO_IOS_AGENT_PORT",
        "GO_IOS_PPROF",
        "USBMUXD_SOCKET_ADDRESS",
    ):
        environment.pop(name, None)
    environment["PATH"] = os.pathsep.join(
        [str(target_dir), "/usr/bin", "/bin"] if os.name != "nt" else [str(target_dir)]
    )
    version_markers = expected_version_markers(target)
    for name, args in expected_programs(target).items():
        file_path = target_dir / name
        if not file_path.is_file():
            raise VerifyError(f"Required bundled program is missing: {name}")
        verify_machine(file_path, target)
        result = subprocess.run(
            [str(file_path), *args],
            cwd=target_dir,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=20,
            check=False,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
        if result.returncode != 0:
            raise VerifyError(
                f"Smoke check failed for {name} ({result.returncode}): "
                f"{result.stdout[:1000]}"
            )
        missing_markers = [
            marker for marker in version_markers[name] if marker not in result.stdout
        ]
        if missing_markers:
            raise VerifyError(
                f"Unexpected version output for {name}; missing {missing_markers}: "
                f"{result.stdout[:1000]}"
            )
        first_line = next(
            (line.strip() for line in result.stdout.splitlines() if line.strip()), "ready"
        )
        print(f"{name}: {first_line[:200]}")

    verify_ffmpeg_capabilities(
        target_dir / ("ffmpeg.exe" if target.startswith("windows-") else "ffmpeg"),
        target_dir,
        environment,
    )

    if target.startswith("windows-"):
        for name in (
            "AdbWinApi.dll",
            "AdbWinUsbApi.dll",
            "SDL3.dll",
            "libusb-1.0.dll",
            "avcodec-62.dll",
            "avformat-62.dll",
            "avutil-60.dll",
            "swresample-6.dll",
        ):
            dependency = target_dir / name
            if not dependency.is_file():
                raise VerifyError(f"Required Windows runtime is missing: {name}")
            verify_machine(dependency, target)

    server = target_dir / "scrcpy-server"
    if not server.is_file() or server.stat().st_size < 10_000:
        raise VerifyError("The matching scrcpy-server payload is missing or truncated")

def verify_ffmpeg_capabilities(
    executable: Path, target_dir: Path, environment: dict[str, str]
) -> None:
    checks = {
        "decoder h264": (["-hide_banner", "-decoders"], r"(?m)^\s*\S+\s+h264(?:\s|$)"),
        "encoder mjpeg": (["-hide_banner", "-encoders"], r"(?m)^\s*\S+\s+mjpeg(?:\s|$)"),
        "filter format": (["-hide_banner", "-filters"], r"(?m)^\s*\S+\s+format(?:\s|$)"),
        "filter scale": (["-hide_banner", "-filters"], r"(?m)^\s*\S+\s+scale(?:\s|$)"),
        "muxer mpjpeg": (["-hide_banner", "-muxers"], r"(?m)^\s*\S+\s+mpjpeg(?:\s|$)"),
    }
    for label, (args, pattern) in checks.items():
        result = subprocess.run(
            [str(executable), *args],
            cwd=target_dir,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=20,
            check=False,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
        if result.returncode != 0 or re.search(pattern, result.stdout) is None:
            raise VerifyError(f"Bundled FFmpeg lacks required {label}")

    protocols = subprocess.run(
        [str(executable), "-hide_banner", "-protocols"],
        cwd=target_dir,
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=20,
        check=False,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if protocols.returncode != 0:
        raise VerifyError("Unable to inspect bundled FFmpeg protocols")
    protocol_lines = {line.strip() for line in protocols.stdout.splitlines()}
    if not {"file", "pipe"}.issubset(protocol_lines):
        raise VerifyError("Bundled FFmpeg lacks the required file/pipe protocols")

    # Capability listings alone do not prove that FFmpeg can negotiate the
    # decoded H.264 pixel format into MJPEG. Run the same pipeline used by the
    # embedded screen view so missing scale/conversion support fails the build.
    transcode = subprocess.run(
        [
            str(executable),
            "-hide_banner",
            "-loglevel",
            "warning",
            "-probesize",
            "32768",
            "-analyzeduration",
            "0",
            "-fpsprobesize",
            "0",
            "-f",
            "h264",
            "-i",
            "pipe:0",
            "-an",
            "-vf",
            "format=yuvj420p",
            "-fps_mode",
            "passthrough",
            "-c:v",
            "mjpeg",
            "-q:v",
            "6",
            "-flush_packets",
            "1",
            "-f",
            "mpjpeg",
            "pipe:1",
        ],
        cwd=target_dir,
        env=environment,
        input=base64.b64decode(H264_SMOKE_FRAME),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=20,
        check=False,
    )
    if (
        transcode.returncode != 0
        or b"content-type: image/jpeg" not in transcode.stdout.lower()
        or b"\xff\xd8" not in transcode.stdout
        or b"\xff\xd9" not in transcode.stdout
    ):
        diagnostic = transcode.stderr.decode("utf-8", errors="replace")[:1000]
        raise VerifyError(
            "Bundled FFmpeg failed the H.264-to-MJPEG screen pipeline: "
            f"{diagnostic}"
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--lock", type=Path, default=Path("packaging/toolchain.lock.json"))
    parser.add_argument("--target", required=True, choices=sorted(TARGETS))
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--skip-execution", action="store_true")
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    lock_path = args.lock if args.lock.is_absolute() else repo_root / args.lock
    root = args.root if args.root.is_absolute() else repo_root / args.root
    lock = load_json(lock_path)
    verify_manifest(root.resolve(), args.target, lock)
    if not args.skip_execution:
        host_target = native_target()
        if host_target != args.target:
            raise VerifyError(
                f"Cannot execute {args.target} tools on {host_target or 'this host'}; "
                "use --skip-execution only for an intentional cross-architecture audit"
            )
        run_smoke_checks((root / args.target).resolve(), args.target)
    print(f"Verified {args.target} tool bundle")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (VerifyError, subprocess.TimeoutExpired) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
