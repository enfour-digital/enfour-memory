# Development

[Enfour Memory](../README.md)

Run commands from the repository root.

Use targeted checks for regular changes. With a fix, include the failure and the checks that apply.
Public tests use a synthetic dictionary. Release builds must remove `test-support`.

```sh
scripts/enfour check helpers
scripts/enfour check product --language language-private/dictionary.json
scripts/enfour diagrams --setup
scripts/enfour diagrams
scripts/enfour diagrams --check
```

Diagram tools use Node.js, npm, and Chromium on the computer that creates these files only.
SVGs come from the [Mermaid source](architecture.md). Run retrieval evaluations at evaluation milestones.

Use `scripts/enfour --help` for the Python commands. Use systemd to control a NixOS service.

Use `scripts/enfour diagrams --preview artifacts/readme` to check the README views.
