#!/usr/bin/env python3
"""Check Python help and messages with the Rust writing validator."""
import argparse
import ast
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent


def literal(node, docstring):
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value
    if isinstance(node, ast.Name) and node.id == "__doc__":
        return docstring
    if isinstance(node, ast.JoinedStr):
        return "".join(part.value if isinstance(part, ast.Constant) else "`VALUE`" for part in node.values)
    return None


def prose(path):
    tree = ast.parse(path.read_text(), filename=str(path))
    docstring = ast.get_docstring(tree)
    def values(node):
        text = literal(node, docstring)
        if text is not None:
            yield text
        elif isinstance(node, ast.Name):
            # Help tables bind one literal string per subcommand.
            for loop in ast.walk(tree):
                if not isinstance(loop, ast.For) or not isinstance(loop.target, ast.Tuple):
                    continue
                if not isinstance(loop.iter, (ast.List, ast.Tuple)):
                    continue
                for index, target in enumerate(loop.target.elts):
                    if isinstance(target, ast.Name) and target.id == node.id:
                        for row in loop.iter.elts:
                            if isinstance(row, (ast.List, ast.Tuple)) and len(row.elts) > index:
                                if text := literal(row.elts[index], docstring):
                                    yield text

    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        for keyword in node.keywords:
            if keyword.arg in {"description", "help", "epilog"}:
                for text in values(keyword.value):
                    yield node.lineno, text
        name = node.func.id if isinstance(node.func, ast.Name) else node.func.attr if isinstance(node.func, ast.Attribute) else None
        if name in {"print", "ValueError", "RuntimeError", "SystemExit", "TimeoutError", "error"} and node.args:
            for text in values(node.args[0]):
                yield node.lineno, text
        if name == "require" and len(node.args) > 1:
            for text in values(node.args[1]):
                yield node.lineno, text


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--language", type=Path, default=Path(os.environ.get("ENFOUR_LANGUAGE", ROOT / "language-private/dictionary.json")))
    parser.add_argument("--binary", type=Path, default=Path(os.environ.get("ENFOUR_BINARY", ROOT / "target/debug/enfour-memory")))
    args = parser.parse_args()
    # Preserve command syntax as literal evidence. Validate the explanatory text.
    records = []
    document = bytearray()
    for path in sorted((ROOT / "scripts").iterdir()):
        if not path.is_file() or path.name.startswith("test-"):
            continue
        for line, text in prose(path):
            parts = []
            for part in text.splitlines():
                if part.startswith(("Usage:", "Example:")):
                    parts.append("```text\n" + part + "\n```")
                else:
                    parts.append(part)
            start = len(document)
            document.extend(("\n".join(parts) + "\n\n").encode())
            records.append({"file": str(path.relative_to(ROOT)), "line": line, "start": start, "end": len(document)})
    with tempfile.TemporaryDirectory(prefix="enfour-prose-") as temporary:
        source = Path(temporary) / "python-help.md"
        source.write_bytes(document)
        report = subprocess.run([str(args.binary.resolve()), "--language", str(args.language.resolve()),
                                 "--minify", "uglify-json", "validate-memory", "--document", str(source)],
                                capture_output=True, text=True)
        if report.returncode not in (0, 2):
            raise RuntimeError(report.stderr.strip() or "Writing checks failed.")
        result = json.loads(report.stdout)
    findings = []
    for diagnostic in result["diagnostics"]:
        location = next((record for record in records if record["start"] <= diagnostic["range"]["start"] < record["end"]), {})
        findings.append({**location, **diagnostic,
                         "text": document[diagnostic["range"]["start"]:diagnostic["range"]["end"]].decode()})
    print(json.dumps({"accepted": result["accepted"], "versions": result["versions"], "diagnostics": findings}, indent=2))
    return 0 if result["accepted"] else 2


if __name__ == "__main__":
    raise SystemExit(main())
