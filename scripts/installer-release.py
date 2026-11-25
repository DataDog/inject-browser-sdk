#!/usr/bin/env python3
# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2025-Present Datadog, Inc.

"""
This script automates the release process for the installer.

Usage:
======
To release the installer:
./installer-release.py --github-token <GH_TOKEN> --gitlab-token <GITLAB_TOKEN> --version-tag installer-0.1.3 --sha <COMMIT_SHA> <GH_ACTIONS_RUN_ID>

The run id is the GitHub Actions workflow run that produced the installer
artifacts (the `installer` workflow). Find it in the run URL, e.g.
https://github.com/DataDog/inject-browser-sdk/actions/runs/<RUN_ID>.

The run must be a successful `installer` run for a push to main of
DataDog/inject-browser-sdk, at exactly <COMMIT_SHA>. Pull request and fork runs
are rejected before anything is downloaded or signed. The version tag must not
exist yet on GitHub or GitLab: both releases create it at <COMMIT_SHA>.

The GitHub release is a draft. Its tag only exists once the draft is published:
after publishing, check that the tag points to <COMMIT_SHA>.
"""

import re
import argparse
import hashlib
import itertools
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import urllib.error
import urllib.parse
import urllib.request
import tarfile
import zipfile

project_id = "DataDog%2Finject-browser-sdk" # DataDog/inject-browser-sdk
gh_repo = "DataDog/inject-browser-sdk"
gh_api_url = "https://api.github.com"
gitlab_api_url = "https://gitlab.ddbuild.io/api/v4"
release_workflow_path = ".github/workflows/installer.yml"

class VerboseDict(dict):
    """This makes debugging a little easier."""

    def __getitem__(self, key):
        try:
            return super().__getitem__(key)
        except KeyError as error:
            # In addition to the missing key, print the object itself.
            message = f"{repr(key)} is not in {self}"
            error.args = (message, )
            raise error


class MissingDependency(Exception):
    pass

def get_gh():
    exe_path = shutil.which("gh")
    if exe_path is None:
        raise MissingDependency(
            'The "gh" command must be available to publish a release to GitHub.  Installation instructions are available at <https://cli.github.com/>.'
        )
    return exe_path

def get_gpg():
    exe_path = shutil.which("gpg")
    if exe_path is None:
        raise MissingDependency(
            'The "gpg" command must be available and configured to be able to create detached signatures of release artifacts.'
        )
    return exe_path

def run(command, *args, **kwargs):
    command = [str(arg) for arg in command]
    print("+", shlex.join(command), file=sys.stderr)
    return subprocess.run(command, *args, **kwargs)

def gh_request(url, method="GET", accept="application/vnd.github+json"):
    if not url.startswith("http"):
        url = f"{gh_api_url}{url}"
    request = urllib.request.Request(
        url,
        method=method,
        headers={
            "Accept": accept,
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "inject-browser-sdk-release-script",
        },
    )
    # Artifact downloads redirect to blob storage, which rejects the token.
    request.add_unredirected_header("Authorization", f"Bearer {github_token}")
    print("+", request.get_method(), request.full_url)
    try:
        return urllib.request.urlopen(request)
    except urllib.error.HTTPError as error:
        body = error.read().decode("utf-8", errors="replace")
        rate_remaining = error.headers.get("X-RateLimit-Remaining")
        rate_reset = error.headers.get("X-RateLimit-Reset")
        raise Exception(
            f"GitHub API {method} {url} failed: HTTP {error.code} {error.reason}. "
            f"Body: {body}. "
            f"RateLimit-Remaining={rate_remaining} Reset={rate_reset}."
        ) from error


def gh_request_json(url):
    response = gh_request(url)
    return response, json.load(response, object_hook=VerboseDict)


def list_run_artifacts(run_id: str):
    artifacts = []
    url = f"/repos/{gh_repo}/actions/runs/{run_id}/artifacts?per_page=100"
    while url:
        response, body = gh_request_json(url)
        artifacts.extend(body["artifacts"])
        link = response.headers.get("Link", "")
        next_match = re.search(r'<([^>]+)>;\s*rel="next"', link)
        url = next_match.group(1) if next_match else None
    return artifacts


def release_sha(value: str) -> str:
    if not re.fullmatch(r"[0-9a-f]{40}", value):
        raise argparse.ArgumentTypeError("must be a full 40-character lowercase commit SHA")
    return value


def check_release_run(run, sha: str):
    """Raise unless run is a successful installer run for a push to main at sha."""
    expected = {
        "status": "completed",
        "conclusion": "success",
        "path": release_workflow_path,
        "event": "push",
        "head_branch": "main",
        "head_sha": sha,
    }
    for field, value in expected.items():
        if run[field] != value:
            raise Exception(
                f'Run {run["id"]} must have {field}={value!r} (got {run[field]!r}).'
            )
    for field in ("repository", "head_repository"):
        name = run[field]["full_name"]
        if name != gh_repo:
            raise Exception(
                f'Run {run["id"]} must have {field}={gh_repo!r} (got {name!r}).'
            )


