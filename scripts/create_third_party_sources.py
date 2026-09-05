#!/usr/bin/env python3
"""Create the source-compliance companion archive for a Mobius release."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sys
import tarfile
import tempfile
from pathlib import Path
from typing import Any

from prepare_tool_bundle import (
    BundleError,
    checked_artifact,
    download_locked,
    extract_locked_regular_member,
    extract_locked_regular_members,
    extract_locked_regular_members_named,
    extract_locked_regular_tree,
    qt_attribution_license_references,
    require_regular,
    safe_relative_path,
    sha256_file,
    verify_locked_regular,
)


def add_regular(archive: tarfile.TarFile, source: Path, relative: str) -> None:
    if source.is_symlink() or not source.is_file():
        raise BundleError(f"Source package input is not a regular file: {source}")
    info = archive.gettarinfo(str(source), arcname=f"mobius-third-party-sources/{relative}")
    info.uid = 0
    info.gid = 0
    info.uname = "root"
    info.gname = "root"
    info.mtime = 0
    info.mode = 0o644
    with source.open("rb") as stream:
        archive.addfile(info, stream)


def write_text(destination: Path, value: str) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(value, encoding="utf-8", newline="\n")
    destination.chmod(0o644)


def copy_regular(source: Path, destination: Path, label: str) -> None:
    require_regular(source, label)
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    destination.chmod(0o644)


def source_records(lock: dict[str, Any]) -> list[dict[str, Any]]:
    components = lock["components"]
    scrcpy = components["scrcpy"]
    dependencies = scrcpy["portableDependencies"]
    all_targets = [
        "windows-x86_64",
        "linux-x86_64",
        "macos-aarch64",
        "macos-x86_64",
    ]
    records: list[dict[str, Any]] = [
        {
            "label": "scrcpy",
            "version": scrcpy["version"],
            "license": scrcpy["license"],
            "projectUrl": scrcpy["projectUrl"],
            "source": scrcpy["source"],
            "licenseFiles": scrcpy["licenseFiles"],
            "usedBy": all_targets,
        }
    ]
    for name in ("ffmpeg", "sdl", "libusb", "dav1d", "mingw-w64"):
        dependency = dependencies.get(name)
        if dependency is None:
            continue
        records.append(
            {
                "label": f"scrcpy-{name}",
                "version": dependency["version"],
                "license": dependency["license"],
                "projectUrl": dependency["projectUrl"],
                "source": dependency["source"],
                "licenseFiles": dependency["licenseFiles"],
                "usedBy": sorted(dependency["linkage"]),
                "linkage": dependency["linkage"],
            }
        )

    zlib = dependencies["zlib"]
    seen_zlib_sources: set[str] = set()
    for _, target_value in sorted(zlib["targets"].items()):
        source = target_value.get("source")
        if source is None:
            continue
        digest = source["sha256"]
        if digest in seen_zlib_sources:
            continue
        seen_zlib_sources.add(digest)
        used_by = sorted(
            candidate
            for candidate, value in zlib["targets"].items()
            if value.get("source", {}).get("sha256") == digest
        )
        records.append(
            {
                "label": f"scrcpy-zlib-{target_value['version']}",
                "version": target_value["version"],
                "license": zlib["license"],
                "projectUrl": zlib["projectUrl"],
                "source": source,
                "licenseFiles": target_value["licenseFiles"],
                "usedBy": used_by,
                "linkage": {
                    target: zlib["targets"][target]["linkage"] for target in used_by
                },
            }
        )

    for name in ("ffmpeg", "go-ios"):
        component = components[name]
        record = {
            "label": name,
            "version": component["version"],
            "license": component["license"],
            "projectUrl": component["projectUrl"],
            "source": component["source"],
            "licenseFiles": component["licenseFiles"],
            "usedBy": all_targets,
        }
        if "patch" in component:
            record["patch"] = component["patch"]
        if "targetPatches" in component:
            record["targetPatches"] = component["targetPatches"]
        if "targetBuildRequirements" in component:
            record["targetBuildRequirements"] = component[
                "targetBuildRequirements"
            ]
        records.append(record)

    diec = components["diec"]
    records.append(
        {
            "label": "diec",
            "version": diec["version"],
            "license": diec["license"],
            "projectUrl": diec["projectUrl"],
            "source": diec["source"],
            "licenseFiles": diec["licenseFiles"],
            "usedBy": all_targets,
        }
    )
    die_targets = diec["targets"]
    for label, dependency in sorted(diec["sourceDependencies"].items()):
        used_by = sorted(
            target
            for target, target_value in die_targets.items()
            if label in target_value["sourceDependencies"]
        )
        if not used_by:
            raise BundleError(f"Unused Detect It Easy source dependency: {label}")
        records.append(
            {
                "label": f"diec-{label}",
                "version": dependency["version"],
                "license": dependency["license"],
                "projectUrl": dependency["projectUrl"],
                "source": dependency["source"],
                "licenseFiles": dependency["licenseFiles"],
                "usedBy": used_by,
                "linkage": {
                    target: dependency.get("linkage", "shared")
                    for target in used_by
                },
                "qtAttributions": dependency.get("qtAttributions"),
                "qtAttributionDirectory": label,
            }
        )
    go_toolchain = components["go-ios"]["goToolchain"]
    records.append(
        {
            "label": "go-toolchain",
            "version": go_toolchain["version"],
            "license": go_toolchain["license"],
            "projectUrl": go_toolchain["projectUrl"],
            "source": go_toolchain["source"],
            "licenseFiles": go_toolchain["licenseFiles"],
            "usedBy": all_targets,
            "linkage": {target: "static-runtime" for target in all_targets},
        }
    )
    for dependency in components["mobius-ssh"]["dependencies"]:
        records.append(
            {
                "label": f"mobius-ssh-{dependency['module']}",
                "version": dependency["version"],
                "license": dependency["license"],
                "projectUrl": dependency["projectUrl"],
                "source": dependency["source"],
                "licenseFiles": dependency["licenseFiles"],
                "usedBy": dependency["usedBy"],
                "linkage": {
                    target: "static-go-module" for target in dependency["usedBy"]
                },
            }
        )
    return records


def binary_only_runtime_records(lock: dict[str, Any]) -> list[dict[str, Any]]:
    windows = lock["components"]["diec"]["targets"]["windows-x86_64"]
    proprietary = windows["proprietaryRuntime"]
    return [
        {
            "name": proprietary["name"],
            "version": proprietary["version"],
            "license": proprietary["license"],
            "projectUrl": proprietary["projectUrl"],
            "licenseUrl": proprietary["licenseUrl"],
            "redistributableListUrl": proprietary["redistributableListUrl"],
            "usedBy": ["windows-x86_64"],
            "sourceAvailable": False,
            "redistributionConstraint": proprietary["redistributionConstraint"],
            "files": proprietary["files"],
            "containedIn": windows["url"],
        }
    ]


def stage_sources_and_licenses(
    lock: dict[str, Any],
    archive_root: Path,
    cache: Path,
    work: Path,
) -> list[dict[str, Any]]:
    index: list[dict[str, Any]] = []
    used_names: set[str] = set()
    for record in source_records(lock):
        label = str(record["label"])
        artifact = checked_artifact(f"{label} source", record["source"])
        file_name = artifact.get("fileName")
        if not isinstance(file_name, str):
            raise BundleError(f"{label} source: locked fileName is missing")
        file_relative = safe_relative_path(file_name)
        if len(file_relative.parts) != 1:
            raise BundleError(f"{label} source: fileName must be a basename")
        if file_relative.name.lower() in used_names:
            raise BundleError(f"Duplicate source archive name: {file_relative.name}")
        used_names.add(file_relative.name.lower())

        downloaded = download_locked(f"{label} source", artifact, cache)
        source_destination = archive_root / "sources" / file_relative.name
        copy_regular(downloaded, source_destination, f"{label} source")

        copied_licenses: list[str] = []
        license_files = record.get("licenseFiles")
        if not isinstance(license_files, list) or not license_files:
            raise BundleError(f"{label}: no locked source license files")
        license_work = work / f"{label}-licenses"
        license_work.mkdir(parents=True, exist_ok=False)
        for index_number, license_item in enumerate(license_files):
            if not isinstance(license_item, dict):
                raise BundleError(f"{label}: invalid license entry")
            source_value = license_item.get("path")
            destination_value = license_item.get("destination")
            if not isinstance(source_value, str) or not isinstance(destination_value, str):
                raise BundleError(f"{label}: incomplete license entry")
            source_relative = safe_relative_path(source_value)
            destination_relative = safe_relative_path(destination_value)
            if not destination_relative.parts or destination_relative.parts[0] != "licenses":
                raise BundleError(f"{label}: license destination must be under licenses/")
            license_source = extract_locked_regular_member(
                f"{label} source",
                artifact,
                source_relative.as_posix(),
                cache,
                license_work / f"{index_number:02d}-{source_relative.name}",
            )
            if "size" in license_item or "sha256" in license_item:
                license_source = verify_locked_regular(
                    license_source,
                    f"{label} license {source_value}",
                    license_item,
                )
            license_destination = archive_root.joinpath(*destination_relative.parts)
            if license_destination.exists():
                raise BundleError(f"Duplicate license destination: {destination_value}")
            copy_regular(license_source, license_destination, f"{label} license")
            copied_licenses.append(destination_relative.as_posix())

        copied_attributions: list[str] = []
        copied_attribution_licenses: list[str] = []
        attribution_specification = record.get("qtAttributions")
        if attribution_specification is not None:
            if not isinstance(attribution_specification, dict):
                raise BundleError(f"{label}: invalid Qt attribution lock")
            member_name = attribution_specification.get("memberName")
            expected_count = attribution_specification.get("count")
            attribution_directory = record.get("qtAttributionDirectory")
            if (
                member_name != "qt_attribution.json"
                or not isinstance(expected_count, int)
                or expected_count <= 0
                or not isinstance(attribution_directory, str)
            ):
                raise BundleError(f"{label}: incomplete Qt attribution lock")
            extracted_attributions = extract_locked_regular_members_named(
                f"{label} source",
                artifact,
                member_name,
                cache,
                work / f"{label}-qt-attributions",
            )
            if len(extracted_attributions) != expected_count:
                raise BundleError(
                    f"{label}: expected {expected_count} Qt attribution files, "
                    f"found {len(extracted_attributions)}"
                )
            attribution_root = safe_relative_path(attribution_directory)
            if len(attribution_root.parts) != 1:
                raise BundleError(f"{label}: invalid Qt attribution directory")
            attribution_license_references = set()
            for attribution_relative, attribution_source in extracted_attributions:
                attribution_license_references.update(
                    qt_attribution_license_references(
                        attribution_relative, attribution_source
                    )
                )
            extracted_attribution_licenses = extract_locked_regular_members(
                f"{label} Qt attribution licenses",
                artifact,
                attribution_license_references,
                cache,
                work / f"{label}-qt-attribution-licenses",
            )
            module_license_files = {}
            license_directory_value = attribution_specification.get(
                "licenseDirectory"
            )
            expected_license_count = attribution_specification.get(
                "licenseFileCount"
            )
            if (
                license_directory_value is not None
                or expected_license_count is not None
            ):
                if (
                    not isinstance(license_directory_value, str)
                    or not isinstance(expected_license_count, int)
                    or expected_license_count <= 0
                ):
                    raise BundleError(f"{label}: incomplete Qt module license lock")
                license_directory = safe_relative_path(license_directory_value)
                module_license_files = extract_locked_regular_tree(
                    f"{label} Qt module licenses",
                    artifact,
                    license_directory,
                    cache,
                    work / f"{label}-qt-module-licenses",
                )
                if len(module_license_files) != expected_license_count:
                    raise BundleError(
                        f"{label}: expected {expected_license_count} Qt module "
                        f"license files, found {len(module_license_files)}"
                    )
            for attribution_relative, attribution_source in extracted_attributions:
                destination_relative = Path(
                    "licenses",
                    "die-qt-attributions",
                    attribution_root.name,
                    *attribution_relative.parts,
                )
                attribution_destination = archive_root / destination_relative
                if attribution_destination.exists():
                    raise BundleError(
                        f"Duplicate Qt attribution destination: {destination_relative}"
                    )
                copy_regular(
                    attribution_source,
                    attribution_destination,
                    f"{label} Qt attribution",
                )
                copied_attributions.append(destination_relative.as_posix())
            all_attribution_licenses = dict(module_license_files)
            all_attribution_licenses.update(extracted_attribution_licenses)
            for license_relative, license_source in sorted(
                all_attribution_licenses.items(),
                key=lambda item: item[0].as_posix(),
            ):
                destination_relative = Path(
                    "licenses",
                    "die-qt-attributions",
                    attribution_root.name,
                    *license_relative.parts,
                )
                attribution_destination = archive_root / destination_relative
                if attribution_destination.exists():
                    raise BundleError(
                        "Duplicate Qt attribution license destination: "
                        f"{destination_relative}"
                    )
                copy_regular(
                    license_source,
                    attribution_destination,
                    f"{label} Qt attribution license",
                )
                copied_attribution_licenses.append(
                    destination_relative.as_posix()
                )

        index_entry = {
            "name": label,
            "version": record["version"],
            "license": record["license"],
            "projectUrl": record["projectUrl"],
            "sourceFile": f"sources/{file_relative.name}",
            "sourceSize": artifact["size"],
            "sourceSha256": artifact["sha256"],
            "usedBy": record["usedBy"],
            "linkage": record.get("linkage"),
            "licenseFiles": copied_licenses,
            "qtAttributionFiles": copied_attributions,
            "qtAttributionLicenseFiles": copied_attribution_licenses,
        }
        for metadata_key in ("patch", "targetPatches", "targetBuildRequirements"):
            if metadata_key in record:
                index_entry[metadata_key] = record[metadata_key]
        index.append(index_entry)
    return index


def stage_android_notices(
    lock: dict[str, Any],
    archive_root: Path,
    cache: Path,
    work: Path,
) -> list[dict[str, Any]]:
    platform_tools = lock["components"]["scrcpy"]["androidPlatformTools"]
    target_labels = {
        "windows-x86_64": "windows",
        "linux-x86_64": "linux",
        "macos-aarch64": "macos",
    }
    report: list[dict[str, Any]] = []
    for target, platform_label in target_labels.items():
        artifact = checked_artifact(
            f"Android Platform Tools {target}", platform_tools["targets"][target]
        )
        notice = artifact.get("notice")
        if not isinstance(notice, dict) or not isinstance(notice.get("path"), str):
            raise BundleError(f"Android Platform Tools {target}: NOTICE lock is missing")
        notice_relative = safe_relative_path(notice["path"])
        notice_source = extract_locked_regular_member(
            f"Android Platform Tools {target}",
            artifact,
            notice_relative.as_posix(),
            cache,
            work / f"android-platform-tools-{platform_label}-NOTICE.txt",
        )
        notice_source = verify_locked_regular(
            notice_source,
            f"Android Platform Tools {target} NOTICE",
            notice,
        )
        destination_relative = (
            f"licenses/android-platform-tools-{platform_tools['version']}-"
            f"{platform_label}-NOTICE.txt"
        )
        copy_regular(
            notice_source,
            archive_root / destination_relative,
            f"Android Platform Tools {target} NOTICE",
        )
        report.append(
            {
                "target": target,
                "archiveUrl": artifact["url"],
                "archiveSize": artifact["size"],
                "archiveSha256": artifact["sha256"],
                "noticeFile": destination_relative,
                "noticeSize": notice["size"],
                "noticeSha256": notice["sha256"],
            }
        )
    report.append(
        {
            "target": "macos-x86_64",
            "sameAs": "macos-aarch64",
            "noticeFile": (
                f"licenses/android-platform-tools-{platform_tools['version']}-"
                "macos-NOTICE.txt"
            ),
        }
    )
    return report


def stage_project_inputs(
    repo_root: Path,
    lock_path: Path,
    lock: dict[str, Any],
    archive_root: Path,
) -> None:
    patch_values = {
        lock["components"]["go-ios"]["patch"],
        *lock["components"]["ffmpeg"].get("targetPatches", {}).values(),
    }
    included = [
        lock_path,
        *(
            repo_root.joinpath(*safe_relative_path(value).parts)
            for value in sorted(patch_values)
        ),
        repo_root / "packaging/SCRCPY_REBUILD.md",
        repo_root / "scripts/prepare_tool_bundle.py",
        repo_root / "scripts/verify_tool_bundle.py",
        repo_root / "scripts/refresh_tool_manifest.py",
        repo_root / "scripts/create_third_party_sources.py",
        repo_root / "src-tauri/resources/tools/THIRD_PARTY_NOTICES.txt",
        repo_root / "tools/mobius-ssh/main.go",
        repo_root / "tools/mobius-ssh/main_test.go",
        repo_root / "tools/mobius-ssh/go.mod",
        repo_root / "tools/mobius-ssh/go.sum",
    ]
    for source in included:
        relative = source.relative_to(repo_root)
        copy_regular(source, archive_root / relative, relative.as_posix())


def write_checksums(archive_root: Path) -> None:
    checksum_lines = []
    for source in sorted(archive_root.rglob("*")):
        if source.is_symlink():
            raise BundleError(f"Source package contains a symlink: {source}")
        if not source.is_file() or source.name == "SHA256SUMS.txt":
            continue
        relative = source.relative_to(archive_root).as_posix()
        checksum_lines.append(f"{sha256_file(source)}  {relative}")
    write_text(archive_root / "SHA256SUMS.txt", "\n".join(checksum_lines) + "\n")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--lock", type=Path, default=Path("packaging/toolchain.lock.json"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cache", type=Path, default=Path(".cache/tool-bundles"))
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    lock_path = args.lock if args.lock.is_absolute() else repo_root / args.lock
    cache = args.cache if args.cache.is_absolute() else repo_root / args.cache
    output = args.output if args.output.is_absolute() else repo_root / args.output
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    if lock.get("schemaVersion") != 1:
        raise BundleError("Unsupported toolchain lock schema")

    with tempfile.TemporaryDirectory(prefix="mobius-third-party-sources-") as temporary:
        temporary_root = Path(temporary)
        archive_root = temporary_root / "archive"
        work = temporary_root / "work"
        archive_root.mkdir()
        work.mkdir()

        source_index = stage_sources_and_licenses(lock, archive_root, cache, work)
        adb_notices = stage_android_notices(lock, archive_root, cache, work)
        stage_project_inputs(repo_root, lock_path, lock, archive_root)
        write_text(
            archive_root / "SOURCE_INDEX.json",
            json.dumps(
                {
                    "schemaVersion": 1,
                    "bundleRevision": lock["bundleRevision"],
                    "sources": source_index,
                    "binaryOnlyRuntimes": binary_only_runtime_records(lock),
                    "androidPlatformToolsNotices": adb_notices,
                },
                indent=2,
                ensure_ascii=False,
            )
            + "\n",
        )
        write_text(
            archive_root / "README.txt",
            """Mobius Device Lab third-party source package
