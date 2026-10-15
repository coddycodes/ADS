//! The `device_*` MCP tools: list, open, screenshot, and close Android emulators on this
//! server. They exist only while `enableDevicePreviews` is on.

use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

use super::{snapshot_tool_result, tool_failure};
use crate::device::{
    DeviceManager, DeviceOpError,
    catalog::{DeviceSnapshot, DeviceState},
    grpc::proto::image_format::ImgFormat,
    input::oriented_size,
};

/// Matches the manager's 180 s emulator boot timeout.
const BOOT_TIMEOUT: Duration = Duration::from_secs(180);
/// Widest screenshot an agent receives.
const SCREENSHOT_MAX_WIDTH: u32 = 1080;

/// The tools are on only while `DeviceManager::enabled` is true, the same gate the `device.*`
/// RPC methods use.
#[derive(Clone)]
pub struct DeviceTools {
    manager: DeviceManager,
}

impl DeviceTools {
    pub fn new(manager: DeviceManager) -> Self {
        Self { manager }
    }

    pub(super) fn enabled(&self) -> bool {
        self.manager.enabled()
    }

    /// Runs `operation` (the tool name without `device_`) and returns the MCP tool result.
    /// `principal_can_operate` is false for a caller that may look but not change anything.
    pub(super) async fn call(
        &self,
        operation: &str,
        arguments: Value,
        principal_can_operate: bool,
    ) -> Value {
        if !self.enabled() {
            return failure(&DeviceOpError::Disabled);
        }
        let result = match operation {
            "list" => Ok(self.list().await),
            "open" => {
                self.guarded(principal_can_operate, self.open(&arguments))
                    .await
            }
            "screenshot" => self.screenshot(&arguments).await,
            "close" => {
                self.guarded(principal_can_operate, self.close(&arguments))
                    .await
            }
            _ => return tool_failure("McpUnknownToolError", "Unknown tool."),
        };
        result.unwrap_or_else(|error| failure(&error))
    }

    async fn guarded(
        &self,
        principal_can_operate: bool,
        operation: impl std::future::Future<Output = Result<Value, DeviceOpError>>,
    ) -> Result<Value, DeviceOpError> {
        if !principal_can_operate {
            return Err(DeviceOpError::InvalidInput(
                "This connection can only look at devices. Ask someone who can operate this thread to start or stop one."
                    .into(),
            ));
        }
        operation.await
    }

    async fn list(&self) -> Value {
        self.manager.refresh_now().await;
        let catalog =
            serde_json::to_value(&*self.manager.catalog().borrow()).unwrap_or_else(|_| json!({}));
        success(catalog)
    }

    async fn open(&self, arguments: &Value) -> Result<Value, DeviceOpError> {
        let avd_name = match string_argument(arguments, "avdName") {
            Some(name) => name.to_owned(),
            None => self.only_avd().await?,
        };
        let device_id = self.manager.open(&avd_name).await?;
        match self.manager.wait_ready(&device_id, BOOT_TIMEOUT).await {
            Ok(device) => Ok(success(json!({ "deviceId": device_id, "device": device }))),
            // Turning the setting off stops the device, which ends the wait early.
            Err(_) if !self.manager.enabled() => Err(DeviceOpError::Disabled),
            Err(error) => Err(error),
        }
    }

