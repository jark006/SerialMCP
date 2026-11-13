//! A short registry lock leases a port to one operation. Different ports never
//! hold each other's I/O lock; competing operations fail instead of queueing writes.
use crate::{
    mcp, output,
    serial::{PortStatus, SerialHub, normalize_port},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Default)]
pub struct SerialService {
    state: Mutex<State>,
    lifecycle: Mutex<()>,
}
struct State {
    hub: SerialHub,
    busy: BTreeMap<String, Busy>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            hub: SerialHub::new(),
            busy: BTreeMap::new(),
        }
    }
}
struct Busy {
    status: Option<PortStatus>,
    operation: String,
    request_id: Value,
    cancel: Arc<AtomicBool>,
    cancellable: bool,
}

struct Lease<'a> {
    service: &'a SerialService,
    hub: SerialHub,
    port: String,
    select: bool,
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        let mut state = self.service.state.lock().unwrap_or_else(|e| e.into_inner());
        state.hub.merge(&mut self.hub, &self.port, self.select);
        state.busy.remove(&self.port);
    }
}

impl SerialService {
    pub fn handle(&self, message: Value) -> Option<Value> {
        if !mcp::valid_request(&message) {
            return mcp::handle_message(message, &mut SerialHub::new());
        }
        let method = message.get("method").and_then(Value::as_str);
        if method == Some("notifications/cancelled")
            && message.get("jsonrpc") == Some(&json!("2.0"))
            && message.get("id").is_none()
        {
            if let Some(id) = message.pointer("/params/requestId") {
                let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let mut matching = state
                    .busy
                    .values()
                    .filter(|busy| busy.cancellable && &busy.request_id == id);
                if let Some(busy) = matching.next()
                    && matching.next().is_none()
                {
                    busy.cancel.store(true, Ordering::Relaxed);
                }
            }
            return None;
        }
        if method != Some("tools/call")
            || message.get("jsonrpc") != Some(&json!("2.0"))
            || message.get("id").is_none()
        {
            return mcp::handle_message(message, &mut SerialHub::new());
        }
        let name = message
            .pointer("/params/name")
            .and_then(Value::as_str)
            .unwrap_or("");
        if matches!(name, "serial_list_ports" | "serial_get_output")
            || !mcp::tool_names().contains(&name)
        {
            return mcp::handle_message(message, &mut SerialHub::new());
        }
        let id = message["id"].clone();
        let mut arguments = message
            .pointer("/params/arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !arguments.is_object() {
            return mcp::handle_message(message, &mut SerialHub::new());
        }
        let options = match mcp::response_options(&mut arguments) {
            Ok(options) => options,
            Err(error) => return Some(mcp::rpc_error(id, -32602, &error.message)),
        };
        if name == "serial_status" {
            if !arguments.as_object().unwrap().is_empty() {
                return Some(mcp::rpc_error(
                    id,
                    -32602,
                    "serial_status accepts only response_mode and output_limit",
                ));
            }
            let listed = SerialHub::list_ports();
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let mut status = json!(state.hub.status_with(listed));
            let active = state.hub.resolve_port(None).ok();
            let ports = status["ports"].as_array_mut().unwrap();
            for (port, busy) in &state.busy {
                ports.retain(|status| status["port"] != *port);
                let mut entry = busy
                    .status
                    .as_ref()
                    .map(|s| json!(s))
                    .unwrap_or_else(|| json!({"port": port}));
                entry["busy"] = json!(true);
                entry["operation"] = json!(busy.operation);
                entry["cancellable"] = json!(busy.cancellable);
                entry["active"] = json!(active.as_deref() == Some(port));
                entry["status_note"] = json!(
                    "Snapshot from before operation; hardware presence is not refreshed while busy."
                );
                ports.push(entry);
            }
            ports.sort_by(|a, b| a["port"].as_str().cmp(&b["port"].as_str()));
            return Some(mcp::rpc_result(
                id,
                output::success(name, status, options.0, options.1),
            ));
        }
        let explicit_port = match arguments.get("port") {
            Some(Value::String(port)) if !normalize_port(port).is_empty() => {
                Some(normalize_port(port))
            }
            Some(Value::Null) => None,
            Some(_) => return mcp::handle_message(message, &mut SerialHub::new()),
            None => None,
        };
        if name == "serial_cancel" {
            if arguments
                .as_object()
                .unwrap()
                .keys()
                .any(|key| key != "port")
            {
                return Some(mcp::rpc_error(
                    id,
                    -32602,
                    "serial_cancel accepts only port, response_mode, output_limit",
                ));
            }
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let port = match state.hub.resolve_port(explicit_port.as_deref()) {
                Ok(port) => port,
                Err(error) => return Some(failure(id, &error)),
            };
            let Some(busy) = state.busy.get(&port) else {
                return Some(failure(id, "no operation is running on this port"));
            };
            if !busy.cancellable {
                return Some(failure(
                    id,
                    "this operation cannot be cancelled; file transfers and driver writes must finish or reach their timeout",
                ));
            }
            busy.cancel.store(true, Ordering::Relaxed);
            return Some(mcp::rpc_result(
                id,
                output::success(
                    name,
                    json!({"port":port, "cancel_requested":true, "remote_interrupted":false}),
                    options.0,
                    options.1,
                ),
            ));
        }
        let lifecycle = matches!(name, "serial_open" | "serial_close" | "serial_reconnect");
        let _lifecycle_guard = if lifecycle {
            match self.lifecycle.try_lock() {
                Ok(guard) => Some(guard),
                Err(_) => {
                    return Some(failure(
                        id,
                        "another port open/close/reconnect is in progress; retry after it finishes",
                    ));
                }
            }
        } else {
            None
        };
        let mut lease = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let port = match state.hub.resolve_port(explicit_port.as_deref()) {
                Ok(port) => port,
                Err(_) => {
                    drop(state);
                    return mcp::handle_message(message, &mut SerialHub::new());
                }
            };
            if let Some(busy) = state.busy.get(&port) {
                return Some(failure(
                    id,
                    &format!(
                        "{port} is busy with {}; no bytes were written by this request",
                        busy.operation
                    ),
                ));
            }
            let status = state.hub.status_for(&port);
            let mut hub = state.hub.detach(&port);
            hub.reserved.extend(state.busy.keys().cloned());
            let cancel = hub.cancel.clone();
            state.busy.insert(
                port.clone(),
                Busy {
                    status,
                    operation: name.into(),
                    request_id: id.clone(),
                    cancel,
                    cancellable: matches!(
                        name,
                        "serial_read"
                            | "serial_run_command"
                            | "serial_exec"
                            | "serial_exchange"
                            | "serial_check_modem_tools"
                    ),
                },
            );
            Lease {
                service: self,
                hub,
                port,
                select: false,
            }
        };
        let mut operation_message = message.clone();
        if !matches!(name, "serial_open" | "serial_select") {
            if operation_message.pointer("/params/arguments").is_none() {
                operation_message["params"]["arguments"] = json!({});
            }
            operation_message["params"]["arguments"]["port"] = json!(lease.port);
        }
        let response = mcp::handle_message(operation_message, &mut lease.hub);
        lease.select = matches!(name, "serial_open" | "serial_reconnect" | "serial_select")
            && response
                .as_ref()
                .is_some_and(|r| r.get("error").is_none() && r["result"]["isError"] == false);
        response
    }
}

