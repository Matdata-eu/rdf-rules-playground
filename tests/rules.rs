use oxrdf::{Graph, Literal, NamedNode, Term, Triple};
use shacl_af_playground::input::parse_turtle;
use shacl_af_playground::rules::{Options, Outcome, infer};

fn load(text: &str) -> Graph {
    parse_turtle(text, None).expect("valid Turtle").0
}

fn example(name: &str, opts: &Options) -> Outcome {
    let dir = format!("{}/examples/{name}", env!("CARGO_MANIFEST_DIR"));
    let data = load(&std::fs::read_to_string(format!("{dir}/data.ttl")).unwrap());
    let shapes = load(&std::fs::read_to_string(format!("{dir}/rules.ttl")).unwrap());
    let out = infer(&data, &shapes, opts).unwrap();
    for r in &out.rules {
        assert!(r.errors.is_empty(), "{name}: rule {} failed: {:?}", r.id, r.errors);
    }
    out
}

fn iri(s: &str) -> NamedNode {
    NamedNode::new(s).unwrap()
}

fn has(out: &Outcome, s: &str, p: &str, o: impl Into<Term>) -> bool {
    let t = Triple::new(iri(s), iri(p), o.into());
    out.inferred.iter().any(|i| i.triple == t)
}

const EX: &str = "http://example.org/shapes#";
const FAM: &str = "http://example.org/family#";
const SHOP: &str = "http://example.org/shop#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

#[test]
fn rectangles_from_the_spec() {
    let out = example("rectangles", &Options::default());
    let ex = |l: &str| format!("{EX}{l}");
    assert!(has(&out, &ex("SquareRectangle"), RDF_TYPE, iri(&ex("Square"))));
    assert!(!has(&out, &ex("NonSquareRectangle"), RDF_TYPE, iri(&ex("Square"))));
    assert!(has(&out, &ex("NonSquareRectangle"), &ex("area"), Literal::from(6)));
    assert!(has(&out, &ex("SquareRectangle"), &ex("area"), Literal::from(16)));
    // The invalid rectangle fails the sh:condition of both rules.
    assert!(out.inferred.iter().all(|i| i.triple.subject.to_string() != format!("<{}>", ex("InvalidRectangle"))));
    assert_eq!(out.inferred.len(), 3);
    assert!(out.stopped.is_none());
}

#[test]
fn family_needs_iteration() {
    let full = example("family", &Options::default());
    let f = |l: &str| format!("{FAM}{l}");
    assert!(has(&full, &f("Alice"), &f("ancestor"), iri(&f("Erin"))));
    assert!(has(&full, &f("Alice"), &f("grandparent"), iri(&f("Dave"))));
    assert!(has(&full, &f("Alice"), &f("mother"), iri(&f("Carol"))));
    assert!(!has(&full, &f("Alice"), &f("mother"), iri(&f("Bob"))));
    assert!(has(&full, &f("Bob"), &f("child"), iri(&f("Alice"))));
    assert!(has(&full, &f("Alice"), &f("sibling"), iri(&f("Frank"))));
    assert!(!has(&full, &f("Alice"), &f("sibling"), iri(&f("Alice"))));
    assert!(full.iterations > 2);

    let single = example(
        "family",
        &Options {
            iterate: false,
            ..Options::default()
        },
    );
    assert_eq!(single.iterations, 1);
    assert!(single.inferred.len() < full.inferred.len());
}

#[test]
fn aggregates_functions_and_order() {
    let out = example("orders", &Options::default());
    let s = |l: &str| format!("{SHOP}{l}");
    let dec = |v: &str| Literal::new_typed_literal(v, oxrdf::vocab::xsd::DECIMAL);
    assert!(has(&out, &s("Order1"), &s("lineCount"), Literal::from(2)));
    assert!(has(&out, &s("Order1"), &s("total"), dec("89.8")));
    assert!(has(&out, &s("Order2"), &s("total"), dec("219")));
    assert!(has(&out, &s("Order1"), &s("maxAmount"), dec("49.90")));
    assert!(has(&out, &s("Order1"), &s("customerName"), Literal::new_simple_literal("Ann Peeters")));
    assert!(has(&out, &s("Order2"), &s("customerName"), Literal::new_simple_literal("Ben Janssens")));
    assert!(has(&out, &s("Order2"), RDF_TYPE, iri(&s("BigOrder"))));
    assert!(!has(&out, &s("Order1"), RDF_TYPE, iri(&s("BigOrder"))));
    // sh:order made the line totals, the order total and the "big order"
    // check run in one round.
    let total = out.rules.iter().position(|r| r.label.as_deref() == Some("total")).unwrap();
    let first_total = out.inferred.iter().find(|i| i.rule == total).unwrap();
    assert_eq!(first_total.iteration, 1);
}