def check_tag_absent(version_tag: str):
    """Raise if version_tag already exists on GitHub or GitLab.

    Both releases create the tag at the release SHA, but reuse an existing tag
    whatever it points to.
    """
    tag_path = urllib.parse.quote(version_tag, safe="")
    try:
        gh_request(f"/repos/{gh_repo}/git/ref/tags/{tag_path}")
    except Exception as error:
        if not isinstance(error.__cause__, urllib.error.HTTPError) or error.__cause__.code != 404:
            raise
    else:
        raise Exception(f"Tag {version_tag} already exists on GitHub.")

    request = urllib.request.Request(
        f"{gitlab_api_url}/projects/{project_id}/repository/tags/{tag_path}",
        headers={"PRIVATE-TOKEN": gitlab_token},
    )
    try:
        urllib.request.urlopen(request)
    except urllib.error.HTTPError as error:
        if error.code != 404:
            raise
    else:
        raise Exception(f"Tag {version_tag} already exists on GitLab.")


def download_artifact(artifact, destination_dir: Path):
    """Download and extract a GitHub Actions artifact zip into destination_dir.

    Returns the list of files extracted. Rejects a zip whose SHA-256 does not
    match the artifact digest, and any zip member whose path resolves outside
    destination_dir (zip-slip guard).
    """
    archive_url = f"/repos/{gh_repo}/actions/artifacts/{artifact['id']}/zip"
    zip_path = destination_dir / f"{artifact['name']}.zip"
    print(f"Downloading {archive_url} to {zip_path}")
    digest = hashlib.sha256()
    with gh_request(archive_url) as response, open(zip_path, "wb") as output:
        while chunk := response.read(1 << 16):
            digest.update(chunk)
            output.write(chunk)
    actual = f"sha256:{digest.hexdigest()}"
    if actual != artifact.get("digest"):
        raise Exception(
            f"Artifact '{artifact['name']}' ({artifact['id']}) digest mismatch: "
            f"expected {artifact.get('digest')}, got {actual}."
        )
    print(f"Artifact '{artifact['name']}' id={artifact['id']} digest={actual}")

    dest_root = destination_dir.resolve()
    extracted = []
    with zipfile.ZipFile(zip_path) as archive:
        for member in archive.namelist():
            target = (destination_dir / member).resolve()
            if dest_root not in target.parents and target != dest_root:
                raise Exception(
                    f"Refusing to extract {member!r} from artifact "
                    f"'{artifact['name']}': path escapes {dest_root}."
                )
            archive.extract(member, destination_dir)
            extracted.append(destination_dir / member)
    zip_path.unlink()
    return extracted


def package(files, out):
    if not isinstance(files, list):
        files = [files]

    with tarfile.open(out, "w:gz") as tar:
        for f in files:
            if not f.is_file():
                print(f"{f} is not a valid file and will be skipped")
                continue

            tar.add(f, arcname=os.path.basename(f))


def sign_package(package_path: str) -> None:
    command = [gpg_exe, "--armor", "--detach-sign", package_path]
    run(command, check=True)


def upload_to_package_registry(package_path: str, package_name: str, version: str):
    """Upload a file to the GitLab Generic Package Registry."""
    file_name = os.path.basename(package_path)
    
    with open(package_path, 'rb') as f:
        file_content = f.read()
    
    url = f"{gitlab_api_url}/projects/{project_id}/packages/generic/{package_name}/{version}/{file_name}"
    headers = {
        "PRIVATE-TOKEN": gitlab_token
    }
    
    request = urllib.request.Request(
        url,
        data=file_content,
        headers=headers,
        method="PUT"
    )
    
    response = urllib.request.urlopen(request)
    if response.status != 201:
        raise Exception(f"Failed to upload package file {file_name}: {response.read()}")
    
    return url


def create_gitlab_release(version_tag: str, sha: str, package_urls: set):
    """Create a GitLab release with links to the package files."""
    headers = {
        "PRIVATE-TOKEN": gitlab_token,
        "Content-Type": "application/json"
    }

    release_url = f"{gitlab_api_url}/projects/{project_id}/releases"
    assets = {
        "links": [
            {
                "name": os.path.basename(url),
                "url": url,
                "link_type": "other"
            }
            for url in package_urls
        ]
    }
    
    release_data = {
        "tag_name": version_tag,
        "name": version_tag,
        "ref": sha,
        "description": "Release created by automated script",
        "assets": assets
    }
    
    request = urllib.request.Request(
        release_url,
        data=json.dumps(release_data).encode('utf-8'),
        headers=headers,
        method="POST"
    )
    
    response = urllib.request.urlopen(request)
    if response.status != 201:
        raise Exception(f"Failed to create GitLab release: {response.read()}")