    async fn only_avd(&self) -> Result<String, DeviceOpError> {
        self.manager.refresh().await;
        let catalog = self.manager.catalog().borrow().clone();
        let mut names = catalog.avds.iter().map(|avd| avd.name.as_str());
        match (names.next(), names.next()) {
            (Some(name), None) => Ok(name.to_owned()),
            (None, _) => Err(DeviceOpError::Sdk(catalog.availability)),
            _ => Err(DeviceOpError::InvalidInput(format!(
                "Several virtual devices exist. Pass avdName as one of: {}.",
                catalog
                    .avds
                    .iter()
                    .map(|avd| avd.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    async fn screenshot(&self, arguments: &Value) -> Result<Value, DeviceOpError> {
        let device_id = self.device_id(arguments)?;
        let (client, _serial, (width, height), orientation) = self.manager.client(&device_id)?;
        let (width, _) = oriented_size(width, height, orientation);
        let image = client
            .screenshot(ImgFormat::Png, width.min(SCREENSHOT_MAX_WIDTH))
            .await
            .map_err(|error| {
                tracing::warn!(%error, "device screenshot failed");
                DeviceOpError::Emulator(
                    "The emulator did not return a screenshot. Try again in a moment.".into(),
                )
            })?;
        let size = image
            .format
            .as_ref()
            .map_or((0, 0), |format| (format.width, format.height));
        Ok(snapshot_tool_result(json!({
            "deviceId": device_id,
            "screenshot": {
                "data": STANDARD.encode(&image.image),
                "mimeType": "image/png",
                "width": size.0,
                "height": size.1,
            },
        })))
    }

    async fn close(&self, arguments: &Value) -> Result<Value, DeviceOpError> {
        let Some(device_id) = string_argument(arguments, "deviceId") else {
            return Err(DeviceOpError::InvalidInput(
                "deviceId is required. Call device_list to see the running device.".into(),
            ));
        };
        self.manager.close(device_id).await?;
        Ok(success(json!({ "deviceId": device_id, "closed": true })))
    }

    /// The `deviceId` argument, or the only device that is not stopped.
    fn device_id(&self, arguments: &Value) -> Result<String, DeviceOpError> {
        if let Some(id) = string_argument(arguments, "deviceId") {
            return Ok(id.to_owned());
        }
        let catalog = self.manager.catalog().borrow().clone();
        let live: Vec<&DeviceSnapshot> = catalog
            .devices
            .iter()
            .filter(|device| {
                !matches!(
                    device.state,
                    DeviceState::Stopping | DeviceState::Stopped | DeviceState::Failed
                )
            })
            .collect();
        match live.as_slice() {
            [device] => Ok(device.device_id.clone()),
            _ => Err(DeviceOpError::NotFound),
        }
    }
}

fn string_argument<'a>(arguments: &'a Value, name: &str) -> Option<&'a str> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn success(value: Value) -> Value {
    json!({
        "isError": false,
        "structuredContent": value,
        "content": [{"type": "text", "text": value.to_string()}],
    })
}

fn failure(error: &DeviceOpError) -> Value {
    let rpc = error.to_rpc_error();
    tool_failure(
        rpc["_tag"].as_str().unwrap_or("DeviceError"),
        &error.to_string(),
    )
}

pub(super) fn device_tool_descriptors() -> Vec<Value> {
    let on_server = "The device runs on the BiBCode server, not on this machine.";
    let tool = |name: &str, title: &str, description: String, schema: Value, read_only: bool| {
        json!({
            "name": name,
            "description": format!("{description} {on_server}"),
            "inputSchema": schema,
            "annotations": {
                "title": title,
                "readOnlyHint": read_only,
                "destructiveHint": !read_only,
                "idempotentHint": read_only,
                "openWorldHint": false
            }
        })
    };
    vec![
        tool(
            "device_list",
            "List Android devices",
            "List the Android virtual devices available to start and the device that is running."
                .into(),
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
            true,
        ),
        tool(
            "device_open",
            "Start an Android device",
            "Start an Android emulator and wait until it has booted. Only one device runs at a time. Omit avdName when exactly one virtual device exists."
                .into(),
            json!({
                "type": "object",
                "properties": {"avdName": {"type": "string"}},
                "additionalProperties": false
            }),
            false,
        ),
        tool(
            "device_screenshot",
            "Screenshot an Android device",
            "Take a PNG screenshot (at most 1080 px wide) of the running Android device. Omit deviceId when one device is running."
                .into(),
            json!({
                "type": "object",
                "properties": {"deviceId": {"type": "string"}},
                "additionalProperties": false
            }),
            true,
        ),
        tool(
            "device_close",
            "Stop an Android device",
            "Stop a running Android emulator.".into(),
            json!({
                "type": "object",
                "properties": {"deviceId": {"type": "string"}},
                "required": ["deviceId"],
                "additionalProperties": false
            }),
            false,
        ),
    ]
}
