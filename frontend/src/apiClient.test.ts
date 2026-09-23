import { afterEach, describe, expect, it, vi } from "vitest";

import { createSnapshotClient } from "./apiClient";
import { eventEnvelopeFixtures } from "./contracts/events";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("authenticated event stream", () => {
  it("sends bearer authorization and decodes split server events", async () => {
    const event = eventEnvelopeFixtures.find(
      (envelope) => envelope.event.type === "StageFailed",
    );
    if (!event) {
      throw new Error("expected a StageFailed event fixture");
    }

    const encoder = new TextEncoder();
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        const frame = `event: StageFailed\ndata: ${JSON.stringify(event)}\n\n`;
        controller.enqueue(encoder.encode(frame.slice(0, 31)));
        controller.enqueue(encoder.encode(frame.slice(31)));
        controller.close();
      },
    });
    const requests: Array<[RequestInfo | URL, RequestInit?]> = [];
    const request = vi.fn(
      async (input: RequestInfo | URL, init?: RequestInit) => {
        requests.push([input, init]);
        return new Response(body);
      },
    );
    vi.stubGlobal("fetch", request);
    const client = createSnapshotClient({
      baseUrl: "http://conduit.test",
      access: {
        mode: "bearer",
        token: "management-token",
        persisted: false,
      },
    });
    const received: unknown[] = [];
    const opened = vi.fn();

    await client.streamEvents((envelope) => received.push(envelope), opened);

    expect(requests[0]?.[0]).toEqual(
      new URL("/v1/events", "http://conduit.test"),
    );
    expect(requests[0]?.[1]?.headers).toEqual({
      accept: "text/event-stream",
      authorization: "Bearer management-token",
    });
    expect(opened).toHaveBeenCalledOnce();
    expect(received).toEqual([event]);
  });
});
