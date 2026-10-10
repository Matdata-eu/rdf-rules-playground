//! The playground's HTTP API, driven over a real socket.

use shacl_af_playground::web::{AppState, Limits, api};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

fn start(limits: Option<Limits>) -> SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            let app = api::router(AppState::new(limits, None))
                .into_make_service_with_connect_info::<SocketAddr>();
            axum::serve(listener, app).await.unwrap();
        });
    });
    rx.recv().unwrap()
}

/// Send one request; returns (status, headers, body).
fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    // Bodies may be chunked; strip chunk sizes for these small responses.
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut out = String::new();
        let mut rest = body;
        while let Some((size, tail)) = rest.split_once("\r\n") {
            let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
            if n == 0 {
                break;
            }
            out.push_str(&tail[..n]);
            rest = &tail[n + 2..];
        }
        out
    } else {
        body.to_string()
    };
    (status, head.to_string(), body)
}

fn infer(addr: SocketAddr, data: &str, shapes: &str) -> serde_json::Value {
    let body = serde_json::json!({ "data": data, "shapes": shapes }).to_string();
    let (status, _, body) = request(
        addr,
        "POST",
        "/api/infer",
        &[("Content-Type", "application/json")],
        &body,
    );
    assert_eq!(status, 200, "{body}");
    serde_json::from_str(&body).unwrap()
}

const DATA: &str = "@prefix ex: <http://ex/> . ex:a a ex:Thing ; ex:v 2 .";
const SHAPES: &str = r#"@prefix ex: <http://ex/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
ex:S sh:targetClass ex:Thing ;
  sh:rule [ a sh:SPARQLRule ;
    sh:construct "PREFIX ex: <http://ex/> CONSTRUCT { $this ex:double ?d } WHERE { $this ex:v ?v BIND (?v * 2 AS ?d) }" ] ."#;

#[test]
fn infers_and_serves_the_result() {
    let addr = start(None);
    let res = infer(addr, DATA, SHAPES);
    assert_eq!(res["inferred"].as_array().unwrap().len(), 1);
    assert_eq!(res["inferred"][0]["p"], "<http://ex/double>");
    assert!(res["turtle"].as_str().unwrap().contains("ex:double 4"));
    let run = res["run"].as_str().unwrap();

    // SPARQL over data + inferences
    let q =
        "query=SELECT%20%3Fd%20WHERE%20%7B%20%3Fs%20%3Chttp%3A%2F%2Fex%2Fdouble%3E%20%3Fd%20%7D";
    let (status, head, body) = request(
        addr,
        "POST",
        &format!("/api/runs/{run}/sparql"),
        &[
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Accept", "application/sparql-results+json"),
        ],
        q,
    );
    assert_eq!(status, 200, "{body}");
    assert!(head.contains("application/sparql-results+json"));
    assert!(body.contains("\"value\":\"4\""), "{body}");

    // ...including the shapes graph as a named graph
    let ask = "ASK { GRAPH <urn:x-shacl-af-playground:shapes-graph> { ?s a <http://www.w3.org/ns/shacl#SPARQLRule> } }";
    let (status, _, body) = request(
        addr,
        "POST",
        &format!("/api/runs/{run}/sparql"),
        &[("Content-Type", "application/sparql-query")],
        ask,
    );
    assert_eq!(status, 200);
    assert!(body.contains("true"), "{body}");

    // CONSTRUCT returns Turtle
    let (status, head, body) = request(
        addr,
        "GET",
        &format!("/api/runs/{run}/sparql?query=CONSTRUCT%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D"),
        &[("Accept", "text/turtle")],
        "",
    );
    assert_eq!(status, 200);
    assert!(head.contains("text/turtle"));
    assert!(body.contains("ex:a"), "{body}");

    // downloads
    let (status, head, body) = request(
        addr,
        "GET",
        &format!("/api/runs/{run}/download?what=all&format=nt"),
        &[],
        "",
    );
    assert_eq!(status, 200);
    assert!(head.contains("data-with-inferences.nt"));
    assert_eq!(body.lines().count(), 3);

    let (status, _, _) = request(addr, "GET", "/api/runs/nope/download", &[], "");
    assert_eq!(status, 404);
}

#[test]
fn reports_syntax_errors_with_positions() {
    let addr = start(None);
    let res = infer(addr, "@prefix ex: <http://ex/> .\n\nex:a ex:b .", SHAPES);
    assert_eq!(res["syntax"]["data"]["line"], 3);
    assert!(res["syntax"]["shapes"].is_null());
    assert!(res["run"].is_null());
}

#[test]
fn serves_the_page_and_examples() {
    let addr = start(None);
    let (status, _, body) = request(addr, "GET", "/", &[], "");
    assert_eq!(status, 200);
    assert!(body.contains("SHACL-AF playground"));
    let (status, _, body) = request(addr, "GET", "/api/examples", &[], "");
    assert_eq!(status, 200);
    let examples: serde_json::Value = serde_json::from_str(&body).unwrap();
    for ex in examples.as_array().unwrap() {
        let res = infer(
            addr,
            ex["data"].as_str().unwrap(),
            ex["shapes"].as_str().unwrap(),
        );
        assert!(
            !res["inferred"].as_array().unwrap().is_empty(),
            "{} inferred nothing",
            ex["id"]
        );
    }
    let (status, _, _) = request(addr, "GET", "/assets/vendor/matgui/matgui.min.js", &[], "");
    assert_eq!(status, 200);
}

#[test]
fn public_mode_caps_inputs_and_rounds() {
    let addr = start(Some(Limits {
        max_input_bytes: 2000,
        max_triples: 100,
        max_time: Duration::from_secs(10),
        max_iterations: 3,
    }));
    let big = format!(
        "@prefix ex: <http://ex/> .\n{}",
        "ex:a ex:b ex:c .\n".repeat(200)
    );
    let body = serde_json::json!({ "data": big, "shapes": "" }).to_string();
    let (status, _, _) = request(
        addr,
        "POST",
        "/api/infer",
        &[("Content-Type", "application/json")],
        &body,
    );
    assert_eq!(status, 413);

    let endless = r#"@prefix ex: <http://ex/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
        ex:S sh:targetClass ex:Thing ;
          sh:rule [ a sh:SPARQLRule ; sh:construct "PREFIX ex: <http://ex/> CONSTRUCT { $this ex:p [] } WHERE {}" ] ."#;
    let body =
        serde_json::json!({ "data": DATA, "shapes": endless, "maxIterations": 500 }).to_string();
    let (_, _, body) = request(
        addr,
        "POST",
        "/api/infer",
        &[("Content-Type", "application/json")],
        &body,
    );
    let res: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(res["iterations"], 3);
    assert!(res["stopped"].is_string());
}
