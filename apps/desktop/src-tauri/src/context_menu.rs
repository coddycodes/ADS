use serde::Deserialize;
use serde_json::{Map, Value};
use std::{collections::HashMap, sync::Mutex, time::Duration};
use tauri::{
    AppHandle, LogicalPosition, Manager, Runtime, WebviewWindow,
    menu::{MenuItemBuilder, PredefinedMenuItem, Submenu, SubmenuBuilder},
};
use tokio::sync::oneshot;
use uuid::Uuid;

const CONTEXT_MENU_ID_PREFIX: &str = "coddy-studio:context-menu:";
const CONTEXT_MENU_SELECTION_SETTLE_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ContextMenuPosition {
    x: f64,
    y: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NativeContextMenuEntry {
    Item(NativeContextMenuItem),
    Separator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeContextMenuItem {
    native_id: String,
    original_id: String,
    label: String,
    destructive: bool,
    disabled: bool,
    children: Vec<NativeContextMenuEntry>,
}

#[derive(Debug)]
pub(crate) struct NativeContextMenuRequest {
    request_id: String,
    items: Vec<NativeContextMenuEntry>,
    native_to_original: HashMap<String, String>,
}

struct PendingContextMenu {
    request_id: String,
    native_to_original: HashMap<String, String>,
    sender: Option<oneshot::Sender<String>>,
}

pub(crate) struct PendingContextMenuTicket {
    pub(crate) request_id: String,
    receiver: oneshot::Receiver<String>,
}

#[derive(Default)]
pub(crate) struct NativeContextMenuManager {
    pending: Mutex<Option<PendingContextMenu>>,
}

impl NativeContextMenuManager {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn begin(
        &self,
        request: &NativeContextMenuRequest,
    ) -> Result<PendingContextMenuTicket, String> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| "Could not acquire the native context menu state.".to_string())?;
        if pending.is_some() {
            return Err("A native context menu is already open.".to_string());
        }

        let (sender, receiver) = oneshot::channel();
        *pending = Some(PendingContextMenu {
            request_id: request.request_id.clone(),
            native_to_original: request.native_to_original.clone(),
            sender: Some(sender),
        });

        Ok(PendingContextMenuTicket {
            request_id: request.request_id.clone(),
            receiver,
        })
    }

    pub(crate) fn complete_if_context_menu_event(&self, native_id: &str) -> bool {
        if !native_id.starts_with(CONTEXT_MENU_ID_PREFIX) {
            return false;
        }

        let selected = self.pending.lock().ok().and_then(|mut pending| {
            let pending = pending.as_mut()?;
            let original_id = pending.native_to_original.get(native_id)?.clone();
            pending.sender.take().map(|sender| (sender, original_id))
        });

        if let Some((sender, original_id)) = selected {
            let _ = sender.send(original_id);
        }
        true
    }

    pub(crate) async fn finish_after_popup(
        &self,
        ticket: PendingContextMenuTicket,
    ) -> Option<String> {
        let selected = match tokio::time::timeout(
            CONTEXT_MENU_SELECTION_SETTLE_TIMEOUT,
            ticket.receiver,
        )
        .await
        {
            Ok(Ok(selected)) => Some(selected),
            _ => None,
        };
        self.cancel(&ticket.request_id);
        selected
    }

    pub(crate) fn cancel(&self, request_id: &str) {
        if let Ok(mut pending) = self.pending.lock()
            && pending
                .as_ref()
                .is_some_and(|pending| pending.request_id == request_id)
        {
            *pending = None;
        }
    }
}

pub(crate) fn context_menu_request_from_values(items: Vec<Value>) -> NativeContextMenuRequest {
    let request_id = Uuid::new_v4().to_string();
    let mut next_item_index = 0usize;
    let mut native_to_original = HashMap::new();
    let items = normalize_context_menu_items(
        &items,
        &request_id,
        &mut next_item_index,
        &mut native_to_original,
    );

    NativeContextMenuRequest {
        request_id,
        items,
        native_to_original,
    }
}

pub(crate) fn show_native_context_menu<R: Runtime>(
    window: &WebviewWindow<R>,
    request: &NativeContextMenuRequest,
    position: Option<ContextMenuPosition>,
) -> Result<(), String> {
    if request.items.is_empty() {
        return Ok(());
    }

    let menu = build_native_context_menu(window.app_handle(), request)?;
    if let Some(position) = normalize_context_menu_position(position) {
        window
            .popup_menu_at(&menu, position)
            .map_err(|error| format!("Could not show the native context menu: {error}"))
    } else {
        window
            .popup_menu(&menu)
            .map_err(|error| format!("Could not show the native context menu: {error}"))
    }
}

