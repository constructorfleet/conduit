import { describe, expect, it } from "vitest";

import viteConfig, {
  conduitApiTarget,
  conduitVoxTarget,
} from "./vite.config.ts";

describe("Vite dev server proxy", () => {
  it("forwards service API requests to the local Conduit service", () => {
    expect(conduitApiTarget).toBe("http://127.0.0.1:8080");
    expect(conduitVoxTarget).toBe("http://127.0.0.1:8080");
    expect(viteConfig.server?.proxy).toMatchObject({
      "/v1": {
        target: "http://127.0.0.1:8080",
        changeOrigin: true,
        secure: false,
        ws: true,
      },
      "/vox": {
        target: "http://127.0.0.1:8080",
        changeOrigin: true,
        secure: false,
        ws: true,
      },
    });
  });
});
