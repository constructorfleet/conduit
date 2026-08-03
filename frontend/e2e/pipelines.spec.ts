import { expect, test } from "@playwright/test";

/// Captures the Pipelines editor as it ships.
///
/// The console starts at Operator Access, so every run enters anonymous mode
/// before a section is reachable.
test("pipeline editor", async ({ page }, testInfo) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Use anonymous mode" }).click();
  await page.getByRole("tab", { name: "Pipelines" }).click();

  await expect(
    page.getByRole("heading", { name: "Pipeline Editor" }),
  ).toBeVisible();
  await expect(page.getByLabel("Pipeline stages")).toBeVisible();
  await expect(page.getByLabel("Model provider")).toBeVisible();

  // The chain scrolls on its own axis; the page must never scroll sideways.
  const overflow = await page.evaluate(
    () =>
      document.documentElement.scrollWidth -
      document.documentElement.clientWidth,
  );
  expect(overflow).toBeLessThanOrEqual(0);

  await page.screenshot({
    path: `../output/playwright/pipelines-${testInfo.project.name}.png`,
    fullPage: true,
  });
});
