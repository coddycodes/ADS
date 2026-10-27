//! The `device_*` MCP tools: list, open, screenshot, and close Android emulators and iOS
//! simulators on this server. They exist only while `enableDevicePreviews` is on.

use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{PreviewScope, snapshot_tool_result, tool_failure};
use crate::device::{
    DeviceManager, DeviceOpError,
    catalog::{DeviceSnapshot, DeviceState, Platform},
};

/// Matches the manager's 180 s emulator boot timeout.
const BOOT_TIMEOUT: Duration = Duration::from_secs(180);
/// Widest screenshot an agent receives.
const SCREENSHOT_MAX_WIDTH: u32 = 1080;

/// The tools are on only while `DeviceManager::enabled` is true, the same gate the `device.*`
/// RPC methods use.
///
/// There is no per-call permission check, by invariant: a valid provider MCP credential is
/// issued only by `provider_runtime` when it launches a thread's provider, so holding the
/// credential is the operate grant, exactly as for the `preview_*` tools. If MCP credentials
/// are ever issued to lower-privileged principals, both tool families need a scope check.
#[derive(Clone)]
pub struct DeviceTools {
    manager: DeviceManager,
    boot_timeout: Duration,
}

impl DeviceTools {
    pub fn new(manager: DeviceManager) -> Self {
        Self {
            manager,
            boot_timeout: BOOT_TIMEOUT,
        }
    }

    /// Only the unix device-tool tests, which launch a fake SDK, shorten it.
    #[cfg(all(test, unix))]
    pub(super) fn with_boot_timeout(mut self, boot_timeout: Duration) -> Self {
        self.boot_timeout = boot_timeout;
        self
    }

    pub(super) fn enabled(&self) -> bool {
        self.manager.enabled()
    }

