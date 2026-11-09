use groove_core::Error;
use serde::Serialize;
use serde_json::{json, Value};
use std::io::{self, Write};

pub fn success(request_id: Option<&str>, result: impl Serialize) -> Value {
    json!({"api_version":1, "request_id": request_id, "ok": true, "result": result})
}
pub fn failure(request_id: Option<&str>, error: &Error) -> Value {
    json!({"api_version":1, "request_id": request_id, "ok":false,
        "error":{"code":error.code,"message":error.message,"retryable":error.retryable,"details":error.details}})
}
pub fn write_response(response: &Value) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, response)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}
