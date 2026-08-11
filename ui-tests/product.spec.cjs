const { test, expect } = require("@playwright/test");

async function resetDemo(page) {
  await page.goto("/?surface=popover");
  await page.evaluate(async () => {
    const token = window.SIGNOFF.token;
    await fetch("/api/refresh", { method: "POST", headers: { "X-Signoff-Token": token } });
  });
  await page.reload();
  await expect(page.locator(".popover-shell")).toBeVisible();
}

test.describe("Signoff product surfaces", () => {
  test("shows the glance view across multiple repositories", async ({ page }) => {
    await resetDemo(page);

    await expect(page.getByText("1 failing · 2 need attention")).toBeVisible();
    await expect(page.getByRole("button", { name: /Add label-based partial signoff/ })).toBeVisible();
    await expect(page.getByRole("button", { name: /Fix race condition in poller/ })).toBeVisible();
    await expect(page.getByRole("button", { name: "All 3" })).toBeVisible();
  });

  test("runs a configured check and reaches ready to merge", async ({ page }) => {
    await resetDemo(page);

    await page.getByRole("button", { name: /Add label-based partial signoff/ }).click();
    await expect(page.getByText("Waiting on security")).toBeVisible();
    await page.getByRole("button", { name: "Run & sign off" }).click();

    await expect(page.getByText("Ready to merge")).toBeVisible();
    await expect(page.locator("#terminalOutput")).toContainText("signed off security");
    await expect(page.getByRole("button", { name: "Open pull request" })).toBeVisible();
  });

  test("blocks a configured run when the PR is not checked out", async ({ page }) => {
    await resetDemo(page);

    await page.getByRole("button", { name: /Fix race condition in poller/ }).click();
    await expect(page.getByText(/This checkout is not at the PR commit/)).toBeVisible();
    await expect(page.getByText("Checkout PR")).toBeVisible();
    await expect(page.getByRole("button", { name: "Run & sign off" })).toHaveCount(0);
  });

  test("opens the fuzzy command palette for a pending check", async ({ page }) => {
    await resetDemo(page);

    await page.getByRole("button", { name: /Add label-based partial signoff/ }).click();
    await page.keyboard.press("ControlOrMeta+K");
    await page.getByRole("textbox", { name: "Search actions and pull requests" }).fill("rns482");
    await expect(page.getByRole("option", { name: /Run security on #482/ })).toBeVisible();
  });

  test("covers every settings surface", async ({ page }) => {
    await page.setViewportSize({ width: 960, height: 720 });
    await resetDemo(page);
    await page.goto("/?surface=settings&tab=repositories");
    await expect(page.getByRole("heading", { name: "Tracked repositories" })).toBeVisible();

    const tabs = [
      ["Commands", "Local commands"],
      ["Branch rules", "Branch rules"],
      ["History", "Activity history"],
      ["Diagnostics", "Diagnostics"],
    ];
    for (const [tab, heading] of tabs) {
      await page.getByRole("button", { name: tab, exact: true }).click();
      await expect(page.getByRole("heading", { name: heading })).toBeVisible();
    }
  });

  test("honors dark mode and reduced motion", async ({ page }) => {
    await page.emulateMedia({ colorScheme: "dark", reducedMotion: "reduce" });
    await resetDemo(page);
    await expect(page.locator(".popover-shell")).toBeVisible();
    await expect(page.locator(".popover-shell")).toHaveCSS("background-color", "rgb(25, 26, 30)");
  });
});
