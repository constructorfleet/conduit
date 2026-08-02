import { describe, expect, it } from "vitest";

import { createConduitApiClient } from "./client";

function errorResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("conduit api client errors", () => {
  it("surfaces the detail the API returned", async () => {
    const client = createConduitApiClient({
      baseUrl: "http://conduit.test",
      fetch: async () =>
        errorResponse(422, {
          error: "invalid",
          detail: "no providers are configured",
        }),
    });

    await expect(client.testPipeline("kitchen")).rejects.toThrow(
      "no providers are configured",
    );
  });

  it("falls back to the status when the body carries no detail", async () => {
    const client = createConduitApiClient({
      baseUrl: "http://conduit.test",
      fetch: async () => new Response("nope", { status: 503 }),
    });

    await expect(client.listPipelines()).rejects.toThrow(
      /Conduit API request failed: 503/,
    );
  });
});
