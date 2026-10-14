"""Prepare or publish a GitHub-only release; called by the trusted main workflow."""

import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib


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


def prepare():
    if run("git", "status", "--porcelain", capture=True):
        raise SystemExit("Release preparation requires a clean working tree")

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
        # Never force-push: a concurrent human push must not be overwritten.
        run("git", "push", "origin", "HEAD:refs/heads/main")
    commit = tag_commit(tag) or run("git", "rev-parse", "HEAD", capture=True)
    # An existing tag is immutable. Retries always build that exact source.
    should_release = not complete(release_info(tag), release_version)
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"sha={commit}\nversion={release_version}\ntag={tag}\n")
        output.write(f"should_release={str(should_release).lower()}\n")
    print(f"{tag} at {commit}: {'build and publish' if should_release else 'already published'}")


def publish():
    release_version = version()
    tag = f"v{release_version}"
    commit = run("git", "rev-parse", "HEAD", capture=True)
    if tag != os.environ["RELEASE_TAG"] or commit != os.environ["RELEASE_SHA"]:
        raise SystemExit("Release checkout does not match the prepared version and commit")
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
    checksums = directory / "SHA256SUMS"
    lines = []
    for name in sorted(expected):
        with (directory / name).open("rb") as archive:
            digest = hashlib.file_digest(archive, "sha256").hexdigest()
        lines.append(f"{digest}  {name}\n")
    checksums.write_text("".join(lines), encoding="utf-8")

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
    elif sys.argv[1:] == ["publish"]:
        publish()
    else:
        raise SystemExit("Usage: release.py prepare|publish")
