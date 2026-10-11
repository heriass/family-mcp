//! Krónan's OpenAPI document is pinned by packages/kronan-mcp/api/openapi.sha256, and the
//! TypeScript `api:check` and api-types.test.ts gate the TypeScript schemas against it. The Rust
//! request and response shapes are written by hand, so they are pinned to the same hash: when
//! the document changes, this fails until they are revisited and the hash below is updated.
//! The pin itself is checked against the document's bytes here too, so the gate does not depend
//! on the TypeScript scripts.

use std::path::PathBuf;

/// The openapi.json these Rust shapes were last checked against. Before updating it, recheck
/// against the new document: src/input.rs (every tool input and the request bodies and query
/// names built from it, which follow schemas.ts's inputs), src/shapes.rs (the upstream response
/// shapes, which follow schemas.ts's upstream and output schemas), and the endpoint paths and
/// methods in src/api.rs.
const CHECKED: &str = "29d574a541708dcd0346ffbe30bd0d4f77bb773389c16bf4258f7dbdc40fe44d";

#[test]
fn the_rust_shapes_were_checked_against_the_pinned_openapi_document() {
    let pin = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/kronan-mcp/api/openapi.sha256"),
    )
    .unwrap();
    assert_eq!(pin, format!("{CHECKED}  api/openapi.json\n"));
}

/// The pin names the document's actual bytes, as `api:check` hashes them, so an edit to
/// openapi.json without a new pin (and a recheck of the shapes above) fails here.
#[test]
fn the_pin_is_the_sha_256_of_the_openapi_document() {
    let api = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/kronan-mcp/api");
    let document = std::fs::read(api.join("openapi.json")).unwrap();
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &document);
    let hex: String = digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let pin = std::fs::read_to_string(api.join("openapi.sha256")).unwrap();
    assert_eq!(pin, format!("{hex}  api/openapi.json\n"));
}