================================================

This archive accompanies a binary Mobius release. It contains the exact upstream
source archives, license texts, immutable lock, published source patches, and build
control material needed to audit or reproduce the bundled command-line tools.

The scrcpy 4.1 source and the FFmpeg 8.1.2, SDL 3.4.12, libusb 1.0.30, dav1d
1.5.3, target-specific zlib, and Windows MinGW-w64 runtime sources correspond to
the official scrcpy portable archives. Linux and macOS portable builds statically
link FFmpeg, SDL, and libusb; dav1d and some zlib variants are linked statically
through FFmpeg. Windows ships shared FFmpeg, SDL, and libusb libraries, while
dav1d, zlib, and MinGW runtime portions remain statically linked. See
packaging/SCRCPY_REBUILD.md for upstream build scripts and relinking guidance.

The separate FFmpeg 9.0.1 command is built without GPL, nonfree, network, or
external codec components. Its configure flags are in packaging/toolchain.lock.json.
The Windows build applies the included configure-only patch so FFmpeg uses native
Windows timing fallbacks and does not require the external libwinpthread runtime.
go-ios is built with CGO disabled and the published Mobius patch. The first-party
Mobius SSH/SFTP helper is also built with CGO disabled from the source included in
this archive. Its locked x/crypto/ssh and pkg/sftp dependency sources, licenses,
Go checksums, and build metadata are included here or in every target package.

