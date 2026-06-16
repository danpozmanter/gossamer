#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::similar_names)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::cast_ptr_alignment)]
#![allow(clippy::ptr_as_ptr)]
#![allow(clippy::wildcard_imports)]

use std::ffi::CStr;
use std::os::raw::c_char;

use super::*;

// ---------------------------------------------------------------
// encoding::yaml - YAML 1.2 parsing + emission via `serde_yaml`.
// Returns `Result<String, errors::Error>` for fallible operations.
// Mirrors the toml_enc.rs surface so the auto-derive synthesizer
// can reuse the same JSON-as-lingua-franca shape.
// ---------------------------------------------------------------

fn yaml_result_ok(s: &str) -> i128 {
    unsafe { gos_rt_result_new(0, alloc_cstring(s.as_bytes()) as i64) }
}

fn yaml_result_err(msg: &str) -> i128 {
    let cs = std::ffi::CString::new(msg).unwrap_or_default();
    let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
    unsafe { gos_rt_result_new(1, err as i64) }
}

fn serde_yaml_to_json(v: serde_yaml::Value) -> serde_json::Value {
    match v {
        serde_yaml::Value::Null => serde_json::Value::Null,
        serde_yaml::Value::Bool(b) => serde_json::Value::Bool(b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_json::Value::Number(i.into())
            } else if let Some(u) = n.as_u64() {
                serde_json::Value::Number(u.into())
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f)
                    .map_or(serde_json::Value::Null, serde_json::Value::Number)
            } else {
                serde_json::Value::Null
            }
        }
        serde_yaml::Value::String(s) => serde_json::Value::String(s),
        serde_yaml::Value::Sequence(items) => {
            serde_json::Value::Array(items.into_iter().map(serde_yaml_to_json).collect())
        }
        serde_yaml::Value::Mapping(map) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in map {
                let key = match &k {
                    serde_yaml::Value::String(s) => s.clone(),
                    serde_yaml::Value::Number(n) => n.to_string(),
                    serde_yaml::Value::Bool(b) => b.to_string(),
                    _ => format!("{k:?}"),
                };
                obj.insert(key, serde_yaml_to_json(v));
            }
            serde_json::Value::Object(obj)
        }
        serde_yaml::Value::Tagged(t) => serde_yaml_to_json(t.value),
    }
}

fn json_to_serde_yaml(v: &serde_json::Value) -> serde_yaml::Value {
    match v {
        serde_json::Value::Null => serde_yaml::Value::Null,
        serde_json::Value::Bool(b) => serde_yaml::Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_yaml::Value::Number(i.into())
            } else if let Some(f) = n.as_f64() {
                serde_yaml::Value::Number(serde_yaml::Number::from(f))
            } else {
                serde_yaml::Value::Null
            }
        }
        serde_json::Value::String(s) => serde_yaml::Value::String(s.clone()),
        serde_json::Value::Array(items) => {
            serde_yaml::Value::Sequence(items.iter().map(json_to_serde_yaml).collect())
        }
        serde_json::Value::Object(map) => {
            let mut m = serde_yaml::Mapping::new();
            for (k, v) in map {
                m.insert(serde_yaml::Value::String(k.clone()), json_to_serde_yaml(v));
            }
            serde_yaml::Value::Mapping(m)
        }
    }
}

/// `encoding::yaml::parse(text) -> Result<json::Value, Error>`.
/// YAML is parsed and re-projected onto the JSON value tree so the
/// dynamic document path reuses the fully-supported `json::Value`
/// runtime type (`json::get` / `as_str` / …) on every tier - the VM's
/// `yaml::parse` routes through the same yaml->json projection. Err
/// payload is a c-string, matching `gos_rt_json_parse`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_yaml_parse(s: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let text = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        match serde_yaml::from_str::<serde_yaml::Value>(text) {
            Ok(yaml_val) => {
                let json_val = serde_yaml_to_json(yaml_val);
                let ptr = crate::c_abi::json::GosJson::into_raw(json_val);
                unsafe { gos_rt_result_new(0, ptr as i64) }
            }
            Err(e) => {
                let cs = alloc_cstring(format!("yaml::parse: {e}").as_bytes());
                unsafe { gos_rt_result_new(1, cs as i64) }
            }
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_yaml_to_json(s: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let text = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        let yaml_val: serde_yaml::Value = match serde_yaml::from_str(text) {
            Ok(v) => v,
            Err(e) => return yaml_result_err(&format!("yaml::to_json: {e}")),
        };
        let json_val = serde_yaml_to_json(yaml_val);
        match serde_json::to_string(&json_val) {
            Ok(out) => yaml_result_ok(&out),
            Err(e) => yaml_result_err(&format!("yaml::to_json: {e}")),
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_yaml_from_json(s: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let text = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        let json_val: serde_json::Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => return yaml_result_err(&format!("yaml::from_json: {e}")),
        };
        let yaml_val = json_to_serde_yaml(&json_val);
        match serde_yaml::to_string(&yaml_val) {
            Ok(out) => yaml_result_ok(&out),
            Err(e) => yaml_result_err(&format!("yaml::from_json: {e}")),
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_yaml_is_valid(s: *const c_char) -> i64 {
    ffi_entry!(0, {
        let text = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        i64::from(serde_yaml::from_str::<serde_yaml::Value>(text).is_ok())
    })
}
