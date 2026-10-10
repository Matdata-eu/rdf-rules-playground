#!/usr/bin/env python3
"""Write THIRD_PARTY_NOTICES.md: every Rust crate compiled into the binary
and every npm package bundled into the vendored browser code, with its
license and the copyright lines from its license files.

Usage:
    scripts/third_party_notices.py NODE_MODULES[:NAMES_FILE]... > THIRD_PARTY_NOTICES.md

NODE_MODULES: a node_modules folder holding the packages of a vendored bundle
(see assets/vendor/*/NOTICE.md). NAMES_FILE limits the list to the packages
named in it, one per line, e.g. those in the bundle's source map. Build tools
such as esbuild are skipped.
"""
import glob
import json
import os
import re
import subprocess
import sys

SKIP_NPM = {"esbuild", "@esbuild"}
# Packages whose package.json lacks what their license file says.
OVERRIDES = {
    "jsuri": {"license": "MIT"},
    "leaflet.heat": {"license": "BSD-2-Clause"},
    "@matdata/yasgui": {"copyright": ["Copyright (c) Triply and Matdata contributors"]},
    "@matdata/yasqe": {"copyright": ["Copyright (c) Triply and Matdata contributors"]},
    "@matdata/yasr": {"copyright": ["Copyright (c) Triply and Matdata contributors"]},
    "@matdata/yasgui-utils": {"copyright": ["Copyright (c) Triply and Matdata contributors"]},
}
COPYRIGHT = re.compile(r"^\s*(?:\(c\)\s*)?copyright\b.*\d{4}", re.I)


def copyrights(folder):
    lines = []
    for f in sorted(glob.glob(os.path.join(folder, "LICEN[CS]E*")) + glob.glob(os.path.join(folder, "COPYING*"))):
        try:
            text = open(f, encoding="utf-8", errors="replace").read()
        except OSError:
            continue
        for line in text.splitlines():
            line = line.strip()
            if COPYRIGHT.match(line) and "<year>" not in line and "[yyyy]" not in line and line not in lines:
                lines.append(line)
    return lines


def rust():
    meta = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--locked"]))
    used = {n["id"] for n in meta["resolve"]["nodes"]}
    out = []
    for p in sorted(meta["packages"], key=lambda p: p["name"]):
        if p["id"] not in used or p["source"] is None:
            continue
        out.append((p["name"], p["version"], p.get("license") or "see repository", p.get("repository") or "", copyrights(os.path.dirname(p["manifest_path"]))))
    return out


def npm(specs):
    """specs: "NODE_MODULES" or "NODE_MODULES:NAMES_FILE"; with a names file,
    only those packages are listed (e.g. the sources of a bundle's map)."""
    seen = {}
    for spec in specs:
        root, _, names_file = spec.partition(":")
        if names_file:
            names = [n.strip() for n in open(names_file, encoding="utf-8") if n.strip()]
        else:
            names = [os.path.relpath(os.path.dirname(pj), root) for pj in
                     glob.glob(os.path.join(root, "*", "package.json")) + glob.glob(os.path.join(root, "@*", "*", "package.json"))]
        for name in sorted(names):
            folder = os.path.join(root, name)
            try:
                p = json.load(open(os.path.join(folder, "package.json"), encoding="utf-8"))
            except (OSError, ValueError):
                print(f"warning: {name} not found in {root}", file=sys.stderr)
                continue
            if name.split("/")[0] in SKIP_NPM or name.startswith("@esbuild/"):
                continue
            lic = p.get("license")
            if isinstance(lic, dict):
                lic = lic.get("type")
            repo = p.get("repository")
            if isinstance(repo, dict):
                repo = repo.get("url")
            o = OVERRIDES.get(name, {})
            cr = o.get("copyright") or copyrights(folder)
            seen.setdefault(name, (name, p.get("version", ""), o.get("license") or lic or "see repository", repo or "", cr))
    return [seen[k] for k in sorted(seen)]


def table(rows):
    lines = ["| Package | Version | License | Copyright |", "| --- | --- | --- | --- |"]
    for name, version, lic, repo, cr in rows:
        link = f"[{name}]({repo.removeprefix('git+')})" if repo.startswith(("http", "git+http")) else name
        lines.append(f"| {link} | {version} | {lic} | {'<br>'.join(c.replace('|', '/') for c in cr)} |")
    return "\n".join(lines)


def main():
    js = npm(sys.argv[1:])
    print("# Third-party notices\n")
    print("rdf-rules-playground is © 2026 Mathias Vanden Auweele (Matdata), licensed")
    print("under Apache-2.0 (see LICENSE). Questions, bug reports and feature requests:")
    print("https://github.com/Matdata-eu/rdf-rules-playground/issues\n")
    print("It includes the following third-party software, each under its own license. Where a")
    print("package offers a choice of licenses, it is used under any one of them. The")
    print("license texts are at the end of this file.\n")
    print("## Browser code (assets/vendor)\n")
    print("MatGUI (`@matdata/yasgui`), Tabulator and CodeMirror, and the packages bundled into them.\n")
    print(table(js))
    print("\nMatGUI's stylesheet embeds the Font Awesome Free 7 icon font (SIL OFL 1.1;")
    print("the Font Awesome Free license is at the end of this file).")
    print("\n## Rust crates compiled into the binary\n")
    print(table(rust()))
    print("\n## License texts\n")
    for name in ["MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Unicode-3.0", "Zlib", "FontAwesome-Free"]:
        path = os.path.join(os.path.dirname(__file__), "licenses", name + ".txt")
        print(f"### {name}\n\n```text\n{open(path, encoding='utf-8').read().strip()}\n```\n")


main()