#[test]
fn sparql_targets_and_deactivated_rules() {
    let out = example("sparql-target", &Options::default());
    let g = |l: &str| format!("http://example.org/geo#{l}");
    assert!(has(&out, &g("Ghent"), RDF_TYPE, iri(&g("BigCity"))));
    assert!(!has(&out, &g("Leuven"), RDF_TYPE, iri(&g("BigCity"))));
    assert!(has(&out, &g("Ghent"), "http://www.w3.org/2000/01/rdf-schema#label", Literal::new_simple_literal("Ghent")));
    assert!(out.rules.iter().any(|r| r.deactivated && r.inferred == 0));
    assert!(!out.inferred.iter().any(|i| i.triple.predicate.as_str() == g("hasBigCity")));
}

#[test]
fn blank_nodes_in_construct_stop_at_the_limit() {
    let data = load("@prefix ex: <http://ex/> . ex:a a ex:Thing .");
    let shapes = load(
        r#"@prefix ex: <http://ex/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
        ex:S sh:targetClass ex:Thing ;
          sh:rule [ a sh:SPARQLRule ; sh:construct "PREFIX ex: <http://ex/> CONSTRUCT { $this ex:p [] } WHERE {}" ] ."#,
    );
    let out = infer(
        &data,
        &shapes,
        &Options {
            max_iterations: 5,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(out.iterations, 5);
    assert!(out.stopped.is_some());
}

#[test]
fn rule_errors_do_not_stop_the_run() {
    let data = load("@prefix ex: <http://ex/> . ex:a a ex:Thing .");
    let shapes = load(
        r#"@prefix ex: <http://ex/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
        ex:S sh:targetClass ex:Thing ;
          sh:rule [ a sh:SPARQLRule ; sh:construct "CONSTRUCT { $this ex:p 1 } WHERE {" ] ;
          sh:rule [ a sh:TripleRule ; sh:subject sh:this ; sh:predicate ex:q ; sh:object [ sh:bogus 1 ] ] ;
          sh:rule [ a sh:TripleRule ; sh:subject sh:this ; sh:predicate ex:ok ; sh:object true ] .
        ex:NoTargets sh:rule [ a sh:TripleRule ; sh:subject sh:this ; sh:predicate ex:x ; sh:object 1 ] ."#,
    );
    let out = infer(&data, &shapes, &Options::default()).unwrap();
    assert_eq!(out.rules.iter().filter(|r| !r.errors.is_empty()).count(), 2);
    assert_eq!(out.inferred.len(), 1);
    assert!(out.warnings.iter().any(|w| w.contains("NoTargets")));
}

#[test]
fn blank_node_focus_nodes_in_sparql_rules() {
    let data = load("@prefix ex: <http://ex/> . ex:a ex:item [ ex:v 2 ] , [ ex:v 3 ] .");
    let shapes = load(
        r#"@prefix ex: <http://ex/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
        ex:S sh:targetObjectsOf ex:item ;
          sh:rule [ a sh:SPARQLRule ; sh:construct "PREFIX ex: <http://ex/> CONSTRUCT { $this ex:double ?d } WHERE { $this ex:v ?v . BIND (?v * 2 AS ?d) }" ] ."#,
    );
    let out = infer(&data, &shapes, &Options::default()).unwrap();
    let mut doubles: Vec<Term> = out.inferred.iter().map(|i| i.triple.object.clone()).collect();
    doubles.sort_by_key(Term::to_string);
    assert_eq!(doubles, vec![Literal::from(4).into(), Literal::from(6).into()] as Vec<Term>);
}
