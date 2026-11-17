//! Wasm reader oracle (0.3.0): loads the built cdylib in wasmtime and
//! drives the C ABI exactly as the browser JS glue does — open an
//! index, query into a host buffer, read results. Gated on
//! `DOCS_SEARCH_WASM` pointing at the built artifact; CI's wasm step
//! builds it and runs this suite.
//!
//! Test-only lifts (docs-core decoder-pattern precedent): expectations
//! carry context; library paths keep the denies.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use wasmtime::{Engine, Instance, Module, Store, TypedFunc};

/// A loaded reader: the ABI functions the browser JS glue calls.
struct Reader {
    alloc: TypedFunc<u32, u32>,
    free: TypedFunc<(u32, u32), ()>,
    open: TypedFunc<(u32, u32), u32>,
    close: TypedFunc<u32, ()>,
    query: TypedFunc<(u32, u32, u32, u32, u32, u32), u32>,
    memory: wasmtime::Memory,
    store: Store<()>,
}

impl Reader {
    fn load(path: &str) -> Self {
        let engine = Engine::default();
        let module = Module::from_file(&engine, path).expect("wasm module loads");
        let mut store = Store::new(&engine, ());
        let instance =
            Instance::new(&mut store, &module, &[]).expect("instantiates with no imports");
        Self {
            alloc: instance
                .get_typed_func::<u32, u32>(&mut store, "dsidx_alloc")
                .expect("dsidx_alloc exported"),
            free: instance
                .get_typed_func::<(u32, u32), ()>(&mut store, "dsidx_free")
                .expect("dsidx_free exported"),
            open: instance
                .get_typed_func::<(u32, u32), u32>(&mut store, "dsidx_open")
                .expect("dsidx_open exported"),
            close: instance
                .get_typed_func::<u32, ()>(&mut store, "dsidx_close")
                .expect("dsidx_close exported"),
            query: instance
                .get_typed_func::<(u32, u32, u32, u32, u32, u32), u32>(&mut store, "dsidx_query")
                .expect("dsidx_query exported"),
            memory: instance
                .get_memory(&mut store, "memory")
                .expect("exported memory"),
            store,
        }
    }

    /// Copy `bytes` into a fresh allocation; returns (ptr, len).
    fn write_bytes(&mut self, bytes: &[u8]) -> (u32, u32) {
        let ptr = self
            .alloc
            .call(&mut self.store, bytes.len() as u32)
            .expect("alloc");
        {
            let raw = self.memory.data_mut(&mut self.store);
            let dst = &mut raw[ptr as usize..ptr as usize + bytes.len()];
            dst.copy_from_slice(bytes);
        }
        (ptr, bytes.len() as u32)
    }

    /// Open an index over the given bytes; panics on failure (the
    /// oracle asserts success before use).
    fn open_index(&mut self, bytes: &[u8]) -> u32 {
        let (ptr, len) = self.write_bytes(bytes);
        let handle = self
            .open
            .call(&mut self.store, (ptr, len))
            .expect("open call");
        self.free.call(&mut self.store, (ptr, len)).expect("free");
        assert!(handle > 0, "dsidx_open returned 0 (malformed bytes)");
        handle
    }

    /// Run a query, returning (status, JSON results).
    fn check(&mut self, handle: u32, query: &str) -> (u32, String) {
        let (qptr, qlen) = self.write_bytes(query.as_bytes());
        let out_cap = 64 * 1024;
        let out_ptr = self.alloc.call(&mut self.store, out_cap).expect("alloc");
        let written = self
            .query
            .call(&mut self.store, (handle, qptr, qlen, 10, out_ptr, out_cap))
            .expect("query call");
        self.free
            .call(&mut self.store, (qptr, qlen))
            .expect("free query");
        let mut raw = Vec::new();
        if written > 0 {
            let start = out_ptr as usize;
            let bytes = self
                .memory
                .data(&self.store)
                .get(start..start + written as usize)
                .expect("results in memory bounds");
            raw.extend_from_slice(bytes);
        }
        self.free
            .call(&mut self.store, (out_ptr, out_cap))
            .expect("free");
        (written, String::from_utf8(raw).expect("results are UTF-8"))
    }
}

/// Build a tiny two-page index natively (the same bytes a site build
/// emits) to feed the reader.
fn sample_index() -> Vec<u8> {
    let mut builder = docs_search::IndexBuilder::new();
    builder.add_doc("sagas outbox fencing distributed systems failure");
    builder.add_doc("parse dont validate types invariants");
    builder.build().expect("index builds")
}

#[test]
fn wasm_reader_opens_queries_and_frees() {
    let Some(path) = std::env::var_os("DOCS_SEARCH_WASM") else {
        eprintln!("skipped: DOCS_SEARCH_WASM not set (build the cdylib first)");
        return;
    };
    let mut reader = Reader::load(&path.to_string_lossy());

    let index_bytes = sample_index();
    let handle = reader.open_index(&index_bytes);

    // Query "sagas" — the AND matcher requires every term present.
    let (written, json) = reader.check(handle, "sagas");
    assert!(written > 0, "query writes results: {json}");
    assert!(
        json.contains("\"doc\":0"),
        "doc 0 (sagas page) matches: {json}"
    );
    assert!(json.contains("\"score\""), "scores present: {json}");

    // A query matching the second page.
    let (written, json) = reader.check(handle, "parse");
    assert!(written > 0);
    assert!(
        json.contains("\"doc\":1"),
        "doc 1 (parse page) matches: {json}"
    );

    // AND semantics: a term matching neither page yields no hits.
    let (written, json) = reader.check(handle, "sagas zebra");
    assert!(written > 0);
    assert_eq!(json, "[]", "AND miss yields empty results: {json}");

    reader.close.call(&mut reader.store, handle).expect("close");
}

#[test]
fn malformed_index_bytes_open_as_failure() {
    let Some(path) = std::env::var_os("DOCS_SEARCH_WASM") else {
        eprintln!("skipped: DOCS_SEARCH_WASM not set");
        return;
    };
    let mut reader = Reader::load(&path.to_string_lossy());
    let garbage = vec![0u8; 64];
    let (ptr, len) = reader.write_bytes(&garbage);
    let handle = reader
        .open
        .call(&mut reader.store, (ptr, len))
        .expect("open call");
    reader
        .free
        .call(&mut reader.store, (ptr, len))
        .expect("free");
    assert_eq!(handle, 0, "garbage bytes open as failure (handle {handle})");
}
