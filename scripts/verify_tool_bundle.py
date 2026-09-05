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
from pathlib import Path, PurePosixPath
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
DIE_ARM64_MINIMUM_VERSION = (13, 0, 0)
LINUX_ELF_SYSTEM_ALLOWLIST = frozenset(
    {
        "ld-linux-x86-64.so.2",
        "libc.so.6",
        "libdl.so.2",
        "libgcc_s.so.1",
        "libm.so.6",
        "libpthread.so.0",
        "libstdc++.so.6",
    }
)
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


QT_SINGLE_FILE_PATTERN = re.compile(
    r'"(?:LicenseFile|CopyrightFile)"\s*:\s*("(?:\\.|[^"\\])*")'
)
QT_FILE_ARRAY_PATTERN = re.compile(
    r'"LicenseFiles"\s*:\s*\[(.*?)\]', re.DOTALL
)
JSON_STRING_PATTERN = re.compile(r'"(?:\\.|[^"\\])*"')


def resolve_qt_attribution_license(
    attribution: PurePosixPath, raw_reference: str
) -> PurePosixPath:
    if (
        not raw_reference
        or "\\" in raw_reference
        or "\x00" in raw_reference
        or PurePosixPath(raw_reference).is_absolute()
    ):
        raise VerifyError(
            f"Unsafe Qt LicenseFile reference in {attribution}: {raw_reference!r}"
        )
    parts = list(attribution.parent.parts)
    for part in PurePosixPath(raw_reference).parts:
        if part in {"", "."}:
            continue
        if part == "..":
            if not parts:
                raise VerifyError(
                    f"Qt LicenseFile escapes its attribution root in {attribution}"
                )
            parts.pop()
            continue
        parts.append(part)
    if not parts:
        raise VerifyError(f"Empty Qt LicenseFile target in {attribution}")
    return PurePosixPath(*parts)


def qt_attribution_license_references(
    attribution: PurePosixPath, source: Path
) -> set[PurePosixPath]:
    try:
        text = source.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as error:
        raise VerifyError(f"Invalid Qt attribution text: {attribution}") from error
    references: set[PurePosixPath] = set()
    encoded_references = QT_SINGLE_FILE_PATTERN.findall(text)
    for array_body in QT_FILE_ARRAY_PATTERN.findall(text):
        encoded_references.extend(JSON_STRING_PATTERN.findall(array_body))
    for encoded in encoded_references:
        try:
            raw_reference = json.loads(encoded)
        except json.JSONDecodeError as error:
            raise VerifyError(
                f"Invalid Qt LicenseFile string in {attribution}"
            ) from error
        if not isinstance(raw_reference, str):
            raise VerifyError(f"Invalid Qt LicenseFile value in {attribution}")
        references.add(resolve_qt_attribution_license(attribution, raw_reference))
    return references


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
        f"die/diec{suffix}": ["--version"],
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
        f"die/diec{suffix}": ["die 3.21"],
    }


def decode_macos_version(value: int) -> tuple[int, int, int]:
    return (value >> 16, (value >> 8) & 0xFF, value & 0xFF)


