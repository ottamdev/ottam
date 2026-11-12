//! Swift source generation for the protocol wire types.
//!
//! The emitter consumes the same in-memory schema bundle the JSON artifacts are
//! written from, so "schema artifacts fresh" transitively implies the generated
//! Swift reflects the current Rust types. Output is committed at
//! `app/Sources/OttamBackendClient/ProtocolTypes.generated.swift` and
//! byte-guarded by `tests/artifacts.rs`.

mod model;
mod overrides;
mod render;

pub fn generate_swift_protocol_source() -> String {
    let bundle = crate::protocol_schema_bundle();
    let model = model::build(&bundle);
    render::render(&model)
}
