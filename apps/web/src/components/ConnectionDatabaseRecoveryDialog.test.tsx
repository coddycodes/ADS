// @vitest-environment happy-dom

import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

vi.mock("./ui/dialog", () => {
  const passthrough = ({ children, ...props }: React.HTMLAttributes<HTMLDivElement>) => (
    <div {...props}>{children}</div>
  );
  return {
    Dialog: ({ open, children }: { open: boolean; children?: ReactNode }) =>
      open ? <div>{children}</div> : null,
    DialogDescription: passthrough,
    DialogFooter: passthrough,
    DialogHeader: passthrough,
    DialogPanel: passthrough,
    DialogPopup: ({
      showCloseButton: _showCloseButton,
      ...props
    }: React.HTMLAttributes<HTMLDivElement> & { showCloseButton?: boolean }) => <div {...props} />,
    DialogTitle: passthrough,
  };
});
vi.mock("./ui/button", () => ({
  Button: (props: React.ButtonHTMLAttributes<HTMLButtonElement>) => <button {...props} />,
}));

import {
  monitorConnectionDatabaseOpenRequest,
  reportConnectionDatabaseUnavailable,
  resetConnectionDatabaseHealthForTest,
} from "../connection/databaseHealth";
import { ConnectionDatabaseRecoveryDialog } from "./ConnectionDatabaseRecoveryDialog";

class FakeRequest extends EventTarget implements IDBOpenDBRequest {
  error: DOMException | null = null;
  onblocked: ((this: IDBOpenDBRequest, ev: IDBVersionChangeEvent) => unknown) | null = null;
  onerror: ((this: IDBRequest<IDBDatabase>, ev: Event) => unknown) | null = null;
  onsuccess: ((this: IDBRequest<IDBDatabase>, ev: Event) => unknown) | null = null;
  onupgradeneeded: ((this: IDBOpenDBRequest, ev: IDBVersionChangeEvent) => unknown) | null = null;
  readyState: IDBRequestReadyState = "pending";
  result = {} as IDBDatabase;
  source = {} as IDBObjectStore;
  transaction = null;

  fire(type: string): void {
    if (this.readyState === "done") throw new Error("IndexedDB request already settled");
    if (type === "success" || type === "error") this.readyState = "done";
    this.dispatchEvent(new Event(type));
  }
}

let container: HTMLDivElement;
let root: Root;

function button(text: string): HTMLButtonElement {
  const match = Array.from(container.querySelectorAll("button")).find((entry) =>
    entry.textContent?.includes(text),
  );
  expect(match).toBeDefined();
  return match!;
}

function acknowledgementCheckbox(): HTMLInputElement {
  const match = container.querySelector<HTMLInputElement>('input[type="checkbox"]');
  expect(match).not.toBeNull();
  return match!;
}

function publishOpenEvent(type: "blocked" | "error", error?: DOMException): void {
  const request = new FakeRequest();
  request.error = error ?? null;
  monitorConnectionDatabaseOpenRequest(request);
  request.fire(type);
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  resetConnectionDatabaseHealthForTest();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  resetConnectionDatabaseHealthForTest();
  vi.unstubAllGlobals();
});

