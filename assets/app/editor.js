// rdf-rules-playground: Turtle editors on CodeMirror 6 (bundled as the
// global ShaclCodeMirror, see assets/vendor/codemirror/NOTICE.md).
(function () {
  "use strict";

  const cm = window.ShaclCodeMirror;

  class TurtleEditor {
    /**
     * @param {HTMLElement} host
     * @param {{ value?: string, readOnly?: boolean, onChange?: (text: string) => void, onRun?: () => void, placeholder?: string }} opts
     */
    constructor(host, opts) {
      this.opts = opts || {};
      const extensions = [
        cm.lineNumbers(),
        cm.foldGutter(),
        cm.highlightSpecialChars(),
        cm.bracketMatching(),
        cm.EditorView.lineWrapping,
        cm.syntaxHighlighting(cm.classHighlighter),
        cm.turtle(),
      ];
      if (this.opts.readOnly) {
        extensions.push(cm.EditorState.readOnly.of(true));
      } else {
        extensions.push(
          cm.history(),
          cm.drawSelection(),
          cm.highlightActiveLine(),
          cm.highlightActiveLineGutter(),
          cm.highlightSelectionMatches(),
          cm.indentOnInput(),
          cm.lintGutter(),
          cm.keymap.of([
            { key: "Mod-Enter", run: () => (this.opts.onRun && this.opts.onRun(), true) },
            ...cm.defaultKeymap,
            ...cm.historyKeymap,
            ...cm.searchKeymap,
            ...cm.foldKeymap,
            cm.indentWithTab,
          ]),
          cm.EditorView.updateListener.of((u) => {
            if (u.docChanged && this.opts.onChange) this.opts.onChange(this.value);
          }),
        );
        if (this.opts.placeholder) extensions.push(cm.placeholder(this.opts.placeholder));
      }
      this.view = new cm.EditorView({
        state: cm.EditorState.create({ doc: this.opts.value || "", extensions }),
        parent: host,
      });
    }

    get value() {
      return this.view.state.doc.toString();
    }

    set value(text) {
      this.view.dispatch({ changes: { from: 0, to: this.view.state.doc.length, insert: text || "" } });
    }

    /** Insert text at the end of the document and select it. */
    append(text) {
      const doc = this.view.state.doc;
      const sep = doc.length && !doc.toString().endsWith("\n\n") ? (doc.toString().endsWith("\n") ? "\n" : "\n\n") : "";
      const from = doc.length + sep.length;
      this.view.dispatch({
        changes: { from: doc.length, insert: sep + text },
        selection: { anchor: from, head: from + text.length },
        scrollIntoView: true,
      });
      this.view.focus();
    }

    /** Show a parse error (1-based line and column), or clear it with null. */
    setError(err) {
      const doc = this.view.state.doc;
      if (!err) {
        this.view.dispatch(cm.setDiagnostics(this.view.state, []));
        return;
      }
      const line = doc.line(Math.min(Math.max(1, err.line), doc.lines));
      const from = Math.min(line.from + Math.max(0, err.column - 1), line.to);
      const to = Math.max(from + 1, Math.min(line.to, from + 1));
      this.view.dispatch(
        cm.setDiagnostics(this.view.state, [
          { from, to: Math.min(to, doc.length), severity: "error", message: err.message },
        ]),
      );
    }

    /** Select the first occurrence of `needle` and scroll to it. */
    reveal(needle) {
      if (!needle) return false;
      const at = this.value.indexOf(needle);
      if (at < 0) return false;
      this.view.dispatch({
        selection: { anchor: at, head: at + needle.length },
        effects: cm.EditorView.scrollIntoView(at, { y: "center" }),
      });
      this.view.focus();
      return true;
    }

    /** Move the cursor to a 1-based line. */
    gotoLine(n) {
      const doc = this.view.state.doc;
      const line = doc.line(Math.min(Math.max(1, n), doc.lines));
      this.view.dispatch({ selection: { anchor: line.from }, effects: cm.EditorView.scrollIntoView(line.from, { y: "center" }) });
      this.view.focus();
    }
  }

  window.PlaygroundEditor = TurtleEditor;
})();