pub(crate) fn context_menu_request_has_selectable_items(
    request: &NativeContextMenuRequest,
) -> bool {
    !request.native_to_original.is_empty()
}

fn build_native_context_menu<R: Runtime>(
    app: &AppHandle<R>,
    request: &NativeContextMenuRequest,
) -> Result<Submenu<R>, String> {
    let root = SubmenuBuilder::with_id(
        app,
        format!("{CONTEXT_MENU_ID_PREFIX}{}:root", request.request_id),
        "Context Menu",
    )
    .build()
    .map_err(|error| format!("Could not build the native context menu: {error}"))?;
    append_context_menu_items(app, &root, &request.items)
        .map_err(|error| format!("Could not populate the native context menu: {error}"))?;
    Ok(root)
}

/// A row of one native menu level in display order. The separator the native
/// menu adds before its first destructive item is resolved here, and skipped
/// when an explicit separator already precedes that item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeMenuRow<'a> {
    Item(&'a NativeContextMenuItem),
    Separator,
}

fn native_menu_rows(entries: &[NativeContextMenuEntry]) -> Vec<NativeMenuRow<'_>> {
    let mut rows = Vec::with_capacity(entries.len() + 1);
    let mut inserted_destructive_separator = false;
    for entry in entries {
        match entry {
            NativeContextMenuEntry::Separator => rows.push(NativeMenuRow::Separator),
            NativeContextMenuEntry::Item(item) => {
                // Entries are normalised, so a non-empty `rows` always holds an item.
                if item.destructive && !inserted_destructive_separator {
                    if !rows.is_empty() && !matches!(rows.last(), Some(NativeMenuRow::Separator)) {
                        rows.push(NativeMenuRow::Separator);
                    }
                    inserted_destructive_separator = true;
                }
                rows.push(NativeMenuRow::Item(item));
            }
        }
    }
    rows
}

fn append_context_menu_items<R: Runtime>(
    app: &AppHandle<R>,
    menu: &Submenu<R>,
    entries: &[NativeContextMenuEntry],
) -> tauri::Result<()> {
    for row in native_menu_rows(entries) {
        match row {
            NativeMenuRow::Separator => {
                let separator = PredefinedMenuItem::separator(app)?;
                menu.append(&separator)?;
            }
            NativeMenuRow::Item(item) if item.children.is_empty() => {
                let native_item = MenuItemBuilder::with_id(item.native_id.clone(), &item.label)
                    .enabled(!item.disabled)
                    .build(app)?;
                menu.append(&native_item)?;
            }
            NativeMenuRow::Item(item) => {
                let submenu = SubmenuBuilder::with_id(app, item.native_id.clone(), &item.label)
                    .enabled(!item.disabled)
                    .build()?;
                append_context_menu_items(app, &submenu, &item.children)?;
                menu.append(&submenu)?;
            }
        }
    }

    Ok(())
}

fn normalize_context_menu_items(
    values: &[Value],
    request_id: &str,
    next_item_index: &mut usize,
    native_to_original: &mut HashMap<String, String>,
) -> Vec<NativeContextMenuEntry> {
    let entries = values
        .iter()
        .filter_map(|value| {
            normalize_context_menu_entry(value, request_id, next_item_index, native_to_original)
        })
        .collect();
    normalize_separators(entries)
}

/// Drops leading and trailing separators and collapses runs. Runs after the
/// entries a native menu cannot show (headers, empty submenus) are filtered.
fn normalize_separators(entries: Vec<NativeContextMenuEntry>) -> Vec<NativeContextMenuEntry> {
    let mut normalized: Vec<NativeContextMenuEntry> = Vec::with_capacity(entries.len());
    for entry in entries {
        let is_separator = matches!(entry, NativeContextMenuEntry::Separator);
        if is_separator
            && matches!(
                normalized.last(),
                None | Some(NativeContextMenuEntry::Separator)
            )
        {
            continue;
        }
        normalized.push(entry);
    }
    if matches!(normalized.last(), Some(NativeContextMenuEntry::Separator)) {
        normalized.pop();
    }
    normalized
}

