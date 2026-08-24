# Lox IR for VS Code

Declarative VS Code extension for `.lox` IR modules: syntax highlighting
(TextMate grammar), comment/bracket configuration, and snippets for the four
statement forms (`extern`, `block`/`blockp`, `wire`, `set`).

No build step and no extension host code — install by copying or linking the
folder into your extensions directory:

```sh
ln -s "$(pwd)/editor/vscode" ~/.vscode/extensions/lxc.lox-ir-0.1.0
```

(then reload VS Code), or package it properly with
[`vsce`](https://github.com/microsoft/vscode-vsce):

```sh
cd editor/vscode && npx @vscode/vsce package && code --install-extension lox-ir-0.1.0.vsix
```

Real autocompletion (port names from the base config, slug references,
diagnostics-as-you-type) needs a language server; that is scoped in
[docs/roadmap.md](../../docs/roadmap.md) — the library's line-precise parse
errors and `observe` API were shaped so an `lxc lsp` subcommand can be a
thin layer. Until then, `lxc check` and `lxc fmt` in a save hook cover the
validation loop.

Note: the `.lox` extension is also used by the "Lox" teaching language from
*Crafting Interpreters*; if you have such an extension installed, VS Code's
language picker (bottom right) lets you pin `Lox IR` per workspace via
`files.associations`.
