// rdf-rules-playground: the browser side. Two Turtle editors (data, and
// shapes with rules), a run against /api/infer, and four views of the
// result: inferred triples, rules, Turtle, and SPARQL through MatGUI.
(function () {
  "use strict";

  const $ = (id) => document.getElementById(id);
  const STORE_KEY = "rdf-rules-playground";
  const THEME_KEY = "rdf-rules-playground-theme";
  const LAYOUT_KEY = "rdf-rules-playground-layout";
  const WELL_KNOWN = [
    ["rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"],
    ["rdfs", "http://www.w3.org/2000/01/rdf-schema#"],
    ["xsd", "http://www.w3.org/2001/XMLSchema#"],
    ["sh", "http://www.w3.org/ns/shacl#"],
    ["owl", "http://www.w3.org/2002/07/owl#"],
  ];
  const XSD = "http://www.w3.org/2001/XMLSchema#";
  const PLAIN_TYPES = new Set([XSD + "integer", XSD + "decimal", XSD + "double", XSD + "boolean"]);

  const state = {
    meta: null,
    examples: [],
    result: null,
    prefixes: WELL_KNOWN.slice(),
    bnodes: new Map(),
    timer: null,
    saveTimer: null,
    running: false,
    rerun: false,
    seq: 0,
    table: null,
    turtle: null,
    turtleShow: "inferred",
    allTurtle: null,
    yasgui: null,
    tab: "triples",
    // A data graph the server holds ({ id, files, triples, prefixes }), and
    // whether runs use it instead of the data editor.
    serverData: null,
    useServer: false,
    dataYasgui: null,
    // Sections collapsed to their header, and the maximized one.
    collapsed: new Set(),
    maximized: null,
  };

  // ---------- small helpers ----------

  const esc = (s) =>
    String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

  function storage(fn, fallback) {
    try {
      return fn(window.localStorage);
    } catch (_) {
      return fallback;
    }
  }

  function toast(text) {
    const t = $("toast");
    t.textContent = text;
    t.classList.remove("hidden");
    clearTimeout(toast.timer);
    toast.timer = setTimeout(() => t.classList.add("hidden"), 2600);
  }

  function plural(n, one, many) {
    return `${n.toLocaleString()} ${n === 1 ? one : many || one + "s"}`;
  }

  // ---------- terms ----------

  /**
   * Parse an N-Triples term into { kind, value, lang?, datatype? }, or for an
   * RDF 1.2 triple term <<( s p o )>> into { kind: "triple", s, p, o }.
   */
  function parseTerm(t) {
    const r = readTerm(String(t).trim(), 0);
    return r && r.end === String(t).trim().length ? r.term : { kind: "other", value: t };
  }

  /** Read one N-Triples term at `i`; returns { term, end } or null. */
  function readTerm(t, i) {
    while (t[i] === " ") i++;
    if (t.startsWith("<<(", i)) {
      const parts = [];
      let at = i + 3;
      for (let n = 0; n < 3; n++) {
        const r = readTerm(t, at);
        if (!r) return null;
        parts.push(r.term);
        at = r.end;
      }
      while (t[at] === " ") at++;
      if (!t.startsWith(")>>", at)) return null;
      return { term: { kind: "triple", s: parts[0], p: parts[1], o: parts[2] }, end: at + 3 };
    }
    if (t[i] === "<") {
      const end = t.indexOf(">", i);
      return end < 0 ? null : { term: { kind: "iri", value: t.slice(i + 1, end) }, end: end + 1 };
    }
    if (t.startsWith("_:", i)) {
      const m = /^_:[^\s)]+/.exec(t.slice(i));
      return { term: { kind: "bnode", value: m[0].slice(2) }, end: i + m[0].length };
    }
    if (t[i] === '"') {
      const m = /^"((?:[^"\\]|\\.)*)"(?:@([A-Za-z0-9-]+)|\^\^<([^>]*)>)?/.exec(t.slice(i));
      if (!m) return null;
      let value = m[1];
      try {
        value = JSON.parse(`"${m[1]}"`);
      } catch (_) {
        /* keep the escaped form */
      }
      return { term: { kind: "literal", value, lang: m[2] || null, datatype: m[3] || null }, end: i + m[0].length };
    }
    return null;
  }

  function shortIri(iri) {
    let best = null;
    for (const [name, base] of state.prefixes) {
      if (iri.startsWith(base) && (!best || base.length > best[1].length)) {
        const local = iri.slice(base.length);
        if (/^[A-Za-z0-9_\-.]*$/.test(local) && !local.endsWith(".")) best = [name, base];
      }
    }
    return best ? `${best[0]}:${iri.slice(best[1].length)}` : `<${iri}>`;
  }

  function bnodeName(id) {
    if (!state.bnodes.has(id)) state.bnodes.set(id, `_:b${state.bnodes.size + 1}`);
    return state.bnodes.get(id);
  }

  /** Plain-text form of an N-Triples term, using prefixes. */
  function termText(t) {
    return parsedText(parseTerm(t));
  }

  function parsedText(p) {
    switch (p.kind) {
      case "triple":
        return `<<( ${parsedText(p.s)} ${parsedText(p.p)} ${parsedText(p.o)} )>>`;
      case "iri":
        return shortIri(p.value);
      case "bnode":
        return bnodeName(p.value);
      case "literal":
        if (p.lang) return `"${p.value}"@${p.lang}`;
        if (!p.datatype || p.datatype === XSD + "string") return `"${p.value}"`;
        if (PLAIN_TYPES.has(p.datatype)) return p.value;
        return `"${p.value}"^^${shortIri(p.datatype)}`;
      default:
        return p.value;
    }
  }

  /** HTML form of an N-Triples term. */
  function termHtml(t) {
    return parsedHtml(parseTerm(t));
  }

  function parsedHtml(p) {
    switch (p.kind) {
      case "triple":
        return `<span class="t-triple" title="triple term"><span class="t-meta">&lt;&lt;(</span> ${parsedHtml(p.s)} ${parsedHtml(p.p)} ${parsedHtml(p.o)} <span class="t-meta">)&gt;&gt;</span></span>`;
      case "iri":
        return `<span class="t-iri" title="${esc(shortIri(p.value) === `<${p.value}>` ? p.value : `${shortIri(p.value)}\n${p.value}`)}">${esc(shortIri(p.value))}</span>`;
      case "bnode":
        return `<span class="t-bnode" title="blank node">${esc(bnodeName(p.value))}</span>`;
      case "literal": {
        let tail = "";
        if (p.lang) tail = `<span class="t-meta">@${esc(p.lang)}</span>`;
        else if (p.datatype && p.datatype !== XSD + "string" && !PLAIN_TYPES.has(p.datatype))
          tail = `<span class="t-meta">^^${esc(shortIri(p.datatype))}</span>`;
        const plain = p.datatype && PLAIN_TYPES.has(p.datatype);
        return plain
          ? `<span class="t-num" title="${esc(shortIri(p.datatype))}">${esc(p.value)}</span>`
          : `<span class="t-lit">"${esc(p.value)}"</span>${tail}`;
      }
      default:
        return esc(p.value);
    }
  }

  function ruleName(r) {
    if (!r) return "";
    return r.label || termText(r.id);
  }

  // ---------- theme ----------

  function currentTheme() {
    const set = document.documentElement.getAttribute("data-theme");
    if (set) return set;
    return window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }

  function applyTheme(t) {
    if (t === "light" || t === "dark") document.documentElement.setAttribute("data-theme", t);
    else document.documentElement.removeAttribute("data-theme");
    for (const y of [state.yasgui, state.dataYasgui]) if (y && y.themeManager) y.themeManager.setTheme(currentTheme());
  }

  // ---------- sharing and saving ----------

  function b64url(bytes) {
    let s = "";
    for (const b of bytes) s += String.fromCharCode(b);
    return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }

  function unb64url(s) {
    const bin = atob(s.replace(/-/g, "+").replace(/_/g, "/"));
    return Uint8Array.from(bin, (c) => c.charCodeAt(0));
  }

  async function pipe(bytes, stream) {
    const out = new Response(new Blob([bytes]).stream().pipeThrough(stream));
    return new Uint8Array(await out.arrayBuffer());
  }

  function snapshot() {
    return {
      d: state.data.value,
      s: state.shapes.value,
      o: { i: $("opt-iterate").checked, m: Number($("opt-max").value) || 100, sd: $("opt-shapes-in-data").checked },
    };
  }

  async function shareLink() {
    const json = new TextEncoder().encode(JSON.stringify(snapshot()));
    let hash;
    if (window.CompressionStream) hash = "z=" + b64url(await pipe(json, new CompressionStream("deflate-raw")));
    else hash = "j=" + b64url(json);
    const url = `${location.origin}${location.pathname}#${hash}`;
    history.replaceState(null, "", `#${hash}`);
    try {
      await navigator.clipboard.writeText(url);
      toast(
        state.useServer
          ? "Link copied: it holds the rules and options, but not the data graph the server holds"
          : "Link copied: it holds both editors and the options",
      );
    } catch (_) {
      toast("Link is in the address bar");
    }
  }

  async function fromHash() {
    const h = location.hash.slice(1);
    try {
      if (h.startsWith("z=") && window.DecompressionStream) {
        const bytes = await pipe(unb64url(h.slice(2)), new DecompressionStream("deflate-raw"));
        return JSON.parse(new TextDecoder().decode(bytes));
      }
      if (h.startsWith("j=")) return JSON.parse(new TextDecoder().decode(unb64url(h.slice(2))));
    } catch (_) {
      toast("The link could not be read");
    }
    return null;
  }

  function applySnapshot(snap) {
    if (!snap) return;
    state.data.value = snap.d || "";
    state.shapes.value = snap.s || "";
    if (snap.o) {
      $("opt-iterate").checked = snap.o.i !== false;
      $("opt-max").value = snap.o.m || 100;
      $("opt-shapes-in-data").checked = !!snap.o.sd;
    }
  }

  function save() {
    clearTimeout(state.saveTimer);
    state.saveTimer = setTimeout(() => {
      storage((ls) => ls.setItem(STORE_KEY, JSON.stringify({ ...snapshot(), auto: $("opt-auto").checked })));
    }, 400);
  }

  // ---------- running ----------

  function scheduleRun() {
    save();
    if (!$("opt-auto").checked) return;
    clearTimeout(state.timer);
    state.timer = setTimeout(run, 650);
  }

  function setBusy(busy, text) {
    $("spinner").classList.toggle("hidden", !busy);
    $("run-btn").classList.toggle("busy", busy);
    if (text) $("status-text").textContent = text;
  }

  async function run() {
    clearTimeout(state.timer);
    if (state.running) {
      state.rerun = true;
      return;
    }
    state.running = true;
    const seq = ++state.seq;
    setBusy(true, "Running…");
    const body = {
      data: state.useServer ? "" : state.data.value,
      dataId: state.useServer ? state.serverData.id : null,
      shapes: state.shapes.value,
      shapesInData: $("opt-shapes-in-data").checked,
      iterate: $("opt-iterate").checked,
      maxIterations: Number($("opt-max").value) || 100,
    };
    let res = null;
    let failure = null;
    try {
      const r = await fetch("/api/infer", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      if (r.ok) res = await r.json();
      else failure = (await r.text()) || `HTTP ${r.status}`;
    } catch (e) {
      failure = "The playground server cannot be reached.";
    }
    state.running = false;
    if (seq === state.seq) {
      if (failure) {
        setBusy(false, "Failed");
        document.querySelector(".results").classList.add("stale");
        showNotices([{ kind: "error", text: failure }]);
      } else {
        render(res);
      }
    }
    if (state.rerun) {
      state.rerun = false;
      run();
    }
  }

  // ---------- rendering ----------

  function showSyntax(which, err) {
    const editor = which === "data" ? state.data : state.shapes;
    const chip = $(`${which}-error`);
    editor.setError(err || null);
    chip.classList.toggle("hidden", !err);
    if (err) {
      chip.textContent = `Line ${err.line}: syntax error`;
      chip.title = err.message;
      chip.onclick = () => editor.gotoLine(err.line);
    }
  }

  function showNotices(items) {
    $("notices").innerHTML = items
      .map(
        (n) =>
          `<div class="notice ${n.kind}"><svg viewBox="0 0 24 24" aria-hidden="true">${
            n.kind === "error" ? '<circle cx="12" cy="12" r="9"/><path d="M12 7v6M12 16.5h.01"/>' : '<path d="M12 3l9.5 17h-19z"/><path d="M12 10v4M12 17h.01"/>'
          }</svg><div>${n.html || esc(n.text)}</div></div>`,
      )
      .join("");
  }

  function render(res) {
    const broken = res.syntax && (res.syntax.data || res.syntax.shapes);
    document.querySelector(".results").classList.toggle("stale", !!(broken || res.error));
    showSyntax("data", res.syntax && res.syntax.data);
    showSyntax("shapes", res.syntax && res.syntax.shapes);
    if (broken) {
      const parts = [];
      if (res.syntax.data) parts.push(`data graph, line ${res.syntax.data.line}: ${res.syntax.data.message}`);
      if (res.syntax.shapes) parts.push(`shapes graph, line ${res.syntax.shapes.line}: ${res.syntax.shapes.message}`);
      setBusy(false, "Fix the Turtle to run");
      showNotices(parts.map((p) => ({ kind: "error", text: `Turtle syntax error in the ${p}` })));
      return;
    }
    if (res.error) {
      setBusy(false, "Failed");
      showNotices([{ kind: "error", text: res.error }]);
      return;
    }
    state.result = res;
    state.allTurtle = null;
    state.bnodes = new Map();
    state.prefixes = (res.prefixes || []).slice();
    for (const p of WELL_KNOWN) if (!state.prefixes.some(([n]) => n === p[0])) state.prefixes.push(p);

    $("data-count").textContent = plural(res.data_triples, "triple");
    $("shapes-count").textContent = plural(res.shapes_triples, "triple");
    $("sum-inferred").textContent = res.inferred_total;
    $("bar-count").textContent = res.inferred_total;
    $("sum-rules").textContent = res.rules.filter((r) => !r.deactivated).length;
    $("sum-rounds").textContent = res.iterations;
    $("sum-time").textContent = res.millis;
    $("mobile-count").textContent = res.inferred_total;
    // The data with everything inferred is only shown when the editor could hold it.
    const allTooBig = res.data_triples > ((state.meta && state.meta.max_editor_triples) || 20000);
    $("show-all").disabled = allTooBig;
    $("show-all").title = allTooBig ? "Too big to show here; use Download" : "";
    if (allTooBig && state.turtleShow === "all") setTurtleShow("inferred");
    setBusy(false, `Ran at ${new Date().toLocaleTimeString()}`);

    const notices = [];
    if (res.stopped) notices.push({ kind: "error", text: res.stopped });
    if (res.inferred_total > res.inferred.length)
      notices.push({
        kind: "warn",
        text: `The table lists the first ${res.inferred.length.toLocaleString()} of ${res.inferred_total.toLocaleString()} inferred triples. Download them, or query them all in the SPARQL tab.`,
      });
    for (const w of res.warnings) notices.push({ kind: "warn", html: linkTerms(w) });
    const failed = res.rules.filter((r) => r.errors.length);
    if (failed.length)
      notices.push({
        kind: "error",
        html: `${plural(failed.length, "rule")} failed: ${failed.map((r) => `<b>${esc(ruleName(r))}</b>`).join(", ")}. <a href="#" data-goto-tab="rules">See the Rules tab</a>.`,
      });
    showNotices(notices);
    const badge = $("rules-badge");
    badge.classList.toggle("hidden", !failed.length);
    badge.textContent = failed.length;

    renderRuleFilter(res);
    renderTriples(res);
    renderRules(res);
    renderTurtle();
    updateSparqlEndpoint();
  }

  /** Shorten <iri> tokens in a server message. */
  function linkTerms(text) {
    return esc(text).replace(/&lt;([^&\s]+)&gt;/g, (_, iri) => `<code>${esc(shortIri(iri))}</code>`);
  }

  function renderRuleFilter(res) {
    const sel = $("rule-filter");
    const prev = sel.value;
    sel.innerHTML =
      `<option value="">All rules</option>` +
      res.rules.map((r, i) => `<option value="${i}">${esc(ruleName(r))}</option>`).join("");
    if (prev && Number(prev) < res.rules.length) sel.value = prev;
  }

  function rowsFor(res) {
    return res.inferred.map((t, i) => ({
      id: i,
      s: t.s,
      p: t.p,
      o: t.o,
      rule: t.rule,
      ruleName: ruleName(res.rules[t.rule]),
      focus: t.focus,
      iteration: t.iteration,
      text: [termText(t.s), termText(t.p), termText(t.o), ruleName(res.rules[t.rule]), termText(t.focus)].join(" ").toLowerCase(),
    }));
  }

  function renderTriples(res) {
    const rows = rowsFor(res);
    const empty = $("triples-empty");
    if (!rows.length) {
      empty.classList.remove("hidden");
      const active = res.rules.filter((r) => !r.deactivated && !r.errors.length);
      empty.innerHTML = !res.rules.length
        ? "<b>No rules yet.</b> Add a <code>sh:rule</code> to a shape in the shapes editor, or pick an example."
        : active.some((r) => r.focus_nodes)
          ? "<b>Nothing new.</b> The rules ran, but every triple they produced was already in the data."
          : "<b>No focus nodes.</b> No rule found a node to run on: check the targets and conditions in the Rules tab.";
    } else {
      empty.classList.add("hidden");
    }
    const term = (field) => (cell) => termHtml(cell.getRow().getData()[field]);
    const byText = (a, b) => termText(a).localeCompare(termText(b));
    if (!state.table) {
      state.table = new Tabulator($("triples-table"), {
        data: rows,
        layout: "fitColumns",
        responsiveLayout: "hide",
        height: "100%",
        placeholder: "",
        index: "id",
        columnDefaults: { headerSortTristate: true, resizable: true },
        groupHeader: (value, count) => `${esc(value)} <span class="muted">(${count})</span>`,
        columns: [
          { title: "Subject", field: "s", formatter: term("s"), minWidth: 100, widthGrow: 2, responsive: 0, sorter: byText },
          { title: "Predicate", field: "p", formatter: term("p"), minWidth: 90, widthGrow: 1.8, responsive: 0, sorter: byText },
          { title: "Object", field: "o", formatter: term("o"), minWidth: 100, widthGrow: 2, responsive: 0, sorter: byText },
          {
            title: "Rule",
            field: "ruleName",
            minWidth: 90,
            widthGrow: 1.2,
            responsive: 1,
            formatter: (cell) => `<button class="rule-chip c${cell.getRow().getData().rule % 8}" title="Show only this rule">${esc(cell.getValue())}</button>`,
            cellClick: (e, cell) => {
              if (e.target.closest(".rule-chip")) {
                $("rule-filter").value = String(cell.getRow().getData().rule);
                applyTableFilter();
              }
            },
          },
          { title: "Focus node", field: "focus", formatter: term("focus"), minWidth: 90, widthGrow: 1.5, responsive: 3, sorter: byText },
          { title: "Round", field: "iteration", width: 78, hozAlign: "right", responsive: 2, sorter: "number" },
        ],
      });
      state.table.on("tableBuilt", () => {
        state.tableReady = true;
        applyTableFilter();
      });
    } else if (state.tableReady) {
      state.table.replaceData(rows);
      applyTableFilter();
    } else {
      state.table.on("tableBuilt", () => state.table.replaceData(rows));
    }
  }

  function applyTableFilter() {
    if (!state.table || !state.tableReady) return;
    const q = $("triple-filter").value.trim().toLowerCase();
    const rule = $("rule-filter").value;
    state.table.setFilter((row) => (!q || row.text.includes(q)) && (rule === "" || String(row.rule) === rule));
    state.table.setGroupBy($("group-by-rule").checked ? "ruleName" : false);
  }

  function exprRow(name, value) {
    return value ? `<div class="kv"><span>${name}</span><code>${esc(shortenText(value))}</code></div>` : "";
  }

  /** Shorten <iri> and N-Triples terms inside an expression description. */
  function shortenText(text) {
    return String(text).replace(/<([^<>\s]+)>/g, (_, iri) => shortIri(iri)).replace(/_:([A-Za-z0-9]+)/g, (_, id) => bnodeName(id));
  }

  function renderRules(res) {
    const list = $("rules-list");
    if (!res.rules.length) {
      list.innerHTML = `<div class="empty">The shapes graph has no <code>sh:rule</code>. Use the <b>+</b> button above the rules editor to insert one.</div>`;
      return;
    }
    list.innerHTML = res.rules
      .map((r, i) => {
        const badges = [
          `<span class="badge ${r.kind}">${r.kind === "sparql" ? "SPARQL rule" : "Triple rule"}</span>`,
          r.order ? `<span class="badge">order ${r.order}</span>` : "",
          r.deactivated ? `<span class="badge off">deactivated</span>` : "",
          r.errors.length ? `<span class="badge err">failed</span>` : "",
        ].join("");
        const body =
          r.kind === "sparql"
            ? r.construct
              ? `<pre class="query">${esc(dedent(r.construct))}</pre>`
              : ""
            : `<div class="exprs">${exprRow("subject", r.subject)}${exprRow("predicate", r.predicate)}${exprRow("object", r.object)}</div>`;
        const conditions = r.conditions.length
          ? `<div class="kv"><span>conditions</span><code>${r.conditions.map((c) => (c.startsWith("_:") ? "an inline shape" : esc(termText(c)))).join(", ")}</code></div>`
          : "";
        const stats = r.deactivated
          ? `<div class="stats muted">Skipped: <code>sh:deactivated true</code>.</div>`
          : `<div class="stats">
              <span><b>${r.focus_nodes}</b> focus ${r.focus_nodes === 1 ? "node" : "nodes"}</span>
              ${r.conditions.length ? `<span><b>${r.skipped}</b> turned away by conditions</span>` : ""}
              <span class="ok"><b>${r.inferred}</b> new ${r.inferred === 1 ? "triple" : "triples"}</span>
              ${r.existing ? `<span><b>${r.existing}</b> already in the data</span>` : ""}
            </div>`;
        const errors = r.errors.length ? `<ul class="errors">${r.errors.map((e) => `<li>${linkTerms(e)}</li>`).join("")}</ul>` : "";
        const actions = [
          r.inferred ? `<button class="btn small" data-show-rule="${i}">Show triples</button>` : "",
          `<button class="btn small ghost" data-reveal-rule="${i}">Find in editor</button>`,
          r.kind === "sparql" && r.construct ? `<button class="btn small ghost" data-query-rule="${i}">Try in SPARQL</button>` : "",
        ].join("");
        return `<article class="rule-card${r.deactivated ? " off" : ""}${r.errors.length ? " failed" : ""}">
          <header>
            <span class="num c${i % 8}">${i + 1}</span>
            <div class="rule-title">
              <h3>${esc(ruleName(r))}</h3>
              <div class="sub">on <code>${esc(termText(r.shape))}</code>${r.id.startsWith("<") ? ` · <code>${esc(termText(r.id))}</code>` : ""}</div>
            </div>
            <div class="badges">${badges}</div>
          </header>
          ${r.comment ? `<p class="comment">${esc(r.comment)}</p>` : ""}
          ${body}${conditions}${stats}${errors}
          <footer>${actions}</footer>
        </article>`;
      })
      .join("");
  }

  function dedent(text) {
    const lines = String(text).replace(/^\s*\n/, "").replace(/\s+$/, "").split("\n");
    const indents = lines.filter((l) => l.trim()).map((l) => l.match(/^\s*/)[0].length);
    const cut = indents.length ? Math.min(...indents) : 0;
    return lines.map((l) => l.slice(cut)).join("\n");
  }

  function setTurtleShow(what) {
    state.turtleShow = what;
    document.querySelectorAll("[data-show]").forEach((x) => x.classList.toggle("active", x.dataset.show === what));
    renderTurtle();
  }

  function renderTurtle() {
    const res = state.result;
    if (!state.turtle) state.turtle = new window.PlaygroundEditor($("turtle-view"), { readOnly: true });
    if (!res) return;
    if (state.turtleShow === "all") {
      if (state.allTurtle === null) {
        state.turtle.value = "# loading…";
        fetch(`/api/runs/${res.run}/download?what=all&format=ttl`)
          .then((r) => (r.ok ? r.text() : Promise.reject(r.statusText)))
          .then((t) => {
            state.allTurtle = t;
            if (state.turtleShow === "all") state.turtle.value = t;
          })
          .catch(() => (state.turtle.value = "# this run has expired; run the rules again"));
      } else {
        state.turtle.value = state.allTurtle;
      }
    } else {
      state.turtle.value = !res.inferred_total
        ? "# nothing inferred"
        : res.turtle !== null && res.turtle !== undefined
          ? res.turtle
          : `# ${res.inferred_total} triples inferred, too many to show here: use Download`;
    }
  }

  // ---------- SPARQL (MatGUI) ----------

  function endpointUrl() {
    return state.result && state.result.run ? `${location.origin}/api/runs/${state.result.run}/sparql` : "";
  }

  function prefixBlock() {
    return prefixBlockOf(state.prefixes);
  }

  function prefixBlockOf(prefixes) {
    return prefixes.map(([n, iri]) => `PREFIX ${n}: <${iri}>`).join("\n");
  }

  /** A MatGUI instance in `host`, or null when it could not be loaded. */
  function newYasgui(host, endpoint, height) {
    const Y = window.Yasgui && (window.Yasgui.default || window.Yasgui);
    if (typeof Y !== "function") {
      host.innerHTML = '<div class="empty">The SPARQL editor could not be loaded.</div>';
      return null;
    }
    host.innerHTML = "";
    return new Y(host, {
      requestConfig: { endpoint, method: "POST" },
      theme: currentTheme(),
      showThemeToggle: false,
      showSnippetsBar: false,
      endpointDescribe: { enabled: false },
      copyEndpointOnNewTab: true,
      persistenceId: null,
      populateFromUrl: false,
      autoAddOnInit: false,
      endpointCatalogueOptions: { getData: () => [] },
      yasqe: { resizeable: true, editorHeight: height },
      yasr: { persistenceId: null },
    });
  }

  function ensureYasgui() {
    if (state.yasgui) return state.yasgui;
    state.yasgui = newYasgui($("matgui"), endpointUrl(), "240px");
    if (state.yasgui) openQuery("Inferred", `${prefixBlock()}\n\nSELECT ?s ?p ?o\nWHERE {\n  ?s ?p ?o .\n}\nLIMIT 100`, false);
    return state.yasgui;
  }

  function updateSparqlEndpoint() {
    const y = state.yasgui;
    if (!y) return;
    const endpoint = endpointUrl();
    y.config.requestConfig = { ...y.config.requestConfig, endpoint };
    for (const id of Object.keys(y._tabs || {})) {
      const tab = y.getTab(id);
      if (tab && typeof tab.setEndpoint === "function") tab.setEndpoint(endpoint);
    }
  }

  function openQuery(name, query, runIt) {
    const y = ensureYasgui();
    if (!y) return;
    const tab = y.addTab(true, {
      name,
      yasqe: { value: query },
      requestConfig: { ...y.config.requestConfig, endpoint: endpointUrl() },
    });
    if (!runIt) return;
    const attempt = (tries) => {
      const t = typeof tab.getId === "function" ? y.getTab(tab.getId()) : tab;
      if (t && t.getYasqe && t.getYasqe()) {
        t.getYasqe().setValue(query);
        const done = t.query();
        if (done && done.catch) done.catch(() => {});
      } else if (tries > 0) setTimeout(() => attempt(tries - 1), 50);
    };
    attempt(40);
  }

  /** Open a SPARQL rule's CONSTRUCT with $this as a variable over all nodes. */
  function queryRule(i) {
    const r = state.result.rules[i];
    let q = dedent(r.construct).replace(/\$this\b/g, "?this");
    q = q.replace(/\$currentShape\b/g, r.shape.startsWith("<") ? r.shape : "?currentShape");
    const hasPrefix = /^\s*PREFIX/im.test(q);
    q = `# ${ruleName(r)}: $this is a free variable here, so the query runs for every node,\n# not only the rule's focus nodes.\n${hasPrefix ? "" : prefixBlock() + "\n\n"}${q}`;
    switchTab("sparql");
    openQuery(ruleName(r).slice(0, 30), q, true);
  }

  // ---------- data the server holds ----------

  function dataEndpoint() {
    return state.serverData ? `${location.origin}/api/data/${state.serverData.id}/sparql` : "";
  }

  /** Remember a data graph the server holds, and run the rules on it. */
  function setServerData(info) {
    state.serverData = info;
    if (state.dataYasgui) {
      const y = state.dataYasgui;
      y.config.requestConfig = { ...y.config.requestConfig, endpoint: dataEndpoint() };
      for (const id of Object.keys(y._tabs || {})) {
        const tab = y.getTab(id);
        if (tab && typeof tab.setEndpoint === "function") tab.setEndpoint(dataEndpoint());
      }
    }
    useServerData(true);
  }

  /** Switch the data card between the editor and the query view of the server's data. */
  function useServerData(on) {
    state.useServer = !!(on && state.serverData);
    const d = state.serverData;
    $("data-editor").classList.toggle("hidden", state.useServer);
    $("data-server").classList.toggle("hidden", !state.useServer);
    $("data-to-server").classList.toggle("hidden", state.useServer || !d);
    $("data-error").classList.add("hidden");
    if (!state.useServer) return;
    $("data-server-text").innerHTML = `<b>${d.triples.toLocaleString()}</b> triples from ${d.files
      .map((f) => `<b>${esc(f)}</b>`)
      .join(", ")}, too many for the editor. Query them here; the rules run on all of them.`;
    $("data-server-text").title = d.files.join("\n");
    if (!state.dataYasgui) {
      state.dataYasgui = newYasgui($("data-matgui"), dataEndpoint(), "160px");
      if (state.dataYasgui) {
        const prefixes = (d.prefixes || []).slice();
        for (const p of WELL_KNOWN) if (!prefixes.some(([n]) => n === p[0])) prefixes.push(p);
        const tab = state.dataYasgui.addTab(true, {
          name: "Data",
          yasqe: { value: `${prefixBlockOf(prefixes)}\n\nSELECT ?class (COUNT(?s) AS ?instances)\nWHERE {\n  ?s a ?class .\n}\nGROUP BY ?class\nORDER BY DESC(?instances)\nLIMIT 100` },
          requestConfig: { ...state.dataYasgui.config.requestConfig, endpoint: dataEndpoint() },
        });
        void tab;
      }
    }
  }

  // ---------- collapse and maximize ----------

  function applyLayout() {
    const ws = $("workspace");
    for (const name of ["data", "shapes", "results"])
      ws.classList.toggle(`collapse-${name}`, !state.maximized && state.collapsed.has(name));
    if (state.maximized) ws.dataset.max = state.maximized;
    else delete ws.dataset.max;
    document.querySelectorAll(".pane-ctl button").forEach((b) => {
      const label = b.getAttribute("aria-label").replace(/^\w+ /, "");
      b.title = b.dataset.maximize
        ? state.maximized === b.dataset.maximize
          ? "Restore the layout (Esc)"
          : `Maximize ${label} (Esc restores)`
        : `${state.collapsed.has(b.dataset.collapse) ? "Expand" : "Collapse"} ${label}`;
    });
    storage((ls) => ls.setItem(LAYOUT_KEY, JSON.stringify({ collapsed: [...state.collapsed], maximized: state.maximized })));
    if (state.table) setTimeout(() => state.table.redraw(), 0);
  }

  function toggleCollapse(name) {
    if (state.collapsed.has(name)) state.collapsed.delete(name);
    else state.collapsed.add(name);
    state.maximized = null;
    applyLayout();
  }

  function toggleMaximize(name) {
    state.maximized = state.maximized === name ? null : name;
    applyLayout();
  }

  // ---------- tabs, panes, menus ----------

  function switchTab(name) {
    state.tab = name;
    document.querySelectorAll(".tabs button").forEach((b) => b.classList.toggle("active", b.dataset.tab === name));
    document.querySelectorAll(".tab-body").forEach((el) => el.classList.toggle("hidden", el.id !== `tab-${name}`));
    if (name === "sparql") ensureYasgui();
    if (name === "triples" && state.table) state.table.redraw();
  }

  function switchPane(name) {
    document.querySelectorAll(".mobile-switch button").forEach((b) => b.classList.toggle("active", b.dataset.pane === name));
    $("workspace").dataset.pane = name;
    if (name === "results" && state.table) setTimeout(() => state.table.redraw(), 0);
  }

  function toggleMenu(btn, menu, open) {
    const show = open !== undefined ? open : menu.classList.contains("hidden");
    document.querySelectorAll(".menu-pop").forEach((m) => m !== menu && m.classList.add("hidden"));
    menu.classList.toggle("hidden", !show);
    btn.setAttribute("aria-expanded", String(show));
  }

  const SNIPPETS = [
    {
      title: "Triple rule",
      text: `ex:MyShape
    a sh:NodeShape ;
    sh:targetClass ex:Thing ;
    sh:rule [
        a sh:TripleRule ;
        rdfs:label "my rule" ;
        sh:subject sh:this ;
        sh:predicate ex:property ;
        sh:object ex:value ;
    ] .`,
    },
    {
      title: "Triple rule with a path and a condition",
      text: `ex:MyShape
    a sh:NodeShape ;
    sh:targetClass ex:Thing ;
    sh:rule [
        a sh:TripleRule ;
        rdfs:label "copy along a path" ;
        sh:subject sh:this ;
        sh:predicate ex:related ;
        sh:object [ sh:path ( ex:first ex:second ) ] ;
        sh:condition [
            sh:property [ sh:path ex:first ; sh:minCount 1 ] ;
        ] ;
    ] .`,
    },
    {
      title: "SPARQL rule",
      text: `ex:MyShape
    a sh:NodeShape ;
    sh:targetClass ex:Thing ;
    sh:rule [
        a sh:SPARQLRule ;
        rdfs:label "my SPARQL rule" ;
        sh:construct """
            PREFIX ex: <http://example.org/>
            CONSTRUCT { $this ex:derived ?value }
            WHERE { $this ex:source ?value }
            """ ;
    ] .`,
    },
    {
      title: "SPARQL 1.2 rule with triple terms",
      text: `ex:ClaimShape
    a sh:NodeShape ;
    sh:targetSubjectsOf rdf:reifies ;
    sh:rule [
        a sh:SPARQLRule ;
        rdfs:label "state confident claims" ;
        sh:construct """
            PREFIX ex: <http://example.org/>
            PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
            CONSTRUCT { ?s ?p ?o }
            WHERE {
                $this rdf:reifies <<( ?s ?p ?o )>> ;
                      ex:confidence ?c .
                FILTER (?c >= 0.8)
            }
            """ ;
    ] .`,
    },
    {
      title: "SPARQL target",
      text: `ex:MyShape
    a sh:NodeShape ;
    sh:target [
        a sh:SPARQLTarget ;
        sh:select """
            PREFIX ex: <http://example.org/>
            SELECT ?this WHERE { ?this ex:source ?any }
            """ ;
    ] .`,
    },
    {
      title: "SPARQL function",
      text: `ex:myFunction
    a sh:SPARQLFunction ;
    sh:parameter [ sh:path ex:arg ] ;
    sh:select """
        PREFIX ex: <http://example.org/>
        SELECT (UCASE(STR($arg)) AS ?result) WHERE {}
        """ .
# Call it in a triple rule: sh:object ( ex:myFunction [ sh:path ex:name ] )`,
    },
    {
      title: "Prefixes",
      text: `@prefix ex: <http://example.org/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .`,
    },
  ];

  function revealRule(i) {
    const r = state.result.rules[i];
    const ed = state.shapes;
    const needles = [];
    if (r.label) needles.push(`"${r.label}"`);
    if (r.id.startsWith("<")) {
      needles.push(termText(r.id), r.id);
    }
    if (r.construct) needles.push(r.construct.trim().split("\n")[0].trim());
    switchPane("shapes");
    for (const n of needles) if (n && ed.reveal(n)) return;
    toast("Could not find this rule in the editor");
  }

  // ---------- files ----------

  let fileTarget = "data";

  /** Files the editor takes as they are; anything else is parsed on the server. */
  const EDITOR_FILE = /\.(ttl|turtle|shacl|txt)$/i;
  const EDITOR_BYTES = 1024 * 1024;

  async function loadFile(file, target) {
    const editor = target === "shapes" ? state.shapes : state.data;
    if (EDITOR_FILE.test(file.name) && file.size <= EDITOR_BYTES) {
      editor.value = await file.text();
      if (target === "data") useServerData(false);
      toast(`Opened ${file.name}`);
      run();
      return;
    }
    setBusy(true, `Reading ${file.name}…`);
    let res;
    try {
      const r = await fetch(`/api/data?name=${encodeURIComponent(file.name)}`, { method: "POST", body: file });
      if (!r.ok) throw new Error((await r.text()) || `HTTP ${r.status}`);
      res = await r.json();
    } catch (e) {
      setBusy(false, "Ready");
      toast(String(e.message || e).slice(0, 200));
      return;
    }
    setBusy(false, "Ready");
    if (res.text !== null && res.text !== undefined) {
      editor.value = res.text;
      if (target === "data") useServerData(false);
      toast(`Opened ${file.name}`);
    } else if (target === "data") {
      setServerData(res.data);
      toast(`Opened ${file.name}: ${res.data.triples.toLocaleString()} triples, kept on the server`);
    } else {
      toast(`${file.name} is too big for the rules editor`);
      return;
    }
    run();
  }

  function wireDrop(card, target) {
    card.addEventListener("dragover", (e) => {
      e.preventDefault();
      card.classList.add("drop");
    });
    card.addEventListener("dragleave", () => card.classList.remove("drop"));
    card.addEventListener("drop", (e) => {
      e.preventDefault();
      card.classList.remove("drop");
      const f = e.dataTransfer && e.dataTransfer.files[0];
      if (f) loadFile(f, target);
    });
  }

  // ---------- splitters ----------

  function wireSplitters() {
    const split = $("splitter");
    const editors = document.querySelector(".editors");
    split.addEventListener("pointerdown", (e) => {
      e.preventDefault();
      split.setPointerCapture(e.pointerId);
      const rect = editors.getBoundingClientRect();
      const move = (ev) => {
        const frac = Math.min(0.85, Math.max(0.15, (ev.clientY - rect.top) / rect.height));
        editors.style.setProperty("--top", `${frac}fr`);
        editors.style.setProperty("--bottom", `${1 - frac}fr`);
      };
      const up = () => {
        split.removeEventListener("pointermove", move);
        split.removeEventListener("pointerup", up);
      };
      split.addEventListener("pointermove", move);
      split.addEventListener("pointerup", up);
    });
    const vs = $("vsplitter");
    const ws = $("workspace");
    vs.addEventListener("pointerdown", (e) => {
      e.preventDefault();
      vs.setPointerCapture(e.pointerId);
      const rect = ws.getBoundingClientRect();
      const move = (ev) => {
        const frac = Math.min(0.75, Math.max(0.25, (ev.clientX - rect.left) / rect.width));
        ws.style.setProperty("--left", `${frac}fr`);
        ws.style.setProperty("--right", `${1 - frac}fr`);
      };
      const up = () => {
        vs.removeEventListener("pointermove", move);
        vs.removeEventListener("pointerup", up);
        if (state.table) state.table.redraw();
      };
      vs.addEventListener("pointermove", move);
      vs.addEventListener("pointerup", up);
    });
  }

  // ---------- start ----------

  async function init() {
    const savedTheme = storage((ls) => ls.getItem(THEME_KEY), null);
    if (savedTheme) applyTheme(savedTheme);

    state.data = new window.PlaygroundEditor($("data-editor"), {
      onChange: scheduleRun,
      onRun: run,
      placeholder: "Turtle: the data the rules run on",
    });
    state.shapes = new window.PlaygroundEditor($("shapes-editor"), {
      onChange: scheduleRun,
      onRun: run,
      placeholder: "Turtle: shapes with sh:rule",
    });

    const [meta, examples] = await Promise.all([
      fetch("/api/meta").then((r) => r.json()).catch(() => ({})),
      fetch("/api/examples").then((r) => r.json()).catch(() => []),
    ]);
    state.meta = meta;
    state.examples = examples;
    $("version").textContent = meta.version ? `v${meta.version}` : "";
    if (meta.max_iterations) $("opt-max").max = meta.max_iterations;
    $("examples").innerHTML =
      `<option value="">Examples…</option>` + examples.map((e) => `<option value="${esc(e.id)}" title="${esc(e.description)}">${esc(e.title)}</option>`).join("");
    $("snippets-menu").innerHTML = SNIPPETS.map((s, i) => `<button data-snippet="${i}" role="menuitem">${esc(s.title)}</button>`).join("");

    // What to show first: a shared link, files from the command line, the
    // last session, or the first example.
    const fromLink = await fromHash();
    const saved = storage((ls) => JSON.parse(ls.getItem(STORE_KEY) || "null"), null);
    if (saved && saved.auto === false) $("opt-auto").checked = false;
    if (fromLink) applySnapshot(fromLink);
    else if (meta.preload) applySnapshot({ d: meta.preload.data, s: meta.preload.shapes });
    else if (saved && (saved.d || saved.s)) applySnapshot(saved);
    else if (examples.length) applySnapshot({ d: examples[0].data, s: examples[0].shapes });
    // Data files given to the server that are too big for the editor; a shared
    // link with data of its own uses that instead.
    if (meta.server_data) {
      state.serverData = meta.server_data;
      useServerData(!(fromLink && fromLink.d));
    }

    const layout = storage((ls) => JSON.parse(ls.getItem(LAYOUT_KEY) || "null"), null);
    if (layout) {
      state.collapsed = new Set((layout.collapsed || []).filter((n) => ["data", "shapes", "results"].includes(n)));
      state.maximized = ["data", "shapes", "results"].includes(layout.maximized) ? layout.maximized : null;
    }

    wireEvents();
    wireSplitters();
    applyLayout();
    renderTurtle();
    run();
  }

  function wireEvents() {
    $("run-btn").addEventListener("click", run);
    $("share-btn").addEventListener("click", shareLink);
    $("theme-btn").addEventListener("click", () => {
      const next = currentTheme() === "dark" ? "light" : "dark";
      applyTheme(next);
      storage((ls) => ls.setItem(THEME_KEY, next));
    });
    $("examples").addEventListener("change", (e) => {
      const ex = state.examples.find((x) => x.id === e.target.value);
      if (!ex) return;
      applySnapshot({ d: ex.data, s: ex.shapes, o: { i: true, m: Number($("opt-max").value) || 100, sd: false } });
      useServerData(false);
      e.target.value = "";
      history.replaceState(null, "", location.pathname);
      toast(ex.description);
      run();
    });
    $("options-btn").addEventListener("click", (e) => {
      e.stopPropagation();
      toggleMenu($("options-btn"), $("options-menu"));
    });
    for (const id of ["opt-iterate", "opt-max", "opt-shapes-in-data"]) $(id).addEventListener("change", () => (save(), run()));
    $("opt-auto").addEventListener("change", save);
    $("snippets-btn").addEventListener("click", (e) => {
      e.stopPropagation();
      toggleMenu($("snippets-btn"), $("snippets-menu"));
    });
    $("snippets-menu").addEventListener("click", (e) => {
      const b = e.target.closest("[data-snippet]");
      if (!b) return;
      state.shapes.append(SNIPPETS[Number(b.dataset.snippet)].text);
      toggleMenu($("snippets-btn"), $("snippets-menu"), false);
    });
    $("download-btn").addEventListener("click", (e) => {
      e.stopPropagation();
      toggleMenu($("download-btn"), $("download-menu"));
    });
    $("download-menu").addEventListener("click", (e) => {
      const b = e.target.closest("[data-dl]");
      if (!b || !state.result) return;
      const [what, format] = b.dataset.dl.split(":");
      window.location.href = `/api/runs/${state.result.run}/download?what=${what}&format=${format}`;
      toggleMenu($("download-btn"), $("download-menu"), false);
    });
    document.addEventListener("click", (e) => {
      if (!e.target.closest(".menu")) document.querySelectorAll(".menu-pop").forEach((m) => m.classList.add("hidden"));
      const goto = e.target.closest("[data-goto-tab]");
      if (goto) {
        e.preventDefault();
        switchTab(goto.dataset.gotoTab);
      }
    });
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        const open = [...document.querySelectorAll(".menu-pop")].some((m) => !m.classList.contains("hidden"));
        document.querySelectorAll(".menu-pop").forEach((m) => m.classList.add("hidden"));
        if (!open && state.maximized && !e.target.closest(".cm-editor, .CodeMirror")) toggleMaximize(state.maximized);
      }
      if ((e.ctrlKey || e.metaKey) && e.key === "Enter" && !e.target.closest(".cm-editor")) {
        e.preventDefault();
        run();
      }
    });
    document.querySelectorAll(".tabs button").forEach((b) => b.addEventListener("click", () => switchTab(b.dataset.tab)));
    document.querySelectorAll(".mobile-switch button").forEach((b) => b.addEventListener("click", () => switchPane(b.dataset.pane)));
    document.querySelectorAll("[data-show]").forEach((b) => b.addEventListener("click", () => setTurtleShow(b.dataset.show)));
    $("data-to-editor").addEventListener("click", () => (useServerData(false), run()));
    $("data-to-server").addEventListener("click", () => (useServerData(true), run()));
    document.querySelectorAll("[data-collapse]").forEach((b) => b.addEventListener("click", () => toggleCollapse(b.dataset.collapse)));
    document.querySelectorAll("[data-maximize]").forEach((b) => b.addEventListener("click", () => toggleMaximize(b.dataset.maximize)));
    $("copy-turtle").addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText(state.turtle.value);
        toast("Turtle copied");
      } catch (_) {
        toast("Copying is not allowed here");
      }
    });
    $("triple-filter").addEventListener("input", applyTableFilter);
    $("rule-filter").addEventListener("change", applyTableFilter);
    $("group-by-rule").addEventListener("change", applyTableFilter);
    $("rules-list").addEventListener("click", (e) => {
      const show = e.target.closest("[data-show-rule]");
      const reveal = e.target.closest("[data-reveal-rule]");
      const query = e.target.closest("[data-query-rule]");
      if (show) {
        $("rule-filter").value = show.dataset.showRule;
        $("triple-filter").value = "";
        switchTab("triples");
        applyTableFilter();
      } else if (reveal) revealRule(Number(reveal.dataset.revealRule));
      else if (query) queryRule(Number(query.dataset.queryRule));
    });
    document.querySelectorAll("[data-open]").forEach((b) =>
      b.addEventListener("click", () => {
        fileTarget = b.dataset.open;
        $("file-input").value = "";
        $("file-input").click();
      }),
    );
    $("file-input").addEventListener("change", (e) => {
      const f = e.target.files[0];
      if (f) loadFile(f, fileTarget);
    });
    wireDrop($("data-card"), "data");
    wireDrop($("shapes-card"), "shapes");
    window.addEventListener("hashchange", async () => {
      const snap = await fromHash();
      if (snap) {
        applySnapshot(snap);
        if (snap.d) useServerData(false);
        run();
      }
    });
    if (window.matchMedia)
      window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => applyTheme(document.documentElement.getAttribute("data-theme")));
  }

  init();
})();
