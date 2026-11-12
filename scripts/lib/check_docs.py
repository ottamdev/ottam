"""Documentation contract. Run through scripts/check-docs.sh from the repository root."""
from __future__ import annotations

import glob
import hashlib
import json
import os
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path.cwd()
DOCS = ROOT / "docs"
SELF = b"scripts/lib/check_docs.py"
REQUIRED_KEYS = ("kind", "status")
KINDS = ("index", "architecture", "spec", "operations", "reference")
STATUSES = ("active", "proposed", "deferred", "exploratory")
URI_SCHEME = re.compile(r"[A-Za-z][A-Za-z0-9+.-]*:")
# A relative RFC 3986 reference: path and query characters or %XX escapes, then
# at most one fragment. Brackets belong only in a host, which local links lack.
URI_PART = r"(?:[A-Za-z0-9\-._~:/?@!$&'()*+,;=]|%[0-9A-Fa-f]{2})*"
URI_REFERENCE = re.compile(URI_PART + r"(?:#" + URI_PART + r")?\Z")
# Docs state facts directly; PR numbers and commit hashes from the private
# history mean nothing in the public repository. Upstream references such as
# `owner/repo#123` stay allowed.
PR_REFERENCE = re.compile(r"(?<![\w/.-])#\d{2,5}\b", re.ASCII)
COMMIT_HASH = re.compile(
    r"(?<![\w/.#-])(?=[0-9a-f]*[a-f])(?=[0-9a-f]*\d)[0-9a-f]{7,40}(?![\w-])", re.ASCII
)
# private/ is a gitignored checkout of the maintainers' private repository.
# Public files never link to it, carry its marker, or name its repository. The
# marker is split and the name compared by digest so this script holds neither.
PRIVATE_DIR = str(ROOT / "private")
PRIVATE_MARKER = ("ottam" + "-internal").encode()
ORGANIZATION_REPOSITORY = re.compile(rb"ottamdev/[A-Za-z0-9_-]+")
PRIVATE_REPOSITORY_DIGEST = "3f4793d5e1f321cd6055733fee43087700b0046b8837e817418512049bc48896"
SPEC_CITATION = re.compile(r"docs/specs/[A-Za-z0-9_./-]+\.md")
# Build output that can appear inside an export; private/ is deliberately not
# here, because an export must never contain it.
EXPORT_SKIPPED_DIRECTORIES = {".git", ".build", "target", "DerivedData"}

# Apple's Markdown parser is the one the app renders with, so links are read
# the way readers will follow them.
SWIFT_LINK_EXTRACTOR = r"""
import Foundation

struct LinkRecord: Encodable {
    let path: String
    let target: String
}

let docsRoot = URL(
    fileURLWithPath: CommandLine.arguments[1],
    isDirectory: true
)
guard let enumerator = FileManager.default.enumerator(
    at: docsRoot,
    includingPropertiesForKeys: [.isRegularFileKey]
) else {
    FileHandle.standardError.write(Data("could not enumerate docs\n".utf8))
    exit(1)
}

let encoder = JSONEncoder()
var failed = false
for case let fileURL as URL in enumerator
    where fileURL.pathExtension.lowercased() == "md"
{
    do {
        let markdown = try String(contentsOf: fileURL, encoding: .utf8)
        let attributed = try AttributedString(
            markdown: markdown,
            options: .init(interpretedSyntax: .full)
        )
        for run in attributed.runs {
            for target in [run.link, run.imageURL].compactMap({ $0 }) {
                let record = LinkRecord(
                    path: fileURL.path,
                    target: target.relativeString
                )
                let data = try encoder.encode(record)
                print(String(decoding: data, as: UTF8.self))
            }
        }
    } catch {
        let message = "\(fileURL.path): \(error)\n"
        FileHandle.standardError.write(Data(message.utf8))
        failed = true
    }
}

if failed {
    exit(1)
}
"""