def prepare_installer_release_artifact(work_dir: Path, arch: str,
                                       module_artifact, script_artifact):
    arch_dir = work_dir / arch
    arch_dir.mkdir(parents=True, exist_ok=True)
    module_files = download_artifact(module_artifact, arch_dir)
    script_files = download_artifact(script_artifact, work_dir)

    module_path = next((f for f in module_files if f.name == "proxy-configurator"), None)
    script_path = next((f for f in script_files if f.name == "install-proxy-datadog.sh"), None)

    if module_path is None:
        raise Exception(
            f"Artifact '{module_artifact['name']}' does not contain a 'proxy-configurator' file."
        )
    if script_path is None:
        raise Exception(
            f"Artifact '{script_artifact['name']}' does not contain an 'install-proxy-datadog.sh' file."
        )

    tarball_path = work_dir / f"proxy-configurator-{arch}.tgz"
    package(module_path, out=tarball_path)
    sign_package(tarball_path)
    return tarball_path, script_path


def release_installer(args) -> int:
    """
    This subcommand function downloads installer artifacts from the release workflow,
    publishes them to the Gitlab Package Registry, and creates GitLab + Github releases.

    Requirements:
        - You must be logged into the GitHub CLI (gh) to authenticate and publish the release.
        - You must have a GitLab access token with appropriate permissions
        - You must have a GitHub token with `actions:read` scope on DataDog/inject-browser-sdk
    """
    _, run = gh_request_json(f"/repos/{gh_repo}/actions/runs/{args.run_id}")
    check_release_run(run, args.sha)
    check_tag_absent(args.version_tag)
    all_artifacts = list_run_artifacts(args.run_id)
    artifacts_by_name = {a["name"]: a for a in all_artifacts}

    package_urls = []
    with tempfile.TemporaryDirectory() as work_dir_str:
        work_dir = Path(work_dir_str)
        for arch in ("amd64", "arm64"):
            module_name = f"proxy-configurator-{arch}"
            script_name = f"install-proxy-datadog-{arch}"
            if module_name not in artifacts_by_name:
                raise Exception(
                    f"Run {args.run_id} is missing expected artifact '{module_name}'."
                )
            if script_name not in artifacts_by_name:
                raise Exception(
                    f"Run {args.run_id} is missing expected artifact '{script_name}'."
                )

            tar_path, script_path = prepare_installer_release_artifact(
                work_dir,
                arch,
                artifacts_by_name[module_name],
                artifacts_by_name[script_name],
            )

            download_url = upload_to_package_registry(str(tar_path), "proxy-configurator",
                                                      args.version_tag)
            package_urls.append(download_url)

            download_url = upload_to_package_registry(str(script_path), "proxy-configurator",
                                                      args.version_tag)
            package_urls.append(download_url)

            sig_path = str(tar_path) + ".asc"
            if not os.path.exists(sig_path):
                raise Exception(f'Could not find signature file "{sig_path}"')

            download_url = upload_to_package_registry(sig_path, "proxy-configurator",
                                                      args.version_tag)
            package_urls.append(download_url)

        pubkey_file = os.path.join(work_dir, "pubkey.gpg")
        run([gpg_exe, "--output", pubkey_file, "--armor", "--export"],
            check=True)
        download_url = upload_to_package_registry(pubkey_file, "proxy-configurator",
                                                  args.version_tag)
        package_urls.append(download_url)

        # We've tgz'd and signed all of our release modules.
        # Now let's send them to GitHub in a release via `gh release create`.
        release_files = itertools.chain(
            Path(work_dir).glob("*.tgz"),
            Path(work_dir).glob("*.tgz.asc"),
            Path(work_dir).glob("*.sh"),
            (pubkey_file, ),
        )

        command = [
            gh_exe,
            "release",
            "create",
            "--repo",
            gh_repo,
            "--prerelease",
            "--draft",
            "--target",
            args.sha,
            "--title",
            args.version_tag,
            "--notes",
            "TODO",
            args.version_tag,
            *release_files,
        ]
        run(command, check=True)

        create_gitlab_release(args.version_tag, args.sha, set(package_urls))
        print(f"After publishing the GitHub draft, check that {args.version_tag} points to {args.sha}.")

    return 0


if __name__ == "__main__":
    parser = argparse.ArgumentParser(
        description="Build and publish a release of RUM injection installer.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument("--version-tag", required=True)
    parser.add_argument(
        "--sha",
        required=True,
        type=release_sha,
        help="Full commit SHA on main to release. The run must have built exactly this commit.",
    )
    parser.add_argument(
        "--github-token",
        help="GitHub API token with actions:read scope (defaults to `gh auth token`)",
        default=None,
    )
    parser.add_argument("--gitlab-token", help="GitLab API Token", required=True)
    parser.add_argument(
        "run_id",
        type=str,
        help=
        "ID of the GitHub Actions workflow run that produced the installer artifacts. "
        "Example: https://github.com/DataDog/inject-browser-sdk/actions/runs/<RUN_ID>",
    )

    parser.set_defaults(func=release_installer)
    options = parser.parse_args()

    gitlab_token = options.gitlab_token
    try:
        gh_exe = get_gh()
        gpg_exe = get_gpg()
    except (MissingDependency, ValueError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)

    github_token = options.github_token
    if github_token is None:
        result = subprocess.run([gh_exe, "auth", "token"],
                                capture_output=True, text=True, check=True)
        github_token = result.stdout.strip()

    print({
        "gh": gh_exe,
        "gpg": gpg_exe,
    })

    sys.exit(options.func(options))
