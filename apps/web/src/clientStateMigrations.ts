export const RETIRED_TERMINAL_DRAWER_STORAGE_KEY = "coddy-studio:terminal-state:v1";
// Sidebar widths dragged under the 256px default are retired with it; the
// 422px default applies once and later drags persist under the v2 key.
export const RETIRED_SIDEBAR_WIDTH_STORAGE_KEY = "chat_thread_sidebar_width";

export function runClientStateMigrationsV1(storage: Pick<Storage, "removeItem">): void {
  storage.removeItem(RETIRED_TERMINAL_DRAWER_STORAGE_KEY);
  storage.removeItem(RETIRED_SIDEBAR_WIDTH_STORAGE_KEY);
}
