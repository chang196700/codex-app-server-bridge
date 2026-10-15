"""Prepare or publish a GitHub-only release; called by the trusted main workflow."""

import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tomllib
import zipfile


TARGETS = (
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
)


def run(*args, capture=False):
    result = subprocess.run(args, text=True, stdout=subprocess.PIPE if capture else None)
    if result.returncode:
        raise SystemExit(f"{args[0]} failed with exit code {result.returncode}")
    return result.stdout.strip() if capture else None


def version():
    with open("Cargo.toml", "rb") as manifest:
        return tomllib.load(manifest)["package"]["version"]


def archives(release_version):
    return {
        f"codex-app-server-bridge-v{release_version}-{target}."
        + ("zip" if "windows" in target else "tar.gz")
        for target in TARGETS
    }


def release_info(tag):
    result = subprocess.run(
        ["gh", "release", "view", tag, "--json", "isDraft,assets,tagName,url"],
        text=True,
        capture_output=True,
    )
    if result.returncode == 0:
        info = json.loads(result.stdout)
        info["draft"] = info.pop("isDraft")
        return info
    # A missing release is expected before the first publication or after a
    # partial failure. Authentication, rate limiting and other errors must fail.
    # Unlike the REST tag endpoint, gh also looks up unpublished drafts.
    if result.stderr.strip() == "release not found":
        return None
    raise SystemExit(f"Cannot inspect GitHub release: {result.stderr}")


def complete(info, release_version):
    expected = archives(release_version) | {"SHA256SUMS"}
    return bool(
        info
        and not info["draft"]
        and expected.issubset({asset["name"] for asset in info["assets"]})
    )


def tag_commit(tag):
    result = subprocess.run(
        ["git", "rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}"],
        text=True,
        capture_output=True,
    )
    return result.stdout.strip() if result.returncode == 0 else None


def prepared_source():
    directory = Path(os.environ["PREPARED_SOURCE"])
    candidate = json.loads((directory / "release.json").read_text(encoding="utf-8"))
    if candidate.get("schema") != 1:
        raise SystemExit("Unsupported prepared release metadata")
    for key in ("sha", "base_sha"):
        if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", candidate.get(key, "")):
            raise SystemExit(f"Invalid prepared release {key}")
    if candidate["sha"] != os.environ["RELEASE_SHA"]:
        raise SystemExit("Prepared release commit does not match the workflow output")
    if candidate["base_sha"] != os.environ["RELEASE_BASE_SHA"]:
        raise SystemExit("Prepared release base does not match the workflow output")
    if candidate.get("tag") != f"v{candidate.get('version')}":
        raise SystemExit("Prepared release tag does not match its version")
    return directory, candidate


def restore():
    directory, candidate = prepared_source()
    bundle = str(directory / "source.bundle")
    run("git", "bundle", "verify", bundle)
    run("git", "fetch", "--no-tags", bundle,
        "refs/heads/prepared-release:refs/remotes/prepared/release")
    restored = run("git", "rev-parse", "refs/remotes/prepared/release", capture=True)
    if restored != candidate["sha"]:
        raise SystemExit("Prepared source bundle does not contain the expected release commit")
    run("git", "checkout", "--detach", restored)
    if version() != candidate["version"]:
        raise SystemExit("Prepared source version does not match the release metadata")
    print(f"Restored unpublished release {candidate['tag']} at {restored}")