fn normalize_context_menu_entry(
    value: &Value,
    request_id: &str,
    next_item_index: &mut usize,
    native_to_original: &mut HashMap<String, String>,
) -> Option<NativeContextMenuEntry> {
    let object = value.as_object()?;
    // A separator carries no id or label, so it is read before those requirements.
    if bool_property(object, "separator") {
        return Some(NativeContextMenuEntry::Separator);
    }
    if bool_property(object, "header") {
        return None;
    }

    let original_id = string_property(object, "id")?.to_string();
    let base_label = string_property(object, "label")?;
    let label = match (
        bool_property(object, "disabled"),
        string_property(object, "description"),
    ) {
        (true, Some(reason)) if !reason.trim().is_empty() => format!("{base_label} — {reason}"),
        _ => base_label.to_string(),
    };
    let children = object
        .get("children")
        .and_then(Value::as_array)
        .map(|children| {
            normalize_context_menu_items(children, request_id, next_item_index, native_to_original)
        })
        .unwrap_or_default();

    if object.contains_key("children") && children.is_empty() {
        return None;
    }

    let native_id = format!("{CONTEXT_MENU_ID_PREFIX}{request_id}:{}", *next_item_index);
    *next_item_index += 1;

    if children.is_empty() {
        native_to_original.insert(native_id.clone(), original_id.clone());
    }

    Some(NativeContextMenuEntry::Item(NativeContextMenuItem {
        native_id,
        original_id,
        label,
        destructive: bool_property(object, "destructive"),
        disabled: bool_property(object, "disabled"),
        children,
    }))
}

fn normalize_context_menu_position(
    position: Option<ContextMenuPosition>,
) -> Option<LogicalPosition<f64>> {
    position
        .filter(|position| {
            position.x.is_finite()
                && position.y.is_finite()
                && position.x >= 0.0
                && position.y >= 0.0
        })
        .map(|position| LogicalPosition::new(position.x.floor(), position.y.floor()))
}

fn string_property<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}

