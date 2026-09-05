#!/usr/bin/env python3
"""Build a reviewed, target-specific Mobius command-line tool bundle."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path, PurePosixPath
from typing import Any


TARGETS = {
    "windows-x86_64": ("windows", "amd64"),
    "linux-x86_64": ("linux", "amd64"),
    "macos-aarch64": ("darwin", "arm64"),
    "macos-x86_64": ("darwin", "amd64"),
}
MAX_ARCHIVE_BYTES = 512 * 1024 * 1024
MAX_MEMBER_BYTES = 256 * 1024 * 1024
MAX_EXTRACTED_BYTES = 2 * 1024 * 1024 * 1024
DOWNLOAD_ATTEMPTS = 3


class BundleError(RuntimeError):
    pass


def sha256_file(file_path: Path) -> str:
    digest = hashlib.sha256()
    with file_path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def files_equal(left: Path, right: Path) -> bool:
    if left.stat().st_size != right.stat().st_size:
        return False
    with left.open("rb") as left_stream, right.open("rb") as right_stream:
        while True:
            left_block = left_stream.read(1024 * 1024)
            right_block = right_stream.read(1024 * 1024)
            if left_block != right_block:
                return False
            if not left_block:
                return True


def checked_artifact(component: str, value: dict[str, Any]) -> dict[str, Any]:
    required = ("url", "size", "sha256", "archive")
    if any(name not in value for name in required):
        raise BundleError(f"{component}: incomplete locked artifact")
    if not str(value["url"]).startswith("https://"):
        raise BundleError(f"{component}: only HTTPS sources are accepted")
    if not isinstance(value["size"], int) or not 0 < value["size"] <= MAX_ARCHIVE_BYTES:
        raise BundleError(f"{component}: invalid locked archive size")
    if not re.fullmatch(r"[0-9a-f]{64}", str(value["sha256"])):
        raise BundleError(f"{component}: invalid locked SHA-256")
    if value["archive"] not in {"zip", "tar.gz", "tar.xz", "tar.bz2"}:
        raise BundleError(f"{component}: unsupported archive type {value['archive']}")
    return value


def verify_download(file_path: Path, component: str, artifact: dict[str, Any]) -> None:
    actual_size = file_path.stat().st_size
    if actual_size != artifact["size"]:
        raise BundleError(
            f"{component}: size mismatch for {file_path.name}; "
            f"expected {artifact['size']}, got {actual_size}"
        )
    actual_hash = sha256_file(file_path)
    if actual_hash != artifact["sha256"]:
        raise BundleError(
            f"{component}: SHA-256 mismatch for {file_path.name}; "
            f"expected {artifact['sha256']}, got {actual_hash}"
        )


def download_locked(
    component: str, artifact: dict[str, Any], cache_dir: Path
) -> Path:
    artifact = checked_artifact(component, artifact)
    cache_dir.mkdir(parents=True, exist_ok=True)
    url_name = Path(urllib.parse.urlparse(artifact["url"]).path).name
    suffix = url_name or f"{component}.{artifact['archive']}"
    destination = cache_dir / f"{artifact['sha256'][:16]}-{suffix}"
    if destination.is_file():
        try:
            verify_download(destination, component, artifact)
            print(f"Using verified cache for {component}: {destination.name}")
            return destination
        except BundleError:
            destination.unlink()

    error: Exception | None = None
    for attempt in range(1, DOWNLOAD_ATTEMPTS + 1):
        temporary = cache_dir / f".{destination.name}.partial-{os.getpid()}-{attempt}"
        try:
            request = urllib.request.Request(
                artifact["url"], headers={"User-Agent": "Mobius-Tool-Bundler/1"}
            )
            with urllib.request.urlopen(request, timeout=90) as response, temporary.open(
                "wb"
            ) as output:
                written = 0
                while True:
                    block = response.read(1024 * 1024)
                    if not block:
                        break
                    written += len(block)
                    if written > artifact["size"] or written > MAX_ARCHIVE_BYTES:
                        raise BundleError(f"{component}: download exceeded its locked size")
                    output.write(block)
            verify_download(temporary, component, artifact)
            os.replace(temporary, destination)
            print(f"Downloaded and verified {component}: {destination.name}")
            return destination
        except (OSError, urllib.error.URLError, BundleError) as caught:
            error = caught
            temporary.unlink(missing_ok=True)
            if attempt < DOWNLOAD_ATTEMPTS:
                time.sleep(attempt * 2)
    raise BundleError(f"{component}: download failed after retries: {error}")


def safe_relative_path(raw_name: str) -> PurePosixPath:
    normalized = raw_name.replace("\\", "/")
    candidate = PurePosixPath(normalized)
    if (
        not normalized
        or normalized.startswith("/")
        or candidate.is_absolute()
        or ".." in candidate.parts
        or any(part in {"", "."} for part in candidate.parts)
    ):
        raise BundleError(f"Unsafe archive member path: {raw_name!r}")
    return candidate


def safe_extract_zip(archive: Path, destination: Path) -> None:
    total = 0
    with zipfile.ZipFile(archive) as source:
        for member in source.infolist():
            relative = safe_relative_path(member.filename.rstrip("/"))
            unix_mode = member.external_attr >> 16
            if stat.S_ISLNK(unix_mode):
                raise BundleError(f"Archive contains a symlink: {member.filename}")
            if member.file_size > MAX_MEMBER_BYTES:
                raise BundleError(f"Archive member is too large: {member.filename}")
            total += member.file_size
            if total > MAX_EXTRACTED_BYTES:
                raise BundleError("Archive expands beyond the configured safety limit")
            output = destination.joinpath(*relative.parts)
            if member.is_dir():
                output.mkdir(parents=True, exist_ok=True)
                continue
            output.parent.mkdir(parents=True, exist_ok=True)
            with source.open(member) as input_stream, output.open("wb") as output_stream:
                shutil.copyfileobj(input_stream, output_stream)
            if unix_mode & 0o111:
                output.chmod(0o755)


def safe_extract_tar(archive: Path, destination: Path, archive_type: str) -> None:
    modes = {"tar.gz": "r:gz", "tar.xz": "r:xz", "tar.bz2": "r:bz2"}
    try:
        mode = modes[archive_type]
    except KeyError as error:
        raise BundleError(f"Unsupported tar archive type: {archive_type}") from error
    total = 0
    with tarfile.open(archive, mode) as source:
        for member in source:
            relative = safe_relative_path(member.name.rstrip("/"))
            if member.issym() or member.islnk() or member.isdev():
                raise BundleError(f"Archive contains a link or device: {member.name}")
            if not member.isdir() and not member.isfile():
                raise BundleError(f"Archive contains an unsupported member: {member.name}")
            if member.size > MAX_MEMBER_BYTES:
                raise BundleError(f"Archive member is too large: {member.name}")
            total += member.size
            if total > MAX_EXTRACTED_BYTES:
                raise BundleError("Archive expands beyond the configured safety limit")
            output = destination.joinpath(*relative.parts)
            if member.isdir():
                output.mkdir(parents=True, exist_ok=True)
                continue
            extracted = source.extractfile(member)
            if extracted is None:
                raise BundleError(f"Unable to read archive member: {member.name}")
            output.parent.mkdir(parents=True, exist_ok=True)
            with extracted, output.open("wb") as output_stream:
                shutil.copyfileobj(extracted, output_stream)
            output.chmod(0o755 if member.mode & 0o111 else 0o644)


def extract_locked(
    component: str,
    artifact: dict[str, Any],
    cache_dir: Path,
    work_dir: Path,
) -> Path:
    archive = download_locked(component, artifact, cache_dir)
    destination = work_dir / re.sub(r"[^a-zA-Z0-9_.-]", "-", component)
    destination.mkdir(parents=True, exist_ok=False)
    if artifact["archive"] == "zip":
        safe_extract_zip(archive, destination)
    else:
        safe_extract_tar(archive, destination, artifact["archive"])
    return destination


def extract_locked_regular_member(
    component: str,
    artifact: dict[str, Any],
    member_path: str,
    cache_dir: Path,
    destination: Path,
) -> Path:
    """Extract one explicitly locked regular member without following links."""
    artifact = checked_artifact(component, artifact)
    root_value = artifact.get("root")
    if not isinstance(root_value, str):
        raise BundleError(f"{component}: locked archive root is missing")
    root = safe_relative_path(root_value)
    relative = safe_relative_path(member_path)
    member_name = PurePosixPath(*root.parts, *relative.parts).as_posix()
    archive_path = download_locked(component, artifact, cache_dir)
    destination.parent.mkdir(parents=True, exist_ok=True)

    if artifact["archive"] == "zip":
        with zipfile.ZipFile(archive_path) as archive:
            try:
                member = archive.getinfo(member_name)
            except KeyError as error:
                raise BundleError(
                    f"{component}: archive member is missing: {member_name}"
                ) from error
            unix_mode = member.external_attr >> 16
            if member.is_dir() or stat.S_ISLNK(unix_mode):
                raise BundleError(
                    f"{component}: locked member is not a regular file: {member_name}"
                )
            if member.file_size > MAX_MEMBER_BYTES:
                raise BundleError(f"{component}: locked member is too large")
            with archive.open(member) as source, destination.open("wb") as output:
                shutil.copyfileobj(source, output)
    else:
        modes = {"tar.gz": "r:gz", "tar.xz": "r:xz", "tar.bz2": "r:bz2"}
        with tarfile.open(archive_path, modes[artifact["archive"]]) as archive:
            try:
                member = archive.getmember(member_name)
            except KeyError as error:
                raise BundleError(
                    f"{component}: archive member is missing: {member_name}"
                ) from error
            if not member.isfile() or member.size > MAX_MEMBER_BYTES:
                raise BundleError(
                    f"{component}: locked member is not a bounded regular file: "
                    f"{member_name}"
                )
            extracted = archive.extractfile(member)
            if extracted is None:
                raise BundleError(f"{component}: unable to read {member_name}")
            with extracted, destination.open("wb") as output:
                shutil.copyfileobj(extracted, output)
    destination.chmod(0o644)
    return destination


def require_regular(file_path: Path, label: str) -> Path:
    if file_path.is_symlink() or not file_path.is_file():
        raise BundleError(f"{label}: expected regular file is missing: {file_path}")
    return file_path


def normalized_git_patch(source: Path, destination: Path, label: str) -> Path:
    """Copy a Git patch with deterministic LF endings for Git for Windows."""
    require_regular(source, label)
    try:
        data = source.read_bytes()
    except OSError as error:
        raise BundleError(f"{label}: unable to read patch: {error}") from error
    if not data or len(data) > 1024 * 1024:
        raise BundleError(f"{label}: patch must be between 1 byte and 1 MiB")
    data = data.replace(b"\r\n", b"\n")
    if b"\r" in data or b"\x00" in data:
        raise BundleError(f"{label}: patch contains unsupported control bytes")
    try:
        destination.write_bytes(data)
    except OSError as error:
        raise BundleError(f"{label}: unable to stage patch: {error}") from error
    return destination


def locked_root(extracted: Path, artifact: dict[str, Any], label: str) -> Path:
    root_value = artifact.get("root")
    if not isinstance(root_value, str):
        raise BundleError(f"{label}: locked archive root is missing")
    relative = safe_relative_path(root_value)
    root = extracted.joinpath(*relative.parts)
    if root.is_symlink() or not root.is_dir():
        raise BundleError(f"{label}: locked archive root is missing: {root_value}")
    return root


def verify_locked_regular(
    file_path: Path, label: str, expected: dict[str, Any]
) -> Path:
    file_path = require_regular(file_path, label)
    expected_size = expected.get("size")
    expected_hash = expected.get("sha256")
    if not isinstance(expected_size, int) or expected_size <= 0:
        raise BundleError(f"{label}: invalid locked file size")
    if not isinstance(expected_hash, str) or not re.fullmatch(
        r"[0-9a-f]{64}", expected_hash
    ):
        raise BundleError(f"{label}: invalid locked file SHA-256")
    if file_path.stat().st_size != expected_size:
        raise BundleError(
            f"{label}: size mismatch; expected {expected_size}, "
            f"got {file_path.stat().st_size}"
        )
    actual_hash = sha256_file(file_path)
    if actual_hash != expected_hash:
        raise BundleError(
            f"{label}: SHA-256 mismatch; expected {expected_hash}, got {actual_hash}"
        )
    return file_path


class Stager:
    def __init__(self, output: Path):
        self.output = output
        self.file_components: dict[str, str] = {}

    def copy(
        self,
        source: Path,
        relative: str,
        component: str,
        executable: bool = False,
    ) -> Path:
        source = require_regular(source, component)
        relative_path = safe_relative_path(relative)
        destination = self.output.joinpath(*relative_path.parts)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
        destination.chmod(0o755 if executable else 0o644)
        self.file_components[relative_path.as_posix()] = component
        return destination

    def write_text(self, relative: str, value: str, component: str) -> Path:
        relative_path = safe_relative_path(relative)
        destination = self.output.joinpath(*relative_path.parts)
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(value, encoding="utf-8", newline="\n")
        destination.chmod(0o644)
        self.file_components[relative_path.as_posix()] = component
        return destination


def stage_locked_source_licenses(
    label: str,
    component: dict[str, Any],
    stager: Stager,
    cache: Path,
    work: Path,
    owner: str = "scrcpy",
) -> dict[str, Path]:
    source_artifact = checked_artifact(f"{label} source", component["source"])
    license_files = component.get("licenseFiles")
    if not isinstance(license_files, list) or not license_files:
        raise BundleError(f"{label}: no locked source license files")
    extracted_files: dict[str, Path] = {}
    license_work = work / f"{re.sub(r'[^A-Za-z0-9_.-]', '-', label)}-licenses"
    license_work.mkdir(parents=True, exist_ok=False)
    for index, item in enumerate(license_files):
        if not isinstance(item, dict):
            raise BundleError(f"{label}: invalid source license entry")
        source_value = item.get("path")
        destination_value = item.get("destination")
        if not isinstance(source_value, str) or not isinstance(
            destination_value, str
        ):
            raise BundleError(f"{label}: incomplete source license entry")
        source_relative = safe_relative_path(source_value)
        destination_relative = safe_relative_path(destination_value)
        if not destination_relative.parts or destination_relative.parts[0] != "licenses":
            raise BundleError(f"{label}: source license must be staged under licenses/")
        source_file = extract_locked_regular_member(
            f"{label} source",
            source_artifact,
            source_relative.as_posix(),
            cache,
            license_work / f"{index:02d}-{source_relative.name}",
        )
        if "size" in item or "sha256" in item:
            source_file = verify_locked_regular(
                source_file,
                f"{label} source license {source_value}",
                item,
            )
        stager.copy(
            source_file,
            destination_relative.as_posix(),
            owner,
        )
        extracted_files[source_relative.as_posix()] = source_file
    return extracted_files


def stage_scrcpy(
    lock: dict[str, Any], target: str, stager: Stager, cache: Path, work: Path
) -> None:
    component = lock["components"]["scrcpy"]
    artifact = checked_artifact("scrcpy", component["targets"][target])
    extracted = extract_locked("scrcpy", artifact, cache, work)
    root = locked_root(extracted, artifact, "scrcpy")
    source_licenses = stage_locked_source_licenses(
        "scrcpy", component, stager, cache, work
    )
    executable_suffix = ".exe" if target.startswith("windows-") else ""
    runtime_components = {
        f"scrcpy{executable_suffix}": "scrcpy",
        "scrcpy-server": "scrcpy",
        "scrcpy.png": "scrcpy",
        "disconnected.png": "scrcpy",
    }
    if target.startswith("windows-"):
        runtime_components.update(
            {
                "SDL3.dll": "scrcpy",
                "libusb-1.0.dll": "scrcpy",
                "avcodec-62.dll": "scrcpy",
                "avformat-62.dll": "scrcpy",
                "avutil-60.dll": "scrcpy",
                "swresample-6.dll": "scrcpy",
            }
        )
    for name, owner in runtime_components.items():
        stager.copy(
            root / name,
            name,
            owner,
            executable=name == f"scrcpy{executable_suffix}",
        )

    license_name = "LICENSE.txt" if target.startswith("windows-") else "LICENSE"
    portable_license = require_regular(root / license_name, "scrcpy portable license")
    source_license = require_regular(
        source_licenses["LICENSE"], "scrcpy source license"
    )
    if sha256_file(portable_license) != sha256_file(source_license):
        raise BundleError("scrcpy portable and source licenses do not match")

    platform_tools = component["androidPlatformTools"]
    platform_artifact = checked_artifact(
        "Android Platform Tools", platform_tools["targets"][target]
    )
    platform_extracted = extract_locked(
        "android-platform-tools", platform_artifact, cache, work
    )
    platform_root = locked_root(
        platform_extracted, platform_artifact, "Android Platform Tools"
    )
    matched_files = platform_artifact.get("matchedFiles")
    if not isinstance(matched_files, list) or not matched_files:
        raise BundleError("Android Platform Tools: no locked ADB files to compare")
    matched_report: list[dict[str, Any]] = []
    for expected in matched_files:
        if not isinstance(expected, dict) or not isinstance(expected.get("path"), str):
            raise BundleError("Android Platform Tools: invalid matched file entry")
        relative = safe_relative_path(expected["path"])
        platform_file = verify_locked_regular(
            platform_root.joinpath(*relative.parts),
            f"Android Platform Tools {expected['path']}",
            expected,
        )
        portable_file = verify_locked_regular(
            root.joinpath(*relative.parts),
            f"scrcpy portable {expected['path']}",
            expected,
        )
        if not files_equal(platform_file, portable_file):
            raise BundleError(
                f"scrcpy portable {expected['path']} does not match "
                "the locked Android Platform Tools archive"
            )
        stager.copy(
            portable_file,
            relative.as_posix(),
            "scrcpy",
            executable=relative.name == f"adb{executable_suffix}",
        )
        matched_report.append(
            {
                "path": relative.as_posix(),
                "size": expected["size"],
                "sha256": expected["sha256"],
            }
        )

    notice = platform_artifact.get("notice")
    if not isinstance(notice, dict) or not isinstance(notice.get("path"), str):
        raise BundleError("Android Platform Tools: locked NOTICE is missing")
    notice_relative = safe_relative_path(notice["path"])
    notice_file = verify_locked_regular(
        platform_root.joinpath(*notice_relative.parts),
        "Android Platform Tools NOTICE",
        notice,
    )
    stager.copy(
        notice_file,
        "licenses/android-platform-tools-NOTICE.txt",
        "scrcpy",
    )

    dependencies = component.get("portableDependencies")
    if not isinstance(dependencies, dict):
        raise BundleError("scrcpy portable dependency lock is missing")
    dependency_report: dict[str, Any] = {}
    dependency_names = ["ffmpeg", "sdl", "libusb", "dav1d"]
    if target.startswith("windows-"):
        dependency_names.append("mingw-w64")
    for name in dependency_names:
        dependency = dependencies.get(name)
        if not isinstance(dependency, dict):
            raise BundleError(f"scrcpy portable dependency is missing: {name}")
        stage_locked_source_licenses(
            f"scrcpy-{name}", dependency, stager, cache, work
        )
        dependency_report[name] = {
            "version": dependency["version"],
            "license": dependency["license"],
            "projectUrl": dependency["projectUrl"],
            "source": dependency["source"],
            "linkage": dependency["linkage"][target],
        }

    zlib = dependencies.get("zlib")
    if not isinstance(zlib, dict) or not isinstance(zlib.get("targets"), dict):
        raise BundleError("scrcpy zlib dependency lock is missing")
    zlib_target = zlib["targets"].get(target)
    if not isinstance(zlib_target, dict):
        raise BundleError(f"scrcpy zlib dependency is missing for {target}")
    if "source" in zlib_target:
        stage_locked_source_licenses(
            f"scrcpy-zlib-{zlib_target['version']}",
            zlib_target,
            stager,
            cache,
            work,
        )
    else:
        stager.write_text(
            "licenses/scrcpy-zlib-system.txt",
            "scrcpy 4.1 uses the operating system libz on this target; "
            "no zlib binary is included in the portable archive.\n",
            "scrcpy",
        )
    dependency_report["zlib"] = {
        "version": zlib_target["version"],
        "license": zlib["license"],
        "projectUrl": zlib["projectUrl"],
        "source": zlib_target.get("source"),
        "linkage": zlib_target["linkage"],
    }
    stager.write_text(
        "licenses/scrcpy-portable-provenance.json",
        json.dumps(
            {
                "scrcpyVersion": component["version"],
                "target": target,
                "portableArchive": artifact,
                "sourceArchive": component["source"],
                "androidPlatformTools": {
                    "version": platform_tools["version"],
                    "license": platform_tools["license"],
                    "archive": platform_artifact,
                    "matchedFiles": matched_report,
                },
                "portableDependencies": dependency_report,
            },
            indent=2,
            ensure_ascii=False,
        )
        + "\n",
        "scrcpy",
    )


def stage_aapt2(
    lock: dict[str, Any], target: str, stager: Stager, cache: Path, work: Path
) -> None:
    component = lock["components"]["aapt2"]
    artifact = checked_artifact("aapt2", component["targets"][target])
    extracted = extract_locked("aapt2", artifact, cache, work)
    stager.copy(
        extracted / artifact["binary"],
        artifact["binary"],
        "aapt2",
        executable=True,
    )
    stager.copy(
        extracted / "NOTICE",
        "licenses/aapt2-NOTICE.txt",
        "aapt2",
    )


def build_environment(target: str) -> dict[str, str]:
    environment = os.environ.copy()
    if not target.startswith("macos-"):
        return environment

    configured = environment.get("MACOSX_DEPLOYMENT_TARGET")
    if configured:
        match = re.fullmatch(r"(\d+)(?:\.(\d+))?(?:\.(\d+))?", configured)
        if match is None:
            raise BundleError(
                "MACOSX_DEPLOYMENT_TARGET must be a numeric macOS version"
            )
        version = tuple(int(value or 0) for value in match.groups())
        if version <= (12, 0, 0):
            return environment
    environment["MACOSX_DEPLOYMENT_TARGET"] = "12.0"
    return environment


def run(
    command: list[str],
    cwd: Path,
    env: dict[str, str] | None = None,
    *,
    print_output: bool = True,
) -> str:
    shown = " ".join(command)
    print(f"+ ({cwd}) {shown}")
    result = subprocess.run(
        command,
        cwd=cwd,
        env=env,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if result.stdout and print_output:
        print(result.stdout.rstrip())
    if result.returncode != 0:
        raise BundleError(f"Command failed with exit code {result.returncode}: {shown}")
    return result.stdout


def decode_concatenated_json(value: str) -> list[dict[str, Any]]:
    decoder = json.JSONDecoder()
    position = 0
    decoded: list[dict[str, Any]] = []
    while position < len(value):
        while position < len(value) and value[position].isspace():
            position += 1
        if position >= len(value):
            break
        item, position = decoder.raw_decode(value, position)
        decoded.append(item)
    return decoded


def license_candidates(module_dir: Path) -> list[Path]:
    names = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "PATENTS")
    files: list[Path] = []
    for child in sorted(module_dir.iterdir(), key=lambda path: path.name.lower()):
        if (
            child.is_file()
            and not child.is_symlink()
            and child.stat().st_size <= 1024 * 1024
            and child.name.upper().startswith(names)
        ):
            files.append(child)
    return files


def external_tool_path(value: str) -> Path:
    """Translate native Windows paths emitted by Go when running under MSYS."""
    candidate = Path(value)
    if candidate.is_dir() or not re.match(r"^[A-Za-z]:[\\/]", value):
        return candidate
    cygpath = shutil.which("cygpath")
    if cygpath is None:
        return candidate
    translated = subprocess.run(
        [cygpath, "-u", value],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if translated.returncode != 0 or not translated.stdout.strip():
        return candidate
    return Path(translated.stdout.strip())


def collect_go_module_licenses(
    modules_output: str,
    linked_modules: set[tuple[str, str]],
    stager: Stager,
) -> list[dict[str, Any]]:
    report: list[dict[str, Any]] = []
    used_names: set[str] = set()
    for module in decode_concatenated_json(modules_output):
        if module.get("Main"):
            continue
        module_path = str(module.get("Path", "unknown"))
        version = str(module.get("Version", "unknown"))
        if (module_path, version) not in linked_modules:
            continue
        module_dir_value = module.get("Dir")
        copied: list[str] = []
        if module_dir_value:
            module_dir = external_tool_path(str(module_dir_value))
            if module_dir.is_dir():
                base = re.sub(r"[^A-Za-z0-9_.-]+", "_", f"{module_path}@{version}")
                base = base[-150:]
                for index, source in enumerate(license_candidates(module_dir), start=1):
                    destination_name = f"{base}-{index}-{source.name}.txt"
                    counter = 2
                    while destination_name.lower() in used_names:
                        destination_name = (
                            f"{base}-{index}-{counter}-{source.name}.txt"
                        )
                        counter += 1
                    used_names.add(destination_name.lower())
                    relative = f"licenses/go-ios-dependencies/{destination_name}"
                    stager.copy(source, relative, "go-ios")
                    copied.append(relative)
        if not copied:
            raise BundleError(
                f"go-ios linked module has no reviewable root license file: "
                f"{module_path}@{version}"
            )
        report.append({"path": module_path, "version": version, "licenseFiles": copied})
    return report


def parse_linked_go_modules(build_info: str) -> set[tuple[str, str]]:
    linked: set[tuple[str, str]] = set()
    for line in build_info.splitlines():
        fields = line.split("\t")
        if len(fields) >= 4 and fields[1] == "dep":
            linked.add((fields[2], fields[3]))
    if not linked:
        raise BundleError("go-ios build information did not list linked Go modules")
    return linked


def stage_go_ios(
    repo_root: Path,
    lock: dict[str, Any],
    target: str,
    stager: Stager,
    cache: Path,
    work: Path,
) -> None:
    component = lock["components"]["go-ios"]
    go_toolchain = component.get("goToolchain")
    if not isinstance(go_toolchain, dict):
        raise BundleError("go-ios: locked Go toolchain source is missing")
    if component.get("goVersion") != go_toolchain.get("version"):
        raise BundleError("go-ios: Go build and source-license versions differ")
    stage_locked_source_licenses(
        "go-toolchain",
        go_toolchain,
        stager,
        cache,
        work,
        owner="go-ios",
    )
    artifact = checked_artifact("go-ios source", component["source"])
    extracted = extract_locked("go-ios-source", artifact, cache, work)
    source = extracted / artifact["root"]
    patch_path = normalized_git_patch(
        repo_root / component["patch"], work / "go-ios-mobius.patch", "go-ios patch"
    )
    run(["git", "apply", "--check", str(patch_path)], source)
    run(["git", "apply", str(patch_path)], source)

    goos, goarch = TARGETS[target]
    environment = build_environment(target)
    environment.update(
        {
            "CGO_ENABLED": "0",
            "GOOS": goos,
            "GOARCH": goarch,
            "GOFLAGS": "-mod=readonly",
        }
    )
    go_version = run(["go", "version"], source, environment).split()
    expected_go_version = f"go{component['goVersion']}"
    if len(go_version) < 3 or go_version[2] != expected_go_version:
        actual = go_version[2] if len(go_version) >= 3 else "unknown"
        raise BundleError(
            f"go-ios requires {expected_go_version}; active Go is {actual}"
        )
    run(["go", "mod", "download"], source, environment)
    modules = run(
        ["go", "list", "-m", "-json", "all"],
        source,
        environment,
        print_output=False,
    )
    executable = "ios.exe" if target.startswith("windows-") else "ios"
    built = work / f"go-ios-{target}-{executable}"
    run(
        [
            "go",
            "build",
            "-trimpath",
            "-buildvcs=false",
            "-mod=readonly",
            "-ldflags=-s -w",
            "-o",
            str(built),
            ".",
        ],
        source,
        environment,
    )
    build_info = run(["go", "version", "-m", str(built)], source, environment)
    stager.copy(built, executable, "go-ios", executable=True)
    stager.copy(source / "LICENSE", "licenses/go-ios-LICENSE.txt", "go-ios")
    report = collect_go_module_licenses(
        modules, parse_linked_go_modules(build_info), stager
    )
    stager.write_text(
        "licenses/go-ios-modules.json",
        json.dumps(report, indent=2, ensure_ascii=False) + "\n",
        "go-ios",
    )
    stager.write_text(
        "licenses/go-ios-build-info.txt",
        build_info,
        "go-ios",
    )
    stager.copy(
        patch_path,
        "licenses/go-ios-1.3.2-mobius.patch",
        "go-ios",
    )


def stage_mobius_ssh(
    repo_root: Path,
    lock: dict[str, Any],
    target: str,
    stager: Stager,
    cache: Path,
    work: Path,
) -> None:
    component = lock["components"]["mobius-ssh"]
    if component.get("goVersion") != lock["components"]["go-ios"].get("goVersion"):
        raise BundleError("mobius-ssh: Go version must match the locked go-ios toolchain")
    source_path = component.get("sourcePath")
    if not isinstance(source_path, str):
        raise BundleError("mobius-ssh: first-party source path is missing")
    source_relative = safe_relative_path(source_path)
    source = repo_root.joinpath(*source_relative.parts)
    if source.is_symlink() or not source.is_dir():
        raise BundleError("mobius-ssh: first-party source directory is unavailable")
    for name in ("main.go", "main_test.go", "go.mod", "go.sum"):
        require_regular(source / name, f"mobius-ssh {name}")

    dependencies = component.get("dependencies")
    if not isinstance(dependencies, list) or not dependencies:
        raise BundleError("mobius-ssh: locked Go dependencies are missing")
    expected_linked: set[tuple[str, str]] = set()
    license_report: list[dict[str, Any]] = []
    for dependency in dependencies:
        if not isinstance(dependency, dict):
            raise BundleError("mobius-ssh: invalid dependency lock")
        module = dependency.get("module")
        version = dependency.get("version")
        used_by = dependency.get("usedBy")
        if (
            not isinstance(module, str)
            or not isinstance(version, str)
            or not isinstance(used_by, list)
        ):
            raise BundleError("mobius-ssh: incomplete dependency lock")
        if target not in used_by:
            continue
        expected_linked.add((module, version))
        extracted = stage_locked_source_licenses(
            f"mobius-ssh-{module}",
            dependency,
            stager,
            cache,
            work,
            owner="mobius-ssh",
        )
        license_report.append(
            {
                "path": module,
                "version": version,
                "licenseFiles": [
                    item["destination"] for item in dependency["licenseFiles"]
                ],
                "sourceLicenseSha256": {
                    name: sha256_file(file_path) for name, file_path in extracted.items()
                },
            }
        )

    goos, goarch = TARGETS[target]
    environment = build_environment(target)
    environment.update(
        {
            "CGO_ENABLED": "0",
            "GOOS": goos,
            "GOARCH": goarch,
            "GOFLAGS": "-mod=readonly",
        }
    )
    go_version = run(["go", "version"], source, environment).split()
    expected_go_version = f"go{component['goVersion']}"
    if len(go_version) < 3 or go_version[2] != expected_go_version:
        actual = go_version[2] if len(go_version) >= 3 else "unknown"
        raise BundleError(f"mobius-ssh requires {expected_go_version}; active Go is {actual}")
    run(["go", "mod", "download"], source, environment)
    run(["go", "test", "./..."], source, environment)
    modules_output = run(
        ["go", "list", "-m", "-json", "all"],
        source,
        environment,
        print_output=False,
    )
    modules = {
        (str(module.get("Path", "")), str(module.get("Version", ""))): module
        for module in decode_concatenated_json(modules_output)
        if not module.get("Main")
    }

    suffix = ".exe" if target.startswith("windows-") else ""
    built = work / f"mobius-ssh-{target}{suffix}"
    run(
        [
            "go",
            "build",
            "-trimpath",
            "-buildvcs=false",
            "-mod=readonly",
            "-ldflags=-s -w",
            "-o",
            str(built),
            ".",
        ],
        source,
        environment,
    )
    build_info = run(["go", "version", "-m", str(built)], source, environment)
    linked = parse_linked_go_modules(build_info)
    if linked != expected_linked:
        raise BundleError(
            "mobius-ssh linked dependency set differs from the target lock; "
            f"expected={sorted(expected_linked)}, actual={sorted(linked)}"
        )

    # The Go module checksum validates build inputs. Also compare each linked
    # module's cached root license to the independently locked upstream source
    # license, so the staged notices cannot silently drift from compiled code.
    for dependency, report in zip(
        (item for item in dependencies if target in item["usedBy"]),
        license_report,
        strict=True,
    ):
        module_key = (dependency["module"], dependency["version"])
        module = modules.get(module_key)
        if not module or not module.get("Dir"):
            raise BundleError(f"mobius-ssh linked module is unavailable: {module_key}")
        module_dir = external_tool_path(str(module["Dir"]))
        for license_item in dependency["licenseFiles"]:
            license_name = safe_relative_path(license_item["path"]).as_posix()
            cached_license = require_regular(
                module_dir.joinpath(*safe_relative_path(license_name).parts),
                f"mobius-ssh cached license {module_key}",
            )
            expected_hash = report["sourceLicenseSha256"].get(license_name)
            if sha256_file(cached_license) != expected_hash:
                raise BundleError(
                    f"mobius-ssh cached license differs from locked source: {module_key}"
                )

    stager.copy(built, f"ssh{suffix}", "mobius-ssh", executable=True)
    stager.copy(built, f"scp{suffix}", "mobius-ssh", executable=True)
    stager.write_text(
        "licenses/mobius-ssh-modules.json",
        json.dumps(license_report, indent=2, ensure_ascii=False) + "\n",
        "mobius-ssh",
    )
    stager.write_text(
        "licenses/mobius-ssh-build-info.txt",
        build_info,
        "mobius-ssh",
    )


def stage_ffmpeg(
    lock: dict[str, Any],
    target: str,
    stager: Stager,
    cache: Path,
    work: Path,
) -> None:
    component = lock["components"]["ffmpeg"]
    artifact = checked_artifact("FFmpeg source", component["source"])
    extracted = extract_locked("ffmpeg-source", artifact, cache, work)
    source = extracted / artifact["root"]
    configure = [str(source / "configure"), *component["configure"]]
    environment = build_environment(target)
    if target.endswith("x86_64") and shutil.which("nasm") is None:
        print("nasm was not found; building the locked C-only FFmpeg fallback")
        configure.append("--disable-x86asm")
    run(configure, source, environment)
    jobs = max(1, min(os.cpu_count() or 2, 4))
    run(["make", f"-j{jobs}", "ffmpeg"], source, environment)
    executable = "ffmpeg.exe" if target.startswith("windows-") else "ffmpeg"
    built = source / executable
    stager.copy(built, executable, "ffmpeg", executable=True)
    stager.copy(
        source / "COPYING.LGPLv2.1",
        "licenses/ffmpeg-COPYING.LGPLv2.1.txt",
        "ffmpeg",
    )
    stager.copy(source / "LICENSE.md", "licenses/ffmpeg-LICENSE.md", "ffmpeg")
    stager.write_text(
        "licenses/ffmpeg-configure.txt",
        " ".join(component["configure"]) + "\n",
        "ffmpeg",
    )


def write_manifest(
    lock: dict[str, Any], target: str, stager: Stager, output: Path
) -> None:
    files: list[dict[str, Any]] = []
    for file_path in sorted(output.rglob("*")):
        if file_path.is_symlink():
            raise BundleError(f"Generated bundle contains a symlink: {file_path}")
        if not file_path.is_file() or file_path.name == "manifest.json":
            continue
        relative = file_path.relative_to(output).as_posix()
        files.append(
            {
                "path": relative,
                "component": stager.file_components.get(relative, "unknown"),
                "size": file_path.stat().st_size,
                "sha256": sha256_file(file_path),
                "executable": bool(file_path.stat().st_mode & 0o111),
            }
        )
    component_names = ["scrcpy", "aapt2", "go-ios", "mobius-ssh", "ffmpeg"]
    components = []
    for name in component_names:
        item = lock["components"][name]
        components.append(
            {
                "name": name,
                "version": item["version"],
                "license": item["license"],
                "projectUrl": item["projectUrl"],
            }
        )
    manifest = {
        "schemaVersion": 1,
        "bundleRevision": lock["bundleRevision"],
        "target": target,
        "components": components,
        "files": files,
    }
    destination = output / "manifest.json"
    destination.write_text(
        json.dumps(manifest, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    destination.chmod(0o644)


def validated_output(output: Path, target: str) -> Path:
    absolute = output.expanduser().resolve()
    if absolute.name != target or absolute.parent.name != "tools":
        raise BundleError(
            "Output must be the matching <...>/tools/<target> directory; "
            f"refusing unsafe target {absolute}"
        )
    return absolute


def load_lock(file_path: Path) -> dict[str, Any]:
    try:
        value = json.loads(file_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise BundleError(f"Unable to load toolchain lock: {error}") from error
    if value.get("schemaVersion") != 1 or not isinstance(value.get("components"), dict):
        raise BundleError("Unsupported or incomplete toolchain lock")
    return value


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--lock", type=Path, default=Path("packaging/toolchain.lock.json"))
    parser.add_argument("--target", required=True, choices=sorted(TARGETS))
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--cache", type=Path, default=Path(".cache/tool-bundles"))
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    lock_path = args.lock if args.lock.is_absolute() else repo_root / args.lock
    cache = args.cache if args.cache.is_absolute() else repo_root / args.cache
    output = validated_output(args.output, args.target)
    lock = load_lock(lock_path)

    if output.exists():
        shutil.rmtree(output)
    output.mkdir(parents=True, exist_ok=False)
    stager = Stager(output)
    try:
        with tempfile.TemporaryDirectory(prefix="mobius-tool-bundle-") as temporary:
            work = Path(temporary)
            stage_scrcpy(lock, args.target, stager, cache, work)
            stage_aapt2(lock, args.target, stager, cache, work)
            stage_go_ios(repo_root, lock, args.target, stager, cache, work)
            stage_mobius_ssh(repo_root, lock, args.target, stager, cache, work)
            stage_ffmpeg(lock, args.target, stager, cache, work)
        notices = repo_root / "src-tauri/resources/tools/THIRD_PARTY_NOTICES.txt"
        stager.copy(
            notices,
            "licenses/Mobius-THIRD-PARTY-NOTICES.txt",
            "mobius",
        )
        write_manifest(lock, args.target, stager, output)
    except Exception:
        shutil.rmtree(output, ignore_errors=True)
        raise

    print(f"Prepared {args.target} tool bundle at {output}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except BundleError as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