fn failure(id: Value, message: &str) -> Value {
    mcp::rpc_result(id, mcp::error_tool_result(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serial::test_driver::TestPort;
    use std::{sync::mpsc, time::Duration};

    fn request(id: i32, name: &str, arguments: Value) -> Value {
        json!({"jsonrpc":"2.0", "id":id, "method":"tools/call", "params":{"name":name,"arguments":arguments}})
    }
    fn data(response: &Value) -> Value {
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    #[test]
    fn two_live_ports_are_independent_and_cancel_returns_partial_data() {
        let service = Arc::new(SerialService::default());
        let (tx, rx) = mpsc::channel();
        let first = TestPort {
            waiting: Some(tx),
            incoming: [0, 255].into(),
            ..TestPort::default()
        };
        let first_writes = first.writes.clone();
        {
            let mut state = service.state.lock().unwrap();
            state.hub.insert_test_port("COM_A", first);
            state.hub.insert_test_port("COM_B", TestPort::default());
            state.hub.select_port("COM_A").unwrap();
        }
        let reader = service.clone();
        let thread = std::thread::spawn(move || {
            reader.handle(request(10, "serial_read", json!({"timeout_ms":10000})))
        });
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let busy = service
            .handle(request(
                11,
                "serial_write",
                json!({"port":"COM_A", "data":"FF"}),
            ))
            .unwrap();
        assert_eq!(busy["result"]["isError"], true);
        assert!(first_writes.lock().unwrap().is_empty());
        let second = service
            .handle(request(
                12,
                "serial_exchange",
                json!({"port":"COM_B", "data":"01 02 FE", "max_bytes":3}),
            ))
            .unwrap();
        assert_eq!(data(&second)["data"], "01 02 FE");
        let configured = service
            .handle(request(
                14,
                "serial_configure",
                json!({"port":"COM_B", "profile":"binary"}),
            ))
            .unwrap();
        assert_eq!(data(&configured)["active"], false);
        service
            .handle(request(13, "serial_select", json!({"port":"COM_B"})))
            .unwrap();
        assert!(service.handle(json!({"jsonrpc":"2.0", "method":"notifications/cancelled", "params":{"requestId":10}})).is_none());
        let result = thread.join().unwrap().unwrap();
        assert_eq!(data(&result)["data"], "00 FF");
        assert_eq!(data(&result)["stop_reason"], "cancelled");
        let state = service.state.lock().unwrap();
        assert!(state.busy.is_empty());
        assert_eq!(state.hub.resolve_port(None).unwrap(), "COM_B");
    }

    #[test]
    fn worker_panic_returns_port_ownership() {
        let service = SerialService::default();
        service.state.lock().unwrap().hub.insert_test_port(
            "COM_TEST",
            TestPort {
                panic_on_read: true,
                ..TestPort::default()
            },
        );
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            service.handle(request(1, "serial_read", json!({})))
        }));
        assert!(panic.is_err());
        let state = service.state.lock().unwrap();
        assert!(state.busy.is_empty());
        assert!(state.hub.status_for("COM_TEST").unwrap().open);
    }

    #[test]
    fn disconnect_returns_error_with_partial_bytes_and_retains_reconnect_state() {
        let service = SerialService::default();
        service.state.lock().unwrap().hub.insert_test_port(
            "COM_TEST",
            TestPort {
                incoming: [0, 255].into(),
                read_error_when_empty: Some(std::io::ErrorKind::BrokenPipe),
                ..TestPort::default()
            },
        );
        let response = service
            .handle(request(1, "serial_read", json!({})))
            .unwrap();
        assert_eq!(response["result"]["isError"], true);
        let result = data(&response);
        assert_eq!(result["data"], "00 FF");
        assert_eq!(result["stop_reason"], "error");
        let state = service.state.lock().unwrap();
        let port = state.hub.status_for("COM_TEST").unwrap();
        assert_eq!(port.state, "disconnected");
        assert!(port.active);
    }
}