fn bool_property(object: &Map<String, Value>, key: &str) -> bool {
    object.get(key).and_then(Value::as_bool).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(entry: &NativeContextMenuEntry) -> &NativeContextMenuItem {
        match entry {
            NativeContextMenuEntry::Item(item) => item,
            NativeContextMenuEntry::Separator => panic!("expected an item, found a separator"),
        }
    }

    fn entry_kinds(entries: &[NativeContextMenuEntry]) -> Vec<&str> {
        entries
            .iter()
            .map(|entry| match entry {
                NativeContextMenuEntry::Item(item) => item.original_id.as_str(),
                NativeContextMenuEntry::Separator => "---",
            })
            .collect()
    }

    fn row_kinds<'a>(rows: &[NativeMenuRow<'a>]) -> Vec<&'a str> {
        rows.iter()
            .map(|row| match *row {
                NativeMenuRow::Item(item) => item.original_id.as_str(),
                NativeMenuRow::Separator => "---",
            })
            .collect()
    }

    #[test]
    fn normalizes_context_menu_items_for_native_menus() {
        let request = context_menu_request_from_values(vec![
            json!({ "id": "header", "label": "Group", "header": true }),
            json!({ "label": "Missing id" }),
            json!({ "id": "open", "label": "Open" }),
            json!({
                "id": "share",
                "label": "Share",
                "children": [
                    { "id": "copy-link", "label": "Copy link", "disabled": true },
                    { "id": "empty", "label": "Empty", "children": [] }
                ]
            }),
            json!({ "id": "delete", "label": "Delete", "destructive": true }),
        ]);

        assert_eq!(request.items.len(), 3);
        assert_eq!(item(&request.items[0]).original_id, "open");
        assert_eq!(item(&request.items[1]).original_id, "share");
        assert_eq!(item(&request.items[1]).children.len(), 1);
        assert_eq!(
            item(&item(&request.items[1]).children[0]).original_id,
            "copy-link"
        );
        assert!(item(&item(&request.items[1]).children[0]).disabled);
        assert!(item(&request.items[2]).destructive);
        assert_eq!(request.native_to_original.len(), 3);
        assert!(request.native_to_original.values().any(|id| id == "open"));
        assert!(
            request
                .native_to_original
                .values()
                .any(|id| id == "copy-link")
        );
        assert!(request.native_to_original.values().any(|id| id == "delete"));
        assert!(context_menu_request_has_selectable_items(&request));
        assert!(!context_menu_request_has_selectable_items(
            &context_menu_request_from_values(vec![json!({
                "id":"header",
                "label":"Header",
                "header":true
            })])
        ));
    }

    #[test]
    fn keeps_explicit_separators_without_ids_between_items() {
        let request = context_menu_request_from_values(vec![
            json!({ "id": "open", "label": "Open" }),
            json!({ "separator": true }),
            json!({ "id": "copy", "label": "Copy" }),
        ]);

        assert_eq!(entry_kinds(&request.items), vec!["open", "---", "copy"]);
        assert_eq!(request.native_to_original.len(), 2);
        let disabled = context_menu_request_from_values(vec![json!({
            "id": "pull", "label": "Pull", "disabled": true,
            "description": "Workspace is unavailable."
        })]);
        assert_eq!(
            item(&disabled.items[0]).label,
            "Pull — Workspace is unavailable."
        );
    }

    #[test]
    fn trims_and_collapses_separators_after_filtering() {
        let request = context_menu_request_from_values(vec![
            json!({ "separator": true }),
            json!({ "id": "header", "label": "Group", "header": true }),
            json!({ "id": "open", "label": "Open" }),
            json!({ "separator": true }),
            json!({ "id": "empty", "label": "Empty", "children": [{ "separator": true }] }),
            json!({ "separator": true }),
            json!({ "id": "copy", "label": "Copy" }),
            json!({ "separator": true }),
        ]);
        assert_eq!(entry_kinds(&request.items), vec!["open", "---", "copy"]);

        let nested = context_menu_request_from_values(vec![json!({
            "id": "open-in",
            "label": "Open in",
            "children": [
                { "id": "a", "label": "A" },
                { "separator": true },
                { "separator": true },
                { "id": "b", "label": "B" },
                { "separator": true }
            ]
        })]);
        assert_eq!(
            entry_kinds(&item(&nested.items[0]).children),
            vec!["a", "---", "b"]
        );
    }

    #[test]
    fn never_separates_a_leading_destructive_group_at_root_or_in_a_submenu() {
        let values = vec![
            json!({ "separator": true }),
            json!({ "id": "delete", "label": "Delete", "destructive": true }),
            json!({ "id": "purge", "label": "Purge", "destructive": true }),
            json!({ "separator": true }),
            json!({ "separator": true }),
            json!({ "id": "copy", "label": "Copy" }),
            json!({ "separator": true }),
        ];
        let root = context_menu_request_from_values(values.clone());
        assert_eq!(
            row_kinds(&native_menu_rows(&root.items)),
            vec!["delete", "purge", "---", "copy"]
        );
        let nested = context_menu_request_from_values(vec![json!({
            "id": "remove", "label": "Remove Project…", "children": values
        })]);
        assert_eq!(
            row_kinds(&native_menu_rows(&item(&nested.items[0]).children)),
            vec!["delete", "purge", "---", "copy"]
        );
    }

    #[test]
    fn skips_the_automatic_destructive_separator_after_an_explicit_one() {
        let explicit = context_menu_request_from_values(vec![
            json!({ "id": "rename", "label": "Rename" }),
            json!({ "separator": true }),
            json!({ "id": "delete", "label": "Delete", "destructive": true }),
        ]);
        assert_eq!(
            row_kinds(&native_menu_rows(&explicit.items)),
            vec!["rename", "---", "delete"]
        );

        let implicit = context_menu_request_from_values(vec![
            json!({ "id": "rename", "label": "Rename" }),
            json!({ "id": "delete", "label": "Delete", "destructive": true }),
            json!({ "id": "purge", "label": "Purge", "destructive": true }),
        ]);
        assert_eq!(
            row_kinds(&native_menu_rows(&implicit.items)),
            vec!["rename", "---", "delete", "purge"]
        );

        let leading = context_menu_request_from_values(vec![
            json!({ "id": "delete", "label": "Delete", "destructive": true }),
            json!({ "id": "rename", "label": "Rename" }),
        ]);
        assert_eq!(
            row_kinds(&native_menu_rows(&leading.items)),
            vec!["delete", "rename"]
        );
    }

    #[test]
    fn normalizes_context_menu_position_to_non_negative_logical_pixels() {
        assert_eq!(
            normalize_context_menu_position(Some(ContextMenuPosition { x: 12.9, y: 3.2 })),
            Some(LogicalPosition::new(12.0, 3.0))
        );
        assert_eq!(
            normalize_context_menu_position(Some(ContextMenuPosition { x: -1.0, y: 3.0 })),
            None
        );
        assert_eq!(
            normalize_context_menu_position(Some(ContextMenuPosition {
                x: f64::NAN,
                y: 3.0
            })),
            None
        );
    }

    #[tokio::test]
    async fn manager_resolves_matching_context_menu_events() {
        let manager = NativeContextMenuManager::new();
        let request = context_menu_request_from_values(vec![json!({
            "id": "open",
            "label": "Open"
        })]);
        let native_id = request
            .native_to_original
            .iter()
            .find_map(|(native_id, original_id)| {
                (original_id == "open").then_some(native_id.clone())
            })
            .expect("native id for open item");
        let ticket = manager.begin(&request).expect("begin context menu");

        assert!(manager.complete_if_context_menu_event(&native_id));
        assert_eq!(
            manager.finish_after_popup(ticket).await,
            Some("open".to_string())
        );
    }

    #[tokio::test]
    async fn manager_times_out_without_selection() {
        let manager = NativeContextMenuManager::new();
        let request = context_menu_request_from_values(vec![json!({
            "id": "open",
            "label": "Open"
        })]);
        let ticket = manager.begin(&request).expect("begin context menu");

        assert_eq!(manager.finish_after_popup(ticket).await, None);
    }
}
