# shacl-af-playground

Try [SHACL Advanced Features](https://www.w3.org/TR/shacl-af/) rules in your
browser. Type or paste a data graph and a shapes graph with `sh:rule`, and see
which triples the rules infer, which rule inferred each one, on which focus
node, and in which round. The same engine runs from the command line.

![The playground with the family example: data and rules on the left, the inferred triples on the right](screenshot-webapp.png)

## Features

- **Triple rules** (`sh:TripleRule`) with node expressions for `sh:subject`,
  `sh:predicate` and `sh:object`.
- **SPARQL rules** (`sh:SPARQLRule`) with `sh:construct`, `sh:prefixes`, and
  `$this`, `$currentShape` and `$shapesGraph` pre-bound.
- **Conditions, order and deactivation**: `sh:condition` (checked with a full
  SHACL Core and SHACL-SPARQL validator), `sh:order`, `sh:deactivated`.
- **Iteration**: rules repeat until a round infers nothing new, so rules can
  build on each other's output. Switch it off to run each rule once.
- **Targets**: `sh:targetClass`, `sh:targetNode`, `sh:targetSubjectsOf`,
  `sh:targetObjectsOf`, implicit class targets, and SPARQL-based targets
  (`sh:target [ a sh:SPARQLTarget ; sh:select … ]`).
- **Explanations**: every inferred triple shows its rule, focus node and round;
  the Rules tab shows each rule's focus nodes, how many its conditions turned
  away, what it inferred, and its errors, without stopping the other rules.
- **SPARQL**: query the data with everything inferred in
  [MatGUI](https://github.com/Matdata-eu/MatGUI). "Try in SPARQL" opens a SPARQL
  rule's CONSTRUCT query there.
- **Editors** with Turtle highlighting, syntax errors marked on their line,
  rule templates, files opened or dropped in, and running while you type.
- **Share links** that hold both editors and the options, compressed into the
  URL. Nothing is stored on the server.
- **Turtle and N-Triples downloads** of the inferred triples, or of the data
  with the inferred triples added.
- Light and dark mode, and a phone layout.

![The Rules tab](screenshot-rules.png)

### Node expressions

| Expression | Meaning |
| --- | --- |
| `sh:this` | The focus node. |
| an IRI or literal | Itself. |
| `[ sh:path p ]` | The values of the SHACL property path `p` from the focus node. |
| `[ sh:path p ; sh:nodes e ]` | The values of `p` from every node of `e`. |
| `[ sh:filterShape s ; sh:nodes e ]` | The nodes of `e` that conform to the shape `s`. |
| `[ sh:intersection ( e1 e2 … ) ]` | The nodes in every one of the expressions. |
| `[ sh:union ( e1 e2 … ) ]` | The nodes in any of the expressions. |
| `[ sh:count e ]` | The number of nodes of `e`, as an `xsd:integer`. |
| `[ sh:min e ]`, `[ sh:max e ]` | The smallest or largest value of `e`. |
| `[ sh:sum e ]` | The sum of the numbers of `e`, typed like SPARQL's `+`. |
| `( f e1 e2 … )` | A call to `f`, a `sh:SPARQLFunction` declared in the shapes graph, once for every combination of argument values. |

`sh:count`, `sh:min`, `sh:max` and `sh:sum` follow the SHACL 1.2 drafts;
the rest is in the SHACL-AF note.

## Install

From a release on GitHub (Linux, macOS, Windows), with Cargo, or as a Docker
image:

```sh
cargo install --git https://github.com/Matdata-eu/shacl-af-playground
docker run --rm -p 8080:8080 ghcr.io/matdata-eu/shacl-af-playground
```

## Usage

### The playground

```sh
shacl-af-playground                     # opens the playground in your browser
shacl-af-playground serve --data data.ttl --shapes rules.ttl   # with your files loaded
```

It starts with the first example; the **Examples** menu has four:

| Example | Shows |
| --- | --- |
| Rectangles (from the spec) | The two examples of the SHACL-AF note: a triple rule with conditions, a SPARQL rule. |
| Family relations | Path expressions, `sh:union`, `sh:filterShape`, and a rule that needs several rounds. |
| Orders: aggregates and functions | `sh:count`, `sh:sum`, `sh:max`, a SPARQL function call, and `sh:order`. |
| Cities: SPARQL target | A SPARQL-based target, and a deactivated rule. |

Press **Ctrl+Enter** (Cmd+Enter on a Mac) to run, or leave "Run while typing"
on. Under **Options**:

- **Repeat until nothing new**: iterate to a fixpoint (on by default). A rule
  that creates a blank node, or a new value, every time it runs never settles;
  the run then stops after **Most rounds** and says so.
- **Read rules from the data too**: for files that keep data and shapes together.

The SPARQL tab queries the data graph with everything inferred. The shapes
graph is the named graph `<urn:x-shacl-af-playground:shapes-graph>`. Each run
has its own endpoint at `/api/runs/{id}/sparql`, so it also works from other
SPARQL clients while the playground keeps that run (the last 32 runs).

### From the command line

```sh
shacl-af-playground infer --data data.ttl --shapes rules.ttl
```

prints the inferred triples as Turtle. Without `--shapes`, the rules are read
from the data graph.

| Option | |
| --- | --- |
| `-d, --data FILE…` | Data graph files (Turtle, N-Triples, RDF/XML, TriG, N-Quads, also gzipped); `-` reads Turtle from standard input. |
| `-s, --shapes FILE…` | Shapes graph files holding the rules. |
| `--format FORMAT` | Syntax of every input file, if the extension does not tell. |
| `-o, --output FILE` | Write to a file instead of standard output. |
| `--output-format turtle\|ntriples` | Syntax of the result. |
| `--all` | Write the data graph with the inferred triples added. |
| `--single-pass` | Run every rule once instead of repeating until nothing new is inferred. |
| `--max-iterations N` | Most rounds (default 100). |
| `--explain` | Print, for every rule, its focus nodes, how many its conditions turned away, and what it inferred. |
| `-q, --quiet` | Print only errors on standard error. |

```console
$ shacl-af-playground infer -d examples/rectangles/data.ttl -s examples/rectangles/rules.ttl --explain
@prefix ex: <http://example.org/shapes#> .

ex:NonSquareRectangle
    ex:area 6 .

ex:SquareRectangle
    a ex:Square ;
    ex:area 16 .

rule Area [sparql on <http://example.org/shapes#Rectangle>, order 0]: focus-nodes=2 skipped-by-condition=1 inferred=2 already-present=0
rule Squares [triple on <http://example.org/shapes#Rectangle>, order 0]: focus-nodes=1 skipped-by-condition=2 inferred=1 already-present=0
rules=2  rounds=2  inferred=3  in 5 ms
```

The exit code is 0 on success, 1 when a rule failed or a limit stopped the
run, and 2 when the input could not be read.

### Hosting it

`serve --public` (or `SHACL_AF_PLAYGROUND_PUBLIC=true`, as in the Docker image)
runs the playground as a public website: it never opens a browser and caps
every request.

| Flag | Environment variable | Default |
| --- | --- | --- |
| `--bind ADDR` | `SHACL_AF_PLAYGROUND_BIND` | `127.0.0.1:0` (Docker: `0.0.0.0:8080`) |
| `--public` | `SHACL_AF_PLAYGROUND_PUBLIC` | off (Docker: on) |
| `--max-input-bytes BYTES` | `SHACL_AF_PLAYGROUND_MAX_INPUT_BYTES` | 2 MB per editor |
| `--max-triples N` | `SHACL_AF_PLAYGROUND_MAX_TRIPLES` | 50 000 per editor, and inferred |
| `--max-seconds S` | `SHACL_AF_PLAYGROUND_MAX_SECONDS` | 20 per run or query |

Public runs are also limited to 50 rounds, and to as many at once as the
machine has cores.

## How rules run

1. Every `sh:rule` in the shapes graph is collected and sorted by `sh:order`
   (default 0), then by shape and by rule label, so runs are repeatable.
2. For each rule that is not deactivated, the focus nodes are the targets of
   its shape, minus those that do not conform to every `sh:condition`.
3. The rule runs on each focus node. New triples go into the data graph
   straight away, so the next rule already sees them.
4. When a round inferred something, the next round starts at step 2. A round
   that infers nothing new ends the run.

## Not supported

- `sh:values` and `sh:defaultValue` on property shapes.
- `sh:JSRule` and JavaScript functions.
- Function expressions over SPARQL's built-in functions; declare a
  `sh:SPARQLFunction` instead.
- Custom target types (`sh:SPARQLTargetType`).

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

The web assets in `assets/` are embedded in the binary at compile time. The
SHACL validator in `src/validate` comes from
[shacl-report](https://github.com/Matdata-eu/shacl-report), where it is tested
against the W3C SHACL test suite.

Third-party browser code is vendored under `assets/vendor`: MatGUI (MIT),
Tabulator (MIT) and CodeMirror 6 with a Turtle grammar (MIT). The `NOTICE.md`
files there list versions and how to rebuild the bundles.

## License

Apache-2.0
