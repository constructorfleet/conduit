import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";

import App from "./App";

describe("Operator Console shell", () => {
  beforeEach(() => {
    sessionStorage.clear();
    localStorage.clear();
  });

  it("starts at Operator Access and stores bearer tokens in session memory", async () => {
    const user = userEvent.setup();
    render(<App />);

    expect(
      screen.getByRole("heading", { name: "Operator Access" }),
    ).toBeInTheDocument();

    await user.type(
      screen.getByLabelText("Management bearer token"),
      "management-token",
    );
    await user.click(screen.getByRole("button", { name: "Connect" }));

    expect(screen.getByRole("tab", { name: "Overview" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(sessionStorage.getItem("conduit.operator.access")).toContain(
      "management-token",
    );
    expect(localStorage.getItem("conduit.operator.access")).toBeNull();
  });

  it("requires an explicit choice before local token persistence", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.type(
      screen.getByLabelText("Management bearer token"),
      "remembered-token",
    );
    await user.click(screen.getByLabelText("Remember on this browser"));
    await user.click(screen.getByRole("button", { name: "Connect" }));

    expect(localStorage.getItem("conduit.operator.access")).toContain(
      "remembered-token",
    );
    expect(sessionStorage.getItem("conduit.operator.access")).toBeNull();
  });

  it("enters explicit anonymous mode and exposes the five top-level sections", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(
      screen.getByRole("button", { name: "Use anonymous mode" }),
    );

    for (const section of [
      "Overview",
      "Pipelines",
      "Providers",
      "Events",
      "Settings",
    ]) {
      expect(screen.getByRole("tab", { name: section })).toBeInTheDocument();
    }
    expect(screen.getByText("Anonymous operator access")).toBeInTheDocument();
  });
});
