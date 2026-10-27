import { describe, expect, it } from "vite-plus/test";

import { createMemoryStorage, resolveStorage } from "./storage";

describe("resolveStorage", () => {
  it("reads Coddy Studio values", () => {
    const base = createMemoryStorage();
    base.setItem("coddy-studio:panel", "saved");
    const storage = resolveStorage(base);

    expect(storage.getItem("coddy-studio:panel")).toBe("saved");
    expect(base.getItem("coddy-studio:panel")).toBe("saved");
    expect(base.getItem("coddy-studio:panel")).toBe("saved");
  });
});