def prepare():
    if run("git", "status", "--porcelain", capture=True):
        raise SystemExit("Release preparation requires a clean working tree")

    base_commit = run("git", "rev-parse", "HEAD", capture=True)
    current_version = version()
    changelog = Path("CHANGELOG.md")
    original_changelog = None
    if not tag_commit(f"v{current_version}") and changelog.exists():
        original = changelog.read_bytes()
        headings = list(re.finditer(rb"(?m)^##[ \t]+[^\r\n]*(?:\r?\n|$)", original))
        pending_heading = re.compile(
            rb"^##[ \t]+\[" + re.escape(current_version.encode("utf-8"))
            + rb"\](?=[ \t(\r\n]|$)"
        )
        sections = [
            (heading.start(), headings[index + 1].start() if index + 1 < len(headings) else len(original))
            for index, heading in enumerate(headings)
            if pending_heading.match(heading.group())
        ]
        if sections:
            # release-plz preserves an existing entry for the manifest's version,
            # even before its tag exists. Remove only that pending entry so a
            # cancelled preparation followed by more commits gets fresh notes.
            original_changelog = original
            refreshed = original
            for start, end in reversed(sections):
                refreshed = refreshed[:start] + refreshed[end:]
    try:
        arguments = ["release-plz", "update", "--config", "release-plz.toml"]
        if original_changelog is not None:
            changelog.write_bytes(refreshed)
            arguments.append("--allow-dirty")
        run(*arguments)
    except BaseException:
        if original_changelog is not None:
            changelog.write_bytes(original_changelog)
        raise

    release_version = version()
    tag = f"v{release_version}"
    tracked = [path for path in ("Cargo.toml", "Cargo.lock", "CHANGELOG.md") if Path(path).exists()]
    run("git", "add", "--", *tracked)
    if run("git", "diff", "--cached", "--name-only", capture=True):
        run("git", "config", "user.name", "github-actions[bot]")
        run("git", "config", "user.email", "41898282+github-actions[bot]@users.noreply.github.com")
        run("git", "commit", "-m", f"chore: release {tag}")
    commit = tag_commit(tag) or run("git", "rev-parse", "HEAD", capture=True)
    # An existing tag is immutable. Retries always build that exact source.
    should_release = not complete(release_info(tag), release_version)
    if should_release:
        directory = Path(os.environ["PREPARED_SOURCE"])
        directory.mkdir(parents=True, exist_ok=True)
        run("git", "update-ref", "refs/heads/prepared-release", commit)
        # A self-contained bundle carries the local version commit between jobs
        # without publishing any branch or tag before compilation succeeds.
        run("git", "bundle", "create", str(directory / "source.bundle"),
            "refs/heads/prepared-release")
        (directory / "release.json").write_text(json.dumps({
            "schema": 1,
            "base_sha": base_commit,
            "sha": commit,
            "tag": tag,
            "version": release_version,
        }, indent=2) + "\n", encoding="utf-8")
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"base_sha={base_commit}\nsha={commit}\nversion={release_version}\ntag={tag}\n")
        output.write(f"should_release={str(should_release).lower()}\n")
    print(f"{tag} at {commit}: {'build and publish' if should_release else 'already published'}")


def verify_buildinfo(metadata, binary, release_version, target, commit):
    fields = {}
    for line in metadata.read(65537).decode("utf-8-sig").splitlines():
        key, separator, value = line.partition(": ")
        if separator:
            if key in fields:
                raise SystemExit(f"Duplicate BUILDINFO field {key} for {target}")
            fields[key] = value
    expected = {
        "Package": "codex-app-server-bridge",
        "Version": release_version,
        "Target": target,
        "Source commit": commit,
    }
    for key, value in expected.items():
        if fields.get(key) != value:
            raise SystemExit(f"Incorrect BUILDINFO {key} for {target}: {fields.get(key)!r}")
    digest = hashlib.file_digest(binary, "sha256").hexdigest()
    if fields.get("Binary SHA256") != digest:
        raise SystemExit(f"Packaged binary checksum does not match BUILDINFO for {target}")


def verify_archive(path, release_version, target, commit):
    prefix = f"codex-app-server-bridge-v{release_version}-{target}"
    metadata_name = f"{prefix}/BUILDINFO.txt"
    binary_name = f"{prefix}/codex-app-server-bridge" + (".exe" if "windows" in target else "")
    if "windows" in target:
        with zipfile.ZipFile(path) as archive:
            members = archive.namelist()
            if any(members.count(name) != 1 for name in (metadata_name, binary_name)):
                raise SystemExit(f"Missing or duplicate packaged binary/BUILDINFO in {path.name}")
            with archive.open(metadata_name) as metadata, archive.open(binary_name) as binary:
                verify_buildinfo(metadata, binary, release_version, target, commit)
    else:
        with tarfile.open(path, "r:gz") as archive:
            members = archive.getmembers()
            selected = []
            for name in (metadata_name, binary_name):
                matches = [member for member in members if member.name == name]
                if len(matches) != 1 or not matches[0].isfile():
                    raise SystemExit(f"Missing or duplicate packaged binary/BUILDINFO in {path.name}")
                selected.append(matches[0])
            with archive.extractfile(selected[0]) as metadata, archive.extractfile(selected[1]) as binary:
                verify_buildinfo(metadata, binary, release_version, target, commit)