def markdown_targets_by_document(errors: list[str]) -> dict[str, list[str]]:
    environment = dict(
        os.environ,
        CLANG_MODULE_CACHE_PATH=str(ROOT / ".build/clang-module-cache"),
        SWIFT_MODULECACHE_PATH=str(ROOT / ".build/swift-module-cache"),
    )
    result = subprocess.run(
        ["swift", "-e", SWIFT_LINK_EXTRACTOR, str(DOCS)],
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        encoding="utf-8",
        errors="replace",
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or "unknown Swift parser failure"
        errors.append(f"documentation Markdown parsing failed: {detail}")

    targets: dict[str, list[str]] = defaultdict(list)
    for line in result.stdout.splitlines():
        try:
            record = json.loads(line)
            targets[record["path"]].append(record["target"])
        except (ValueError, KeyError, TypeError) as error:
            errors.append(f"documentation Markdown parser returned invalid output: {error}")
    return targets


def front_matter_values(header: str) -> dict[str, str]:
    values = {}
    for line in header.splitlines():
        match = re.match(r"([a-z_]+):\s*(.*?)\s*$", line)
        if match:
            values[match.group(1)] = match.group(2)
    return values


def check_link(path: Path, relative: str, target: str, errors: list[str]) -> None:
    if not target or target.startswith("#") or URI_SCHEME.match(target):
        return
    if not URI_REFERENCE.match(target):
        errors.append(f"{relative}: invalid local link target {target}")
        return
    path_part = urlsplit(target).path
    if not path_part or path_part.startswith("/"):
        return
    decoded = re.sub(r"\\(.)", r"\1", unquote(path_part))
    resolved = os.path.normpath(os.path.join(str(path.parent), decoded))
    if resolved == PRIVATE_DIR or resolved.startswith(PRIVATE_DIR + "/"):
        errors.append(f"{relative}: links into the maintainers' private/ checkout ({target})")
        return
    if not os.path.exists(resolved):
        errors.append(f"{relative}: missing local link target {target}")


def check_documents(documents: list[Path], errors: list[str]) -> None:
    link_targets = markdown_targets_by_document(errors)

    root_documents = [ROOT / name for name in ("AGENTS.md", "README.md", "CONTRIBUTING.md", "SECURITY.md")]
    for path in documents + root_documents:
        if not path.exists():
            continue
        relative = str(path.relative_to(ROOT))
        content = path.read_text(encoding="utf-8", errors="surrogateescape")
        match = PR_REFERENCE.search(content)
        if match:
            errors.append(f"{relative}: cites pull request {match.group(0)}; state the fact instead")
        match = COMMIT_HASH.search(content)
        if match:
            errors.append(f"{relative}: cites commit {match.group(0)}; state the fact instead")

    if (DOCS / "tmp").exists():
        errors.append("docs/tmp must not exist; promote, reconcile, or delete temporary documents")

    for path in documents:
        relative = str(path.relative_to(ROOT))
        content = path.read_text(encoding="utf-8", errors="surrogateescape")
        if not content.startswith("---\n"):
            errors.append(f"{relative}: missing YAML front matter")
            continue
        delimiter = content.find("\n---\n", 4)
        if delimiter < 0:
            errors.append(f"{relative}: unterminated YAML front matter")
            continue

        values = front_matter_values(content[4:delimiter])
        for key in REQUIRED_KEYS:
            if not values.get(key):
                errors.append(f"{relative}: missing front-matter key {key}")
        if "kind" in values and values["kind"] not in KINDS:
            errors.append(f"{relative}: unsupported kind {json.dumps(values['kind'])}")
        if "status" in values and values["status"] not in STATUSES:
            errors.append(f"{relative}: unsupported status {json.dumps(values['status'])}")

        for target in link_targets.get(str(path), []):
            check_link(path, relative, target, errors)


def repository_files(errors: list[str]) -> list[bytes]:
    # A `git archive` export is not a checkout, even when it sits inside one, so
    # git has nothing useful to say about it: walk it. A checkout that git
    # cannot list fails, because a walk there would read ignored files.
    if (ROOT / ".git").exists():
        # Paths are bytes; the locale may not be able to decode them.
        listed = subprocess.run(
            ["git", "ls-files", "-coz", "--exclude-standard"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if listed.returncode != 0:
            errors.append(f"could not list repository files: {listed.stderr.decode(errors='replace').strip()}")
        return [path for path in listed.stdout.split(b"\0") if path]
    files = []
    for directory, subdirectories, names in os.walk(b"."):
        subdirectories[:] = [name for name in subdirectories if name.decode(errors="surrogateescape") not in EXPORT_SKIPPED_DIRECTORIES]
        files.extend(os.path.relpath(os.path.join(directory, name), b".") for name in names)
    return files


def check_repository_files(errors: list[str]) -> None:
    for candidate in repository_files(errors):
        if candidate == SELF or not os.path.isfile(candidate):
            continue
        try:
            with open(candidate, "rb") as handle:
                content = handle.read()
        except (FileNotFoundError, IsADirectoryError):
            continue
        name = candidate.decode(errors="surrogateescape")
        # A leak counts in any encoding, so match the raw bytes.
        if PRIVATE_MARKER in content:
            errors.append(f"{name}: contains the private-repository marker")
        if any(
            hashlib.sha256(found).hexdigest() == PRIVATE_REPOSITORY_DIGEST
            for found in ORGANIZATION_REPOSITORY.findall(content)
        ):
            errors.append(f"{name}: names the private repository")
        # Specs are deleted when they ship, so code and config cite architecture
        # docs instead. Markdown may link specs; test fixtures may name them.
        # Only a named spec file is a citation; bare directories and globs pass.
        if name.endswith(".md") or name.startswith("app/Tests/"):
            continue
        try:
            text = content.decode("utf-8")
        except UnicodeDecodeError:
            continue
        cited = SPEC_CITATION.search(text)
        if cited:
            errors.append(f"{name}: cites {cited.group(0)}; cite docs/architecture/ or nothing")


def main() -> int:
    errors: list[str] = []
    # glob skips dot-directories and sorts as strings, as the docs tree expects.
    documents = [Path(path) for path in sorted(glob.glob(str(DOCS / "**/*.md"), recursive=True))]
    check_documents(documents, errors)
    check_repository_files(errors)

    if not errors:
        print(f"Documentation contract passed ({len(documents)} documents)")
        return 0
    print("Documentation contract failed:", file=sys.stderr)
    for error in dict.fromkeys(errors):
        print(f"- {error}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
