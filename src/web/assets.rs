//! Static files of the playground, embedded in the binary.

pub struct Asset {
    pub path: &'static str,
    pub mime: &'static str,
    pub bytes: &'static [u8],
}

macro_rules! asset {
    ($p:literal, $mime:literal) => {
        Asset {
            path: concat!("/assets/", $p),
            mime: $mime,
            bytes: include_bytes!(concat!("../../assets/", $p)),
        }
    };
}

pub static ASSETS: &[Asset] = &[
    asset!("app/index.html", "text/html; charset=utf-8"),
    asset!("app/app.js", "application/javascript"),
    asset!("app/editor.js", "application/javascript"),
    asset!("app/styles.css", "text/css; charset=utf-8"),
    asset!("app/favicon.svg", "image/svg+xml"),
    asset!(
        "vendor/codemirror/codemirror.min.js",
        "application/javascript"
    ),
    asset!("vendor/matgui/matgui.min.js", "application/javascript"),
    asset!("vendor/matgui/matgui.min.css", "text/css; charset=utf-8"),
    asset!(
        "vendor/tabulator/tabulator.min.js",
        "application/javascript"
    ),
    asset!(
        "vendor/tabulator/tabulator.min.css",
        "text/css; charset=utf-8"
    ),
];

pub fn lookup(path: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|a| a.path == path)
}

/// One of the examples offered in the playground.
pub struct Example {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub data: &'static str,
    pub shapes: &'static str,
}

macro_rules! example {
    ($id:literal, $title:literal, $description:literal) => {
        Example {
            id: $id,
            title: $title,
            description: $description,
            data: include_str!(concat!("../../examples/", $id, "/data.ttl")),
            shapes: include_str!(concat!("../../examples/", $id, "/rules.ttl")),
        }
    };
}

pub static EXAMPLES: &[Example] = &[
    example!(
        "rectangles",
        "Rectangles (from the spec)",
        "A triple rule with conditions infers squares; a SPARQL rule computes areas."
    ),
    example!(
        "family",
        "Family relations",
        "Path expressions, sh:union, sh:filterShape, and a rule that needs several rounds."
    ),
    example!(
        "orders",
        "Orders: aggregates and functions",
        "sh:count, sh:sum, sh:max, a SPARQL function call, and sh:order between rules."
    ),
    example!(
        "sparql-target",
        "Cities: SPARQL target",
        "A SPARQL-based target picks focus nodes; one rule is deactivated."
    ),
];