def publish():
    _, candidate = prepared_source()
    release_version = version()
    tag = f"v{release_version}"
    commit = run("git", "rev-parse", "HEAD", capture=True)
    if tag != os.environ["RELEASE_TAG"] or commit != os.environ["RELEASE_SHA"]:
        raise SystemExit("Release checkout does not match the prepared version and commit")
    if release_version != candidate["version"] or tag != candidate["tag"]:
        raise SystemExit("Release checkout does not match the prepared metadata")
    existing = tag_commit(tag)
    if existing and existing != commit:
        raise SystemExit(f"Refusing to change the source of existing tag {tag}")
    info = release_info(tag)
    if complete(info, release_version):
        print(f"{tag} is already published with all expected assets")
        return
    if info and not info["draft"]:
        raise SystemExit("An incomplete public release exists; refusing to overwrite its assets")

    directory = Path(os.environ["RELEASE_ASSETS"])
    expected = archives(release_version)
    actual = {path.name for path in directory.iterdir() if path.is_file()}
    if actual != expected:
        raise SystemExit(f"Unexpected archive set: missing={expected - actual}, extra={actual - expected}")
    for target in TARGETS:
        extension = "zip" if "windows" in target else "tar.gz"
        archive = directory / f"codex-app-server-bridge-v{release_version}-{target}.{extension}"
        verify_archive(archive, release_version, target, commit)
    checksums = directory / "SHA256SUMS"
    lines = []
    for name in sorted(expected):
        with (directory / name).open("rb") as archive:
            digest = hashlib.file_digest(archive, "sha256").hexdigest()
        lines.append(f"{digest}  {name}\n")
    checksums.write_text("".join(lines), encoding="utf-8")

    # Restore Git authentication on retries too, even when the version commit
    # was pushed by an earlier publication attempt.
    run("gh", "auth", "setup-git")
    run("git", "fetch", "--no-tags", "origin", "refs/heads/main:refs/remotes/origin/main")
    remote_commit = run("git", "rev-parse", "refs/remotes/origin/main", capture=True)
    if remote_commit not in (candidate["base_sha"], commit):
        print(f"Skipping stale candidate {tag}: main advanced from {candidate['base_sha']} to {remote_commit}")
        return
    if not existing and remote_commit != commit:
        # Only the publish job has write credentials. A non-fast-forward push
        # also rejects a concurrent main update after the check above; never
        # rebase a candidate because that would publish an unbuilt commit.
        run("git", "push", "origin", f"{commit}:refs/heads/main")
    if not existing:
        run("release-plz", "release", "--config", "release-plz.toml")
    run("git", "fetch", "origin", f"refs/tags/{tag}:refs/tags/{tag}")
    if tag_commit(tag) != commit:
        raise SystemExit("Created release tag does not match the built commit")
    info = release_info(tag)
    if info is None:
        # release-plz skips existing tags. Recover if a previous run created
        # the tag but failed before creating its GitHub release.
        notes = Path(os.environ["RUNNER_TEMP"]) / "release-notes.md"
        changelog = Path("CHANGELOG.md").read_text(encoding="utf-8")
        heading = f"## [{release_version}]"
        start = changelog.find(heading)
        if start < 0:
            raise SystemExit(f"No changelog section for {release_version}")
        end = changelog.find("\n## ", start + len(heading))
        notes.write_text(changelog[start:end if end >= 0 else None], encoding="utf-8")
        run("gh", "release", "create", tag, "--verify-tag", "--draft", "--title", tag,
            "--notes-file", str(notes))
    run("gh", "release", "upload", tag, *map(str, sorted(directory.iterdir())), "--clobber")
    uploaded = release_info(tag)
    if not uploaded or not (expected | {"SHA256SUMS"}).issubset(
        {asset["name"] for asset in uploaded["assets"]}
    ):
        raise SystemExit("Release is missing required assets; leaving it as a draft")
    run("gh", "release", "edit", tag, "--draft=false", "--latest")
    print(f"Published {tag} with {len(expected)} native archives and SHA256SUMS")


if __name__ == "__main__":
    if sys.argv[1:] == ["prepare"]:
        prepare()
    elif sys.argv[1:] == ["restore"]:
        restore()
    elif sys.argv[1:] == ["publish"]:
        publish()
    else:
        raise SystemExit("Usage: release.py prepare|restore|publish")
