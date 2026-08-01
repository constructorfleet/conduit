import { beforeEach, describe, expect, it } from "vitest";

import {
  clearOperatorAccess,
  loadOperatorAccess,
  saveAnonymousAccess,
  saveBearerAccess,
} from "./operatorAccess";

describe("operator access storage", () => {
  beforeEach(() => {
    sessionStorage.clear();
    localStorage.clear();
  });

  it("keeps management tokens in session storage by default", () => {
    saveBearerAccess("management-token", false);

    expect(loadOperatorAccess()).toEqual({
      mode: "bearer",
      token: "management-token",
      persisted: false,
    });
    expect(sessionStorage.getItem("conduit.operator.access")).toContain(
      "management-token",
    );
    expect(localStorage.getItem("conduit.operator.access")).toBeNull();
  });

  it("persists a management token only after explicit remember choice", () => {
    saveBearerAccess("remembered-token", true);

    expect(loadOperatorAccess()).toEqual({
      mode: "bearer",
      token: "remembered-token",
      persisted: true,
    });
    expect(sessionStorage.getItem("conduit.operator.access")).toBeNull();
    expect(localStorage.getItem("conduit.operator.access")).toContain(
      "remembered-token",
    );
  });

  it("stores explicit anonymous mode without a bearer token", () => {
    saveAnonymousAccess();

    expect(loadOperatorAccess()).toEqual({
      mode: "anonymous",
      persisted: false,
    });
    expect(sessionStorage.getItem("conduit.operator.access")).toContain(
      "anonymous",
    );
    expect(localStorage.getItem("conduit.operator.access")).toBeNull();
  });

  it("clears both temporary and remembered access", () => {
    saveBearerAccess("remembered-token", true);

    clearOperatorAccess();

    expect(loadOperatorAccess()).toEqual({ mode: "none" });
    expect(sessionStorage.getItem("conduit.operator.access")).toBeNull();
    expect(localStorage.getItem("conduit.operator.access")).toBeNull();
  });
});