Google's proprietary Platform Tools archives are not copied into this source
package. Their official URLs, exact sizes and hashes are locked, and each target's
verified NOTICE is included here. The prepare script independently compares every
bundled portable ADB file byte-for-byte with Platform Tools 37.0.0 before packaging.

Detect It Easy 3.21 and the exact Qt/ICU module sources used by its target
packages are included. Linux also includes the matching Ubuntu ICU packaging
patches. Every Qt module's source qt_attribution.json files are preserved both
beside the source index and in each applicable target bundle. The Windows
portable package contains proprietary Microsoft Visual C++
v14 runtime DLLs; they have no corresponding source offer. Their exact hashes,
official license links, and publisher redistribution constraint are recorded in
SOURCE_INDEX.json and the lock instead of being represented as open source.

SOURCE_INDEX.json maps every archived source and license to its target and linkage.
SHA256SUMS.txt authenticates every other file in this archive.
""",
        )
        write_checksums(archive_root)

        output.parent.mkdir(parents=True, exist_ok=True)
        temporary_output = output.with_name(f".{output.name}.tmp-{os.getpid()}")
        temporary_output.unlink(missing_ok=True)
        with tarfile.open(temporary_output, "w:xz", format=tarfile.PAX_FORMAT) as archive:
            for source in sorted(archive_root.rglob("*")):
                if source.is_file():
                    add_regular(
                        archive,
                        source,
                        source.relative_to(archive_root).as_posix(),
                    )
        os.replace(temporary_output, output)

    digest = sha256_file(output)
    print(f"Created {output} ({output.stat().st_size} bytes, sha256:{digest})")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (BundleError, OSError, KeyError, TypeError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
