import { describe, expect, it } from "vitest";

import { fieldLabel, fieldLabels } from "./fieldLabel";

describe("field labels", () => {
  it("separates words on underscores and title-cases them", () => {
    expect(fieldLabel("threshold_percent")).toBe("Threshold Percent");
    expect(fieldLabel("models_dir")).toBe("Models Dir");
    expect(fieldLabel("where")).toBe("Where");
  });

  it("upper-cases initialisms whole", () => {
    expect(fieldLabel("base_url")).toBe("Base URL");
    expect(fieldLabel("api_key")).toBe("API Key");
    expect(fieldLabel("url")).toBe("URL");
    expect(fieldLabel("speaker_id")).toBe("Speaker ID");
  });

  it("reads hyphens as word separators too", () => {
    expect(fieldLabel("max-rounds")).toBe("Max Rounds");
  });

  it("falls back to the wire spelling when there are no words to show", () => {
    // A control with an empty accessible name is worse than one named the way
    // the JSON spells it.
    expect(fieldLabel("__")).toBe("__");
  });

  it("lists several fields the way a sentence does", () => {
    expect(fieldLabels(["base_url", "model"])).toBe("Base URL, Model");
  });
});