    /// Runs `operation` (the tool name without `device_`) for the thread `scope` names, and
    /// returns the MCP tool result. `cancellation` ends a wait for boot; the device stays
    /// with the manager and keeps booting.
    pub(super) async fn call(
        &self,
        scope: &PreviewScope,
        operation: &str,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Value {
        // Which thread drove the device; never the arguments, which name images and devices.
        tracing::debug!(
            environment_id = %scope.environment_id,
            thread_id = %scope.thread_id,
            operation,
            "device MCP tool call"
        );
        if !self.enabled() {
            return failure(&DeviceOpError::Disabled);
        }
        let result = match operation {
            "list" => Ok(self.list().await),
            "open" => self.open(&arguments, &cancellation).await,
            "screenshot" => self.screenshot(&arguments).await,
            "close" => self.close(&arguments).await,
            _ => return tool_failure("McpUnknownToolError", "Unknown tool."),
        };
        result.unwrap_or_else(|error| failure(&error))
    }

    async fn list(&self) -> Value {
        self.manager.refresh_now().await;
        let catalog =
            serde_json::to_value(&*self.manager.catalog().borrow()).unwrap_or_else(|_| json!({}));
        success(catalog)
    }

    async fn open(
        &self,
        arguments: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, DeviceOpError> {
        if let Some(unknown) = arguments.as_object().and_then(|arguments| {
            arguments
                .keys()
                .find(|key| !matches!(key.as_str(), "platform" | "imageId"))
        }) {
            return Err(DeviceOpError::InvalidInput(format!(
                "device_open has no {unknown} argument. Pass platform (android or ios) and imageId."
            )));
        }
        let platform = match string_argument(arguments, "platform")? {
            None | Some("android") => Platform::Android,
            Some("ios") => Platform::Ios,
            Some(_) => {
                return Err(DeviceOpError::InvalidInput(
                    "platform must be android or ios.".into(),
                ));
            }
        };
        let image_id = match string_argument(arguments, "imageId")? {
            Some(id) => id.to_owned(),
            None => self.only_image(platform).await?,
        };
        let device_id = self.manager.open(platform, &image_id).await?;
        let ready = tokio::select! {
            ready = self.manager.wait_ready(&device_id, self.boot_timeout) => ready,
            () = cancellation.cancelled() => {
                return Err(DeviceOpError::Emulator(
                    "The request was cancelled. The device keeps starting; device_list shows when it is ready."
                        .into(),
                ));
            }
        };
        match ready {
            Ok(device) => Ok(success(json!({ "deviceId": device_id, "device": device }))),
            // Turning the setting off stops the device, which ends the wait early.
            Err(_) if !self.manager.enabled() => Err(DeviceOpError::Disabled),
            Err(DeviceOpError::NotReady(DeviceState::Failed)) => {
                let failure = self.failure_of(&device_id);
                Err(DeviceOpError::Emulator(match failure {
                    Some(failure) => format!("The device failed to start: {failure}"),
                    None => "The device failed to start. Check device_list for details.".into(),
                }))
            }
            Err(DeviceOpError::NotReady(DeviceState::Starting | DeviceState::Booting)) => {
                Err(DeviceOpError::Emulator(
                    "The device is still booting. device_list shows when it is ready.".into(),
                ))
            }
            Err(error) => Err(error),
        }
    }

    fn failure_of(&self, device_id: &str) -> Option<String> {
        self.manager
            .catalog()
            .borrow()
            .devices
            .iter()
            .find(|device| device.device_id == device_id)
            .and_then(|device| device.failure.clone())
    }

    /// The id of the platform's only image, for a `device_open` without `imageId`.
    async fn only_image(&self, platform: Platform) -> Result<String, DeviceOpError> {
        let (devices, images) = match platform {
            Platform::Android => ("Android emulators", "virtual devices"),
            Platform::Ios => ("iOS simulators", "simulators"),
        };
        self.manager.refresh().await;
        let Some(listed) = self
            .manager
            .catalog()
            .borrow()
            .platforms
            .iter()
            .find(|listed| listed.platform == platform)
            .cloned()
        else {
            return Err(DeviceOpError::InvalidInput(format!(
                "This server cannot run {devices}."
            )));
        };
        let mut ids = listed.images.iter().map(|image| image.id.as_str());
        match (ids.next(), ids.next()) {
            (Some(id), None) => Ok(id.to_owned()),
            (None, _) => Err(DeviceOpError::Sdk(listed.availability)),
            _ => Err(DeviceOpError::InvalidInput(format!(
                "Several {images} exist. Pass imageId as one of: {}.",
                listed
                    .images
                    .iter()
                    .map(|image| image.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    async fn screenshot(&self, arguments: &Value) -> Result<Value, DeviceOpError> {
        let device_id = self.device_id(arguments)?;
        let image = self.manager.png(&device_id, SCREENSHOT_MAX_WIDTH).await?;
        Ok(snapshot_tool_result(json!({
            "deviceId": device_id,
            "platform": self.platform_of(&device_id),
            "screenshot": {
                "data": STANDARD.encode(&image.png),
                "mimeType": image.mime_type,
                "width": image.width,
                "height": image.height,
            },
        })))
    }

    async fn close(&self, arguments: &Value) -> Result<Value, DeviceOpError> {
        let Some(device_id) = string_argument(arguments, "deviceId")? else {
            return Err(DeviceOpError::InvalidInput(
                "deviceId is required. Call device_list to see the running device.".into(),
            ));
        };
        let platform = self.platform_of(device_id);
        self.manager.close(device_id).await?;
        Ok(success(
            json!({ "deviceId": device_id, "platform": platform, "closed": true }),
        ))
    }

    fn platform_of(&self, device_id: &str) -> Option<Platform> {
        self.manager
            .catalog()
            .borrow()
            .devices
            .iter()
            .find(|device| device.device_id == device_id)
            .map(|device| device.platform)
    }

    /// The `deviceId` argument, or the only device that is not stopped.
    fn device_id(&self, arguments: &Value) -> Result<String, DeviceOpError> {
        if let Some(id) = string_argument(arguments, "deviceId")? {
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

/// The named argument as a non-empty string; `None` when absent or empty, an input error when
/// present with another type.
fn string_argument<'a>(arguments: &'a Value, name: &str) -> Result<Option<&'a str>, DeviceOpError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.as_str()).filter(|value| !value.is_empty())),
        Some(_) => Err(DeviceOpError::InvalidInput(format!(
            "{name} must be a string."
        ))),
    }
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
    let on_server = "The device runs on the Coddy Studio server, not on this machine.";
    let kinds = "Android emulators and iOS simulators (macOS servers)";
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
            "List devices",
            format!(
                "List the {kinds}: per platform, whether it can run devices and the images it can start, and the running devices."
            ),
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
            true,
        ),
        tool(
            "device_open",
            "Start a device",
            format!(
                "Start one of the {kinds} and wait until it has booted. One device runs per platform. platform defaults to android. Omit imageId when the platform has exactly one image; device_list shows the ids."
            ),
            json!({
                "type": "object",
                "properties": {
                    "platform": {"type": "string", "enum": ["android", "ios"], "default": "android"},
                    "imageId": {
                        "type": "string",
                        "description": "The AVD name or simulator UDID, as device_list reports it."
                    }
                },
                "additionalProperties": false
            }),
            false,
        ),
        tool(
            "device_screenshot",
            "Screenshot a device",
            format!(
                "Take a screenshot of a running device, one of the {kinds}: a PNG at most 1080 px wide, or a full-size JPEG for a view-only iOS simulator. Omit deviceId when one device is running."
            ),
            json!({
                "type": "object",
                "properties": {"deviceId": {"type": "string"}},
                "additionalProperties": false
            }),
            true,
        ),
        tool(
            "device_close",
            "Stop a device",
            format!("Stop a running device, one of the {kinds}."),
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
