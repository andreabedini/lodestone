//! Module loader for macros: local files, `http(s)` URLs, and the Lodestone
//! glue embedded in the binary. TypeScript, TSX and JSX are transpiled with
//! deno_ast. No `npm:`, `node:` or `jsr:` specifiers.
//!
//! Ported from deno_core's `examples/ts_module_loader.rs`.
//!
//! Module loading is not subject to the macro's fs permissions (it never
//! was): a macro can import any local file.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use deno_ast::{MediaType, ParseParams, SourceMapOption};
use deno_core::error::ModuleLoaderError;
use deno_core::{
    resolve_import, ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader,
    ModuleSource, ModuleSourceCode, ModuleSpecifier, ModuleType, RequestedModuleType,
    ResolutionKind,
};
use deno_error::JsErrorBox;
use tracing::debug;

use crate::embedded_glue;

type SourceMapStore = Rc<RefCell<HashMap<String, Vec<u8>>>>;

pub struct TypescriptModuleLoader {
    http: reqwest::Client,
    source_maps: SourceMapStore,
}

impl Default for TypescriptModuleLoader {
    fn default() -> Self {
        Self {
            http: reqwest::Client::new(),
            source_maps: Default::default(),
        }
    }
}

fn transpile(
    specifier: &ModuleSpecifier,
    code: String,
    media_type: MediaType,
    source_maps: &SourceMapStore,
) -> Result<String, JsErrorBox> {
    let parsed = deno_ast::parse_module(ParseParams {
        specifier: specifier.clone(),
        text: code.into(),
        media_type,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })
    .map_err(JsErrorBox::from_err)?;
    let res = parsed
        .transpile(
            &deno_ast::TranspileOptions {
                imports_not_used_as_values: deno_ast::ImportsNotUsedAsValues::Remove,
                decorators: deno_ast::DecoratorsTranspileOption::Ecma,
                ..Default::default()
            },
            &deno_ast::TranspileModuleOptions { module_kind: None },
            &deno_ast::EmitOptions {
                source_map: SourceMapOption::Separate,
                inline_sources: true,
                ..Default::default()
            },
        )
        .map_err(JsErrorBox::from_err)?
        .into_source();
    if let Some(source_map) = res.source_map {
        source_maps
            .borrow_mut()
            .insert(specifier.to_string(), source_map.into_bytes());
    }
    Ok(res.text)
}

/// How a module is loaded: its module type, and whether it has to be
/// transpiled first. Honours `with { type: "json" }`.
fn module_type_of(
    media_type: MediaType,
    requested: &RequestedModuleType,
    specifier: &ModuleSpecifier,
) -> Result<(ModuleType, bool), JsErrorBox> {
    if matches!(requested, RequestedModuleType::Json) {
        return Ok((ModuleType::Json, false));
    }
    Ok(match media_type {
        MediaType::JavaScript | MediaType::Mjs | MediaType::Cjs => (ModuleType::JavaScript, false),
        MediaType::Jsx
        | MediaType::TypeScript
        | MediaType::Mts
        | MediaType::Cts
        | MediaType::Dts
        | MediaType::Dmts
        | MediaType::Dcts
        | MediaType::Tsx => (ModuleType::JavaScript, true),
        MediaType::Json => (ModuleType::Json, false),
        _ => {
            return Err(JsErrorBox::generic(format!(
                "Unknown module type of {specifier}"
            )))
        }
    })
}

/// `specifier` is the requested module, `found` where it was loaded from
/// (they differ after an HTTP redirect).
fn finish(
    specifier: &ModuleSpecifier,
    found: &ModuleSpecifier,
    code: String,
    media_type: MediaType,
    requested: &RequestedModuleType,
    source_maps: &SourceMapStore,
) -> Result<ModuleSource, ModuleLoaderError> {
    let (module_type, should_transpile) = module_type_of(media_type, requested, found)?;
    let code = if should_transpile {
        transpile(found, code, media_type, source_maps)?
    } else {
        code
    };
    Ok(ModuleSource::new_with_redirect(
        module_type,
        ModuleSourceCode::String(code.into()),
        specifier,
        found,
        None,
    ))
}

impl ModuleLoader for TypescriptModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        resolve_import(specifier, referrer).map_err(JsErrorBox::from_err)
    }

    fn load(
        &self,
        specifier: &ModuleSpecifier,
        _referrer: Option<&ModuleLoadReferrer>,
        options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        let requested = options.requested_module_type;
        let source_maps = self.source_maps.clone();

        if let Some(path) = embedded_glue::glue_path(specifier.as_str()) {
            // Lodestone's own glue: serve the copy embedded in this build,
            // never the one on GitHub.
            let load = || {
                let code = embedded_glue::get_glue(path).ok_or_else(|| {
                    JsErrorBox::generic(format!(
                        "{specifier} is not part of the Lodestone glue embedded in this build (no core/{path})"
                    ))
                })?;
                debug!("Loading {specifier} from the embedded glue");
                finish(
                    specifier,
                    specifier,
                    code.to_string(),
                    MediaType::from_path(Path::new(path)),
                    &requested,
                    &source_maps,
                )
            };
            return ModuleLoadResponse::Sync(load());
        }

        match specifier.scheme() {
            "file" => {
                let load = || {
                    let path = specifier.to_file_path().map_err(|_| {
                        JsErrorBox::generic(format!("Invalid file URL: {specifier}"))
                    })?;
                    let code = std::fs::read_to_string(&path).map_err(|e| {
                        JsErrorBox::generic(format!("Failed to read {}: {e}", path.display()))
                    })?;
                    finish(
                        specifier,
                        specifier,
                        code,
                        MediaType::from_path(&path),
                        &requested,
                        &source_maps,
                    )
                };
                ModuleLoadResponse::Sync(load())
            }
            "http" | "https" => {
                let http = self.http.clone();
                let specifier = specifier.clone();
                ModuleLoadResponse::Async(Box::pin(async move {
                    let res = http
                        .get(specifier.as_str())
                        .send()
                        .await
                        .and_then(|r| r.error_for_status())
                        .map_err(|e| {
                            JsErrorBox::generic(format!("Failed to fetch module {specifier}: {e}"))
                        })?;
                    let found = res.url().clone();
                    let content_type = res
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    let media_type =
                        MediaType::from_specifier_and_content_type(&found, content_type.as_deref());
                    let code = res.text().await.map_err(|e| {
                        JsErrorBox::generic(format!("Failed to read module {specifier}: {e}"))
                    })?;
                    finish(
                        &specifier,
                        &found,
                        code,
                        media_type,
                        &requested,
                        &source_maps,
                    )
                }))
            }
            other => ModuleLoadResponse::Sync(Err(JsErrorBox::generic(format!(
                "Unsupported module specifier scheme {other:?}: {specifier}"
            )))),
        }
    }

    fn get_source_map(&self, specifier: &str) -> Option<Cow<'_, [u8]>> {
        self.source_maps
            .borrow()
            .get(specifier)
            .map(|v| v.clone().into())
    }
}
