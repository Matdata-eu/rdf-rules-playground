# CodeMirror

`codemirror.min.js` bundles the parts of [CodeMirror 6](https://codemirror.net/)
that the playground's Turtle editors need, as the global `ShaclCodeMirror`. It
is the same editor and Turtle grammar that MatGUI's Response view uses, plus
editing, history, search and lint support.

| Package | Version | License |
| --- | --- | --- |
| @codemirror/state | 6.7.6 | MIT |
| @codemirror/view | 6.43.14 | MIT |
| @codemirror/language | 6.13.1 | MIT |
| @codemirror/commands | 6.11.1 | MIT |
| @codemirror/lint | 6.9.7 | MIT |
| @codemirror/search | 6.7.2 | MIT |
| @lezer/common | 1.5.3 | MIT |
| @lezer/highlight | 1.2.5 | MIT |
| @lezer/lr | 1.4.11 | MIT |
| codemirror-lang-turtle | 0.0.2 | MIT |
| crelt | 1.0.7 | MIT |
| style-mod | 4.1.4 | MIT |
| w3c-keyname | 2.2.8 | MIT |
| @marijn/find-cluster-break | 1.0.4 | MIT |

CodeMirror and Lezer: Copyright (C) 2018-2021 by Marijn Haverbeke and others.
codemirror-lang-turtle: Copyright (c) 2023 Achraf Atauil.

## Rebuilding

```sh
npm i @codemirror/state@6.7.6 @codemirror/view@6.43.14 @codemirror/language@6.13.1 \
  @codemirror/commands@6.11.1 @codemirror/lint@6.9.7 @codemirror/search@6.7.2 \
  @lezer/highlight@1.2.5 codemirror-lang-turtle@0.0.2 esbuild
npx esbuild entry.js --bundle --minify --format=iife --global-name=ShaclCodeMirror \
  --legal-comments=none --outfile=codemirror.min.js
```

with this `entry.js`:

```js
export { EditorState, RangeSetBuilder, Compartment } from "@codemirror/state";
export { EditorView, lineNumbers, highlightSpecialChars, Decoration, ViewPlugin, keymap, drawSelection, highlightActiveLine, highlightActiveLineGutter, placeholder } from "@codemirror/view";
export { foldGutter, bracketMatching, syntaxHighlighting, syntaxTree, indentOnInput, foldKeymap } from "@codemirror/language";
export { classHighlighter } from "@lezer/highlight";
export { turtle } from "codemirror-lang-turtle";
export { history, defaultKeymap, historyKeymap, indentWithTab } from "@codemirror/commands";
export { linter, lintGutter, setDiagnostics } from "@codemirror/lint";
export { searchKeymap, highlightSelectionMatches } from "@codemirror/search";
```
