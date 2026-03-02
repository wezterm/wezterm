use std::collections::{BTreeMap, HashMap};
// use std::collections::Map;
use log::Level;

use reqwest::header;

use crate::mlua::prelude::*;

use serde_json::Value;

pub fn fetch<'lua>(
    lua: &'lua Lua,
    (url, optsv): (String, LuaValue<'lua>),
) -> LuaResult<LuaTable<'lua>> {
    let mut body = "".to_string();
    let mut method = reqwest::Method::GET;
    let mut builder = reqwest::blocking::ClientBuilder::new();
    if !optsv.is_nil() && optsv.is_table() {
        let opts = optsv.as_table().unwrap();
        if opts.contains_key("headers").is_ok() {
            let value = opts.get::<_, LuaValue>("headers")?;
            if !value.is_nil() {
                let headers = opts.get::<_, LuaTable>("headers")?;
                let mut header_map = header::HeaderMap::new();
                for pair in headers.pairs::<String, String>() {
                    let (name, value) = pair?;
                    let header_name = header::HeaderName::from_bytes(name.as_bytes())
                        .map_err(LuaError::external)?;
                    header_map.insert(header_name, value.parse().unwrap());
                }
                builder = builder.default_headers(header_map);
            }
        }
        if opts.contains_key("timeout").is_ok() {
            let value = opts.get::<_, LuaValue>("timeout")?;
            if !value.is_nil() {
                let timeout = opts.get::<_, u64>("timeout")?;
                builder = builder.timeout(std::time::Duration::from_secs(timeout));
            }
        }

        if opts.contains_key("connect_timeout").is_ok() {
            let value = opts.get::<_, LuaValue>("connect_timeout")?;
            if !value.is_nil() {
                println!("connect_timeout");
                let timeout = opts.get::<_, u64>("connect_timeout")?;
                builder = builder.connect_timeout(std::time::Duration::from_secs(timeout));
            }
        }

        if opts.contains_key("version").is_ok() {
            let value = opts.get::<_, LuaValue>("version")?;
            if !value.is_nil() {
                let version = opts.get::<_, u32>("version")?;
                match version {
                    1 => {
                        builder = builder.http1_only();
                    }
                    2 => {
                        builder = builder.http2_prior_knowledge();
                    }
                    _ => return Err(LuaError::external("invalid version")),
                };
            }
        }

        if opts.contains_key("body").is_ok() {
            let value = opts.get::<_, LuaValue>("body")?;
            if value.is_string() {
                body = opts.get::<_, String>("body")?;
            }
        }

        if opts.contains_key("method").is_ok() {
            let value = opts.get::<_, LuaValue>("method")?;
            if value.is_string() {
                let method_str = opts.get::<_, String>("method")?;
                method = reqwest::Method::from_bytes(method_str.as_bytes())
                    .map_err(LuaError::external)?;
            }
        }

        if opts.contains_key("tls_verify").is_ok() {
            let value = opts.get::<_, LuaValue>("tls_verify")?;
            if !value.is_nil() {
                let tls_verify = opts.get::<_, bool>("tls_verify")?;
                builder = builder.danger_accept_invalid_certs(!tls_verify);
            }
        }
    }

    builder = builder.tls_sni(true);
    let client = builder.build().map_err(LuaError::external)?;

    let res = client.request(method, &url).body(body).send();

    let res = match res {
        Ok(res) => res,
        Err(e) => return Err(LuaError::external(e)),
    };

    let headers = res.headers().clone();
    let status = res.status().as_u16();
    let body = res.bytes().map_err(LuaError::external)?;

    let table = lua.create_table()?;
    let body_bytes = lua.create_string(&body)?;
    if let Some(content_type) = headers.get("content-type") {
        if let Ok(content_type) = content_type.to_str() {
            if content_type.contains("application/json") {
                // let json: Value = res.json().map_err(LuaError::external)?;
                let mut hm: HashMap<String, Value> = serde_json::from_str(body_bytes.to_str()?).map_err(LuaError::external)?;
                let table = lua.create_table()?;
                for (k, v) in hm {
                    table.set(k, v.to_string())?;
                }
                log::info!("json: {:?}", table);
                return Ok(table);
            }
        }
    }
    table.set("body", body_bytes)?;
    table.set("status", status)?;
    let lheaders = lua.create_table()?;
    for (name, value) in headers {
        let rname = match name {
            Some(name) => name.to_string(),
            None => continue,
        };
        let rvalue = match value.to_str() {
            Ok(value) => value,
            Err(_) => continue,
        };
        lheaders.set(rname, rvalue)?;
    }
    table.set("headers", lheaders)?;
    log::info!("table: {:?}", table);
    Ok(table)
}