describe("ConnectionDatabaseRecoveryDialog", () => {
  it("offers the acknowledged reset for WebKit's permanent database-file failure", async () => {
    publishOpenEvent(
      "error",
      new DOMException("Unable to establish IDB database file", "UnknownError"),
    );
    const deleteDatabase = vi.fn(async () => "deleted" as const);
    const reloadPage = vi.fn();
    await act(async () => {
      root.render(
        <ConnectionDatabaseRecoveryDialog
          deleteDatabase={deleteDatabase}
          reloadPage={reloadPage}
        />,
      );
    });

    expect(container.textContent).toContain("Connection database needs reset");
    expect(container.textContent).toContain(
      "This Coddy Studio build uses an older browser engine than the one that last wrote its connection data, so the connection database cannot be opened.",
    );
    await act(async () => button("Reset saved connection data").click());
    expect(deleteDatabase).not.toHaveBeenCalled();
    expect(button("Delete saved connection data").disabled).toBe(true);

    await act(async () => acknowledgementCheckbox().click());
    expect(button("Delete saved connection data").disabled).toBe(false);
    await act(async () => button("Delete saved connection data").click());
    expect(deleteDatabase).toHaveBeenCalledOnce();
    expect(reloadPage).toHaveBeenCalledOnce();
  });

  it("requires a separate acknowledged confirmation that a double-click cannot trigger", async () => {
    publishOpenEvent("error", new DOMException("newer database", "VersionError"));
    const deleteDatabase = vi.fn(async () => "deleted" as const);
    const reloadPage = vi.fn();
    await act(async () => {
      root.render(
        <ConnectionDatabaseRecoveryDialog
          deleteDatabase={deleteDatabase}
          reloadPage={reloadPage}
        />,
      );
    });

    for (const text of [
      "Saved remote servers",
      "Connection credentials",
      "Accepted storage identities",
      "Cached environment shell state",
      "Cached thread state",
    ]) {
      expect(container.textContent).toContain(text);
    }
    expect(button("Reload").disabled).toBe(false);
    expect(deleteDatabase).not.toHaveBeenCalled();
    expect(button("Reset saved connection data").getAttribute("aria-expanded")).toBe("false");
    await act(async () => button("Reset saved connection data").click());
    await act(async () => button("Reset saved connection data").click());
    expect(deleteDatabase).not.toHaveBeenCalled();
    expect(container.textContent).toContain("cannot be undone");
    expect(button("Reset saved connection data").getAttribute("aria-expanded")).toBe("true");
    expect(button("Reset saved connection data").getAttribute("aria-controls")).toBe(
      "connection-database-reset-confirmation",
    );
    expect(button("Delete saved connection data").disabled).toBe(true);
    expect(document.activeElement).toBe(acknowledgementCheckbox());

    await act(async () => acknowledgementCheckbox().click());
    expect(button("Delete saved connection data").disabled).toBe(false);
    await act(async () => button("Delete saved connection data").click());

    expect(deleteDatabase).toHaveBeenCalledOnce();
    expect(reloadPage).toHaveBeenCalledOnce();
  });

  it("omits catalog categories on a protected desktop where they are not deleted", async () => {
    // The reset only drops the IndexedDB database; on a protected desktop the
    // catalog (servers, credentials, identities) lives in the native store,
    // so listing it as deleted would be a false purge assurance.
    vi.stubGlobal("window", {
      desktopBridge: {
        getConnectionCatalog: vi.fn(),
        compareAndSetConnectionCatalog: vi.fn(),
      },
    });
    publishOpenEvent("error", new DOMException("newer database", "VersionError"));
    await act(async () => {
      root.render(
        <ConnectionDatabaseRecoveryDialog deleteDatabase={vi.fn()} reloadPage={vi.fn()} />,
      );
    });

    expect(container.textContent).toContain("Cached environment shell state");
    expect(container.textContent).toContain("Cached thread state");
    expect(container.textContent).not.toContain("Saved remote servers");
    expect(container.textContent).not.toContain("Connection credentials");
    expect(container.textContent).not.toContain("Accepted storage identities");
  });

  it("reloads only after successful deletion", async () => {
    publishOpenEvent("error", new DOMException("newer database", "VersionError"));
    const reloadPage = vi.fn();
    await act(async () => {
      root.render(
        <ConnectionDatabaseRecoveryDialog
          deleteDatabase={async () => "deleted"}
          reloadPage={reloadPage}
        />,
      );
    });

    await act(async () => button("Reset saved connection data").click());
    await act(async () => acknowledgementCheckbox().click());
    await act(async () => button("Delete saved connection data").click());
    expect(reloadPage).toHaveBeenCalledOnce();
  });

  it("keeps a blocked deletion visibly pending and reloads after its later success", async () => {
    publishOpenEvent("error", new DOMException("newer database", "VersionError"));
    const deletion = new FakeRequest();
    vi.stubGlobal("indexedDB", {
      deleteDatabase: () => deletion,
    });
    const reloadPage = vi.fn();
    await act(async () => {
      root.render(<ConnectionDatabaseRecoveryDialog reloadPage={reloadPage} />);
    });

    await act(async () => button("Reset saved connection data").click());
    await act(async () => acknowledgementCheckbox().click());
    await act(async () => button("Delete saved connection data").click());
    expect(button("Deleting…").disabled).toBe(true);
    expect(button("Reload").disabled).toBe(false);
    await act(async () => deletion.fire("blocked"));

    expect(container.textContent).toContain("queued");
    expect(container.textContent).toContain("pending");
    expect(reloadPage).not.toHaveBeenCalled();

    await act(async () => deletion.fire("success"));
    expect(reloadPage).toHaveBeenCalledOnce();
  });

  it("does not reload when a blocked deletion later fails", async () => {
    publishOpenEvent("error", new DOMException("newer database", "VersionError"));
    const deletion = new FakeRequest();
    vi.stubGlobal("indexedDB", {
      deleteDatabase: () => deletion,
    });
    const reloadPage = vi.fn();
    await act(async () => {
      root.render(<ConnectionDatabaseRecoveryDialog reloadPage={reloadPage} />);
    });

    await act(async () => button("Reset saved connection data").click());
    await act(async () => acknowledgementCheckbox().click());
    await act(async () => button("Delete saved connection data").click());
    await act(async () => deletion.fire("blocked"));
    deletion.error = new DOMException("deletion denied", "UnknownError");
    await act(async () => deletion.fire("error"));

    expect(container.textContent).toContain("could not be deleted");
    expect(container.querySelectorAll('[role="alert"]')).toHaveLength(1);
    expect(reloadPage).not.toHaveBeenCalled();
  });

  it("keeps blocked and generic unavailable recovery non-destructive", async () => {
    publishOpenEvent("blocked");
    const reloadPage = vi.fn();
    await act(async () => {
      root.render(<ConnectionDatabaseRecoveryDialog reloadPage={reloadPage} />);
    });
    expect(container.textContent).toContain("Connection database is blocked");
    expect(container.textContent).not.toContain("Reset saved connection data");

    const copyText = vi.fn(async () => undefined);
    await act(async () => {
      reportConnectionDatabaseUnavailable("open denied");
      root.render(<ConnectionDatabaseRecoveryDialog reloadPage={reloadPage} copyText={copyText} />);
    });
    await act(async () => button("Copy diagnostics").click());
    expect(copyText).toHaveBeenCalledWith(expect.stringContaining("unavailable"));
  });
});
