// Inject the fixed DOM helper into an isolated world per frame, and provide
// the high-level `call_helper` primitive the API handlers use to invoke it.
//
// The helper script bytes are checked in at
// `crates/injected-js/dist/snapshot-helper.js` and `include_str!`'d here.
// On daemon startup the SHA-256 is computed once and surfaced via
// `helper_sha256()`. Any tampering with the file makes the hash change; the
// CI gate verifies the file is the one we shipped.

use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use chromiumoxide::cdp::browser_protocol::page::{
    AddScriptToEvaluateOnNewDocumentParams, CreateIsolatedWorldParams, FrameId,
};
use chromiumoxide::cdp::js_protocol::runtime::{
    CallArgument, CallFunctionOnParams, ExecutionContextId, RemoteObject,
};
use chromiumoxide::Page;
use sha2::{Digest, Sha256};
use serde_json::Value;

const HELPER_JS: &str = include_str!("../../../injected-js/dist/snapshot-helper.js");
const ACB_WORLD: &str = "acb_world";

pub fn helper_source() -> &'static str {
    HELPER_JS
}

pub fn helper_sha256() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| {
        let mut h = Sha256::new();
        h.update(HELPER_JS.as_bytes());
        let d = h.finalize();
        let mut s = String::with_capacity(d.len() * 2);
        for b in d {
            s.push_str(&format!("{b:02x}"));
        }
        s
    })
}

/// Install the auto-injection hook on a page: every navigation will run the
/// helper in an isolated world named `acb_world`.
pub async fn install_auto_inject(page: &Page) -> Result<()> {
    page.execute(AddScriptToEvaluateOnNewDocumentParams {
        source: HELPER_JS.to_string(),
        world_name: Some(ACB_WORLD.to_string()),
        include_command_line_api: Some(false),
        run_immediately: Some(true),
    })
    .await?;
    Ok(())
}

/// Idempotently obtain the isolated-world execution context id for the
/// page's main frame. Each call to `createIsolatedWorld` returns the
/// existing context if one already exists for that frame+name pair (per CDP
/// docs); we just call it on demand.
pub async fn isolated_context(page: &Page) -> Result<ExecutionContextId> {
    let frame_id = page
        .mainframe()
        .await?
        .ok_or_else(|| anyhow!("no main frame"))?;
    let r = page
        .execute(CreateIsolatedWorldParams {
            frame_id: FrameId::from(frame_id.inner().clone()),
            world_name: Some(ACB_WORLD.to_string()),
            grant_univeral_access: Some(false),
        })
        .await?;
    Ok(r.execution_context_id.clone())
}

/// Invoke a method on `__acb` inside the isolated world. `method_name` is
/// the property of `__acb` to call; `args` are JSON-serialized and passed
/// positionally.
pub async fn call_helper(page: &Page, method: &str, args: &[Value]) -> Result<Value> {
    let ctx = isolated_context(page).await?;
    // We pass args as one JSON array and parse inside the call so we don't
    // have to convert each into `CallArgument`. The function declaration
    // re-extracts them. This keeps the IDL simple and avoids string-
    // concatenation eval surfaces.
    let args_json = serde_json::to_string(args)?;
    let function_declaration = format!(
        r#"function(rawArgs) {{
            const args = JSON.parse(rawArgs);
            const fn = __acb[{method:?}];
            if (typeof fn !== "function") throw new Error("no such helper: {method}");
            return fn.apply(null, args);
        }}"#,
        method = method
    );
    let arg = CallArgument {
        value: Some(Value::String(args_json)),
        unserializable_value: None,
        object_id: None,
    };
    let r = page
        .execute(
            CallFunctionOnParams::builder()
                .function_declaration(function_declaration)
                .execution_context_id(ctx)
                .return_by_value(true)
                .argument(arg)
                .build()
                .map_err(|e| anyhow!("CallFunctionOnParams build: {e}"))?,
        )
        .await?;
    if let Some(ex) = &r.result.exception_details {
        return Err(anyhow!("helper exception: {ex:?}"));
    }
    Ok(remote_value(&r.result.result))
}

fn remote_value(obj: &RemoteObject) -> Value {
    obj.value.clone().unwrap_or(Value::Null)
}