def verify_macos_minimum_version(
    file_path: Path,
    target: str,
    expected_cpu: int,
    supported_version: tuple[int, int, int] = MACOS_MINIMUM_VERSION,
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
    if minimum > supported_version:
        actual = ".".join(str(part) for part in minimum)
        supported = ".".join(str(part) for part in supported_version)
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
    die_payload = "die" in file_path.parts or "Frameworks" in file_path.parts
    supported_version = (
        DIE_ARM64_MINIMUM_VERSION
        if target == "macos-aarch64" and die_payload
        else MACOS_MINIMUM_VERSION
    )
    magic = data[:4]
    if magic == b"\xcf\xfa\xed\xfe":
        cpu = struct.unpack_from("<I", data, 4)[0]
        if cpu != expected_cpu:
            raise VerifyError(f"Unexpected Mach-O architecture: {file_path}")
        verify_macos_minimum_version(
            file_path, target, expected_cpu, supported_version
        )
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
        verify_macos_minimum_version(
            file_path, target, expected_cpu, supported_version
        )
        return
    raise VerifyError(f"Expected a Mach-O executable: {file_path}")


def elf_dynamic_metadata(file_path: Path) -> tuple[list[str], str | None]:
    """Read DT_NEEDED and DT_SONAME from a little-endian ELF64 without host tools."""
    with file_path.open("rb") as stream:
        header = stream.read(64)
        if (
            len(header) != 64
            or header[:4] != b"\x7fELF"
            or header[4] != 2
            or header[5] != 1
        ):
            raise VerifyError(f"Expected a little-endian ELF64 file: {file_path}")
        program_offset = struct.unpack_from("<Q", header, 32)[0]
        program_entry_size = struct.unpack_from("<H", header, 54)[0]
        program_count = struct.unpack_from("<H", header, 56)[0]
        if (
            program_entry_size < 56
            or program_entry_size > 4096
            or program_count == 0
            or program_count > 4096
        ):
            raise VerifyError(f"Invalid ELF program header table: {file_path}")

        load_segments: list[tuple[int, int, int]] = []
        dynamic_segment: tuple[int, int] | None = None
        for index in range(program_count):
            stream.seek(program_offset + index * program_entry_size)
            entry = stream.read(program_entry_size)
            if len(entry) != program_entry_size:
                raise VerifyError(f"Truncated ELF program header table: {file_path}")
            segment_type = struct.unpack_from("<I", entry, 0)[0]
            file_offset = struct.unpack_from("<Q", entry, 8)[0]
            virtual_address = struct.unpack_from("<Q", entry, 16)[0]
            file_size = struct.unpack_from("<Q", entry, 32)[0]
            if segment_type == 1:  # PT_LOAD
                load_segments.append((file_offset, virtual_address, file_size))
            elif segment_type == 2:  # PT_DYNAMIC
                if dynamic_segment is not None:
                    raise VerifyError(
                        f"ELF contains multiple dynamic segments: {file_path}"
                    )
                dynamic_segment = (file_offset, file_size)
        if dynamic_segment is None or not load_segments:
            raise VerifyError(f"ELF dynamic metadata is missing: {file_path}")

        dynamic_offset, dynamic_size = dynamic_segment
        if dynamic_size == 0 or dynamic_size > 16 * 1024 * 1024:
            raise VerifyError(f"Invalid ELF dynamic segment size: {file_path}")
        stream.seek(dynamic_offset)
        dynamic = stream.read(dynamic_size)
        if len(dynamic) != dynamic_size:
            raise VerifyError(f"Truncated ELF dynamic segment: {file_path}")

        string_table_address: int | None = None
        string_table_size: int | None = None
        needed_offsets: list[int] = []
        soname_offset: int | None = None
        for position in range(0, len(dynamic) - 15, 16):
            tag, value = struct.unpack_from("<QQ", dynamic, position)
            if tag == 0:  # DT_NULL
                break
            if tag == 1:  # DT_NEEDED
                needed_offsets.append(value)
            elif tag == 5:  # DT_STRTAB
                string_table_address = value
            elif tag == 10:  # DT_STRSZ
                string_table_size = value
            elif tag == 14:  # DT_SONAME
                soname_offset = value
        if (
            string_table_address is None
            or string_table_size is None
            or string_table_size <= 0
            or string_table_size > 64 * 1024 * 1024
        ):
            raise VerifyError(f"ELF string table is invalid: {file_path}")

        string_table_offset: int | None = None
        for file_offset, virtual_address, file_size in load_segments:
            if (
                virtual_address <= string_table_address
                and string_table_address - virtual_address < file_size
            ):
                string_table_offset = (
                    file_offset + string_table_address - virtual_address
                )
                break
        if string_table_offset is None:
            raise VerifyError(f"ELF string table is outside load segments: {file_path}")
        stream.seek(string_table_offset)
        string_table = stream.read(string_table_size)
        if len(string_table) != string_table_size:
            raise VerifyError(f"Truncated ELF string table: {file_path}")

    def dynamic_string(offset: int) -> str:
        if offset < 0 or offset >= len(string_table):
            raise VerifyError(f"ELF dynamic string offset is invalid: {file_path}")
        terminator = string_table.find(b"\0", offset)
        if terminator < 0:
            raise VerifyError(f"ELF dynamic string is unterminated: {file_path}")
        try:
            value = string_table[offset:terminator].decode("utf-8")
        except UnicodeDecodeError as error:
            raise VerifyError(f"ELF dynamic string is not UTF-8: {file_path}") from error
        if not value or "/" in value or "\\" in value or "\x00" in value:
            raise VerifyError(f"ELF dynamic library name is unsafe: {file_path}")
        return value

    needed = [dynamic_string(offset) for offset in needed_offsets]
    soname = dynamic_string(soname_offset) if soname_offset is not None else None
    return needed, soname


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

    expected_components = {
        "scrcpy",
        "aapt2",
        "go-ios",
        "mobius-ssh",
        "ffmpeg",
        "diec",
    }
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
    verify_die_layout(target_dir, target, lock)
    return manifest


def verify_die_layout(target_dir: Path, target: str, lock: dict[str, Any]) -> None:
    suffix = ".exe" if target.startswith("windows-") else ""
    executable = target_dir / "die" / f"diec{suffix}"
    if not executable.is_file():
        raise VerifyError("Detect It Easy console executable is missing")
    verify_machine(executable, target)
    database = target_dir / "die" / "db"
    database_files = [path for path in database.rglob("*") if path.is_file()]
    component = lock["components"]["diec"]
    database_validation = component.get("databaseValidation")
    if not isinstance(database_validation, dict):
        raise VerifyError("Detect It Easy database validation lock is missing")
    expected_total = database_validation.get("expectedTotalFiles")
    if (
        not database.is_dir()
        or not isinstance(expected_total, int)
        or expected_total <= 0
        or len(database_files) != expected_total
    ):
        raise VerifyError("Detect It Easy signature database is missing or incomplete")
    required_files = database_validation.get("requiredFiles")
    if not isinstance(required_files, list) or not required_files:
        raise VerifyError("Detect It Easy required database entries are not locked")
    for value in required_files:
        if (
            not isinstance(value, str)
            or Path(value).is_absolute()
            or ".." in Path(value).parts
            or not (database / value).is_file()
        ):
            raise VerifyError(f"Detect It Easy database entry is missing: {value!r}")
    rule_directories = database_validation.get("ruleDirectories")
    if not isinstance(rule_directories, list) or not rule_directories:
        raise VerifyError("Detect It Easy database rule-count lock is missing")
    for entry in rule_directories:
        if not isinstance(entry, dict):
            raise VerifyError("Detect It Easy database rule-count lock is invalid")
        relative = entry.get("path")
        expected_rules = entry.get("expectedRuleFiles")
        if (
            not isinstance(relative, str)
            or Path(relative).is_absolute()
            or ".." in Path(relative).parts
            or not isinstance(expected_rules, int)
            or expected_rules <= 0
        ):
            raise VerifyError("Detect It Easy database rule-count lock is invalid")
        rule_count = sum(
            1 for path in (database / relative).rglob("*.sg") if path.is_file()
        )
        if rule_count != expected_rules:
            raise VerifyError(
                f"Detect It Easy {relative} rules are incomplete: "
                f"expected exactly {expected_rules}, found {rule_count}"
            )

    artifact = component["targets"][target]
    dependencies = component.get("sourceDependencies")
    dependency_labels = artifact.get("sourceDependencies")
    if not isinstance(dependencies, dict) or not isinstance(dependency_labels, list):
        raise VerifyError("Detect It Easy source dependency lock is missing")
    expected_attribution_count = 0
    for label in dependency_labels:
        dependency = dependencies.get(label)
        if not isinstance(dependency, dict):
            raise VerifyError(f"Detect It Easy source dependency is missing: {label!r}")
        specification = dependency.get("qtAttributions")
        if specification is None:
            continue
        if not isinstance(specification, dict):
            raise VerifyError(f"Detect It Easy Qt attribution lock is invalid: {label}")
        expected_count = specification.get("count")
        if (
            specification.get("memberName") != "qt_attribution.json"
            or not isinstance(expected_count, int)
            or expected_count <= 0
        ):
            raise VerifyError(f"Detect It Easy Qt attribution lock is invalid: {label}")
        attribution_directory = (
            target_dir / "licenses" / "die-qt-attributions" / label
        )
        attribution_files = [
            path
            for path in attribution_directory.rglob("qt_attribution.json")
            if path.is_file()
        ]
        if len(attribution_files) != expected_count:
            raise VerifyError(
                f"Detect It Easy {label} attribution set is incomplete: "
                f"expected {expected_count}, found {len(attribution_files)}"
            )
        expected_license_files: set[PurePosixPath] = set()
        for attribution_file in attribution_files:
            attribution_relative = PurePosixPath(
                attribution_file.relative_to(attribution_directory).as_posix()
            )
            expected_license_files.update(
                qt_attribution_license_references(
                    attribution_relative, attribution_file
                )
            )
        license_directory_value = specification.get("licenseDirectory")
        expected_module_license_count = specification.get("licenseFileCount")
        if (
            license_directory_value is not None
            or expected_module_license_count is not None
        ):
            if (
                not isinstance(license_directory_value, str)
                or not isinstance(expected_module_license_count, int)
                or expected_module_license_count <= 0
            ):
                raise VerifyError(
                    f"Detect It Easy Qt module license lock is invalid: {label}"
                )
            license_directory = PurePosixPath(license_directory_value)
            if (
                license_directory.is_absolute()
                or ".." in license_directory.parts
                or any(part in {"", "."} for part in license_directory.parts)
            ):
                raise VerifyError(
                    f"Detect It Easy Qt module license path is invalid: {label}"
                )
            module_license_root = attribution_directory.joinpath(
                *license_directory.parts
            )
            module_license_files = {
                PurePosixPath(path.relative_to(attribution_directory).as_posix())
                for path in module_license_root.rglob("*")
                if path.is_file()
            }
            if len(module_license_files) != expected_module_license_count:
                raise VerifyError(
                    f"Detect It Easy {label} Qt module license set is incomplete: "
                    f"expected {expected_module_license_count}, "
                    f"found {len(module_license_files)}"
                )
            expected_license_files.update(module_license_files)
        for relative in expected_license_files:
            license_file = attribution_directory.joinpath(*relative.parts)
            if license_file.is_symlink() or not license_file.is_file():
                raise VerifyError(
                    f"Detect It Easy {label} Qt attribution license is missing: "
                    f"{relative.as_posix()}"
                )
            if license_file.stat().st_size <= 0:
                raise VerifyError(
                    f"Detect It Easy {label} Qt attribution license is empty: "
                    f"{relative.as_posix()}"
                )
        actual_license_files = {
            PurePosixPath(path.relative_to(attribution_directory).as_posix())
            for path in attribution_directory.rglob("*")
            if path.is_file() and path.name != "qt_attribution.json"
        }
        if actual_license_files != expected_license_files:
            raise VerifyError(
                f"Detect It Easy {label} Qt attribution license set differs; "
                f"missing={sorted(path.as_posix() for path in expected_license_files - actual_license_files)}, "
                f"unexpected={sorted(path.as_posix() for path in actual_license_files - expected_license_files)}"
            )
        expected_attribution_count += expected_count
    attribution_root = target_dir / "licenses" / "die-qt-attributions"
    actual_attribution_count = sum(
        1
        for path in attribution_root.rglob("qt_attribution.json")
        if path.is_file()
    )
    if actual_attribution_count != expected_attribution_count:
        raise VerifyError(
            "Detect It Easy bundle contains an unexpected Qt attribution set"
        )

    if target.startswith("windows-"):
        for name in artifact["runtimeFiles"]:
            runtime = target_dir / "die" / name
            if not runtime.is_file():
                raise VerifyError(f"Detect It Easy runtime is missing: {name}")
            verify_machine(runtime, target)
    elif target.startswith("linux-"):
        runtime_names = list(artifact["runtimeFiles"])
        runtime_names.extend(
            item["destination"] for item in artifact["icuRuntime"]["runtimeFiles"]
        )
        runtime_packages = artifact.get("linuxRuntimePackages")
        if not isinstance(runtime_packages, list) or not runtime_packages:
            raise VerifyError("Detect It Easy Linux runtime package lock is missing")
        package_names: set[str] = set()
        for package in runtime_packages:
            if not isinstance(package, dict):
                raise VerifyError("Detect It Easy Linux runtime package lock is invalid")
            package_name = package.get("name")
            package_files = package.get("runtimeFiles")
            notice_destination = package.get("noticeDestination")
            package_sources = package.get("sourceDependencies")
            if (
                not isinstance(package_name, str)
                or not package_name
                or package_name in package_names
                or not isinstance(package_files, list)
                or not package_files
                or not isinstance(notice_destination, str)
                or not isinstance(package_sources, list)
                or not package_sources
                or any(label not in dependency_labels for label in package_sources)
            ):
                raise VerifyError(
                    "Detect It Easy Linux runtime package lock is invalid"
                )
            package_names.add(package_name)
            for item in package_files:
                if not isinstance(item, dict) or not isinstance(
                    item.get("destination"), str
                ):
                    raise VerifyError(
                        f"Detect It Easy Linux runtime file lock is invalid: {package_name}"
                    )
                destination = PurePosixPath(item["destination"])
                if (
                    destination.is_absolute()
                    or len(destination.parts) != 1
                    or destination.name != item["destination"]
                ):
                    raise VerifyError(
                        f"Detect It Easy Linux runtime destination is invalid: {package_name}"
                    )
                runtime_names.append(destination.name)
            notice_relative = PurePosixPath(notice_destination)
            if (
                notice_relative.is_absolute()
                or not notice_relative.parts
                or notice_relative.parts[0] != "licenses"
                or ".." in notice_relative.parts
                or any(part in {"", "."} for part in notice_relative.parts)
            ):
                raise VerifyError(
                    f"Detect It Easy Linux package notice path is invalid: {package_name}"
                )
            notice = target_dir.joinpath(*notice_relative.parts)
            if notice.is_symlink() or not notice.is_file() or notice.stat().st_size <= 0:
                raise VerifyError(
                    f"Detect It Easy Linux package notice is missing: {package_name}"
                )
        if len(runtime_names) != len(set(runtime_names)):
            raise VerifyError("Detect It Easy Linux runtime names are duplicated")
        for name in runtime_names:
            runtime = target_dir / "die" / name
            if not runtime.is_file():
                raise VerifyError(f"Detect It Easy runtime is missing: {name}")
            verify_machine(runtime, target)

        needed_lock = artifact.get("elfNeeded")
        system_allowlist_value = artifact.get("elfSystemAllowlist")
        if not isinstance(needed_lock, dict) or not isinstance(
            system_allowlist_value, list
        ):
            raise VerifyError("Detect It Easy ELF dependency closure lock is missing")
        if (
            any(
                not isinstance(name, str)
                or not name
                or "/" in name
                or "\\" in name
                for name in system_allowlist_value
            )
        ):
            raise VerifyError("Detect It Easy ELF system allowlist is invalid")
        system_allowlist = set(system_allowlist_value)
        if system_allowlist != LINUX_ELF_SYSTEM_ALLOWLIST:
            raise VerifyError(
                "Detect It Easy ELF system allowlist differs from the reviewed baseline"
            )
        expected_files = {"diec", *runtime_names}
        if set(needed_lock) != expected_files:
            raise VerifyError(
                "Detect It Easy ELF closure file set differs from the staged runtime"
            )
        bundled_libraries = expected_files - {"diec"}
        observed_dependencies: set[str] = set()
        for name in sorted(expected_files):
            expected_needed = needed_lock.get(name)
            if not isinstance(expected_needed, list) or any(
                not isinstance(dependency, str) for dependency in expected_needed
            ):
                raise VerifyError(
                    f"Detect It Easy ELF dependency lock is invalid: {name}"
                )
            file_path = executable if name == "diec" else target_dir / "die" / name
            actual_needed, soname = elf_dynamic_metadata(file_path)
            if actual_needed != expected_needed:
                raise VerifyError(
                    f"Detect It Easy ELF dependencies differ for {name}; "
                    f"expected={expected_needed}, actual={actual_needed}"
                )
            expected_soname = None if name == "diec" else name
            if soname != expected_soname:
                raise VerifyError(
                    f"Detect It Easy ELF SONAME differs for {name}; "
                    f"expected={expected_soname!r}, actual={soname!r}"
                )
            observed_dependencies.update(actual_needed)
        unresolved = observed_dependencies - bundled_libraries - system_allowlist
        if unresolved:
            raise VerifyError(
                "Detect It Easy ELF closure has unbundled dependencies: "
                f"{sorted(unresolved)}"
            )
        unused_libraries = bundled_libraries - observed_dependencies
        if unused_libraries:
            raise VerifyError(
                "Detect It Easy bundle contains unreferenced runtime libraries: "
                f"{sorted(unused_libraries)}"
            )
    else:
        version = artifact["frameworkVersion"]
        for name in artifact["frameworks"]:
            runtime = (
                target_dir
                / "Frameworks"
                / f"{name}.framework"
                / "Versions"
                / version
                / name
            )
            if not runtime.is_file():
                raise VerifyError(f"Detect It Easy framework is missing: {name}")
            verify_machine(runtime, target)


def run_smoke_checks(target_dir: Path, target: str) -> None:
    environment = os.environ.copy()
    # Do not let caller-controlled loaders, injected libraries, or Qt/QML search
    # paths make an incomplete bundle appear healthy (or execute foreign code).
    for name in list(environment):
        if name.startswith(("LD_", "DYLD_", "QT_", "QML")):
            environment.pop(name, None)
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
    if target.startswith("linux-"):
        environment["LD_LIBRARY_PATH"] = str(target_dir / "die")
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

    diec = target_dir / "die" / (
        "diec.exe" if target.startswith("windows-") else "diec"
    )
    die_scan = subprocess.run(
        [str(diec), "-D", str(target_dir / "die" / "db"), "-j", str(diec)],
        cwd=target_dir,
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=30,
        check=False,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    try:
        die_payload = json.loads(die_scan.stdout)
    except json.JSONDecodeError as error:
        raise VerifyError(
            "Detect It Easy failed to return JSON with its bundled database: "
            f"{die_scan.stdout[:500]} {die_scan.stderr[:500]}"
        ) from error
    if die_scan.returncode != 0 or not isinstance(die_payload, dict):
        raise VerifyError(
            "Detect It Easy failed to scan with its bundled database: "
            f"{die_scan.stderr[:1000]}"
        )

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
