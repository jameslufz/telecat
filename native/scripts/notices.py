#!/usr/bin/env python3
"""Generate notices from the locked macOS dependency graph and checked-in license sources.

No network requests. Run after sourcing scripts/env.sh; package.sh calls this automatically.
Includes normal/build dependencies conservatively, including feature-unified optional crates.
"""
import hashlib
import json
import pathlib
import subprocess

root = pathlib.Path(__file__).resolve().parent.parent
host = subprocess.check_output(["rustc", "-vV"], text=True).split("host: ")[1].splitlines()[0]
metadata = json.loads(subprocess.check_output([
    "cargo", "metadata", "--locked", "--offline", "--format-version", "1", "--filter-platform", host
], cwd=root))
packages = {p["id"]: p for p in metadata["packages"]}
nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
seen = set()

def visit(package_id):
    if package_id in seen:
        return
    seen.add(package_id)
    for dep in nodes[package_id]["deps"]:
        if any(k["kind"] in (None, "build") for k in dep["dep_kinds"]):
            visit(dep["pkg"])

visit(next(p["id"] for p in packages.values() if p["name"] == "telecat-app"))
fallbacks = json.loads((root / "licenses/upstream/sources.json").read_text())
texts = {}

def register(text):
    text = text.strip() + "\n"
    key = hashlib.sha256(text.encode()).hexdigest()[:16]
    texts[key] = text
    return key

sections = [(root / "licenses/README.txt").read_text().strip()]
for p in sorted((packages[i] for i in seen), key=lambda p: (p["name"], p["version"])):
    # These are original application crates; engine and source ports are covered below.
    if p["name"].startswith("telecat-") and p["name"] != "telecat-editor-engine":
        continue
    directory = pathlib.Path(p["manifest_path"]).parent
    label = p['name'] + '@' + p['version']
    lines = [label, "License: " + (p["license"] or "see license file"),
             "Repository: " + (p["repository"] or "see source archive"),
             "Authors: " + ", ".join(p["authors"])]
    if p["source"]:
        lines.append(f"Source archive: https://crates.io/api/v1/crates/{p['name']}/{p['version']}/download")
    else:
        lines.append("Vendored source: native/" + str(directory.relative_to(root)))
    files = sorted(x for x in directory.rglob("*") if x.is_file()
                   and len(x.relative_to(directory).parts) <= 4
                   and x.name.lower().startswith(("license", "licence", "copying", "copyright", "notice")))
    if p.get("license_file"):
        file = pathlib.Path(p["license_file"])
        if not file.is_absolute():
            file = directory / file
        if file not in files:
            files.append(file)
    for file in files:
        lines.append(f"Text {register(file.read_text())}: {file.relative_to(directory)}")
    if not files:
        fallback = fallbacks[label]  # Fail rather than silently omit a new dependency.
        lines.append(f"Text {register((root / 'licenses/upstream' / fallback['file']).read_text())}: {fallback['source']}")
    sections.append("\n".join(lines))

for label, file in [
    ("mysql2 error-code mapping (MIT), ported in telecat-db", "db/src/mysql-errors.LICENSE"),
    ("sql-formatter (MIT), ported in telecat-editor", "editor/sql-formatter.LICENSE"),
    ("@codemirror/lang-sql dialect data (MIT), ported in telecat-editor", "editor/codemirror-sql.LICENSE"),
    ("gpui-kit editor engine (Apache-2.0)", "editor/engine/LICENSE-APACHE"),
]:
    sections.append(f"{label}\nText {register((root / file).read_text())}: native/{file}")

# Some published crates provide only a declaration/link, not the license body.
# Include complete common bodies, alongside their verbatim notices and author metadata.
for name in ["MIT.txt", "Apache-2.0.txt"]:
    sections.append(f"Common license body {name}: Text {register((root / 'licenses' / name).read_text())}")
output = "\n\n".join(sections) + "\n\n" + "=" * 78 + "\nLICENSE TEXTS\n"
for key, text in sorted(texts.items()):
    output += "\n" + "=" * 78 + f"\nText {key}\n" + "=" * 78 + "\n" + text
path = root / "THIRD-PARTY-NOTICES.txt"
path.write_text(output)
print(f"Notices: {len(seen)} dependency packages reviewed; {len(texts)} distinct texts; {path.stat().st_size:,} bytes")
